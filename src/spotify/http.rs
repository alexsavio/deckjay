//! How a Spotify answer is read: the status, the `Retry-After` and the body of
//! a [`Reply`], and the [`ApiError`] an error answer becomes. The web API client
//! and the sign-in both build on it.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::DeserializeOwned;

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

    /// deckjay's own hold-back: the call is refused before it is sent.
    pub(super) fn rate_limited(wait: Duration) -> ApiError {
        ApiError {
            status: 429,
            reason: None,
            message: "deckjay is waiting for Spotify's rate limit".into(),
            retry_after: Some(wait),
        }
    }

    /// The same error, told to wait `wait` before the next try.
    pub(super) fn waiting(mut self, wait: Duration) -> ApiError {
        self.retry_after = Some(wait);
        self
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

/// For a 429 without a readable `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

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

    /// How long Spotify wants no requests after this answer.
    pub fn retry_wait(&self) -> Duration {
        self.retry_after.unwrap_or(DEFAULT_RETRY_AFTER)
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn error(&self) -> ApiError {
        ApiError::parse(self.status, &self.body, self.retry_after)
    }

    /// A 204, or any answer without a body: nothing to read.
    pub fn is_empty(&self) -> bool {
        self.status == 204 || self.body.trim().is_empty()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body).context("Spotify sent an answer deckjay cannot read")
    }
}
