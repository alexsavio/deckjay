//! The `podcasts` thread: refreshes the feeds, downloads, publishes and
//! sweeps. Waiting happens in `recv_timeout` on the [`NowPlaying`] channel, so
//! a pin arrives at once and a dropped sender ends the thread.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use ureq::Agent;

use super::cache::{self, Manifest};
use super::download::{self, Failure};
use super::feed::{self, Feed, RemoteEpisode};
use super::plan::{self, Plan, Stored};
use super::{FeedSettings, NowPlaying, Publisher, Settings, Snapshot, Timing, feed_state};
use crate::net;

const MB: u64 = 1024 * 1024;

pub(super) struct Worker {
    settings: Settings,
    timing: Timing,
    publisher: Box<dyn Publisher>,
    snapshots: Sender<Snapshot>,
    pins: Receiver<NowPlaying>,
    api: Agent,
    stream: Agent,
    pinned: Option<String>,
    /// Paths given to the publisher and not taken back since.
    published: HashSet<PathBuf>,
    /// Slugs of the feeds whose last fetch failed.
    failing: HashSet<String>,
    /// Picture URLs that failed, so each is logged once.
    broken_pictures: HashSet<String>,
    doomed: Doomed,
}

/// One feed during a refresh.
struct Run {
    feed: FeedSettings,
    dir: PathBuf,
    old: Manifest,
    /// `feed.json` could not be read, so `disk` may miss cached episodes.
    manifest_broken: bool,
    disk: Vec<Stored>,
    /// `None` when the fetch failed.
    remote: Option<Feed>,
    keep: usize,
    /// Unfinished downloads to keep for the next try.
    parts: HashSet<String>,
    downloaded: usize,
}

impl Run {
    fn episodes(&self) -> Option<&[RemoteEpisode]> {
        self.remote.as_ref().map(|feed| feed.episodes.as_slice())
    }
}

impl Worker {
    pub fn new(
        settings: Settings,
        timing: Timing,
        publisher: Box<dyn Publisher>,
        snapshots: Sender<Snapshot>,
        pins: Receiver<NowPlaying>,
    ) -> Worker {
        Worker {
            settings,
            timing,
            publisher,
            snapshots,
            pins,
            api: net::api_agent(download::FEED_TIMEOUT),
            stream: net::stream_agent(),
            pinned: None,
            published: HashSet::new(),
            failing: HashSet::new(),
            broken_pictures: HashSet::new(),
            doomed: Doomed::default(),
        }
    }

