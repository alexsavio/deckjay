//! Serves the music folder over HTTP so the speaker can download the tracks.

use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

/// `files` holds the paths, relative to `music_dir`, that the speaker may download.
pub fn spawn(music_dir: PathBuf, port: u16, files: HashSet<PathBuf>) -> Result<()> {
    // Bind here, so a port that is already in use is reported at startup.
    let listener = TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("cannot listen on port {port} (in use?)"))?;
    serve(listener, music_dir, files)
}

/// Serves on an already bound listener from a new `http` thread.
fn serve(listener: TcpListener, music_dir: PathBuf, files: HashSet<PathBuf>) -> Result<()> {
    listener.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let listener = {
        let _guard = runtime.enter();
        tokio::net::TcpListener::from_std(listener)?
    };
    let app = Router::new()
        .nest_service("/music", ServeDir::new(music_dir))
        .layer(middleware::from_fn_with_state(Arc::new(files), only_listed))
        .layer(TraceLayer::new_for_http());
    std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            if let Err(err) = runtime.block_on(async { axum::serve(listener, app).await }) {
                tracing::error!("web server stopped: {err}");
            }
        })?;
    Ok(())
}

/// `ServeDir` alone would also serve dotfiles, other files, and files behind
/// symlinks that lead out of the music folder.
async fn only_listed(
    State(files): State<Arc<HashSet<PathBuf>>>,
    req: Request,
    next: Next,
) -> Response {
    let listed = req
        .uri()
        .path()
        .strip_prefix("/music/")
        .and_then(|path| percent_decode_str(path).decode_utf8().ok())
        .is_some_and(|path| files.contains(Path::new(&*path)));
    if listed {
        next.run(req).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{scan, served_files, url_for};

    fn touch(path: &Path, body: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn start(root: &Path) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/music", listener.local_addr().unwrap());
        serve(
            listener,
            root.to_path_buf(),
            served_files(&scan(root).unwrap()),
        )
        .unwrap();
        base
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn status(url: &str) -> u16 {
        agent().get(url).call().unwrap().status().as_u16()
    }

    #[test]
    fn serves_every_track_and_cover_at_its_url() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in [
            "100% Kids #1?.mp3",
            "Kühe & Co+.mp3",
            "a;b=c'(x)!.mp3",
            " lead space.mp3",
            "cover.jpg",
        ] {
            touch(&root.join("Äl bum #2").join(name), b"x");
        }
        let base = start(root);

        let albums = scan(root).unwrap();
        let files = served_files(&albums);
        assert_eq!(files.len(), 5);
        for rel in &files {
            let url = url_for(&base, rel);
            assert_eq!(status(&url), 200, "{url}");
        }
    }

    #[test]
    fn refuses_files_the_scan_did_not_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"), b"x");
        touch(&root.join("Album/notes.txt"), b"x");
        touch(&root.join(".env"), b"x");
        let outside = tempfile::tempdir().unwrap();
        touch(&outside.path().join("secret.txt"), b"x");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.join("Album/escape")).unwrap();
        let base = start(root);

        for path in [
            "/.env",
            "/Album/notes.txt",
            "/Album/escape/secret.txt",
            "/Album/",
        ] {
            assert_eq!(status(&format!("{base}{path}")), 404, "{path}");
        }
    }

    #[test]
    fn answers_range_and_head_requests() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"), b"0123456789");
        let url = format!("{}/Album/01.mp3", start(root));

        let mut reply = agent()
            .get(&url)
            .header("range", "bytes=2-4")
            .call()
            .unwrap();
        assert_eq!(reply.status().as_u16(), 206);
        assert_eq!(reply.body_mut().read_to_string().unwrap(), "234");
        assert_eq!(agent().head(&url).call().unwrap().status().as_u16(), 200);
    }
}
