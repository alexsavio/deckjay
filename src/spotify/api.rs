//! A blocking client for the parts of the Spotify Web API that deckjay
//! uses: Connect devices, playback control, the account name and playlist
//! pictures. It keeps the access token fresh and saves a rotated refresh token.

use std::fmt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracing::{debug, warn};
use ureq::Agent;

use super::auth;
use super::token::{Secret, TokenFile};

const TIMEOUT: Duration = Duration::from_secs(10);
/// An access token this close to its end is replaced before a request, so it
/// cannot expire on the way.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// A 429 asking for at most this long is waited out inside the call; a longer
/// one fails the call at once.
const MAX_INLINE_WAIT: Duration = Duration::from_secs(5);
/// For a 429 without a readable `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);
const SERVER_ERROR_PAUSE: Duration = Duration::from_secs(1);

/// Base URLs, so tests can point the client at a fake server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Web API, up to and including the version: `https://api.spotify.com/v1`.
    pub api: String,
    /// Accounts service root: `https://accounts.spotify.com`.
    pub accounts: String,
}

impl Default for Endpoints {
    fn default() -> Endpoints {
        Endpoints {
            api: "https://api.spotify.com/v1".into(),
            accounts: "https://accounts.spotify.com".into(),
        }
    }
}

/// An error answer from Spotify. Callers find it with
/// `anyhow::Error::downcast_ref::<ApiError>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    /// Web API: `NO_ACTIVE_DEVICE`, `PREMIUM_REQUIRED`, `VOLUME_CONTROL_DISALLOW`,
    /// `NO_NEXT_TRACK`, `ALREADY_PAUSED`, … Accounts service: the OAuth error,
    /// such as `invalid_grant`.
    pub reason: Option<String>,
    pub message: String,
    /// On 429: how long Spotify wants no requests.
    pub retry_after: Option<Duration>,
}

impl ApiError {
    /// Reads both error shapes: the Web API's
    /// `{"error": {"status": 404, "message": "…", "reason": "…"}}` and the
    /// accounts service's `{"error": "invalid_grant", "error_description": "…"}`.
    pub fn parse(status: u16, body: &str, retry_after: Option<Duration>) -> ApiError {
        #[derive(Deserialize)]
        struct Body {
            error: Field,
            error_description: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Field {
            Api {
                #[serde(default)]
                message: String,
                reason: Option<String>,
            },
            OAuth(String),
        }

        let (reason, message) = match serde_json::from_str::<Body>(body) {
            Ok(Body {
                error: Field::Api { message, reason },
                ..
            }) => (reason, message),
            Ok(Body {
                error: Field::OAuth(error),
                error_description,
            }) => (Some(error), error_description.unwrap_or_default()),
            Err(_) => (None, body.trim().chars().take(200).collect()),
        };
        ApiError {
            status,
            reason,
            message,
            retry_after,
        }
    }

    fn rate_limited(wait: Duration) -> ApiError {
        ApiError {
            status: 429,
            reason: None,
            message: "deckjay is waiting for Spotify's rate limit".into(),
            retry_after: Some(wait),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Spotify answered {}", self.status)?;
        if let Some(reason) = &self.reason {
            write!(f, " {reason}")?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        if let Some(wait) = self.retry_after {
            write!(f, " (try again in {} s)", wait.as_secs_f64().ceil())?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// A status, a `Retry-After` in seconds, and the whole body.
pub(super) struct Reply {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: String,
}

impl Reply {
    pub fn read(mut reply: ureq::http::Response<ureq::Body>) -> Result<Reply> {
        let retry_after = reply
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok())
            .map(Duration::from_secs);
        Ok(Reply {
            status: reply.status().as_u16(),
            retry_after,
            body: reply
                .body_mut()
                .read_to_string()
                .context("cannot read Spotify's answer")?,
        })
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn error(&self) -> ApiError {
        ApiError::parse(self.status, &self.body, self.retry_after)
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body).context("Spotify sent an answer deckjay cannot read")
    }
}

/// A Spotify Connect device from `GET /me/player/devices`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Device {
    /// `None` for a device the Web API cannot control.
    pub id: Option<String>,
    pub name: String,
    /// `Computer`, `Smartphone`, `Speaker`, `TV`, `AVR`, `CastAudio`, …
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub is_active: bool,
    /// Commands to a restricted device fail.
    #[serde(default)]
    pub is_restricted: bool,
    #[serde(default)]
    pub supports_volume: bool,
    pub volume_percent: Option<u8>,
}

/// What `GET /me/player` says is playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackState {
    pub device_id: Option<String>,
    pub is_playing: bool,
    /// The playlist, album or artist being played; `None` for single tracks.
    pub context_uri: Option<String>,
    pub progress_ms: Option<u64>,
    /// The track or episode; `None` during ads or in a private session.
    pub item_name: Option<String>,
}

/// A playlist picture; Spotify leaves the size out for some of them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Image {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Get,
    Put,
    Post,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
            Method::Post => "POST",
        }
    }
}