    pub fn run(mut self) {
        self.log_foreign_folders();
        self.publish_cached();
        let mut next_refresh = Instant::now();
        let mut retry = self.timing.first_retry;
        loop {
            let now = Instant::now();
            if now >= next_refresh {
                let Some(all_failed) = self.refresh() else {
                    return;
                };
                let delay = if all_failed {
                    let delay = retry;
                    retry = retry.saturating_mul(2).min(self.timing.refresh);
                    delay
                } else {
                    retry = self.timing.first_retry;
                    self.timing.refresh
                };
                next_refresh = later(Instant::now(), delay);
                continue;
            }
            let sweep_at = self.doomed.next(self.pinned.as_deref());
            if sweep_at.is_some_and(|at| at <= now) {
                self.sweep(now);
                continue;
            }
            let wake = sweep_at.map_or(next_refresh, |at| at.min(next_refresh));
            match self.pins.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(NowPlaying(id)) => self.pinned = id,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn drain_pins(&mut self) {
        while let Ok(NowPlaying(id)) = self.pins.try_recv() {
            self.pinned = id;
        }
    }

    fn log_foreign_folders(&self) {
        let slugs: HashSet<&str> = self
            .settings
            .feeds
            .iter()
            .map(|f| f.slug.as_str())
            .collect();
        for dir in cache::foreign_folders(&self.settings.cache_dir, &slugs) {
            info!(
                "podcast cache: leaving {} alone, no configured feed uses it",
                dir.display()
            );
        }
    }

    /// The cache is playable before the first refresh finishes, even offline.
    fn publish_cached(&mut self) {
        let mut cached = BTreeSet::new();
        for feed in &self.settings.feeds {
            let Ok(dir) = cache::feed_dir(&self.settings.cache_dir, &feed.slug) else {
                continue;
            };
            let manifest = cache::load(&dir).unwrap_or_default();
            let present = Manifest {
                episodes: cache::present(&dir, &manifest),
                ..manifest
            };
            cached.extend(files_to_publish(&dir, &feed.slug, &present));
        }
        self.publish(cached);
    }

    fn publish(&mut self, wanted: BTreeSet<PathBuf>) {
        let new: Vec<PathBuf> = wanted
            .into_iter()
            .filter(|rel| !self.published.contains(rel))
            .collect();
        if !new.is_empty() {
            self.publisher.add(&new);
            self.published.extend(new);
        }
    }

    /// `None` when nobody listens to snapshots any more, else whether every
    /// feed failed.
    fn refresh(&mut self) -> Option<bool> {
        self.drain_pins();
        let feeds = self.settings.feeds.clone();
        let mut runs: Vec<Run> = feeds.into_iter().filter_map(|f| self.fetch(f)).collect();

        let sizes: Vec<Vec<u64>> = runs
            .iter()
            .map(|r| plan::wanted_sizes(r.episodes(), &r.disk, r.keep))
            .collect();
        let counts = plan::fit_budget(&sizes, self.settings.max_cache_bytes);
        let mut disk_ok = true;
        for (run, keep) in runs.iter_mut().zip(counts) {
            run.keep = keep;
            disk_ok = self.download(run, disk_ok);
        }

        self.drain_pins();
        let mut snapshot = Snapshot::default();
        let mut wanted = BTreeSet::new();
        let mut unwanted = Vec::new();
        let mut dropped = 0;
        for (run, plan) in runs.iter().zip(self.final_plans(&runs)) {
            dropped += plan.drop.len();
            let manifest = self.settle(run, plan.publish);
            wanted.extend(files_to_publish(&run.dir, &run.feed.slug, &manifest));
            // Without the feed or the old manifest, `manifest` can miss cached
            // files that are still good; they wait for a refresh that has both.
            if run.remote.is_some() && !run.manifest_broken {
                let keep = keep_names(run, &manifest);
                let stale = cache::unwanted(&run.dir, &keep);
                unwanted.extend(
                    stale
                        .iter()
                        .map(|name| Path::new(&run.feed.slug).join(name)),
                );
            }
            snapshot.feeds.push(feed_state(
                &run.dir,
                &run.feed,
                &manifest,
                &manifest.episodes,
            ));
        }
        self.publish(wanted);
        self.doomed
            .schedule(unwanted, Instant::now(), self.timing.sweep_grace);
        log_summary(&runs, &snapshot, dropped);
        self.snapshots.send(snapshot).ok()?;
        Some(!runs.is_empty() && runs.iter().all(|run| run.remote.is_none()))
    }

    fn fetch(&mut self, feed: FeedSettings) -> Option<Run> {
        let dir = cache::feed_dir(&self.settings.cache_dir, &feed.slug).ok()?;
        let loaded = cache::load(&dir);
        let manifest_broken = loaded.is_none();
        let old = loaded.unwrap_or_default();
        let disk = cache::present(&dir, &old);
        let url = without_query(&feed.url);
        let remote = match download::fetch_feed(&self.api, &feed.url).and_then(|b| feed::parse(&b))
        {
            Ok(remote) => {
                if self.failing.remove(&feed.slug) {
                    info!("podcast {}: {url} works again", feed.name);
                }
                Some(remote)
            }
            Err(err) => {
                let kept = disk.len();
                if self.failing.insert(feed.slug.clone()) {
                    warn!(
                        "podcast {}: cannot refresh {url}: {err:#}; keeping {kept} episodes",
                        feed.name
                    );
                } else {
                    debug!("podcast {}: still cannot refresh {url}: {err:#}", feed.name);
                }
                None
            }
        };
        let keep = self.settings.keep(&feed);
        Some(Run {
            feed,
            dir,
            old,
            manifest_broken,
            disk,
            remote,
            keep,
            parts: HashSet::new(),
            downloaded: 0,
        })
    }

    /// Downloads what the plan asks for, unless `allowed` is false after a
    /// disk failure. Returns false after a disk failure.
    fn download(&mut self, run: &mut Run, allowed: bool) -> bool {
        let plan = plan::plan(run.episodes(), &run.disk, run.keep, self.pinned.as_deref());
        let targets: Vec<(RemoteEpisode, String)> = plan
            .download
            .into_iter()
            .map(|r| {
                let name = cache::episode_name(r.published, &r.title, &r.id, r.ext);
                (r, name)
            })
            .collect();
        run.parts = targets
            .iter()
            .map(|(_, name)| cache::part_name(name))
            .collect();
        if !allowed {
            return false;
        }
        for (remote, name) in targets {
            match self.fetch_episode(&run.dir, &remote, &name) {
                Ok(bytes) => {
                    run.downloaded += 1;
                    run.disk.push(Stored {
                        id: remote.id,
                        title: remote.title,
                        published: remote.published,
                        file: name,
                        content_type: remote.mime.to_string(),
                        bytes,
                        picture: None,
                    });
                }
                Err(Failure::Disk(err)) => {
                    warn!(
                        "podcast {}: {err:#}; no more downloads until the next refresh",
                        run.feed.name
                    );
                    return false;
                }
                Err(failure) => warn!(
                    "podcast {}: cannot download {:?}: {failure}",
                    run.feed.name, remote.title
                ),
            }
        }
        true
    }

    /// A finished file that no manifest lists yet (the program stopped
    /// between the rename and the manifest) is used as it is.
    fn fetch_episode(
        &self,
        dir: &Path,
        remote: &RemoteEpisode,
        name: &str,
    ) -> Result<u64, Failure> {
        if let Ok(meta) = fs::metadata(dir.join(name))
            && meta.is_file()
            && meta.len() > 0
        {
            return Ok(meta.len());
        }
        let max = self.settings.max_episode_bytes;
        download::episode(&self.stream, &remote.audio_url, dir, name, max)
    }

    /// Plans with the downloads done; the budget is checked again with the
    /// real file sizes.
    fn final_plans(&self, runs: &[Run]) -> Vec<Plan> {
        let pinned = self.pinned.as_deref();
        let plans: Vec<Plan> = runs
            .iter()
            .map(|r| plan::plan(r.episodes(), &r.disk, r.keep, pinned))
            .collect();
        let sizes: Vec<Vec<u64>> = plans
            .iter()
            .map(|p| p.publish.iter().map(|s| s.bytes).collect())
            .collect();
        let counts = plan::fit_budget(&sizes, self.settings.max_cache_bytes);
        runs.iter()
            .zip(plans)
            .zip(counts)
            .map(|((run, plan), count)| {
                if count < plan.publish.len() {
                    plan::plan(run.episodes(), &run.disk, count, pinned)
                } else {
                    plan
                }
            })
            .collect()
    }

    /// Adds pictures to the published episodes and writes the manifest when
    /// it changed.
    fn settle(&mut self, run: &Run, publish: Vec<Stored>) -> Manifest {
        let (title, picture) = match &run.remote {
            Some(remote) => (
                remote.title.clone(),
                remote
                    .picture_url
                    .as_deref()
                    .and_then(|url| self.picture(run, url)),
            ),
            None => (run.old.title.clone(), run.old.picture.clone()),
        };
        let urls: HashMap<&str, &str> = run
            .episodes()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| Some((r.id.as_str(), r.picture_url.as_deref()?)))
            .collect();
        let episodes = publish
            .into_iter()
            .map(|stored| match urls.get(stored.id.as_str()) {
                Some(url) => Stored {
                    picture: self.picture(run, url),
                    ..stored
                },
                None => stored,
            })
            .collect();
        let manifest = Manifest {
            title,
            picture,
            episodes,
        };
        if manifest != run.old
            && let Err(err) = cache::save(&run.dir, &manifest)
        {
            warn!("podcast {}: {err:#}", run.feed.name);
        }
        manifest
    }

    fn picture(&mut self, run: &Run, url: &str) -> Option<String> {
        let name = cache::picture_name(url);
        if run.dir.join(&name).is_file() {
            return Some(name);
        }
        match download::picture(&self.api, url, &run.dir, &name) {
            Ok(()) => {
                self.broken_pictures.remove(url);
                Some(name)
            }
            Err(err) => {
                let first = self.broken_pictures.insert(url.to_string());
                let url = without_query(url);
                if first {
                    warn!("podcast {}: no picture from {url}: {err:#}", run.feed.name);
                } else {
                    debug!(
                        "podcast {}: still no picture from {url}: {err:#}",
                        run.feed.name
                    );
                }
                None
            }
        }
    }

    fn sweep(&mut self, now: Instant) {
        let mut removed = Vec::new();
        for rel in self.doomed.take_due(now, self.pinned.as_deref()) {
            match cache::delete(&self.settings.cache_dir.join(&rel)) {
                Ok(()) => {
                    debug!("podcast cache: deleted {}", rel.display());
                    if self.published.remove(&rel) {
                        removed.push(rel);
                    }
                }
                Err(err) => warn!("podcast cache: cannot delete {}: {err}", rel.display()),
            }
        }
        if !removed.is_empty() {
            self.publisher.remove(&removed);
        }
    }
}

/// Episode files and pictures of `manifest`, relative to the cache folder.
fn files_to_publish(dir: &Path, slug: &str, manifest: &Manifest) -> Vec<PathBuf> {
    let episodes = manifest
        .episodes
        .iter()
        .flat_map(|e| [Some(&e.file), e.picture.as_ref()]);
    episodes
        .chain([manifest.picture.as_ref()])
        .flatten()
        .filter(|name| dir.join(name).is_file())
        .map(|name| Path::new(slug).join(name))
        .collect()
}

fn keep_names(run: &Run, manifest: &Manifest) -> HashSet<String> {
    let episodes = manifest
        .episodes
        .iter()
        .flat_map(|e| [Some(e.file.clone()), e.picture.clone()]);
    episodes
        .chain([manifest.picture.clone()])
        .flatten()
        .chain(run.parts.iter().cloned())
        .collect()
}

fn log_summary(runs: &[Run], snapshot: &Snapshot, dropped: usize) {
    let ok = runs.iter().filter(|run| run.remote.is_some()).count();
    let new: usize = runs.iter().map(|run| run.downloaded).sum();
    let episodes = snapshot.feeds.iter().flat_map(|feed| &feed.episodes);
    let (count, bytes) = episodes.fold((0, 0), |(n, sum), e| (n + 1, sum + e.bytes));
    info!(
        "podcasts: {ok} of {} feeds refreshed, {new} new, {count} episodes ({} MB), {dropped} dropped",
        runs.len(),
        bytes / MB
    );
}

/// Feed URLs may carry a private token in the query; logs leave it out.
fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or_default()
}

