//! `type = "podcast"` sources: feeds whose newest episodes are downloaded
//! into a cache folder.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::source::Look;

/// Checked podcast settings of one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Podcast {
    /// Newest episodes kept per feed, unless the feed sets its own.
    pub keep: usize,
    pub refresh: Duration,
    /// For all feeds of the source together.
    pub max_cache_bytes: u64,
    /// Bigger episodes are not downloaded.
    pub max_episode_bytes: u64,
    pub feeds: Vec<Feed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub order: Order,
    pub keep: Option<usize>,
}

/// Which episodes come first on the deck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    #[default]
    Newest,
    /// For serial stories that are heard from the first part on.
    Oldest,
}

const MB: u64 = 1024 * 1024;
const MAX_KEEP: usize = 50;

/// A podcast `[[source]]` table as written in the file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPodcast {
    pub(super) name: Option<String>,
    /// Writable; defaults to `<state_dir>/podcasts/<source name>`.
    pub(super) cache_dir: Option<std::path::PathBuf>,
    #[serde(default = "default_keep")]
    keep: usize,
    #[serde(default = "default_refresh_hours")]
    refresh_hours: u64,
    #[serde(default = "default_max_cache_mb")]
    max_cache_mb: u64,
    #[serde(default = "default_max_episode_mb")]
    max_episode_mb: u64,
    pub(super) picture: Option<std::path::PathBuf>,
    pub(super) color: Option<super::source::Color>,
    #[serde(default)]
    feed: Vec<Feed>,
}

fn default_keep() -> usize {
    5
}
fn default_refresh_hours() -> u64 {
    6
}
fn default_max_cache_mb() -> u64 {
    4000
}
fn default_max_episode_mb() -> u64 {
    500
}

impl RawPodcast {
    pub(super) fn look(&self) -> Look {
        Look {
            picture: self.picture.clone(),
            color: self.color,
        }
    }

    /// `name` is the source's name, for the error messages.
    pub(super) fn check(&self, name: &str) -> Result<Podcast> {
        if self.feed.is_empty() {
            bail!(
                "podcast source {name} has no feed; add one:\n\n\
                 [[source.feed]]\nname = \"...\"\nurl = \"https://...\""
            );
        }
        let keep_ok = |keep: usize| (1..=MAX_KEEP).contains(&keep);
        if !keep_ok(self.keep) {
            bail!("podcast source {name}: keep must be between 1 and {MAX_KEEP}");
        }
        if !(1..=24 * 7).contains(&self.refresh_hours) {
            bail!("podcast source {name}: refresh_hours must be between 1 and 168");
        }
        if self.max_cache_mb < 10 || self.max_episode_mb == 0 {
            bail!(
                "podcast source {name}: max_cache_mb must be at least 10 and max_episode_mb at least 1"
            );
        }
        let mut names = std::collections::HashSet::new();
        for feed in &self.feed {
            if feed.name.trim().is_empty() {
                bail!("podcast source {name}: a feed has an empty name");
            }
            if !names.insert(feed.name.as_str()) {
                bail!("podcast source {name}: two feeds are named {:?}", feed.name);
            }
            let scheme_ok = feed.url.starts_with("https://") || feed.url.starts_with("http://");
            if !scheme_ok {
                bail!(
                    "podcast {}: the url must start with https:// or http://",
                    feed.name
                );
            }
            if feed.keep.is_some_and(|keep| !keep_ok(keep)) {
                bail!(
                    "podcast {}: keep must be between 1 and {MAX_KEEP}",
                    feed.name
                );
            }
        }
        Ok(Podcast {
            keep: self.keep,
            refresh: Duration::from_hours(self.refresh_hours),
            max_cache_bytes: self.max_cache_mb * MB,
            max_episode_bytes: self.max_episode_mb * MB,
            feeds: self.feed.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Result<Podcast> {
        let raw: RawPodcast = toml::from_str(text)?;
        raw.check("maus")
    }

    const FEED: &str = "[[feed]]\nname = \"Die Maus\"\nurl = \"https://example.org/maus.xml\"\n";

    #[test]
    fn fills_in_defaults() {
        let podcast = check(FEED).unwrap();
        assert_eq!(
            podcast,
            Podcast {
                keep: 5,
                refresh: Duration::from_hours(6),
                max_cache_bytes: 4000 * MB,
                max_episode_bytes: 500 * MB,
                feeds: vec![Feed {
                    name: "Die Maus".into(),
                    url: "https://example.org/maus.xml".into(),
                    order: Order::Newest,
                    keep: None,
                }],
            }
        );
    }

    #[test]
    fn reads_every_key() {
        let podcast = check(
            "keep = 3\nrefresh_hours = 12\nmax_cache_mb = 100\nmax_episode_mb = 50\n\
             [[feed]]\nname = \"A\"\nurl = \"http://a.example/feed\"\norder = \"oldest\"\nkeep = 10\n",
        )
        .unwrap();
        assert_eq!(
            (podcast.keep, podcast.refresh, podcast.max_cache_bytes),
            (3, Duration::from_hours(12), 100 * MB)
        );
        assert_eq!(
            (podcast.feeds[0].order, podcast.feeds[0].keep),
            (Order::Oldest, Some(10))
        );
    }

    #[test]
    fn rejects_bad_settings() {
        for (text, needle) in [
            ("", "no feed"),
            (&format!("keep = 0\n{FEED}") as &str, "keep"),
            (&format!("keep = 51\n{FEED}"), "keep"),
            (&format!("refresh_hours = 0\n{FEED}"), "refresh_hours"),
            (&format!("max_cache_mb = 5\n{FEED}"), "max_cache_mb"),
            (
                "[[feed]]\nname = \"A\"\nurl = \"ftp://a/feed\"\n",
                "https://",
            ),
            (
                "[[feed]]\nname = \" \"\nurl = \"https://a/feed\"\n",
                "empty name",
            ),
            (&format!("{FEED}{FEED}"), "two feeds"),
            (&format!("{FEED}keep = 0\n"), "keep"),
            (
                "[[feed]]\nname = \"A\"\nurl = \"https://a\"\norder = \"random\"\n",
                "random",
            ),
            (
                "[[feed]]\nname = \"A\"\nurl = \"https://a\"\nlink = \"x\"\n",
                "link",
            ),
        ] {
            let err = check(text).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{text}: {err:#}");
        }
    }
}
