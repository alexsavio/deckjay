//! What the deck remembers across restarts, in `<state_dir>/state.json`:
//! the shelf on the deck, the page of each shelf, and how far each item that
//! resumes (audiobooks) got.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const FILE_NAME: &str = "state.json";
const VERSION: u32 = 1;
/// Progress changes every few seconds; writing at most this often spares
/// the Pi's SD card.
const SAVE_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    version: u32,
    /// The name of the shelf on the deck.
    shelf: Option<String>,
    /// The page of each shelf, by shelf name.
    pages: BTreeMap<String, usize>,
    /// By item key.
    progress: BTreeMap<String, Progress>,
}

impl Default for State {
    fn default() -> State {
        State {
            version: VERSION,
            shelf: None,
            pages: BTreeMap::new(),
            progress: BTreeMap::new(),
        }
    }
}

/// How far an item got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub track: usize,
    /// The track's path in the library (`<source>/<path>`), to find it
    /// again after files were added to or removed from the item.
    pub file: String,
    pub position_ms: u64,
    /// The track's length, when the speaker told it.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Played to the end: the next press starts over.
    #[serde(default)]
    pub finished: bool,
    /// Unix seconds.
    pub saved: u64,
}

impl Progress {
    /// Where to play again, given the item's track paths now: the saved
    /// track and position, or the start once finished or when the file is
    /// gone.
    pub fn resume_at(&self, files: &[&Path]) -> (usize, Duration) {
        if self.finished {
            return (0, Duration::ZERO);
        }
        let same = |i: &usize| files.get(*i).is_some_and(|f| *f == Path::new(&self.file));
        let track = Some(self.track)
            .filter(same)
            .or_else(|| (0..files.len()).find(same));
        match track {
            Some(track) => (track, Duration::from_millis(self.position_ms)),
            None => (0, Duration::ZERO),
        }
    }

    /// How much of an item of `tracks` tracks is done, from 0.0 to 1.0.
    pub fn done(&self, tracks: usize) -> f32 {
        if self.finished {
            return 1.0;
        }
        let in_track = match self.duration_ms {
            Some(duration) if duration > 0 => (self.position_ms as f32 / duration as f32).min(1.0),
            _ => 0.0,
        };
        ((self.track as f32 + in_track) / tracks.max(1) as f32).min(1.0)
    }
}

pub struct Store {
    /// `None`: the state cannot be saved, so it lives only in memory.
    path: Option<PathBuf>,
    state: State,
    dirty: bool,
    last_save: Option<Instant>,
    /// The last save failed; the warning is not repeated until one works.
    failing: bool,
}

impl Store {
    /// Reads `dir/state.json`. A missing file starts empty. A broken one is
    /// kept as `state.json.bad` and the store starts empty. A folder that
    /// cannot be written leaves the store in memory only, with a warning.
    pub fn open(dir: &Path) -> Store {
        let path = dir.join(FILE_NAME);
        if let Err(err) = std::fs::create_dir_all(dir) {
            warn!(
                "cannot create the state folder {}: {err}; nothing will be remembered",
                dir.display()
            );
            return Store::memory(State::default());
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Store::file(path, State::default());
            }
            Err(err) => {
                warn!(
                    "cannot read {}: {err}; nothing will be remembered",
                    path.display()
                );
                return Store::memory(State::default());
            }
        };
        match serde_json::from_str::<State>(&text) {
            Ok(state) if state.version > VERSION => {
                warn!(
                    "{} is from a newer deckjay; it is left as it is and nothing will be remembered",
                    path.display()
                );
                Store::memory(State::default())
            }
            Ok(state) => Store::file(path, state),
            Err(err) => {
                let bad = path.with_extension("json.bad");
                warn!(
                    "{} is broken ({err}); it is moved to {} and the deck starts afresh",
                    path.display(),
                    bad.display()
                );
                if let Err(err) = std::fs::rename(&path, &bad) {
                    warn!("cannot move {}: {err}", path.display());
                }
                Store::file(path, State::default())
            }
        }
    }

    /// A store that never touches the disk.
    pub fn in_memory() -> Store {
        Store::memory(State::default())
    }

    fn memory(state: State) -> Store {
        Store {
            path: None,
            state,
            dirty: false,
            last_save: None,
            failing: false,
        }
    }

    fn file(path: PathBuf, state: State) -> Store {
        Store {
            path: Some(path),
            state,
            dirty: false,
            last_save: None,
            failing: false,
        }
    }

    pub fn shelf(&self) -> Option<&str> {
        self.state.shelf.as_deref()
    }

    pub fn set_shelf(&mut self, name: &str) {
        if self.state.shelf.as_deref() != Some(name) {
            self.state.shelf = Some(name.into());
            self.dirty = true;
        }
    }

    pub fn page(&self, shelf: &str) -> usize {
        self.state.pages.get(shelf).copied().unwrap_or(0)
    }

    pub fn set_page(&mut self, shelf: &str, page: usize) {
        if self.page(shelf) != page {
            self.state.pages.insert(shelf.into(), page);
            self.dirty = true;
        }
    }

    pub fn progress(&self, key: &str) -> Option<&Progress> {
        self.state.progress.get(key)
    }

    /// `file` is the track's path in the library.
    pub fn set_progress(
        &mut self,
        key: &str,
        track: usize,
        file: &Path,
        position: Duration,
        duration: Option<Duration>,
    ) {
        let progress = Progress {
            track,
            file: file.to_string_lossy().into_owned(),
            position_ms: millis(position),
            duration_ms: duration.map(millis),
            finished: false,
            saved: unix_now(),
        };
        self.state.progress.insert(key.into(), progress);
        self.dirty = true;
    }

    pub fn finish(&mut self, key: &str) {
        if let Some(progress) = self.state.progress.get_mut(key) {
            progress.finished = true;
            progress.saved = unix_now();
        } else {
            self.state.progress.insert(
                key.into(),
                Progress {
                    track: 0,
                    file: String::new(),
                    position_ms: 0,
                    duration_ms: None,
                    finished: true,
                    saved: unix_now(),
                },
            );
        }
        self.dirty = true;
    }

    /// Saves unsaved changes, unless the last save was less than 10 s ago.
    pub fn save_if_due(&mut self, now: Instant) {
        let due = self
            .last_save
            .is_none_or(|last| now.duration_since(last) >= SAVE_EVERY);
        if self.dirty && due {
            self.save(now);
        }
    }

    /// Saves unsaved changes now, e.g. on pause or when an item ends.
    pub fn save_now(&mut self, now: Instant) {
        if self.dirty {
            self.save(now);
        }
    }

    fn save(&mut self, now: Instant) {
        self.last_save = Some(now);
        let Some(path) = &self.path else {
            self.dirty = false;
            return;
        };
        match write_atomically(path, &self.state) {
            Ok(()) => {
                if self.failing {
                    info!("saved {} again", path.display());
                }
                self.dirty = false;
                self.failing = false;
            }
            Err(err) if !self.failing => {
                warn!(
                    "cannot save {}: {err:#}; trying again every 10 s",
                    path.display()
                );
                self.failing = true;
            }
            Err(_) => {}
        }
    }
}

/// Writes a temporary file and renames it over `path`, so a power cut
/// leaves the old state or the new one, never half of one.
fn write_atomically(path: &Path, state: &State) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(state)?;
    let mut file =
        File::create(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        // The rename itself is only on disk once the folder is synced.
        let _ = File::open(dir).and_then(|dir| dir.sync_all());
    }
    Ok(())
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests;
