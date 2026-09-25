//! This computer's own sound output ("audio out"): the album plays from the
//! files in the music folder, with no network speaker.
//!
//! [`decode`] reads the files (symphonia), [`engine`] keeps the current track
//! flowing to the sound card without locks, and [`output`] is the sound card
//! itself (cpal). The sound card is opened at the first album, not at start
//! up, so a computer without one fails only when an album is pressed; a
//! broken stream fails the next command or poll, and the next album opens
//! the output again. A file that cannot be decoded (Opus, a broken file) is
//! skipped with a warning. The place in a track counts the samples the sound
//! card took, not the ones decoded, which run ahead. `docs/local-audio.md`
//! has the details.

mod decode;
mod engine;
mod output;

use std::time::Duration;

use anyhow::Result;
use tracing::{debug, info, warn};

use self::decode::Source;
use self::output::OpenOutput;
pub use self::output::output_devices;
use super::progress::Place;
use super::{Emitter, PlayerCmd, PlayerEvent, Speaker, Start, TrackInfo};
use crate::library::ItemId;

/// How often the player looks for the end of a track, which is also the
/// longest gap between two tracks.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// "Previous" restarts the current track once it is this far in.
const RESTART_AFTER: Duration = Duration::from_secs(5);

/// Opens the sound output; tests put in one without a sound card.
type Open = Box<dyn FnMut() -> Result<OpenOutput>>;

pub(super) struct LocalPlayer {
    open: Open,
    /// `None` until the first album, and after a failure.
    output: Option<OpenOutput>,
    /// Id of the item we started last.
    item: ItemId,
    /// Tracks of the album we started last.
    tracks: Vec<TrackInfo>,
    volume: f32,
    /// The track playing or paused; `None` while no album is active.
    current: Option<usize>,
    /// The length of that track, when its file tells it.
    duration: Option<Duration>,
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
            item: ItemId(0),
            tracks: Vec::new(),
            volume: 1.0,
            current: None,
            duration: None,
            paused: false,
        }
    }
}

impl Speaker for LocalPlayer {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        match cmd {
            PlayerCmd::Play {
                item,
                content,
                volume,
            } => {
                let list = content.into_tracks()?;
                self.report_place(events);
                events.begin(item, list.progress);
                self.item = item;
                self.tracks = list.tracks;
                self.volume = super::clamp_volume(volume);
                self.current = None;
                // A new album is the moment to try a broken output again.
                if let Some(Err(err)) = self.output.as_ref().map(|o| o.engine.check()) {
                    debug!("reopening the sound output: {err:#}");
                    self.output = None;
                }
                self.play_from(list.start, events)
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
                        self.play_from(from_top(track + 1), events)
                    }
                    _ => Ok(()),
                }
            }
            PlayerCmd::Prev => {
                self.check()?;
                let Some(track) = self.current else {
                    return Ok(());
                };
                let into = self
                    .output
                    .as_ref()
                    .map_or(Duration::ZERO, |o| o.engine.position());
                if into >= RESTART_AFTER {
                    self.play_from(from_top(track), events)
                } else {
                    self.play_from(from_top(track.saturating_sub(1)), events)
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
            if !self.start_track(from_top(track + 1), events)? {
                events.finished();
                self.end_album(events);
            }
        } else {
            self.report_place(events);
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
                if self.paused {
                    self.report_place(events);
                    events.emit(PlayerEvent::Paused(self.item));
                } else {
                    events.emit(PlayerEvent::Playing(self.item));
                }
                Ok(())
            }
            // The album ended (or failed): start it again, from the top or
            // where a resuming item got to.
            _ if !self.tracks.is_empty() => {
                let start = events.resume_point();
                self.play_from(start, events)
            }
            _ => {
                events.emit(PlayerEvent::Stopped);
                Ok(())
            }
        }
    }

    /// Plays the first track from `start` on that can be decoded; with none
    /// left, the album is over.
    fn play_from(&mut self, start: Start, events: &mut Emitter) -> Result<()> {
        if !self.start_track(start, events)? {
            self.end_album(events);
        }
        Ok(())
    }

    /// Starts the first track from `start.track` on that can be decoded, at
    /// about `start.position` if it is that track; false when there is none.
    fn start_track(&mut self, start: Start, events: &mut Emitter) -> Result<bool> {
        let output = match &mut self.output {
            Some(output) => output,
            empty @ None => empty.insert((self.open)()?),
        };
        output.engine.set_volume(self.volume);
        for (index, track) in self.tracks.iter().enumerate().skip(start.track) {
            let position = if index == start.track {
                start.position
            } else {
                Duration::ZERO
            };
            match Source::open_at(&track.path, position) {
                Ok(source) => {
                    let duration = source.duration();
                    let at = Place {
                        track: index,
                        position: source.start(),
                        duration,
                    };
                    output.engine.play(source)?;
                    info!(album = %track.album, track = %track.title, "playing");
                    self.current = Some(index);
                    self.duration = duration;
                    self.paused = false;
                    events.place(at, true);
                    events.emit(PlayerEvent::Playing(self.item));
                    return Ok(true);
                }
                Err(err) => warn!("skipping {}: {err:#}", track.path.display()),
            }
        }
        Ok(false)
    }

    fn end_album(&mut self, events: &mut Emitter) {
        if let Some(output) = &mut self.output {
            output.engine.stop();
        }
        info!("end of the album");
        self.current = None;
        self.paused = false;
        events.emit(PlayerEvent::Stopped);
    }

    /// Offers the place in the current track, as far as the sound card got.
    fn report_place(&self, events: &mut Emitter) {
        if let (Some(track), Some(output)) = (self.current, &self.output) {
            let at = Place {
                track,
                position: output.engine.position(),
                duration: self.duration,
            };
            events.place(at, false);
        }
    }
}

/// The start of `track`.
fn from_top(track: usize) -> Start {
    Start {
        track,
        position: Duration::ZERO,
    }
}

#[cfg(test)]
mod tests;