fn later(now: Instant, delay: Duration) -> Instant {
    const YEAR: Duration = Duration::from_hours(365 * 24);
    now.checked_add(delay).unwrap_or(now + YEAR)
}

/// Unwanted cache files and the time each may be deleted.
#[derive(Debug, Default)]
struct Doomed(HashMap<PathBuf, Instant>);

impl Doomed {
    /// Files already waiting keep their time; files that are wanted again
    /// are spared.
    fn schedule(&mut self, unwanted: Vec<PathBuf>, now: Instant, grace: Duration) {
        let due = later(now, grace);
        self.0 = unwanted
            .into_iter()
            .map(|rel| {
                let at = self.0.get(&rel).copied().unwrap_or(due);
                (rel, at)
            })
            .collect();
    }

    /// The next deletion; files of the pinned episode wait.
    fn next(&self, pinned: Option<&str>) -> Option<Instant> {
        self.0
            .iter()
            .filter(|(rel, _)| !is_pinned(rel, pinned))
            .map(|(_, &at)| at)
            .min()
    }

    fn take_due(&mut self, now: Instant, pinned: Option<&str>) -> Vec<PathBuf> {
        let due: Vec<PathBuf> = self
            .0
            .iter()
            .filter(|&(rel, &at)| at <= now && !is_pinned(rel, pinned))
            .map(|(rel, _)| rel.clone())
            .collect();
        for rel in &due {
            self.0.remove(rel);
        }
        due
    }
}

