//! The podcasts thread against a fake feed server: first refresh, offline
//! start, new and dropped episodes, pins, budget and backoff.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use super::feed::hash_hex;
use super::testserver::{FakeServer, Item, closed_port, png, rss};
use super::*;

const WAIT: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_millis(500);

fn timing() -> Timing {
    Timing {
        refresh: Duration::from_millis(100),
        first_retry: Duration::from_millis(50),
        sweep_grace: GRACE,
    }
}

#[derive(Debug)]
enum Event {
    Add(Vec<PathBuf>),
    Remove(Vec<PathBuf>),
}

struct Recorder(Sender<Event>);

impl Publisher for Recorder {
    fn add(&self, rel_paths: &[PathBuf]) {
        let _ = self.0.send(Event::Add(rel_paths.to_vec()));
    }

    fn remove(&self, rel_paths: &[PathBuf]) {
        let _ = self.0.send(Event::Remove(rel_paths.to_vec()));
    }
}

struct World {
    server: FakeServer,
    _files: tempfile::TempDir,
    cache: tempfile::TempDir,
}

impl World {
    /// Episode `n` is `e<n>.mp3`, `n * 100` bytes, published on day `n`.
    fn new() -> World {
        let files = tempfile::tempdir().unwrap();
        for n in 1..=3_u8 {
            let body = vec![n; usize::from(n) * 100];
            fs::write(files.path().join(format!("e{n}.mp3")), body).unwrap();
        }
        fs::write(files.path().join("cover.png"), png(1024, 768)).unwrap();
        World {
            server: FakeServer::start(files.path()),
            _files: files,
            cache: tempfile::tempdir().unwrap(),
        }
    }

    fn set_feed(&self, episodes: &[u8]) {
        let titles: Vec<String> = episodes.iter().map(|n| format!("Episode {n}")).collect();
        let guids: Vec<String> = episodes.iter().map(|n| format!("ep-{n}")).collect();
        let dates: Vec<String> = episodes
            .iter()
            .map(|n| format!("{n:02} Jan 2025 10:00:00 +0000"))
            .collect();
        let urls: Vec<String> = episodes
            .iter()
            .map(|n| self.server.url(&format!("/files/e{n}.mp3")))
            .collect();
        let items: Vec<Item> = (0..episodes.len())
            .map(|i| Item {
                guid: &guids[i],
                title: &titles[i],
                date: &dates[i],
                url: &urls[i],
                picture: None,
            })
            .collect();
        let cover = self.server.url("/files/cover.png");
        self.server.set_feed(&rss("Maus", Some(&cover), &items));
    }

    fn settings(&self, url: &str, keep: usize) -> Settings {
        Settings {
            cache_dir: self.cache.path().to_path_buf(),
            keep: 5,
            refresh: Duration::from_hours(6),
            max_cache_bytes: 1_000_000,
            max_episode_bytes: 10_000,
            feeds: vec![feed("maus", url, Some(keep), Order::NewestFirst)],
        }
    }

    fn feed_url(&self) -> String {
        self.server.url("/feed.xml")
    }
}

fn feed(slug: &str, url: &str, keep: Option<usize>, order: Order) -> FeedSettings {
    FeedSettings {
        slug: slug.into(),
        name: "Die Maus".into(),
        url: url.into(),
        keep,
        order,
    }
}

struct Running {
    now_playing: Sender<NowPlaying>,
    snapshots: Receiver<Snapshot>,
    events: Receiver<Event>,
}

fn start(settings: Settings, timing: Timing) -> Running {
    let (events_tx, events) = mpsc::channel();
    let (snapshots_tx, snapshots) = mpsc::channel();
    let now_playing = spawn(
        settings,
        timing,
        Box::new(Recorder(events_tx)),
        snapshots_tx,
    )
    .unwrap();
    Running {
        now_playing,
        snapshots,
        events,
    }
}

impl Running {
    fn snapshot_until(&self, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let snapshot = self.snapshots.recv_timeout(left).expect("no such snapshot");
            if done(&snapshot) {
                return snapshot;
            }
        }
    }

    fn next_event(&self) -> Event {
        self.events.recv_timeout(WAIT).expect("no event")
    }

    /// Ends the thread and waits until it has dropped the publisher.
    fn stop(self) {
        drop(self.now_playing);
        drop(self.snapshots);
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => panic!("the podcasts thread did not end"),
            }
        }
    }
}

fn titles(snapshot: &Snapshot) -> Vec<Vec<&str>> {
    snapshot
        .feeds
        .iter()
        .map(|f| f.episodes.iter().map(|e| e.title.as_str()).collect())
        .collect()
}

