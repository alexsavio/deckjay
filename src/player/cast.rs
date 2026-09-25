//! Chromecast speakers.
//!
//! Every command opens a short-lived connection, does its job and disconnects.
//! The speaker keeps playing on its own, so there is no long-lived connection
//! (and no heartbeat) to keep alive. While an album is active, the status is
//! polled every few seconds so the deck can show play/pause correctly and
//! notice when the album has finished. The same poll gives the place in the
//! track for items that report progress.

use std::time::Duration;

use anyhow::{Result, anyhow};
use rust_cast::CastDevice;
use rust_cast::channels::media::{
    IdleReason, Image, LoadOptions, Media, MediaQueue, Metadata, MusicTrackMediaMetadata,
    PlayerState, QueueItem, QueueType, ResumeState, StatusEntry, StreamType,
};
use rust_cast::channels::receiver::{Application, CastDeviceApp};
use tracing::{debug, info, warn};

use super::progress::{END_MARGIN, Place};
use super::{Emitter, PlayerCmd, PlayerEvent, Speaker, Start, TrackInfo};
use crate::library::ItemId;

/// App id of Chromecast's built-in Default Media Receiver.
const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
/// Destination id of the platform receiver, for status, volume and app launch.
const RECEIVER: &str = "receiver-0";
const POLL_INTERVAL: Duration = Duration::from_secs(4);
/// "Previous" restarts the current track if it has played longer than this.
const RESTART_THRESHOLD_SECS: f32 = 5.0;

impl TrackInfo {
    fn to_media(&self) -> Media {
        Media {
            content_id: self.url.clone(),
            stream_type: StreamType::Buffered,
            content_type: self.content_type.clone(),
            metadata: Some(Metadata::MusicTrack(MusicTrackMediaMetadata {
                album_name: Some(self.album.clone()),
                title: Some(self.title.clone()),
                album_artist: None,
                artist: None,
                composer: None,
                track_number: None,
                disc_number: None,
                images: self
                    .cover_url
                    .iter()
                    .map(|u| Image::new(u.clone()))
                    .collect(),
                release_date: None,
            })),
            duration: None,
        }
    }
}

pub(super) struct CastPlayer {
    host: String,
    port: u16,
    /// Id of the item we started last.
    item: ItemId,
    /// Tracks of the album we started last.
    tracks: Vec<TrackInfo>,
    /// True while we believe our album is loaded on the speaker.
    active: bool,
    /// The latest place of our album seen in a status.
    last_place: Option<Place>,
}

/// An open connection plus the running media app, if any.
struct Session {
    device: CastDevice<'static>,
    app: Option<Application>,
}

impl CastPlayer {
    pub(super) fn new(host: String, port: u16) -> CastPlayer {
        CastPlayer {
            host,
            port,
            item: ItemId(0),
            tracks: Vec::new(),
            active: false,
            last_place: None,
        }
    }
}

impl Speaker for CastPlayer {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        match cmd {
            PlayerCmd::Play {
                item,
                content,
                volume,
            } => {
                let list = content.into_tracks()?;
                let s = self.open()?;
                if self.active && events.wants_progress() {
                    self.report_place(&s, events);
                }
                events.begin(item, list.progress);
                self.item = item;
                self.tracks = list.tracks;
                s.device.receiver.set_volume(super::clamp_volume(volume))?;
                self.load(s, list.start, events)
            }
            PlayerCmd::SetVolume(volume) => {
                let s = self.open()?;
                s.device.receiver.set_volume(super::clamp_volume(volume))?;
                Ok(())
            }
            PlayerCmd::TogglePause => {
                let s = self.open()?;
                match media_status(&s)? {
                    // Keep the track that is about to start, as HEOS does.
                    Some((_, e)) if loading(&e) => events.emit(PlayerEvent::Playing(self.item)),
                    Some((tid, e))
                        if matches!(
                            e.player_state,
                            PlayerState::Playing | PlayerState::Buffering
                        ) =>
                    {
                        s.device.media.pause(tid, e.media_session_id)?;
                        if let Some(at) = place(&e, &self.tracks) {
                            self.note_place(at, events);
                        }
                        events.emit(PlayerEvent::Paused(self.item));
                    }
                    Some((tid, e)) if matches!(e.player_state, PlayerState::Paused) => {
                        s.device.media.play(tid, e.media_session_id)?;
                        events.emit(PlayerEvent::Playing(self.item));
                    }
                    // Finished or nothing loaded: start our album again, from
                    // the top or where a resuming item got to.
                    _ if !self.tracks.is_empty() => {
                        let start = events.resume_point();
                        self.load(s, start, events)?;
                    }
                    _ => events.emit(PlayerEvent::Stopped),
                }
                Ok(())
            }
            PlayerCmd::Next | PlayerCmd::Prev => {
                let s = self.open()?;
                let Some((_, entry)) = media_status(&s)? else {
                    return Ok(());
                };
                match skip_target(&entry, &self.tracks, matches!(cmd, PlayerCmd::Next)) {
                    Some(track) => self.load(
                        s,
                        Start {
                            track,
                            ..Start::default()
                        },
                        events,
                    ),
                    None => Ok(()),
                }
            }
        }
    }

    fn poll(&mut self, events: &mut Emitter) -> Result<()> {
        let s = self.open()?;
        let entry = media_status(&s)?.map(|(_, e)| e);
        if let Some(at) = entry.as_ref().and_then(|e| place(e, &self.tracks)) {
            self.note_place(at, events);
        }
        let Some(event) = poll_event(entry.as_ref(), &self.tracks, self.item) else {
            return Ok(());
        };
        if event == PlayerEvent::Stopped {
            self.active = false;
            let ended = entry.as_ref().is_some_and(|e| finished(e, &self.tracks))
                || near_end(self.last_place, self.tracks.len());
            if ended {
                events.finished();
            }
        }
        events.emit(event);
        Ok(())
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.active.then_some(POLL_INTERVAL)
    }

    fn reset(&mut self) {
        self.active = false;
    }
}

