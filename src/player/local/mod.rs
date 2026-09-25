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
//!
//! A radio station plays like a track that never ends, from [`netread`].
//! Pause stops it, and play/pause fetches it again, so it goes on live.

mod decode;
mod engine;
mod netread;
mod output;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use self::decode::Source;
use self::netread::{Limits, NetRead};
use self::output::OpenOutput;
pub use self::output::output_devices;
use super::progress::Place;
use super::{Content, Emitter, PlayerCmd, PlayerEvent, Speaker, Start, Station, TrackInfo};
use crate::library::ItemId;
use crate::radio;

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
    /// The station we started last, instead of an album.
    station: Option<Tuned>,
    limits: Limits,
    volume: f32,
    /// The track playing or paused (0 for a station); `None` while no album
    /// or station is active.
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
            station: None,
            limits: Limits::default(),
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
                content: Content::Stream(station),
                volume,
            } => self.play_station(item, &station, volume, events),
            PlayerCmd::Play {
                item,
                content,
                volume,
            } => {
                let list = content.into_tracks()?;
                self.begin(item, list.progress, volume, events);
                self.tracks = list.tracks;
                self.play_from(list.start, events)
            }
            PlayerCmd::Next | PlayerCmd::Prev if self.station.is_some() => self.check(),
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
            PlayerCmd::Off => Speaker::stop(self, events),
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
            if self.station.is_some() {
                warn!("the station stopped sending");
                self.end_album(events);
            } else if !self.start_track(from_top(track + 1), events)? {
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

    fn stop(&mut self, events: &mut Emitter) -> Result<()> {
        self.halt(events);
        Ok(())
    }
}

impl LocalPlayer {
    /// Stops what plays and closes the sound card, which another program
    /// (a Spotify Connect client on a Pi) may need next. The caller reports
    /// what follows.
    fn halt(&mut self, events: &mut Emitter) {
        self.report_place(events);
        if self.output.is_some() {
            info!("closing the sound output");
        }
        self.reset();
    }

    /// Fails once the open output broke.
    fn check(&self) -> Result<()> {
        match &self.output {
            Some(output) => output.engine.check(),
            None => Ok(()),
        }
    }

    fn toggle_pause(&mut self, events: &mut Emitter) -> Result<()> {
        match (self.current, &self.output) {
            (Some(_), Some(_)) if self.station.is_some() => self.toggle_station(events),
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
            _ if self.station.is_some() => self.tune(events),
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

    /// `item` replaces what played; the caller sets its tracks or station.
    fn begin(&mut self, item: ItemId, progress: bool, volume: f32, events: &mut Emitter) {
        self.report_place(events);
        events.begin(item, progress);
        self.item = item;
        self.tracks = Vec::new();
        self.station = None;
        self.volume = super::clamp_volume(volume);
        self.current = None;
        // A new item is the moment to try a broken output again.
        if let Some(Err(err)) = self.output.as_ref().map(|o| o.engine.check()) {
            debug!("reopening the sound output: {err:#}");
            self.output = None;
        }
    }

    /// Resolves the station before anything else: one that cannot be played
    /// leaves the sound card closed.
    fn play_station(
        &mut self,
        item: ItemId,
        station: &Station,
        volume: f32,
        events: &mut Emitter,
    ) -> Result<()> {
        let stream = radio::resolve(&crate::net::stream_agent(), &station.url)?;
        decodable(&stream)?;
        self.begin(item, false, volume, events);
        self.station = Some(Tuned {
            name: station.name.clone(),
            url: stream.url,
            content_type: stream.content_type,
        });
        self.tune(events)
    }

    /// Connects to the station and plays it, live.
    fn tune(&mut self, events: &mut Emitter) -> Result<()> {
        let station = self.station.as_ref().context("no station to play")?;
        let stream = NetRead::open(&station.url, self.limits)?;
        let source = Source::open_stream(stream, station.content_type.as_deref())
            .context("cannot decode the station's stream")?;
        let output = match &mut self.output {
            Some(output) => output,
            empty @ None => empty.insert((self.open)()?),
        };
        output.engine.set_volume(self.volume);
        output.engine.play(source)?;
        info!(station = %station.name, "playing");
        self.current = Some(0);
        self.duration = None;
        self.paused = false;
        events.emit(PlayerEvent::Playing(self.item));
        Ok(())
    }

    /// A paused station is silent and closed, so play/pause goes on live.
    fn toggle_station(&mut self, events: &mut Emitter) -> Result<()> {
        if self.paused {
            return self.tune(events);
        }
        if let Some(output) = &mut self.output {
            output.engine.stop();
        }
        self.paused = true;
        events.emit(PlayerEvent::Paused(self.item));
        Ok(())
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
        if self.station.is_none() {
            info!("end of the album");
        }
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

/// A station, resolved to its stream.
struct Tuned {
    name: String,
    url: String,
    content_type: Option<String>,
}

/// symphonia has no HE-AAC (SBR) and no HLS.
fn decodable(stream: &radio::Stream) -> Result<()> {
    let kind = match stream.content_type.as_deref() {
        _ if stream.is_hls() => "HLS",
        Some("audio/aacp" | "audio/x-aacp") => "HE-AAC",
        _ => return Ok(()),
    };
    bail!("this station sends {kind}, which local playback cannot decode; pick its MP3 stream")
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
