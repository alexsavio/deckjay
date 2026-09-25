//! Turns the bytes of an RSS feed into the episodes this program can play.
//!
//! Feed text is untrusted: it only ever becomes titles, ids (hashed) and
//! http(s) URLs, never file names.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::io::Cursor;

use anyhow::{Result, bail};
use chrono::DateTime;
use rss::Channel;

/// Longer titles are cut: they are only for logs and the manifest.
const MAX_TITLE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub title: String,
    pub picture_url: Option<String>,
    /// Newest first when every item has a date the parser understands,
    /// else in the order of the feed.
    pub episodes: Vec<RemoteEpisode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEpisode {
    /// 16 hex digits, stable across refreshes; see [`episode_id`].
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub published: Option<i64>,
    pub audio_url: String,
    /// The enclosure's `length`; feeds often leave it out or put 0.
    pub length: Option<u64>,
    /// The content type the speaker gets, from the file extension.
    pub mime: &'static str,
    /// File extension without the dot, one that [`content_type`] knows.
    pub ext: &'static str,
    /// The episode's `itunes:image`, else the feed's picture.
    pub picture_url: Option<String>,
}

pub fn parse(body: &[u8]) -> Result<Feed> {
    if looks_like_html(body) {
        bail!("the URL returned an HTML page, not an RSS feed");
    }
    let channel = Channel::read_from(Cursor::new(body)).map_err(|err| match err {
        rss::Error::InvalidStartTag => {
            anyhow::anyhow!("not an RSS feed (Atom and other formats are not supported)")
        }
        err => anyhow::Error::new(err).context("invalid RSS feed"),
    })?;

    let picture_url = channel
        .itunes_ext()
        .and_then(|ext| ext.image())
        .and_then(http_url)
        .or_else(|| channel.image().and_then(|image| http_url(image.url())));

    let mut seen = HashSet::new();
    let mut episodes: Vec<RemoteEpisode> = channel
        .items()
        .iter()
        .filter_map(|item| episode(item, picture_url.as_deref()))
        .filter(|episode| seen.insert(episode.id.clone()))
        .collect();
    if episodes.is_empty() {
        bail!("the feed has no playable audio episodes");
    }
    if episodes.iter().all(|e| e.published.is_some()) {
        episodes.sort_by_key(|e| Reverse(e.published));
    }
    Ok(Feed {
        title: clean_title(channel.title()),
        picture_url,
        episodes,
    })
}

/// `None` for items without a usable audio enclosure.
fn episode(item: &rss::Item, feed_picture: Option<&str>) -> Option<RemoteEpisode> {
    let enclosure = item.enclosure()?;
    let audio_url = http_url(enclosure.url())?;
    let ext = extension(enclosure.mime_type(), &audio_url)?;
    let mime = content_type(ext)?;
    let title = clean_title(item.title().unwrap_or_default());
    Some(RemoteEpisode {
        id: episode_id(
            item.guid().map(rss::Guid::value),
            &audio_url,
            &title,
            item.pub_date(),
        ),
        published: item.pub_date().and_then(parse_date),
        length: enclosure
            .length()
            .trim()
            .parse()
            .ok()
            .filter(|&len: &u64| len > 0),
        picture_url: item
            .itunes_ext()
            .and_then(|ext| ext.image())
            .and_then(http_url)
            .or_else(|| feed_picture.map(str::to_string)),
        title,
        audio_url,
        mime,
        ext,
    })
}

/// The `guid`, else the enclosure URL, else title and date, hashed.
pub fn episode_id(guid: Option<&str>, audio_url: &str, title: &str, date: Option<&str>) -> String {
    let guid = guid.map(str::trim).filter(|g| !g.is_empty());
    let audio_url = Some(audio_url.trim()).filter(|u| !u.is_empty());
    match guid.or(audio_url) {
        Some(key) => hash_hex(key),
        None => hash_hex(&format!("{title}\n{}", date.unwrap_or_default())),
    }
}

/// FNV-1a 64 as 16 hex digits: stable across Rust versions and runs, unlike
/// `DefaultHasher`, so ids and file names survive restarts and upgrades.
pub fn hash_hex(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// RFC 2822, as RSS asks for. A wrong weekday, which some feeds have, is
/// ignored rather than failing the whole date.
fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    DateTime::parse_from_rfc2822(text)
        .or_else(|err| match text.split_once(',') {
            Some((_, rest)) => DateTime::parse_from_rfc2822(rest.trim()),
            None => Err(err),
        })
        .ok()
        .map(|date| date.timestamp())
}

