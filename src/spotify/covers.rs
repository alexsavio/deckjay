//! Playlist covers, fetched once from the Web API and kept in a folder
//! (`<state_dir>/spotify-covers/`), so keys show them without the network.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::api::{Client, Endpoints, Image};

/// Keys are at most 144 px; bigger pictures only cost memory on a Pi.
const SIDE: u32 = 512;
const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Where the cover of `spotify:playlist:<id>` is kept in `folder`.
pub fn path(folder: &Path, uri: &str) -> PathBuf {
    let id = uri.rsplit(':').next().unwrap_or(uri);
    folder.join(format!("{id}.jpg"))
}

/// Fetches into `folder` the covers of `uris` it does not keep yet, with the
/// login in `state_dir`. Failures are logged: a key without a cover shows
/// the Spotify glyph.
pub fn fetch_missing(state_dir: &Path, folder: &Path, uris: &[String], endpoints: Endpoints) {
    let missing: Vec<&String> = uris
        .iter()
        .filter(|uri| !path(folder, uri).is_file())
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut client = match Client::load(state_dir, endpoints) {
        Ok(client) => client,
        Err(err) => {
            warn!("no playlist covers: {err:#}");
            return;
        }
    };
    for uri in missing {
        match fetch(&mut client, folder, uri) {
            Ok(()) => info!(playlist = %uri, "playlist cover saved"),
            Err(err) => warn!("no cover for {uri}: {err:#}"),
        }
    }
}

fn fetch(client: &mut Client, folder: &Path, uri: &str) -> Result<()> {
    let id = uri.rsplit(':').next().unwrap_or(uri);
    let images = client.playlist_images(id)?;
    let image = pick(&images).context("the playlist has no picture")?;
    let agent = crate::net::api_agent(Duration::from_secs(20));
    let mut reply = agent.get(&image.url).call()?;
    if !reply.status().is_success() {
        anyhow::bail!("the picture answered {}", reply.status());
    }
    let mut bytes = Vec::new();
    reply
        .body_mut()
        .as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)?;
    let picture = image::load_from_memory(&bytes)?
        .thumbnail(SIDE, SIDE)
        .to_rgb8();
    let target = path(folder, uri);
    std::fs::create_dir_all(folder)?;
    let tmp = target.with_extension("jpg.tmp");
    picture.save_with_format(&tmp, image::ImageFormat::Jpeg)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// The smallest picture of at least [`SIDE`] px, else the largest one.
fn pick(images: &[Image]) -> Option<&Image> {
    let side = |image: &&Image| image.width.unwrap_or(0).max(image.height.unwrap_or(0));
    images
        .iter()
        .filter(|image| side(image) >= SIDE)
        .min_by_key(side)
        .or_else(|| images.iter().max_by_key(side))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    use serde_json::json;

    use super::*;
    use crate::spotify::fake::{self, Fake};
    use crate::spotify::token::{Secret, TokenFile};

    fn image(url: &str, side: Option<u32>) -> Image {
        Image {
            url: url.into(),
            width: side,
            height: side,
        }
    }

    #[test]
    fn picks_the_smallest_picture_that_is_big_enough() {
        let images = [
            image("big", Some(640)),
            image("mid", Some(600)),
            image("small", Some(300)),
        ];
        assert_eq!(pick(&images).unwrap().url, "mid");
        assert_eq!(
            pick(&[image("a", None), image("b", Some(60))]).unwrap().url,
            "b"
        );
        assert!(pick(&[]).is_none());
    }

    /// Serves one PNG at `/cover.png`.
    fn picture_server() -> String {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(800, 800, image::Rgb([200, 30, 30]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/cover.png", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    png.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&png);
            }
        });
        url
    }

    #[test]
    fn a_missing_cover_is_fetched_shrunk_and_kept() {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        TokenFile {
            client_id: fake::CLIENT_ID.into(),
            refresh_token: Secret::new(fake::REFRESH_TOKEN),
        }
        .save(dir.path())
        .unwrap();
        fake.script().images = json!([{"url": picture_server(), "width": 800, "height": 800}]);
        let uris = ["spotify:playlist:abc".to_string()];

        let folder = dir.path().join("covers");
        fetch_missing(dir.path(), &folder, &uris, fake.endpoints());

        let kept = path(&folder, "spotify:playlist:abc");
        assert_eq!(kept, folder.join("abc.jpg"));
        let cover = image::open(&kept).unwrap();
        assert_eq!((cover.width(), cover.height()), (512, 512));

        fetch_missing(dir.path(), &folder, &uris, fake.endpoints());
        assert_eq!(
            fake.requests_to("/v1/playlists/abc/images").len(),
            1,
            "a kept cover is not fetched again"
        );
    }

    #[test]
    fn without_a_login_nothing_is_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("covers");
        fetch_missing(
            dir.path(),
            &folder,
            &["spotify:playlist:abc".to_string()],
            Endpoints::default(),
        );
        assert!(!folder.exists());
    }
}
