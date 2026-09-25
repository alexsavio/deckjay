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
        let mut changed = false;
        for thread in &self.podcasts {
            let Some(snapshot) = thread.snapshots.try_iter().last() else {
                continue;
            };
            let items = library::podcast::items(&thread.source, &snapshot);
            changed |= self.library.refill(thread.shelf, items);
        }
        if changed {
            self.reshelve();
        }
        changed
    }

    /// Tells each podcast thread which of its episodes plays, so that it
    /// keeps the file until another one plays.
    pub(super) fn pin_playing(&mut self) {
        for thread in &mut self.podcasts {
            let episode = self
                .current
                .filter(|id| self.library.shelves()[thread.shelf].items.contains(id))
                .and_then(|id| {
                    let key = &self.library.item(id).key.0;
                    key.rsplit('/').next().map(str::to_string)
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
