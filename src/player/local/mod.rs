//! This computer's own sound output ("audio out"): the album plays from the
//! files in the music folder, with no network speaker.
//!
//! [`decode`] reads the files (symphonia), [`engine`] keeps the current track
//! flowing to the sound card without locks, and [`output`] is the sound card
//! itself (cpal). The sound card is opened at the first album, not at start
//! up, so a computer without one fails only when an album is pressed; a
//! broken stream fails the next command or poll, and the next album opens
//! the output again. A file that cannot be decoded (Opus, a broken file) is
//! skipped with a warning. `docs/local-audio.md` has the details.

mod decode;
mod engine;
mod output;

use std::time::Duration;

use anyhow::Result;
use tracing::{debug, info, warn};

use self::decode::Source;
use self::output::OpenOutput;
pub use self::output::output_devices;
use super::{Emitter, PlayerCmd, PlayerEvent, Speaker, TrackInfo};
use crate::library::ItemId;

/// How often the player looks for the end of a track, which is also the
/// longest gap between two tracks.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// "Previous" restarts the current track once it has played this long.
const RESTART_AFTER: Duration = Duration::from_secs(5);

/// Opens the sound output; tests put in one without a sound card.
type Open = Box<dyn FnMut() -> Result<OpenOutput>>;

pub(super) struct LocalPlayer {
    open: Open,
    /// `None` until the first album, and after a failure.
    output: Option<OpenOutput>,
    /// Id of the album we started last.
    album: ItemId,
    /// Tracks of the album we started last.
    tracks: Vec<TrackInfo>,
    volume: f32,
    /// The track playing or paused; `None` while no album is active.
    current: Option<usize>,
    paused: bool,
}

impl LocalPlayer {
    /// `device` is part of the output's name; `None` is the default output.
    pub(super) fn new(device: Option<String>) -> LocalPlayer {
        LocalPlayer::with_output(Box::new(move || OpenOutput::open(device.as_deref())))
    }

    fn with_output(open: Open) -> LocalPlayer {
        LocalPlayer {
            open,
            output: None,
            album: ItemId(0),
            tracks: Vec::new(),
            volume: 1.0,
            current: None,
            paused: false,
        }
    }
}

impl Speaker for LocalPlayer {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        match cmd {
            PlayerCmd::PlayAlbum {
                album,
                tracks,
                volume,
            } => {
                self.album = album;
                self.tracks = tracks;
                self.volume = super::clamp_volume(volume);
                self.current = None;
                // A new album is the moment to try a broken output again.
                if let Some(Err(err)) = self.output.as_ref().map(|o| o.engine.check()) {
                    debug!("reopening the sound output: {err:#}");
                    self.output = None;
                }
                self.play_from(0, events)
            }
            PlayerCmd::SetVolume(volume) => {
                self.check()?;
                self.volume = super::clamp_volume(volume);
                if let Some(output) = &self.output {
                    output.engine.set_volume(self.volume);
                }
                Ok(())
            }
            PlayerCmd::TogglePause => {
                self.check()?;
                self.toggle_pause(events)
            }
            PlayerCmd::Next => {
                self.check()?;
                match self.current {
                    Some(track) if track + 1 < self.tracks.len() => {
                        self.play_from(track + 1, events)
                    }
                    _ => Ok(()),
                }
            }
            PlayerCmd::Prev => {
                self.check()?;
                let Some(track) = self.current else {
                    return Ok(());
                };
                let played = self
                    .output
                    .as_ref()
                    .map_or(Duration::ZERO, |o| o.engine.position());
                if played >= RESTART_AFTER {
                    self.play_from(track, events)
                } else {
                    self.play_from(track.saturating_sub(1), events)
                }
            }
        }
    }

    fn poll(&mut self, events: &mut Emitter) -> Result<()> {
        let (Some(track), Some(output)) = (self.current, &self.output) else {
            return Ok(());
        };
        output.engine.check()?;
        // A paused track may have ended just before the pause: it moves on
        // once resumed, not while paused.
        if !self.paused && output.engine.finished() {
            self.play_from(track + 1, events)?;
        }
        Ok(())
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.current.map(|_| POLL_INTERVAL)
    }

    /// Also closes the output, so the next album opens it afresh.
    fn reset(&mut self) {
        self.current = None;
        self.paused = false;
        self.output = None;
    }
}

impl LocalPlayer {
    /// Fails once the open output broke.
    fn check(&self) -> Result<()> {
        match &self.output {
            Some(output) => output.engine.check(),
            None => Ok(()),
        }
    }

    fn toggle_pause(&mut self, events: &mut Emitter) -> Result<()> {
        match (self.current, &self.output) {
            (Some(_), Some(output)) => {
                self.paused = !self.paused;
                output.engine.set_paused(self.paused);
                events.emit(if self.paused {
                    PlayerEvent::Paused(self.album)
                } else {
                    PlayerEvent::Playing(self.album)
                });
                Ok(())
            }
            // The album ended (or failed): start it again from the top.
            _ if !self.tracks.is_empty() => self.play_from(0, events),
            _ => {
                events.emit(PlayerEvent::Stopped);
                Ok(())
            }
        }
    }

    /// Plays the first track from `start` on that can be decoded; with none
    /// left, the album is over.
    fn play_from(&mut self, start: usize, events: &mut Emitter) -> Result<()> {
        let output = match &mut self.output {
            Some(output) => output,
            empty @ None => empty.insert((self.open)()?),
        };
        output.engine.set_volume(self.volume);
        for (index, track) in self.tracks.iter().enumerate().skip(start) {
            match Source::open(&track.path) {
                Ok(source) => {
                    output.engine.play(source)?;
                    info!(album = %track.album, track = %track.title, "playing");
                    self.current = Some(index);
                    self.paused = false;
                    events.emit(PlayerEvent::Playing(self.album));
                    return Ok(());
                }
                Err(err) => warn!("skipping {}: {err:#}", track.path.display()),
            }
        }
        output.engine.stop();
        info!("end of the album");
        self.current = None;
        self.paused = false;
        events.emit(PlayerEvent::Stopped);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
