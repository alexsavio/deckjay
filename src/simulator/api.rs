//! The simulator's HTTP handlers and the deck state they share.

use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use super::{Brightness, Info};

const PAGE: &str = include_str!("page.html");
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const MAX_WAIT: Duration = Duration::from_secs(10);
const CONNECTED_FOR: Duration = Duration::from_secs(2);
/// Oldest presses are dropped beyond this, so clicks without a player cannot
/// grow the queue forever.
const MAX_QUEUED: usize = 64;

type Reply<T> = Result<T, (StatusCode, &'static str)>;
const NOT_A_KEY: (StatusCode, &str) = (StatusCode::NOT_FOUND, "no such key");

pub(super) fn router(info: Info) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/api/info", get(info_json))
        .route("/api/reset", post(reset))
        .route("/api/keys/{n}", get(key_image).put(set_key_image))
        .route("/api/brightness", put(set_brightness))
        .route("/api/presses", get(presses))
        .route("/api/press/{n}", post(press))
        .route("/api/state", get(state))
        .with_state(Arc::new(Sim::new(info)))
}

struct Sim {
    info: Info,
    deck: Mutex<Deck>,
    pressed: tokio::sync::Notify,
}

struct Deck {
    images: Vec<Option<Bytes>>,
    versions: Vec<u64>,
    /// Never reset, so a key never gets a version it had before.
    last_version: u64,
    presses: Vec<usize>,
    brightness: u8,
    /// `/api/presses` calls still waiting.
    polling: usize,
    /// When the last `/api/presses` call ended.
    last_poll: Option<Instant>,
}

impl Sim {
    fn new(info: Info) -> Sim {
        let keys = info.rows * info.cols;
        Sim {
            info,
            deck: Mutex::new(Deck {
                images: vec![None; keys],
                versions: vec![0; keys],
                last_version: 0,
                presses: Vec::new(),
                brightness: 100,
                polling: 0,
                last_poll: None,
            }),
            pressed: tokio::sync::Notify::new(),
        }
    }

    /// A handler that panicked cannot leave `Deck` half-updated in a way that
    /// matters, so a poisoned lock is still used.
    fn deck(&self) -> MutexGuard<'_, Deck> {
        self.deck.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn key(&self, n: &str) -> Reply<usize> {
        n.parse()
            .ok()
            .filter(|&n| n < self.info.rows * self.info.cols)
            .ok_or(NOT_A_KEY)
    }

    async fn wait_for_presses(&self, wait: Duration) -> Vec<usize> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let mut notified = pin!(self.pressed.notified());
            // Registered before the queue check, so a press queued right
            // after the check still wakes this wait.
            notified.as_mut().enable();
            let presses = std::mem::take(&mut self.deck().presses);
            if !presses.is_empty() {
                return presses;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return std::mem::take(&mut self.deck().presses);
            }
        }
    }
}

impl Deck {
    fn connected(&self, now: Instant) -> bool {
        self.polling > 0
            || self
                .last_poll
                .is_some_and(|last| now.duration_since(last) < CONNECTED_FOR)
    }
}

/// Counts an `/api/presses` call as the player being there for as long as
/// it waits, also when the client hangs up mid-wait.
struct Polling<'a>(&'a Sim);

impl<'a> Polling<'a> {
    fn start(sim: &'a Sim) -> Polling<'a> {
        sim.deck().polling += 1;
        Polling(sim)
    }
}

impl Drop for Polling<'_> {
    fn drop(&mut self) {
        let mut deck = self.0.deck();
        deck.polling -= 1;
        deck.last_poll = Some(Instant::now());
    }
}

fn wait_time(wait_ms: Option<u64>) -> Duration {
    Duration::from_millis(wait_ms.unwrap_or(0)).min(MAX_WAIT)
}

async fn page() -> Html<&'static str> {
    Html(PAGE)
}

async fn info_json(State(sim): State<Arc<Sim>>) -> Json<Info> {
    Json(sim.info)
}

async fn reset(State(sim): State<Arc<Sim>>) -> StatusCode {
    let mut deck = sim.deck();
    deck.images.fill(None);
    deck.versions.fill(0);
    deck.presses.clear();
    deck.brightness = 100;
    StatusCode::NO_CONTENT
}

async fn key_image(State(sim): State<Arc<Sim>>, Path(n): Path<String>) -> Reply<impl IntoResponse> {
    let n = sim.key(&n)?;
    let png = sim.deck().images[n]
        .clone()
        .ok_or((StatusCode::NOT_FOUND, "the key has no image"))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    ))
}

async fn set_key_image(
    State(sim): State<Arc<Sim>>,
    Path(n): Path<String>,
    png: Bytes,
) -> Reply<StatusCode> {
    let n = sim.key(&n)?;
    if !png.starts_with(PNG_SIGNATURE) {
        return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, "the body is not a PNG"));
    }
    let mut deck = sim.deck();
    deck.last_version += 1;
    deck.versions[n] = deck.last_version;
    deck.images[n] = Some(png);
    Ok(StatusCode::NO_CONTENT)
}

async fn set_brightness(
    State(sim): State<Arc<Sim>>,
    Json(Brightness { percent }): Json<Brightness>,
) -> StatusCode {
    sim.deck().brightness = percent.min(100);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct PressesQuery {
    wait_ms: Option<u64>,
}

async fn presses(
    State(sim): State<Arc<Sim>>,
    Query(query): Query<PressesQuery>,
) -> Json<Vec<usize>> {
    let _polling = Polling::start(&sim);
    Json(sim.wait_for_presses(wait_time(query.wait_ms)).await)
}

async fn press(State(sim): State<Arc<Sim>>, Path(n): Path<String>) -> Reply<StatusCode> {
    let n = sim.key(&n)?;
    {
        let mut deck = sim.deck();
        if deck.presses.len() == MAX_QUEUED {
            deck.presses.remove(0);
        }
        deck.presses.push(n);
    }
    sim.pressed.notify_waiters();
    Ok(StatusCode::NO_CONTENT)
}

async fn state(State(sim): State<Arc<Sim>>) -> Json<super::State> {
    let deck = sim.deck();
    Json(super::State {
        info: sim.info,
        brightness: deck.brightness,
        connected: deck.connected(Instant::now()),
        versions: deck.versions.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_defaults_to_zero_and_is_capped() {
        assert_eq!(wait_time(None), Duration::ZERO);
        assert_eq!(wait_time(Some(250)), Duration::from_millis(250));
        assert_eq!(wait_time(Some(60_000)), MAX_WAIT);
    }

    #[test]
    fn a_long_poll_in_progress_counts_as_connected() {
        let sim = Sim::new(super::super::Model::Mini.info());
        let later = Instant::now() + Duration::from_secs(60);
        assert!(!sim.deck().connected(later));

        let polling = Polling::start(&sim);
        assert!(sim.deck().connected(later));

        drop(polling);
        assert!(sim.deck().connected(Instant::now()));
        assert!(!sim.deck().connected(later));
    }
}