struct Access {
    token: Secret,
    expires_at: Instant,
}

pub struct Client {
    agent: Agent,
    endpoints: Endpoints,
    state_dir: PathBuf,
    token: TokenFile,
    access: Option<Access>,
    /// Spotify asked for no requests before this (429 `Retry-After`).
    retry_at: Option<Instant>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("endpoints", &self.endpoints)
            .field("state_dir", &self.state_dir)
            .field("token", &self.token)
            .field("has_access_token", &self.access.is_some())
            .field("retry_at", &self.retry_at)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `state_dir` is where a rotated refresh token is saved.
    pub fn new(token: TokenFile, state_dir: &Path, endpoints: Endpoints) -> Client {
        Client {
            agent: crate::net::api_agent(TIMEOUT),
            endpoints,
            state_dir: state_dir.to_path_buf(),
            token,
            access: None,
            retry_at: None,
        }
    }

    pub fn load(state_dir: &Path, endpoints: Endpoints) -> Result<Client> {
        Ok(Client::new(
            TokenFile::load(state_dir)?,
            state_dir,
            endpoints,
        ))
    }

    /// Starts with the access token from the sign-in, which saves a refresh.
    pub fn with_access(mut self, token: Secret, expires_in: Duration) -> Client {
        self.access = Some(Access {
            token,
            expires_at: Instant::now() + expires_in,
        });
        self
    }

    pub fn devices(&mut self) -> Result<Vec<Device>> {
        #[derive(Deserialize)]
        struct Devices {
            devices: Vec<Device>,
        }
        let reply = self.call(Method::Get, "/me/player/devices", &[], None)?;
        Ok(reply.json::<Devices>()?.devices)
    }

    /// Moves playback to the device; `play` starts it there, else the
    /// current state (playing or paused) is kept.
    pub fn transfer(&mut self, device_id: &str, play: bool) -> Result<()> {
        let body = json!({"device_ids": [device_id], "play": play});
        self.call(Method::Put, "/me/player", &[], Some(&body.to_string()))?;
        Ok(())
    }

    pub fn set_volume(&mut self, device_id: &str, percent: u8) -> Result<()> {
        let percent = percent.min(100).to_string();
        let query = [
            ("volume_percent", percent.as_str()),
            ("device_id", device_id),
        ];
        self.call(Method::Put, "/me/player/volume", &query, None)?;
        Ok(())
    }

    /// With a context (`spotify:playlist:…`) plays it from its first track;
    /// without one resumes what was playing.
    pub fn play(&mut self, device_id: &str, context_uri: Option<&str>) -> Result<()> {
        let body = context_uri.map(|uri| {
            json!({"context_uri": uri, "offset": {"position": 0}, "position_ms": 0}).to_string()
        });
        let query = [("device_id", device_id)];
        self.call(Method::Put, "/me/player/play", &query, body.as_deref())?;
        Ok(())
    }

    pub fn pause(&mut self, device_id: &str) -> Result<()> {
        self.command(Method::Put, "/me/player/pause", device_id)
    }

    pub fn next(&mut self, device_id: &str) -> Result<()> {
        self.command(Method::Post, "/me/player/next", device_id)
    }

    pub fn previous(&mut self, device_id: &str) -> Result<()> {
        self.command(Method::Post, "/me/player/previous", device_id)
    }

    fn command(&mut self, method: Method, path: &str, device_id: &str) -> Result<()> {
        self.call(method, path, &[("device_id", device_id)], None)?;
        Ok(())
    }

    /// `None` when nothing is playing or paused on any device (204).
    pub fn player(&mut self) -> Result<Option<PlaybackState>> {
        #[derive(Deserialize)]
        struct Raw {
            device: Option<Id>,
            #[serde(default)]
            is_playing: bool,
            context: Option<Uri>,
            progress_ms: Option<u64>,
            item: Option<Name>,
        }
        #[derive(Deserialize)]
        struct Id {
            id: Option<String>,
        }
        #[derive(Deserialize)]
        struct Uri {
            uri: String,
        }
        #[derive(Deserialize)]
        struct Name {
            name: String,
        }

        let reply = self.call(Method::Get, "/me/player", &[], None)?;
        if reply.status == 204 || reply.body.trim().is_empty() {
            return Ok(None);
        }
        let raw: Raw = reply.json()?;
        Ok(Some(PlaybackState {
            device_id: raw.device.and_then(|device| device.id),
            is_playing: raw.is_playing,
            context_uri: raw.context.map(|context| context.uri),
            progress_ms: raw.progress_ms,
            item_name: raw.item.map(|item| item.name),
        }))
    }