fn titles_are(expected: &[&str]) -> impl Fn(&Snapshot) -> bool {
    move |snapshot| titles(snapshot).first().is_some_and(|t| t == expected)
}

fn rel_of(snapshot: &Snapshot, title: &str) -> PathBuf {
    let episodes = snapshot.feeds.iter().flat_map(|f| &f.episodes);
    let episode = episodes.into_iter().find(|e| e.title == title).unwrap();
    episode.rel.clone()
}

#[test]
fn first_refresh_downloads_and_publishes() {
    let world = World::new();
    world.set_feed(&[1, 2]);
    let run = start(world.settings(&world.feed_url(), 2), timing());

    let snapshot = run.snapshot_until(titles_are(&["Episode 2", "Episode 1"]));

    let Ok(Event::Add(added)) = run.events.try_recv() else {
        panic!("the files were not published before the snapshot");
    };
    let state = &snapshot.feeds[0];
    assert_eq!(
        (state.slug.as_str(), state.title.as_str()),
        ("maus", "Die Maus")
    );
    let picture = state.picture.clone().unwrap();
    assert_eq!(image::image_dimensions(&picture).unwrap(), (512, 384));
    let newest = &state.episodes[0];
    assert_eq!(newest.id, hash_hex("ep-2"));
    assert_eq!(newest.published, Some(1_736_157_600 - 4 * 86_400));
    assert_eq!(newest.content_type, "audio/mpeg");
    assert_eq!(
        (newest.bytes, fs::read(&newest.file).unwrap()),
        (200, vec![2; 200])
    );
    assert_eq!(newest.file, world.cache.path().join(&newest.rel));
    assert!(newest.rel.starts_with("maus"));
    assert_eq!(newest.picture.as_ref(), Some(&picture));
    let expected: BTreeSet<PathBuf> = [
        rel_of(&snapshot, "Episode 1"),
        rel_of(&snapshot, "Episode 2"),
        picture
            .strip_prefix(world.cache.path())
            .unwrap()
            .to_path_buf(),
    ]
    .into();
    assert_eq!(added.into_iter().collect::<BTreeSet<_>>(), expected);
    run.stop();
}

#[test]
fn an_offline_start_publishes_the_cache() {
    let world = World::new();
    world.set_feed(&[1, 2]);
    let online = start(world.settings(&world.feed_url(), 2), timing());
    let filled = online.snapshot_until(titles_are(&["Episode 2", "Episode 1"]));
    online.stop();
    // A finished download that the manifest does not list yet.
    let unlisted = world
        .cache
        .path()
        .join("maus/20250103-episode-3-3a3b3c4d.mp3");
    fs::write(&unlisted, b"abc").unwrap();

    let offline = world.settings(&format!("{}/feed.xml", closed_port()), 2);
    let cached = load_cached(&offline);
    assert_eq!(cached, filled);
    let run = start(offline, timing());

    let Event::Add(added) = run.next_event() else {
        panic!("expected the cached files to be published");
    };
    assert!(added.contains(&rel_of(&filled, "Episode 1")));
    assert!(added.contains(&rel_of(&filled, "Episode 2")));
    assert_eq!(run.snapshot_until(|_| true), cached);
    let quiet = run.events.recv_timeout(GRACE * 2);
    assert!(
        quiet.is_err(),
        "an offline refresh changed files: {quiet:?}"
    );
    assert!(unlisted.exists(), "an offline refresh deleted a file");
    run.stop();
}

fn broken_manifest(world: &World) -> Snapshot {
    world.set_feed(&[1, 2]);
    let online = start(world.settings(&world.feed_url(), 2), timing());
    let filled = online.snapshot_until(titles_are(&["Episode 2", "Episode 1"]));
    online.stop();
    fs::write(world.cache.path().join("maus").join("feed.json"), "{").unwrap();
    filled
}

#[test]
fn a_broken_manifest_and_a_dead_feed_delete_nothing() {
    let world = World::new();
    let filled = broken_manifest(&world);
    let offline = world.settings(&format!("{}/feed.xml", closed_port()), 2);
    let run = start(offline, timing());

    run.snapshot_until(|_| true);
    let quiet = run.events.recv_timeout(GRACE * 3);

    assert!(
        quiet.is_err(),
        "an offline refresh changed files: {quiet:?}"
    );
    let picture = filled.feeds[0].picture.clone().unwrap();
    for file in [
        world.cache.path().join(rel_of(&filled, "Episode 1")),
        world.cache.path().join(rel_of(&filled, "Episode 2")),
        picture,
    ] {
        assert!(file.exists(), "{} was deleted", file.display());
    }
    run.stop();
}

