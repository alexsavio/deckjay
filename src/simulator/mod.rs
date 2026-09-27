//! A Stream Deck simulator: a web page that shows the key images and turns
//! clicks into key presses. Run it with `kids-deck simulator`; the player
//! connects with `--simulator http://HOST:PORT` instead of using USB.
//!
//! HTTP API (the player uses `/api/*` except `/api/press`, which the page uses):
//!
//! | Method | Path                        | Body / reply                                        |
//! |--------|-----------------------------|-----------------------------------------------------|
//! | GET    | `/`                         | the simulator page                                  |
//! | GET    | `/api/info`                 | [`Info`] as JSON                                    |
//! | POST   | `/api/reset`                | clears images and pending presses, brightness 100   |
//! | PUT    | `/api/keys/{n}`             | PNG body; 404 if `n` is not a key, 415 if not a PNG |
//! | GET    | `/api/keys/{n}`             | the key's PNG; 404 if it has none                   |
//! | PUT    | `/api/brightness`           | [`Brightness`] as JSON                              |
//! | PUT    | `/api/notice`               | [`Notice`] as JSON: a line the page shows under the deck, or none |
//! | GET    | `/api/presses?wait_ms=N`    | JSON array of pressed keys; waits up to `N` ms (max 10 000) for one |
//! | POST   | `/api/press/{n}`            | queues a press of key `n`; 404 if `n` is not a key  |
//! | GET    | `/api/state`                | [`State`] as JSON, polled by the page               |

mod api;
#[cfg(test)]
mod tests;

use std::net::TcpListener;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Key grid of the simulated deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub rows: usize,
    pub cols: usize,
    /// Side of a square key image, in pixels.
    pub key_size: u32,
}

/// Body of `PUT /api/brightness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brightness {
    pub percent: u8,
}

/// Body of `PUT /api/notice`: why the player's last command failed, for
/// the page to show under the deck, or nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub text: Option<String>,
}

/// Reply of `GET /api/state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub info: Info,
    pub brightness: u8,
    /// True while the player waits for presses and for 2 seconds after.
    pub connected: bool,
    /// Bumped on every image change of the key; 0 means no image yet.
    pub versions: Vec<u64>,
    /// The player's last failure, while it stands.
    pub notice: Option<String>,
}

/// Stream Deck models the simulator can copy (sizes from `elgato-streamdeck`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Mk2,
    Mini,
    Neo,
    Xl,
    Plus,
}

impl Model {
    /// Names accepted by `--model`.
    pub const NAMES: &[&str] = &["mk2", "mini", "neo", "xl", "plus"];

    pub fn parse(name: &str) -> Option<Model> {
        Some(match name.to_ascii_lowercase().as_str() {
            "mk2" => Model::Mk2,
            "mini" => Model::Mini,
            "neo" => Model::Neo,
            "xl" => Model::Xl,
            "plus" => Model::Plus,
            _ => return None,
        })
    }

    pub fn info(self) -> Info {
        let (rows, cols, key_size) = match self {
            Model::Mk2 => (3, 5, 72),
            Model::Mini => (2, 3, 80),
            Model::Neo => (2, 4, 96),
            Model::Xl => (4, 8, 96),
            Model::Plus => (2, 4, 120),
        };
        Info {
            rows,
            cols,
            key_size,
        }
    }
}

/// Binds `0.0.0.0:port` and serves the simulator until the process ends.
pub fn run(port: u16, model: Model) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("cannot listen on port {port} (in use?)"))?;
    let info = model.info();
    info!(
        "{model:?} deck simulator ({}x{} keys) at http://localhost:{port}/",
        info.rows, info.cols
    );
    serve(listener, info)
}

/// Serves the simulator on an already bound listener. Blocks forever.
pub fn serve(listener: TcpListener, info: Info) -> Result<()> {
    listener.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        axum::serve(listener, api::router(info))
            .await
            .context("deck simulator stopped")
    })
}
