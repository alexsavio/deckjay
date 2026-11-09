//! Podcasts: feeds, downloads and the episode cache.
//!
//! [`start`] starts the `podcasts` thread. It refreshes every feed at once and
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

use crate::config::{self, FeedOrder};

use cache::Manifest;
use plan::Stored;

/// Retry delay after a refresh in which every feed failed; it doubles up to
/// the refresh interval.
const FIRST_RETRY: Duration = Duration::from_secs(60);
/// How long a dropped episode stays on disk and on the server, so the UI and
/// a speaker that is still fetching it can let go first.
const SWEEP_GRACE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Settings {
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
struct FeedSettings {
    /// Folder name in the cache: 1 to 64 of `a-z`, `0-9`, `-`.
    pub slug: String,
    pub name: String,
    pub url: String,
    pub keep: Option<usize>,
    pub order: FeedOrder,
}

impl FeedOrder {
    fn arrange<T>(self, mut newest_first: Vec<T>) -> Vec<T> {
        if self == FeedOrder::Oldest {
            newest_first.reverse();
        }
        newest_first
    }
}

impl Settings {
    fn timing(&self) -> Timing {
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
struct Timing {
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
    /// In the feed's [`FeedOrder`].
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

/// What the cache in `cache_dir` holds for `podcast`, from the manifests
/// alone: no network.
pub fn load_cached(podcast: &config::Podcast, cache_dir: &Path) -> Snapshot {
    cached(&settings(podcast, cache_dir))
}

fn cached(settings: &Settings) -> Snapshot {
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

/// Starts the `podcasts` thread of `podcast`, caching into `cache_dir`. It
/// ends when the returned sender is dropped or `snapshots` has no receiver.
pub fn start(
    podcast: &config::Podcast,
    cache_dir: &Path,
    publisher: Box<dyn Publisher>,
    snapshots: Sender<Snapshot>,
) -> Result<Sender<NowPlaying>> {
    let settings = settings(podcast, cache_dir);
    let timing = settings.timing();
    spawn(settings, timing, publisher, snapshots)
}

/// [`start`] with the delays given: the seam for tests.
fn spawn(
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

/// One cache folder per feed, named by [`slug`] and [`unique`].
fn settings(podcast: &config::Podcast, cache_dir: &Path) -> Settings {
    let mut slugs = HashSet::new();
    let feeds = podcast
        .feeds
        .iter()
        .map(|feed| FeedSettings {
            slug: unique(&slug(&feed.name), &mut slugs),
            name: feed.name.clone(),
            url: feed.url.clone(),
            keep: feed.keep,
            order: feed.order,
        })
        .collect();
    Settings {
        cache_dir: cache_dir.to_path_buf(),
        keep: podcast.keep,
        refresh: podcast.refresh,
        max_cache_bytes: podcast.max_cache_bytes,
        max_episode_bytes: podcast.max_episode_bytes,
        feeds,
    }
}

/// The longest cache folder name [`validate`] accepts.
const MAX_SLUG: usize = 64;

/// A cache folder name: lower-case letters, digits and `-`.
fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(MAX_SLUG).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "feed".into()
    } else {
        slug.into()
    }
}

/// `slug`, or `slug-2`, `slug-3` ... when an earlier feed has it.
fn unique(slug: &str, taken: &mut HashSet<String>) -> String {
    let mut candidate = slug.to_string();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        let suffix = format!("-{n}");
        let stem: String = slug.chars().take(MAX_SLUG - suffix.len()).collect();
        candidate = format!("{stem}{suffix}");
        n += 1;
    }
    candidate
}

fn validate(settings: &Settings) -> Result<()> {
    let mut slugs = HashSet::new();
    for feed in &settings.feeds {
        cache::feed_dir(&settings.cache_dir, &feed.slug)
            .with_context(|| format!("podcast {}", feed.name))?;
        if !slugs.insert(feed.slug.as_str()) {
            bail!("two podcast feeds use the folder name {:?}", feed.slug);
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
