//! Chromecast speakers.
//!
//! Every command opens a short-lived connection, does its job and disconnects.
//! The speaker keeps playing on its own, so there is no long-lived connection
//! (and no heartbeat) to keep alive. While an album is active, the status is
//! polled every few seconds so the deck can show play/pause correctly and
//! notice when the album has finished. The same poll gives the place in the
//! track for items that report progress.
//!
//! A radio station loads as one live item, without a queue. Pausing it
//! stops it instead when the receiver refuses, and play/pause loads it
//! again, so it always goes on live.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use rust_cast::CastDevice;
use rust_cast::channels::media::{
    GenericMediaMetadata, IdleReason, Image, LoadOptions, Media, MediaQueue, Metadata,
    MusicTrackMediaMetadata, PlayerState, QueueItem, QueueType, ResumeState, StatusEntry,
    StreamType,
};
use rust_cast::channels::receiver::{Application, CastDeviceApp};
use tracing::{debug, info, warn};

use super::progress::{END_MARGIN, Place};
use super::{Content, Emitter, PlayerCmd, PlayerEvent, Speaker, Start, Station, TrackInfo};
use crate::library::ItemId;
use crate::radio;

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

/// A radio station, as the receiver gets it.
struct Live {
    /// The stream itself: the "ours" check compares `content_id` with it.
    url: String,
    content_type: String,
    name: String,
    cover_url: Option<String>,
}

impl Live {
    /// The content type is the stream's, else the station's, else MP3.
    fn new(station: &Station, stream: radio::Stream) -> Live {
        Live {
            url: stream.url,
            content_type: stream
                .content_type
                .or_else(|| station.content_type.clone())
                .unwrap_or_else(|| "audio/mpeg".into()),
            name: station.name.clone(),
            cover_url: station.cover_url.clone(),
        }
    }

    fn to_media(&self) -> Media {
        Media {
            content_id: self.url.clone(),
            stream_type: StreamType::Live,
            content_type: self.content_type.clone(),
            metadata: Some(Metadata::Generic(GenericMediaMetadata {
                title: Some(self.name.clone()),
                images: self
                    .cover_url
                    .iter()
                    .map(|u| Image::new(u.clone()))
                    .collect(),
                ..GenericMediaMetadata::default()
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
    /// The station we started last, instead of an album.
    live: Option<Live>,
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
            live: None,
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
                content: Content::Stream(station),
                volume,
            } => self.play_station(item, &station, volume, events),
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
                self.live = None;
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
                let live = self.live.is_some();
                match media_status(&s)? {
                    // Keep the track that is about to start, as HEOS does.
                    Some((_, e)) if loading(&e) => events.emit(PlayerEvent::Playing(self.item)),
                    Some((tid, e))
                        if live
                            && matches!(
                                e.player_state,
                                PlayerState::Playing | PlayerState::Buffering
                            ) =>
                    {
                        self.pause_live(&s, &tid, &e)?;
                        events.emit(PlayerEvent::Paused(self.item));
                    }
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
                    // A station goes on live, below.
                    Some((tid, e)) if !live && matches!(e.player_state, PlayerState::Paused) => {
                        s.device.media.play(tid, e.media_session_id)?;
                        events.emit(PlayerEvent::Playing(self.item));
                    }
                    _ if live => self.load_live(s, events)?,
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
            PlayerCmd::Off => Speaker::stop(self, events),
            PlayerCmd::Next | PlayerCmd::Prev if self.live.is_some() => Ok(()),
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
        let live = self.live.as_ref().map(|l| l.url.as_str());
        let Some(event) = poll_event(entry.as_ref(), &self.tracks, live, self.item) else {
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

    fn stop(&mut self, events: &mut Emitter) -> Result<()> {
        self.halt(events);
        Ok(())
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
        let app = media_app(&s.device, s.app)?;

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

    fn play_station(
        &mut self,
        item: ItemId,
        station: &Station,
        volume: f32,
        events: &mut Emitter,
    ) -> Result<()> {
        let stream = radio::resolve(&crate::net::stream_agent(), &station.url)?;
        let s = self.open()?;
        if self.active && events.wants_progress() {
            self.report_place(&s, events);
        }
        events.begin(item, false);
        self.item = item;
        self.tracks = Vec::new();
        self.live = Some(Live::new(station, stream));
        s.device.receiver.set_volume(super::clamp_volume(volume))?;
        self.load_live(s, events)
    }

    /// Loads our station, live.
    fn load_live(&mut self, s: Session, events: &mut Emitter) -> Result<()> {
        let live = self.live.as_ref().context("no station to play")?;
        let app = media_app(&s.device, s.app)?;
        s.device.media.load_with_opts(
            app.transport_id,
            app.session_id,
            &live.to_media(),
            LoadOptions::default(),
        )?;
        info!(station = %live.name, "playing");
        self.active = true;
        self.last_place = None;
        events.emit(PlayerEvent::Playing(self.item));
        Ok(())
    }

    /// Pauses the station, or stops it when the receiver will not pause a
    /// live stream; polling stops with it, as nothing of ours plays then.
    fn pause_live(&mut self, s: &Session, tid: &str, entry: &StatusEntry) -> Result<()> {
        let id = entry.media_session_id;
        if let Err(err) = s.device.media.pause(tid.to_string(), id) {
            debug!("the receiver cannot pause the station, stopping it: {err:#}");
            s.device.media.stop(tid.to_string(), id)?;
            self.active = false;
        }
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

    /// Ends our item's media session, if the receiver still plays it, and
    /// forgets the item; a failure is only logged. The caller reports what
    /// follows.
    fn halt(&mut self, events: &mut Emitter) {
        if !self.active {
            return;
        }
        self.active = false;
        if let Err(err) = self.end_session(events) {
            warn!("cannot stop the speaker: {err:#}");
        }
    }

    fn end_session(&mut self, events: &mut Emitter) -> Result<()> {
        let s = self.open()?;
        let Some((tid, entry)) = media_status(&s)? else {
            return Ok(());
        };
        if let Some(at) = place(&entry, &self.tracks) {
            self.note_place(at, events);
        }
        let live = self.live.as_ref().map(|l| l.url.as_str());
        if ours(&entry, &self.tracks, live) {
            s.device.media.stop(tid, entry.media_session_id)?;
        }
        Ok(())
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

/// The Default Media Receiver, launched when `app` is not running yet, and
/// connected.
fn media_app(device: &CastDevice<'static>, app: Option<Application>) -> Result<Application> {
    let app = match app {
        Some(app) => app,
        None => device
            .receiver
            .launch_app(&CastDeviceApp::DefaultMediaReceiver)?,
    };
    device.connection.connect(app.transport_id.clone())?;
    Ok(app)
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

/// Whether the entry's media is one of `tracks` or the `live` stream.
fn ours(entry: &StatusEntry, tracks: &[TrackInfo], live: Option<&str>) -> bool {
    let on_air = |url| entry.media.as_ref().is_some_and(|m| m.content_id == url);
    track_index(entry, tracks).is_some() || live.is_some_and(on_air)
}

/// What a status poll reports; `None` keeps the last event.
fn poll_event(
    entry: Option<&StatusEntry>,
    tracks: &[TrackInfo],
    live: Option<&str>,
    item: ItemId,
) -> Option<PlayerEvent> {
    let Some(entry) = entry else {
        return Some(PlayerEvent::Stopped);
    };
    if loading(entry) {
        return None;
    }
    let ours = ours(entry, tracks, live);
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