#[test]
fn a_broken_manifest_is_written_again_from_the_feed_and_the_files() {
    let world = World::new();
    let filled = broken_manifest(&world);
    let settings = world.settings(&world.feed_url(), 2);
    let run = start(settings.clone(), timing());

    let snapshot = run.snapshot_until(titles_are(&["Episode 2", "Episode 1"]));

    assert_eq!(snapshot, filled);
    let Event::Add(added) = run.next_event() else {
        panic!("expected the files to be published again");
    };
    assert!(added.contains(&rel_of(&filled, "Episode 1")));
    assert!(added.contains(&rel_of(&filled, "Episode 2")));
    let quiet = run.events.recv_timeout(GRACE * 2);
    assert!(quiet.is_err(), "a file was deleted: {quiet:?}");
    assert_eq!(load_cached(&settings), filled);
    run.stop();
}

#[test]
fn the_refresh_that_rewrites_a_broken_manifest_deletes_nothing() {
    let world = World::new();
    let filled = broken_manifest(&world);
    let one_refresh = Timing {
        refresh: Duration::from_secs(60),
        ..timing()
    };
    let run = start(world.settings(&world.feed_url(), 1), one_refresh);

    run.snapshot_until(titles_are(&["Episode 2"]));
    assert!(matches!(run.next_event(), Event::Add(_)));
    let quiet = run.events.recv_timeout(GRACE * 3);

    assert!(quiet.is_err(), "a file was touched: {quiet:?}");
    let old = world.cache.path().join(rel_of(&filled, "Episode 1"));
    assert!(old.exists(), "deleted while the manifest was broken");
    run.stop();
}

#[test]
fn a_new_episode_arrives_with_the_next_refresh() {
    let world = World::new();
    world.set_feed(&[1]);
    let run = start(world.settings(&world.feed_url(), 2), timing());
    run.snapshot_until(titles_are(&["Episode 1"]));
    assert!(matches!(run.next_event(), Event::Add(_)));

    world.set_feed(&[1, 2]);
    let snapshot = run.snapshot_until(titles_are(&["Episode 2", "Episode 1"]));

    let Event::Add(added) = run.next_event() else {
        panic!("expected the new episode to be published");
    };
    assert_eq!(added, [rel_of(&snapshot, "Episode 2")]);
    run.stop();
}

#[test]
fn a_dropped_episode_is_deleted_after_the_grace() {
    let world = World::new();
    world.set_feed(&[1]);
    let run = start(world.settings(&world.feed_url(), 1), timing());
    let before = run.snapshot_until(titles_are(&["Episode 1"]));
    let old = rel_of(&before, "Episode 1");
    run.next_event();

    world.set_feed(&[1, 2]);
    run.snapshot_until(titles_are(&["Episode 2"]));
    assert!(
        world.cache.path().join(&old).exists(),
        "deleted before the grace"
    );

    assert!(matches!(run.next_event(), Event::Add(_)));
    let Event::Remove(removed) = run.next_event() else {
        panic!("expected the old episode to be unpublished");
    };
    assert_eq!(removed, std::slice::from_ref(&old));
    assert!(!world.cache.path().join(&old).exists());
    run.stop();
}

#[test]
fn the_playing_episode_is_kept_until_it_stops() {
    let world = World::new();
    world.set_feed(&[1]);
    let run = start(world.settings(&world.feed_url(), 1), timing());
    let before = run.snapshot_until(titles_are(&["Episode 1"]));
    let old = rel_of(&before, "Episode 1");
    run.next_event();
    run.now_playing
        .send(NowPlaying(Some(hash_hex("ep-1"))))
        .unwrap();

    world.set_feed(&[1, 2]);
    run.snapshot_until(titles_are(&["Episode 2"]));
    assert!(matches!(run.next_event(), Event::Add(_)));
    let early = run.events.recv_timeout(GRACE * 3);
    assert!(early.is_err(), "the playing episode was touched: {early:?}");
    assert!(world.cache.path().join(&old).exists());

    run.now_playing.send(NowPlaying(None)).unwrap();
    let Event::Remove(removed) = run.next_event() else {
        panic!("expected the old episode to go once it stopped");
    };
    assert_eq!(removed, std::slice::from_ref(&old));
    assert!(!world.cache.path().join(&old).exists());
    run.stop();
}