    /// The account's display name, or its user id when it has none.
    pub fn me(&mut self) -> Result<String> {
        #[derive(Deserialize)]
        struct Me {
            display_name: Option<String>,
            id: String,
        }
        let me: Me = self.call(Method::Get, "/me", &[], None)?.json()?;
        Ok(me
            .display_name
            .filter(|name| !name.is_empty())
            .unwrap_or(me.id))
    }

    /// Largest first, as Spotify sends them.
    pub fn playlist_images(&mut self, playlist_id: &str) -> Result<Vec<Image>> {
        let path = format!(
            "/playlists/{}/images",
            utf8_percent_encode(playlist_id, NON_ALPHANUMERIC)
        );
        let images: Option<Vec<Image>> = self.call(Method::Get, &path, &[], None)?.json()?;
        Ok(images.unwrap_or_default())
    }

    /// Retries at most once per cause: after a 401 with a new access token,
    /// after a short 429 wait, and a GET after a 5xx.
    fn call(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<Reply> {
        if let Some(at) = self.retry_at {
            let now = Instant::now();
            if at > now {
                return Err(ApiError::rate_limited(at - now).into());
            }
            self.retry_at = None;
        }
        let (mut refreshed, mut waited, mut repeated) = (false, false, false);
        loop {
            let token = self.access_token()?;
            let reply = self.send(method, path, query, body, &token)?;
            match reply.status {
                _ if reply.is_success() => return Ok(reply),
                401 if !refreshed => {
                    refreshed = true;
                    self.access = None;
                }
                429 => {
                    let wait = reply.retry_after.unwrap_or(DEFAULT_RETRY_AFTER);
                    if waited || wait > MAX_INLINE_WAIT {
                        self.retry_at = Some(Instant::now() + wait);
                        let mut error = reply.error();
                        error.retry_after = Some(wait);
                        return Err(error.into());
                    }
                    waited = true;
                    thread::sleep(wait);
                }
                500..=599 if method == Method::Get && !repeated => {
                    repeated = true;
                    thread::sleep(SERVER_ERROR_PAUSE);
                }
                _ => return Err(reply.error().into()),
            }
        }
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&str>,
        token: &Secret,
    ) -> Result<Reply> {
        let url = format!("{}{path}", self.endpoints.api);
        let bearer = format!("Bearer {}", token.expose());
        let reply = match method {
            Method::Get => self
                .agent
                .get(&url)
                .header("authorization", bearer)
                .query_pairs(query.iter().copied())
                .call(),
            Method::Put | Method::Post => {
                let request = if method == Method::Put {
                    self.agent.put(&url)
                } else {
                    self.agent.post(&url)
                };
                let request = request
                    .header("authorization", bearer)
                    .query_pairs(query.iter().copied());
                match body {
                    Some(json) => request.content_type("application/json").send(json),
                    None => request.send_empty(),
                }
            }
        };
        let reply =
            reply.with_context(|| format!("cannot reach Spotify ({} {path})", method.as_str()))?;
        debug!("Spotify {} {path}: {}", method.as_str(), reply.status());
        Reply::read(reply)
    }

    fn access_token(&mut self) -> Result<Secret> {
        if let Some(access) = &self.access
            && access.expires_at > Instant::now() + REFRESH_MARGIN
        {
            return Ok(access.token.clone());
        }
        let tokens = auth::refresh(
            &self.agent,
            &self.endpoints.accounts,
            &self.token.client_id,
            &self.token.refresh_token,
        )?;
        if let Some(refresh_token) = tokens.refresh
            && refresh_token != self.token.refresh_token
        {
            self.token.refresh_token = refresh_token;
            if let Err(err) = self.token.save(&self.state_dir) {
                warn!("cannot save the new Spotify refresh token: {err:#}");
            }
        }
        self.access = Some(Access {
            token: tokens.access.clone(),
            expires_at: Instant::now() + tokens.expires_in,
        });
        Ok(tokens.access)
    }
}

#[cfg(test)]
mod tests;
