//! Serves the music folder over HTTP so the speaker can download the tracks.

use std::net::TcpListener;
use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::Router;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

pub fn spawn(music_dir: PathBuf, port: u16) -> Result<()> {
    // Bind here, so a port that is already in use is reported at startup.
    let listener = TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("cannot listen on port {port} (in use?)"))?;
    listener.set_nonblocking(true)?;

    std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to start tokio runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
                let app = Router::new()
                    .nest_service("/music", ServeDir::new(music_dir))
                    .layer(TraceLayer::new_for_http());
                if let Err(err) = axum::serve(listener, app).await {
                    tracing::error!("web server stopped: {err}");
                }
            });
        })?;
    Ok(())
}
