//! Podcast shelves fill in while the deck runs: each podcast source has a
//! `podcasts` thread that sends a snapshot of its cache after every refresh.

use std::sync::mpsc::{Receiver, Sender};

use tracing::warn;

use super::Ui;
use crate::config::Source;
use crate::library;
use crate::podcasts::{NowPlaying, Snapshot};

pub struct PodcastThread {
    /// The library shelf of `source`.
    shelf: usize,
    source: Source,
    snapshots: Receiver<Snapshot>,
    /// Dropping it ends the thread.
    now_playing: Sender<NowPlaying>,
    /// The episode id last sent to `now_playing`.
    pinned: Option<String>,
}

impl PodcastThread {
    pub fn new(
        shelf: usize,
        source: Source,
        snapshots: Receiver<Snapshot>,
        now_playing: Sender<NowPlaying>,
    ) -> PodcastThread {
        PodcastThread {
            shelf,
            source,
            snapshots,
            now_playing,
            pinned: None,
        }
    }
}

impl Ui {
    pub fn set_podcasts(&mut self, threads: Vec<PodcastThread>) {
        self.podcasts = threads;
    }

    /// Puts the newest snapshot of each podcast thread on its shelf.
    /// Returns whether a shelf changed.
    pub fn take_snapshots(&mut self) -> bool {
        let (mut changed, mut moved) = (false, false);
        for thread in &self.podcasts {
            let Some(snapshot) = thread.snapshots.try_iter().last() else {
                continue;
            };
            let items = library::podcast::items(&thread.source, &snapshot);
            let refilled = self.library.refill(thread.shelf, items);
            changed |= refilled.moved || !refilled.restyled.is_empty();
            for id in refilled.restyled {
                self.tiles.remove(&id);
                self.restyled.insert(id);
            }
            moved |= refilled.moved;
        }
        if moved {
            self.reshelve();
        }
        changed
    }

    /// Tells each podcast thread which of its episodes is loaded, so that
    /// it keeps the file until the episode stops or another item starts.
    /// The key tells the thread, not the shelf: a refresh can take a
    /// playing episode off its shelf.
    pub(super) fn pin_playing(&mut self) {
        for thread in &mut self.podcasts {
            let prefix = format!("{}/", thread.source.name);
            let episode = self.current.and_then(|id| {
                let key = &self.library.item(id).key.0;
                key.strip_prefix(&prefix)?
                    .rsplit('/')
                    .next()
                    .map(str::to_string)
            });
            if episode != thread.pinned {
                if thread
                    .now_playing
                    .send(NowPlaying(episode.clone()))
                    .is_err()
                {
                    warn!("the podcasts thread of {} is gone", thread.source.name);
                }
                thread.pinned = episode;
            }
        }
    }
}