impl CastPlayer {
    /// Loads our album as a queue on the speaker, starting at `start.track`,
    /// `start.position` into it.
    fn load(&mut self, s: Session, start: Start, events: &mut Emitter) -> Result<()> {
        let index = start.track;
        let track = self
            .tracks
            .get(index)
            .ok_or_else(|| anyhow!("no track {index}"))?;
        let app = match s.app {
            Some(app) => app,
            None => s
                .device
                .receiver
                .launch_app(&CastDeviceApp::DefaultMediaReceiver)?,
        };
        s.device.connection.connect(app.transport_id.clone())?;

        let queue = MediaQueue {
            items: self
                .tracks
                .iter()
                .map(|t| QueueItem {
                    media: t.to_media(),
                })
                .collect(),
            start_index: index as u16,
            queue_type: QueueType::Album,
        };
        let options = LoadOptions {
            current_time: start.position.as_secs_f64(),
            ..LoadOptions::default()
        };
        let status = s.device.media.load_with_queue(
            app.transport_id.clone(),
            app.session_id.clone(),
            &track.to_media(),
            Some(&queue),
            options,
        )?;
        if !start.position.is_zero() {
            seek_after_load(&s.device, &app, status.entries.first(), start.position);
        }
        info!(album = %track.album, track = %track.title, "playing");
        self.active = true;
        self.last_place = None;
        events.place(Place::start_of(index, start.position), true);
        events.emit(PlayerEvent::Playing(self.item));
        Ok(())
    }

    /// Reports where the item playing got to; a failure only loses the report.
    fn report_place(&mut self, s: &Session, events: &mut Emitter) {
        match media_status(s) {
            Ok(Some((_, entry))) => {
                if let Some(at) = place(&entry, &self.tracks) {
                    self.note_place(at, events);
                }
            }
            Ok(None) => {}
            Err(err) => debug!("no place for the item playing: {err:#}"),
        }
    }

    fn note_place(&mut self, at: Place, events: &mut Emitter) {
        self.last_place = Some(at);
        events.place(at, false);
    }

    fn open(&self) -> Result<Session> {
        // Fail fast if the speaker is unreachable (rust_cast has no connect timeout).
        super::connect(&self.host, self.port)?;

        let device = CastDevice::connect_without_host_verification(self.host.clone(), self.port)?;
        device.connection.connect(RECEIVER)?;
        let status = device.receiver.get_status()?;
        let app = status
            .applications
            .into_iter()
            .find(|a| a.app_id == DEFAULT_MEDIA_RECEIVER);
        Ok(Session { device, app })
    }
}

