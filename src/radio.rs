//! Internet radio: turns a station's URL into the stream a speaker can play.
//! Station lists often give a `.pls` or `.m3u` playlist that names the
//! stream; speakers want the stream itself and its content type.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used once the speakers play radio")
)]

use anyhow::{Context, Result, bail};
use ureq::Agent;

/// A playlist is a few lines; a bigger body is not one.
const PLAYLIST_LIMIT: u64 = 64 * 1024;
/// A playlist may name another playlist, but not endlessly.
const MAX_PLAYLISTS: usize = 3;
/// What Cast wants for HLS.
pub const HLS: &str = "application/x-mpegURL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub url: String,
    /// As the server sent it, without parameters, e.g. `audio/mpeg`; `HLS`
    /// for an HLS playlist.
    pub content_type: Option<String>,
}

impl Stream {
    pub fn is_hls(&self) -> bool {
        self.content_type.as_deref() == Some(HLS)
    }
}

/// Follows `url` through `.pls` and `.m3u` playlists to the stream. `agent`
/// should have no body deadline (`net::stream_agent`): the reply of a stream
/// is dropped after its headers.
pub fn resolve(agent: &Agent, url: &str) -> Result<Stream> {
    let mut url = url.to_string();
    for _ in 0..=MAX_PLAYLISTS {
        let mut reply = agent
            .get(&url)
            .call()
            .with_context(|| format!("cannot reach {}", without_query(&url)))?;
        let status = reply.status();
        if !status.is_success() {
            bail!("{} answered {status}", without_query(&url));
        }
        let content_type = reply
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
        let Some(list) = playlist_kind(&url, content_type.as_deref()) else {
            return Ok(Stream { url, content_type });
        };
        let body = reply
            .body_mut()
            .with_config()
            .limit(PLAYLIST_LIMIT)
            .read_to_string()
            .with_context(|| format!("cannot read the playlist {}", without_query(&url)))?;
        if list == List::M3u && body.contains("#EXT-X-") {
            return Ok(Stream {
                url,
                content_type: Some(HLS.into()),
            });
        }
        let next = match list {
            List::Pls => first_pls_entry(&body),
            List::M3u => first_m3u_entry(&body),
        };
        url = next.with_context(|| {
            format!(
                "the playlist {} names no http(s) stream",
                without_query(&url)
            )
        })?;
    }
    bail!("playlists name playlists more than {MAX_PLAYLISTS} times")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Pls,
    M3u,
}

/// Whether the reply is a playlist: by its content type, else, for a type
/// that says nothing (`text/plain`, none), by the URL's extension.
fn playlist_kind(url: &str, content_type: Option<&str>) -> Option<List> {
    match content_type {
        Some("audio/x-scpls" | "application/pls+xml") => return Some(List::Pls),
        Some(
            "audio/x-mpegurl"
            | "audio/mpegurl"
            | "application/x-mpegurl"
            | "application/vnd.apple.mpegurl",
        ) => return Some(List::M3u),
        Some("text/plain" | "application/octet-stream" | "") | None => {}
        Some(_) => return None,
    }
    let extension = std::path::Path::new(without_query(url))
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("pls") {
        Some(List::Pls)
    } else if extension.eq_ignore_ascii_case("m3u") || extension.eq_ignore_ascii_case("m3u8") {
        Some(List::M3u)
    } else {
        None
    }
}

/// `File1=http://...` lines; the lowest number first.
fn first_pls_entry(body: &str) -> Option<String> {
    body.lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            let number: u32 = key
                .trim()
                .to_ascii_lowercase()
                .strip_prefix("file")?
                .parse()
                .ok()?;
            Some((number, value.trim()))
        })
        .filter(|(_, url)| is_http(url))
        .min_by_key(|(number, _)| *number)
        .map(|(_, url)| url.to_string())
}

/// The first line that is not a `#` comment.
fn first_m3u_entry(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|line| is_http(line))
        .map(str::to_string)
}

fn is_http(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Station URLs can carry listener tokens in the query; logs leave it out.
fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

#[cfg(test)]
mod tests;