fn is_pinned(rel: &Path, pinned: Option<&str>) -> bool {
    let name = rel.file_name().and_then(|name| name.to_str());
    matches!((name, pinned), (Some(name), Some(id)) if cache::is_file_of(name, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1a2b3c4d5e6f7a8b";

    fn rel(name: &str) -> PathBuf {
        Path::new("maus").join(name)
    }

    #[test]
    fn doomed_files_wait_for_the_grace() {
        let grace = Duration::from_secs(60);
        let t0 = Instant::now();
        let mut doomed = Doomed::default();
        doomed.schedule(vec![rel("p-00000000.jpg")], t0, grace);

        assert_eq!(doomed.next(None), Some(t0 + grace));
        assert_eq!(
            doomed.take_due(t0 + grace.saturating_sub(Duration::from_millis(1)), None),
            [] as [PathBuf; 0]
        );
        assert_eq!(doomed.take_due(t0 + grace, None), [rel("p-00000000.jpg")]);
        assert_eq!(doomed.next(None), None);
    }

    #[test]
    fn a_later_refresh_does_not_move_the_deadline_but_can_spare_a_file() {
        let grace = Duration::from_secs(60);
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(10);
        let mut doomed = Doomed::default();
        doomed.schedule(vec![rel("a"), rel("b")], t0, grace);
        doomed.schedule(vec![rel("a"), rel("c")], t1, grace);

        assert_eq!(doomed.take_due(t0 + grace, None), [rel("a")]);
        assert_eq!(doomed.take_due(t1 + grace, None), [rel("c")]);
    }

    #[test]
    fn the_pinned_episode_waits_until_it_is_unpinned() {
        let grace = Duration::from_secs(60);
        let t0 = Instant::now();
        let episode = rel("20250107-elefant-1a2b3c4d.mp3");
        let mut doomed = Doomed::default();
        doomed.schedule(vec![episode.clone()], t0, grace);

        assert_eq!(doomed.next(Some(ID)), None);
        assert_eq!(
            doomed.take_due(t0 + grace * 10, Some(ID)),
            [] as [PathBuf; 0]
        );
        assert_eq!(doomed.next(Some("ffffffff00000000")), Some(t0 + grace));
        assert_eq!(doomed.take_due(t0 + grace * 10, None), [episode]);
    }

    #[test]
    fn logs_leave_out_url_queries() {
        assert_eq!(
            without_query("https://x.org/feed.xml?token=secret"),
            "https://x.org/feed.xml"
        );
        assert_eq!(without_query("https://x.org/f#a"), "https://x.org/f");
    }
}