/// `rust_cast` sends every queue item with `startTime` 0, which a receiver may
/// follow instead of the LOAD's `currentTime`, so a SEEK follows the LOAD. A
/// failure only costs the place: the track plays from its beginning.
fn seek_after_load(
    device: &CastDevice<'static>,
    app: &Application,
    loaded: Option<&StatusEntry>,
    to: Duration,
) {
    let Some(loaded) = loaded else {
        debug!("no media session to seek in");
        return;
    };
    let sought = device.media.seek(
        app.transport_id.clone(),
        loaded.media_session_id,
        Some(to.as_secs_f32()),
        Some(ResumeState::PlaybackStart),
    );
    if let Err(err) = sought {
        warn!("cannot seek to {to:?}: {err:#}");
    }
}

/// Idle, but loading the next queue item: the receiver reports this between
/// tracks, and the media it names may be missing or the previous one.
fn loading(entry: &StatusEntry) -> bool {
    matches!(entry.player_state, PlayerState::Idle)
        && (entry.loading_item_id.is_some() || entry.extended_status.is_some())
}

/// Position of the entry's track in `tracks`, or `None` if it isn't ours.
fn track_index(entry: &StatusEntry, tracks: &[TrackInfo]) -> Option<usize> {
    let id = &entry.media.as_ref()?.content_id;
    tracks.iter().position(|t| &t.url == id)
}

/// What a status poll reports; `None` keeps the last event.
fn poll_event(
    entry: Option<&StatusEntry>,
    tracks: &[TrackInfo],
    item: ItemId,
) -> Option<PlayerEvent> {
    let Some(entry) = entry else {
        return Some(PlayerEvent::Stopped);
    };
    if loading(entry) {
        return None;
    }
    let ours = track_index(entry, tracks).is_some();
    Some(match entry.player_state {
        PlayerState::Playing | PlayerState::Buffering if ours => PlayerEvent::Playing(item),
        PlayerState::Paused if ours => PlayerEvent::Paused(item),
        _ => PlayerEvent::Stopped,
    })
}

/// Where our album is: the track and the time into it, while it plays or is
/// paused. `None` while the next track loads, when stopped, or not ours.
fn place(entry: &StatusEntry, tracks: &[TrackInfo]) -> Option<Place> {
    let playing_or_paused = matches!(
        entry.player_state,
        PlayerState::Playing | PlayerState::Buffering | PlayerState::Paused
    );
    if !playing_or_paused {
        return None;
    }
    let track = track_index(entry, tracks)?;
    // From the network: negative, NaN or huge values mean "unknown".
    let seconds = |s: f32| Duration::try_from_secs_f32(s).ok();
    Some(Place {
        track,
        position: seconds(entry.current_time?)?,
        duration: entry
            .media
            .as_ref()
            .and_then(|m| m.duration)
            .and_then(seconds)
            .filter(|d| !d.is_zero()),
    })
}

/// The receiver played our last track to its end. Without `media` in the
/// status the track is unknown, and this is false.
fn finished(entry: &StatusEntry, tracks: &[TrackInfo]) -> bool {
    matches!(entry.player_state, PlayerState::Idle)
        && matches!(entry.idle_reason, Some(IdleReason::Finished))
        && !loading(entry)
        && track_index(entry, tracks).is_some_and(|t| t + 1 == tracks.len())
}

/// Whether `last`, the latest place seen, was on the last track within
/// [`END_MARGIN`] of its known length. At the end of the queue the receiver
/// may end the media session, and its status then tells nothing about how.
fn near_end(last: Option<Place>, tracks: usize) -> bool {
    last.is_some_and(|at| {
        at.track + 1 == tracks && at.duration.is_some_and(|d| at.position + END_MARGIN >= d)
    })
}

/// The track that Next (`forward`) or Prev loads; `None` when the media is
/// not ours, or on Next from the last track.
fn skip_target(entry: &StatusEntry, tracks: &[TrackInfo], forward: bool) -> Option<usize> {
    let current = track_index(entry, tracks)?;
    if forward {
        (current + 1 < tracks.len()).then_some(current + 1)
    } else if entry.current_time.unwrap_or(0.0) > RESTART_THRESHOLD_SECS {
        Some(current)
    } else {
        Some(current.saturating_sub(1))
    }
}

/// Returns the transport id and first media status entry of the running media app.
fn media_status(s: &Session) -> Result<Option<(String, StatusEntry)>> {
    let Some(app) = &s.app else { return Ok(None) };
    s.device.connection.connect(app.transport_id.clone())?;
    let status = s.device.media.get_status(app.transport_id.clone(), None)?;
    Ok(status
        .entries
        .into_iter()
        .next()
        .map(|e| (app.transport_id.clone(), e)))
}

#[cfg(test)]
mod tests;