fn clean_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Only http(s): the URL is fetched by this program and by the speaker.
pub fn http_url(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let ok = (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then(|| url.to_string())
}

/// From the enclosure's type, else from the URL. Some feeds send audio as
/// `application/octet-stream` or without a type, so those use the URL too.
fn extension(mime: &str, url: &str) -> Option<&'static str> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let ext = match mime.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "" | "application/octet-stream" => return url_extension(url),
        other if other.starts_with("audio/") => return url_extension(url),
        _ => return None,
    };
    Some(ext)
}

fn url_extension(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let last = path.rsplit('/').next().unwrap_or_default();
    let (_, ext) = last.rsplit_once('.')?;
    KNOWN_EXTENSIONS
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(ext))
}

const KNOWN_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "mp4", "aac", "flac", "ogg", "oga", "opus", "wav",
];

/// The same table as the music library's: formats the Chromecast default
/// receiver can play. The library's copy is private to its module.
pub fn content_type(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        _ => return None,
    })
}

/// A captive portal or a moved feed often answers with a web page.
fn looks_like_html(body: &[u8]) -> bool {
    let start = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let start = &start[start.len() - start.trim_ascii_start().len()..];
    let head = String::from_utf8_lossy(&start[..start.len().min(64)]).to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITUNES: &[u8] = include_bytes!("fixtures/itunes.xml");
    const PLAIN: &[u8] = include_bytes!("fixtures/plain.xml");

    fn titles(feed: &Feed) -> Vec<&str> {
        feed.episodes.iter().map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn fnv1a_64_matches_the_reference_values() {
        assert_eq!(hash_hex(""), "cbf29ce484222325");
        assert_eq!(hash_hex("a"), "af63dc4c8601ec8c");
        assert_eq!(hash_hex("foobar"), "85944171f73967e8");
    }

    #[test]
    fn reads_an_itunes_feed_newest_first() {
        let feed = parse(ITUNES).unwrap();

        assert_eq!(feed.title, "Die Maus zum Hören");
        assert_eq!(
            feed.picture_url.as_deref(),
            Some("https://example.org/maus-itunes.jpg")
        );
        assert_eq!(
            titles(&feed),
            ["Der Elefant", "Die Ente", "Ohne Guid", "Das Schaf"]
        );
        let elefant = &feed.episodes[0];
        assert_eq!(elefant.audio_url, "https://cdn.example.org/elefant.mp3?x=1");
        assert_eq!(elefant.length, Some(1234));
        assert_eq!(elefant.mime, "audio/mpeg");
        assert_eq!(elefant.ext, "mp3");
        assert_eq!(elefant.id, hash_hex("maus-3"));
        assert_eq!(elefant.id.len(), 16);
    }

    #[test]
    fn parses_numeric_and_named_time_zones() {
        let feed = parse(ITUNES).unwrap();
        let date = |title: &str| {
            feed.episodes
                .iter()
                .find(|e| e.title == title)
                .unwrap()
                .published
        };

        // Tue, 07 Jan 2025 08:00:00 +0200 = 06:00 UTC.
        assert_eq!(date("Der Elefant"), Some(1_736_229_600));
        // Mon, 06 Jan 2025 10:00:00 GMT.
        assert_eq!(date("Die Ente"), Some(1_736_157_600));
        // Sun, 05 Jan 2025 09:00:00 EST = 14:00 UTC.
        assert_eq!(date("Ohne Guid"), Some(1_736_085_600));
        // Wrong weekday ("Fri" for a Saturday): the date still counts.
        assert_eq!(date("Das Schaf"), Some(1_735_988_400));
    }

    #[test]
    fn skips_items_it_cannot_play() {
        let feed = parse(ITUNES).unwrap();
        for skipped in ["Video", "Kein Anhang", "Ein PDF", "Nur FTP", "Unbekannt"] {
            assert!(!titles(&feed).contains(&skipped), "{skipped}");
        }
    }

    #[test]
    fn falls_back_to_the_enclosure_url_as_id() {
        let feed = parse(ITUNES).unwrap();
        let episode = feed
            .episodes
            .iter()
            .find(|e| e.title == "Ohne Guid")
            .unwrap();
        assert_eq!(
            episode.id,
            hash_hex("https://cdn.example.org/ohne-guid.m4a")
        );
    }

    #[test]
    fn id_without_guid_or_url_uses_title_and_date() {
        let id = episode_id(
            Some("  "),
            "",
            "Title",
            Some("Mon, 06 Jan 2025 10:00:00 GMT"),
        );
        assert_eq!(id, hash_hex("Title\nMon, 06 Jan 2025 10:00:00 GMT"));
        assert_ne!(id, episode_id(None, "", "Title", None));
    }

    #[test]
    fn episode_picture_falls_back_to_the_feed_picture() {
        let feed = parse(ITUNES).unwrap();
        let picture = |title: &str| {
            feed.episodes
                .iter()
                .find(|e| e.title == title)
                .unwrap()
                .picture_url
                .clone()
        };
        assert_eq!(
            picture("Der Elefant").as_deref(),
            Some("https://example.org/elefant.png")
        );
        assert_eq!(
            picture("Die Ente").as_deref(),
            Some("https://example.org/maus-itunes.jpg")
        );
    }

    #[test]
    fn plain_rss_uses_the_channel_image_and_keeps_feed_order() {
        let feed = parse(PLAIN).unwrap();

        assert_eq!(
            feed.picture_url.as_deref(),
            Some("http://example.org/logo.png")
        );
        // One date is not RFC 2822, so the feed's own order stays.
        assert_eq!(titles(&feed), ["Teil 1", "Teil 2", "Teil 3"]);
        assert_eq!(feed.episodes[1].published, None);
        assert_eq!(feed.episodes[0].id, hash_hex("teil-1"));
    }

    #[test]
    fn mime_types_map_to_extensions() {
        let feed = parse(PLAIN).unwrap();
        let exts: Vec<(&str, &str)> = feed.episodes.iter().map(|e| (e.ext, e.mime)).collect();
        assert_eq!(
            exts,
            [
                ("m4a", "audio/mp4"),
                ("opus", "audio/ogg"),
                ("mp3", "audio/mpeg")
            ]
        );

        for (mime, url, ext) in [
            ("audio/mpeg", "https://x/a", Some("mp3")),
            ("audio/mp3", "https://x/a", Some("mp3")),
            ("audio/MP4; codecs=mp4a", "https://x/a", Some("m4a")),
            ("audio/x-m4a", "https://x/a", Some("m4a")),
            ("audio/aac", "https://x/a", Some("aac")),
            ("audio/ogg", "https://x/a", Some("ogg")),
            ("audio/opus", "https://x/a", Some("opus")),
            ("audio/x-flac", "https://x/a.FLAC?v=2", Some("flac")),
            ("", "https://x/a.wav#t", Some("wav")),
            ("application/octet-stream", "https://x/a.mp3", Some("mp3")),
            ("audio/x-unknown", "https://x/a.xyz", None),
            ("video/mp4", "https://x/a.mp4", None),
            ("application/pdf", "https://x/a.mp3", None),
        ] {
            assert_eq!(extension(mime, url), ext, "{mime} {url}");
        }
    }

    #[test]
    fn only_http_urls_are_used() {
        assert!(http_url(" https://example.org/a.mp3 ").is_some());
        assert!(http_url("HTTP://example.org/a.mp3").is_some());
        for bad in [
            "ftp://x/a.mp3",
            "file:///etc/passwd",
            "/a.mp3",
            "https://x/a b.mp3",
        ] {
            assert!(http_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn html_is_a_clear_error() {
        let page = b"\xef\xbb\xbf\n  <!DOCTYPE html><html><body>Login</body></html>";
        let err = parse(page).unwrap_err().to_string();
        assert!(err.contains("HTML page"), "{err}");
        let err = parse(b"<feed xmlns=\"http://www.w3.org/2005/Atom\"/>").unwrap_err();
        assert!(err.to_string().contains("not an RSS feed"), "{err}");
    }

    #[test]
    fn a_feed_without_playable_episodes_is_an_error() {
        let rss = br#"<rss version="2.0"><channel><title>T</title>
            <item><title>Video</title><enclosure url="https://x/v.mp4" type="video/mp4" length="1"/></item>
            </channel></rss>"#;
        let err = parse(rss).unwrap_err().to_string();
        assert!(err.contains("no playable"), "{err}");
    }

    #[test]
    fn duplicate_guids_keep_the_first_item() {
        let rss = br#"<rss version="2.0"><channel><title>T</title>
            <item><title>A</title><guid>same</guid><enclosure url="https://x/a.mp3" type="audio/mpeg" length="1"/></item>
            <item><title>B</title><guid>same</guid><enclosure url="https://x/b.mp3" type="audio/mpeg" length="1"/></item>
            </channel></rss>"#;
        assert_eq!(titles(&parse(rss).unwrap()), ["A"]);
    }

    #[test]
    fn titles_are_trimmed_and_capped() {
        assert_eq!(clean_title("  Die \n  Maus \t"), "Die Maus");
        assert_eq!(clean_title(&"x".repeat(500)).len(), MAX_TITLE_CHARS);
    }
}