#[test]
fn the_budget_keeps_the_newest_of_every_feed() {
    let world = World::new();
    world.set_feed(&[1, 2, 3]);
    let url = world.feed_url();
    let mut settings = world.settings(&url, 2);
    settings
        .feeds
        .push(feed("serie", &url, None, Order::OldestFirst));
    // 300 + 300 fit, the second newest of either feed (200) does not.
    settings.max_cache_bytes = 700;
    let run = start(settings, timing());

    let snapshot = run.snapshot_until(|s| titles(s) == [["Episode 3"], ["Episode 3"]]);
    let loser = world.cache.path().join("serie");

    let deadline = Instant::now() + WAIT;
    while fs::read_dir(&loser).unwrap().count() > 3 {
        assert!(Instant::now() < deadline, "the over-budget episodes stayed");
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(timing().refresh * 3);
    let names: Vec<String> = fs::read_dir(&loser)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 3, "downloaded again: {names:?}");
    assert!(snapshot.feeds[1].episodes[0].file.exists());
    run.stop();
}

#[test]
fn serial_feeds_list_the_oldest_first() {
    let world = World::new();
    world.set_feed(&[1, 2, 3]);
    let mut settings = world.settings(&world.feed_url(), 2);
    settings.feeds[0].order = Order::OldestFirst;
    let run = start(settings.clone(), timing());

    let snapshot = run.snapshot_until(|s| titles(s) == [["Episode 2", "Episode 3"]]);

    assert_eq!(load_cached(&settings), snapshot);
    run.stop();
}

#[test]
fn backs_off_while_every_feed_fails() {
    let world = World::new();
    world.server.set_feed("<!DOCTYPE html><html></html>");
    let timing = Timing {
        refresh: Duration::from_secs(60),
        first_retry: Duration::from_millis(20),
        sweep_grace: GRACE,
    };
    let run = start(world.settings(&world.feed_url(), 2), timing);

    thread::sleep(Duration::from_millis(700));
    // 0, 20, 60, 140, 300, 620 ms: six tries; without backoff it would be 35.
    let tries = world.server.hits();
    assert!((3..=9).contains(&tries), "{tries} tries");

    world.set_feed(&[1]);
    run.snapshot_until(titles_are(&["Episode 1"]));
    let recovered = world.server.hits();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(world.server.hits(), recovered, "retried after a success");
    run.stop();
}

#[test]
fn the_thread_ends_without_a_now_playing_sender() {
    let world = World::new();
    world.set_feed(&[1]);
    let run = start(world.settings(&world.feed_url(), 1), timing());
    run.snapshot_until(titles_are(&["Episode 1"]));
    run.stop();
}

#[test]
fn spawn_refuses_unsafe_settings() {
    let world = World::new();
    let url = world.feed_url();
    let mut bad_slug = world.settings(&url, 1);
    bad_slug.feeds[0].slug = "../maus".into();
    let mut twice = world.settings(&url, 1);
    twice.feeds.push(twice.feeds[0].clone());
    let mut ftp = world.settings(&url, 1);
    ftp.feeds[0].url = "ftp://example.org/feed.xml".into();

    for settings in [bad_slug.clone(), twice, ftp] {
        let (events, _) = mpsc::channel();
        let (snapshots, _) = mpsc::channel();
        let publisher = Box::new(Recorder(events));
        assert!(spawn(settings, timing(), publisher, snapshots).is_err());
    }
    assert_eq!(load_cached(&bad_slug).feeds, []);
}

#[test]
fn real_timing_follows_the_refresh_interval() {
    let world = World::new();
    let mut settings = world.settings(&world.feed_url(), 1);
    let timing = settings.timing();
    assert_eq!(timing.refresh, Duration::from_hours(6));
    assert_eq!(timing.first_retry, Duration::from_secs(60));
    assert_eq!(timing.sweep_grace, Duration::from_secs(60));
    settings.refresh = Duration::from_secs(10);
    assert_eq!(settings.timing().first_retry, Duration::from_secs(10));
}

#[test]
fn feed_title_falls_back_to_the_feed_then_the_slug() {
    let dir = Path::new("/cache/maus");
    let mut settings = feed("maus", "https://x/feed.xml", None, Order::NewestFirst);
    let manifest = Manifest {
        title: "Maus Podcast".into(),
        ..Manifest::default()
    };
    assert_eq!(feed_state(dir, &settings, &manifest, &[]).title, "Die Maus");
    settings.name = " ".into();
    assert_eq!(
        feed_state(dir, &settings, &manifest, &[]).title,
        "Maus Podcast"
    );
    let empty = Manifest::default();
    assert_eq!(feed_state(dir, &settings, &empty, &[]).title, "maus");
}
