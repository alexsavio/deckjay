//! Podcasts: feeds, downloads and the episode cache.
//!
//! [`spawn`] starts the `podcasts` thread. It refreshes every feed at once and
//! then every [`Timing::refresh`]: fetch, plan, download, write the manifest,
//! [`Publisher::add`] the new files, send a [`Snapshot`]. Files that fall out
//! of the plan are deleted [`Timing::sweep_grace`] later, never while their
//! episode plays ([`NowPlaying`]), and only then [`Publisher::remove`]d.
//! [`load_cached`] reads the manifests alone, for a start without network.

mod cache;
mod download;
mod feed;
mod plan;
mod refresh;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testserver;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use cache::Manifest;
use plan::Stored;

/// Retry delay after a refresh in which every feed failed; it doubles up to
/// the refresh interval.
const FIRST_RETRY: Duration = Duration::from_secs(60);
/// How long a dropped episode stays on disk and on the server, so the UI and
/// a speaker that is still fetching it can let go first.
const SWEEP_GRACE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Must be writable; one folder per feed inside.
    pub cache_dir: PathBuf,
    /// Newest episodes kept per feed, unless the feed sets its own.
    pub keep: usize,
    pub refresh: Duration,
    /// For all feeds together.
    pub max_cache_bytes: u64,
    /// Bigger episodes are not downloaded.
    pub max_episode_bytes: u64,
    pub feeds: Vec<FeedSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSettings {
    /// Folder name in the cache: 1 to 64 of `a-z`, `0-9`, `-`.
    pub slug: String,
    pub name: String,
    pub url: String,
    pub keep: Option<usize>,
    pub order: Order,
}

/// Which way the episodes of a feed are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    NewestFirst,
    /// For serial stories that are heard from the first part on.
    OldestFirst,
}

impl Order {
    fn arrange<T>(self, mut newest_first: Vec<T>) -> Vec<T> {
        if self == Order::OldestFirst {
            newest_first.reverse();
        }
        newest_first
    }
}

impl Settings {
    pub fn timing(&self) -> Timing {
        Timing {
            refresh: self.refresh,
            first_retry: FIRST_RETRY.min(self.refresh),
            sweep_grace: SWEEP_GRACE,
        }
    }

    fn keep(&self, feed: &FeedSettings) -> usize {
        feed.keep.unwrap_or(self.keep)
    }
}

/// Delays of the podcasts thread; [`Settings::timing`] gives the real ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub refresh: Duration,
    pub first_retry: Duration,
    pub sweep_grace: Duration,
}

/// What the cache holds for every configured feed, in the configured order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub feeds: Vec<FeedState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedState {
    pub slug: String,
    /// The configured name, else the feed's own title.
    pub title: String,
    /// Absolute path of a JPEG of at most 512 px per side.
    pub picture: Option<PathBuf>,
    /// In the feed's [`Order`].
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// 16 hex digits, stable across refreshes and restarts.
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub published: Option<i64>,
    /// Absolute path.
    pub file: PathBuf,
    /// `<slug>/<file name>`, relative to the cache folder: the path the file
    /// server gets through the [`Publisher`].
    pub rel: PathBuf,
    pub content_type: String,
    pub bytes: u64,
    /// Absolute path of a JPEG of at most 512 px per side.
    pub picture: Option<PathBuf>,
}

/// Lets the file server hand out cache files. Paths are relative to the cache
/// folder (`<slug>/<file>`). `add` comes before the [`Snapshot`] that names
/// the files, `remove` after they are deleted.
pub trait Publisher: Send {
    fn add(&self, rel_paths: &[PathBuf]);
    fn remove(&self, rel_paths: &[PathBuf]);
}

/// The id of the episode that is playing, or `None`: its file is not deleted
/// until another one plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying(pub Option<String>);

/// Fetches and reads the feed at `url`: its title and how many playable
/// episodes it lists.
pub fn check_feed(url: &str) -> Result<(String, usize)> {
    let agent = crate::net::api_agent(download::FEED_TIMEOUT);
    let feed = feed::parse(&download::fetch_feed(&agent, url)?)?;
    Ok((feed.title, feed.episodes.len()))
}

/// What the cache holds, from the manifests alone: no network.
pub fn load_cached(settings: &Settings) -> Snapshot {
    let feeds = settings
        .feeds
        .iter()
        .filter_map(
            |feed| match cache::feed_dir(&settings.cache_dir, &feed.slug) {
                Ok(dir) => {
                    let manifest = cache::load(&dir).unwrap_or_default();
                    let episodes = cache::present(&dir, &manifest);
                    Some(feed_state(&dir, feed, &manifest, &episodes))
                }
                Err(err) => {
                    tracing::warn!("podcast {}: {err:#}", feed.name);
                    None
                }
            },
        )
        .collect();
    Snapshot { feeds }
}

/// Starts the `podcasts` thread. It ends when the returned sender is dropped
/// or `snapshots` has no receiver.
pub fn spawn(
    settings: Settings,
    timing: Timing,
    publisher: Box<dyn Publisher>,
    snapshots: Sender<Snapshot>,
) -> Result<Sender<NowPlaying>> {
    validate(&settings)?;
    let (now_playing, pins) = mpsc::channel();
    let worker = refresh::Worker::new(settings, timing, publisher, snapshots, pins);
    thread::Builder::new()
        .name("podcasts".into())
        .spawn(move || worker.run())
        .context("cannot start the podcasts thread")?;
    Ok(now_playing)
}

fn validate(settings: &Settings) -> Result<()> {
    let mut slugs = HashSet::new();
    for feed in &settings.feeds {
        cache::feed_dir(&settings.cache_dir, &feed.slug)
            .with_context(|| format!("podcast {}", feed.name))?;
        if !slugs.insert(feed.slug.as_str()) {
            bail!("two podcast feeds use the folder name {:?}", feed.slug);
        }
        if feed::http_url(&feed.url).is_none() {
            bail!(
                "podcast {}: the feed URL must start with http(s)://",
                feed.name
            );
        }
    }
    Ok(())
}

/// `episodes` are newest first.
fn feed_state(
    dir: &Path,
    feed: &FeedSettings,
    manifest: &Manifest,
    episodes: &[Stored],
) -> FeedState {
    let picture = |name: &Option<String>| {
        name.as_ref()
            .map(|name| dir.join(name))
            .filter(|path| path.is_file())
    };
    let episodes = episodes
        .iter()
        .map(|stored| Episode {
            id: stored.id.clone(),
            title: stored.title.clone(),
            published: stored.published,
            file: dir.join(&stored.file),
            rel: Path::new(&feed.slug).join(&stored.file),
            content_type: stored.content_type.clone(),
            bytes: stored.bytes,
            picture: picture(&stored.picture),
        })
        .collect();
    let title = [&feed.name, &manifest.title, &feed.slug]
        .into_iter()
        .find(|t| !t.trim().is_empty())
        .cloned()
        .unwrap_or_default();
    FeedState {
        slug: feed.slug.clone(),
        title,
        picture: picture(&manifest.picture),
        episodes: feed.order.arrange(episodes),
    }
}
