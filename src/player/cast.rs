//! Chromecast speakers.
//!
//! Every command opens a short-lived connection, does its job and disconnects.
//! The speaker keeps playing on its own, so there is no long-lived connection
//! (and no heartbeat) to keep alive. While an album is active, the status is
//! polled every few seconds so the deck can show play/pause correctly and
//! notice when the album has finished.

use std::time::Duration;

use anyhow::{Result, anyhow};
use rust_cast::CastDevice;
use rust_cast::channels::media::{
    Image, LoadOptions, Media, MediaQueue, Metadata, MusicTrackMediaMetadata, PlayerState,
    QueueItem, QueueType, StatusEntry, StreamType,
};
use rust_cast::channels::receiver::{Application, CastDeviceApp};
use tracing::info;

use super::{Emitter, PlayerCmd, PlayerEvent, Speaker, TrackInfo};
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
    /// Id of the album we started last.
    album: ItemId,
    /// Tracks of the album we started last.
    tracks: Vec<TrackInfo>,
    /// True while we believe our album is loaded on the speaker.
    active: bool,
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
            album: ItemId(0),
            tracks: Vec::new(),
            active: false,
        }
    }
}

impl Speaker for CastPlayer {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        match cmd {
            PlayerCmd::PlayAlbum {
                album,
                tracks,
                volume,
            } => {
                self.album = album;
                self.tracks = tracks;
                let s = self.open()?;
                s.device.receiver.set_volume(super::clamp_volume(volume))?;
                self.load(s, 0, events)
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
                    Some((_, e)) if loading(&e) => events.emit(PlayerEvent::Playing(self.album)),
                    Some((tid, e))
                        if matches!(
                            e.player_state,
                            PlayerState::Playing | PlayerState::Buffering
                        ) =>
                    {
                        s.device.media.pause(tid, e.media_session_id)?;
                        events.emit(PlayerEvent::Paused(self.album));
                    }
                    Some((tid, e)) if matches!(e.player_state, PlayerState::Paused) => {
                        s.device.media.play(tid, e.media_session_id)?;
                        events.emit(PlayerEvent::Playing(self.album));
                    }
                    // Finished or nothing loaded: start our album again from the top.
                    _ if !self.tracks.is_empty() => self.load(s, 0, events)?,
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
                    Some(target) => self.load(s, target, events),
                    None => Ok(()),
                }
            }
        }
    }

    fn poll(&mut self, events: &mut Emitter) -> Result<()> {
        let s = self.open()?;
        let entry = media_status(&s)?.map(|(_, e)| e);
        let Some(event) = poll_event(entry.as_ref(), &self.tracks, self.album) else {
            return Ok(());
        };
        if event == PlayerEvent::Stopped {
            self.active = false;
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
    /// Loads our album as a queue on the speaker, starting at `start`.
    fn load(&mut self, s: Session, start: usize, events: &mut Emitter) -> Result<()> {
        let track = self
            .tracks
            .get(start)
            .ok_or_else(|| anyhow!("no track {start}"))?;
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
            start_index: start as u16,
            queue_type: QueueType::Album,
        };
        s.device.media.load_with_queue(
            app.transport_id.clone(),
            app.session_id.clone(),
            &track.to_media(),
            Some(&queue),
            LoadOptions::default(),
        )?;
        info!(album = %track.album, track = %track.title, "playing");
        self.active = true;
        events.emit(PlayerEvent::Playing(self.album));
        Ok(())
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
    album: ItemId,
) -> Option<PlayerEvent> {
    let Some(entry) = entry else {
        return Some(PlayerEvent::Stopped);
    };
    if loading(entry) {
        return None;
    }
    let ours = track_index(entry, tracks).is_some();
    Some(match entry.player_state {
        PlayerState::Playing | PlayerState::Buffering if ours => PlayerEvent::Playing(album),
        PlayerState::Paused if ours => PlayerEvent::Paused(album),
        _ => PlayerEvent::Stopped,
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
