//! Talks to the Chromecast speaker on its own thread.
//!
//! Every command opens a short-lived connection, does its job and disconnects.
//! The speaker keeps playing on its own, so there is no long-lived connection
//! (and no heartbeat) to keep alive. While an album is active, the status is
//! polled every few seconds so the deck can show play/pause correctly and
//! notice when the album has finished.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use rust_cast::CastDevice;
use rust_cast::channels::media::{
    Image, LoadOptions, Media, MediaQueue, Metadata, MusicTrackMediaMetadata, PlayerState,
    QueueItem, QueueType, StatusEntry, StreamType,
};
use rust_cast::channels::receiver::{Application, CastDeviceApp};
use tracing::{debug, info, warn};

/// App id of Chromecast's built-in Default Media Receiver.
const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
/// Destination id of the platform receiver, for status, volume and app launch.
const RECEIVER: &str = "receiver-0";
const POLL_INTERVAL: Duration = Duration::from_secs(4);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// "Previous" restarts the current track if it has played longer than this.
const RESTART_THRESHOLD_SECS: f32 = 5.0;

#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// Where the speaker downloads the track from.
    pub url: String,
    pub content_type: String,
    pub title: String,
    pub album: String,
    pub cover_url: Option<String>,
}

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

#[derive(Debug)]
pub enum PlayerCmd {
    /// `album` is an id chosen by the caller; it is echoed back in events.
    PlayAlbum {
        album: usize,
        tracks: Vec<TrackInfo>,
        volume: f32,
    },
    /// Pauses if playing, resumes if paused, restarts the album if nothing is loaded.
    TogglePause,
    /// Does nothing on the last track.
    Next,
    /// Restarts the current track once past `RESTART_THRESHOLD_SECS`, otherwise goes to the
    /// previous track.
    Prev,
    /// 0.0 to 1.0.
    SetVolume(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The album with this id is playing.
    Playing(usize),
    Paused(usize),
    /// Nothing playing any more (album finished, stopped elsewhere, or failed).
    Stopped,
}

pub fn spawn(host: String, port: u16, events: Sender<PlayerEvent>) -> Sender<PlayerCmd> {
    let (tx, rx) = mpsc::channel();
    let player = CastPlayer {
        host,
        port,
        events,
        album: 0,
        tracks: Vec::new(),
        active: false,
        last: None,
    };
    thread::Builder::new()
        .name("cast".into())
        .spawn(move || player.run(&rx))
        .expect("failed to start cast thread");
    tx
}

struct CastPlayer {
    host: String,
    port: u16,
    events: Sender<PlayerEvent>,
    /// Id of the album we started last.
    album: usize,
    /// Tracks of the album we started last.
    tracks: Vec<TrackInfo>,
    /// True while we believe our album is loaded on the speaker.
    active: bool,
    /// Last event emitted, so `emit` can skip duplicates.
    last: Option<PlayerEvent>,
}

/// An open connection plus the running media app, if any.
struct Session {
    device: CastDevice<'static>,
    app: Option<Application>,
}

impl CastPlayer {
    fn run(mut self, rx: &Receiver<PlayerCmd>) {
        loop {
            let timeout = if self.active {
                POLL_INTERVAL
            } else {
                Duration::from_secs(3600)
            };
            match rx.recv_timeout(timeout) {
                Ok(first) => {
                    let pending: Vec<PlayerCmd> =
                        std::iter::once(first).chain(rx.try_iter()).collect();
                    // The UI already guessed the outcome of these commands, so
                    // the next event must reach it even if it repeats the last.
                    self.last = None;
                    for cmd in coalesce(pending) {
                        debug!(?cmd, "cast command");
                        if let Err(err) = self.handle(cmd) {
                            warn!("speaker command failed: {err:#}");
                            self.emit(PlayerEvent::Stopped);
                            self.active = false;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Err(err) = self.poll() {
                        debug!("status poll failed: {err:#}");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn handle(&mut self, cmd: PlayerCmd) -> Result<()> {
        match cmd {
            PlayerCmd::PlayAlbum {
                album,
                tracks,
                volume,
            } => {
                self.album = album;
                self.tracks = tracks;
                let s = self.open()?;
                s.device.receiver.set_volume(volume)?;
                self.load(s, 0)
            }
            PlayerCmd::SetVolume(volume) => {
                let s = self.open()?;
                s.device.receiver.set_volume(volume)?;
                Ok(())
            }
            PlayerCmd::TogglePause => {
                let s = self.open()?;
                match media_status(&s)? {
                    Some((tid, e))
                        if matches!(
                            e.player_state,
                            PlayerState::Playing | PlayerState::Buffering
                        ) =>
                    {
                        s.device.media.pause(tid, e.media_session_id)?;
                        self.emit(PlayerEvent::Paused(self.album));
                    }
                    Some((tid, e)) if matches!(e.player_state, PlayerState::Paused) => {
                        s.device.media.play(tid, e.media_session_id)?;
                        self.emit(PlayerEvent::Playing(self.album));
                    }
                    // Finished or nothing loaded: start our album again from the top.
                    _ if !self.tracks.is_empty() => self.load(s, 0)?,
                    _ => {}
                }
                Ok(())
            }
            PlayerCmd::Next | PlayerCmd::Prev => {
                let forward = matches!(cmd, PlayerCmd::Next);
                let s = self.open()?;
                let Some((_, entry)) = media_status(&s)? else {
                    return Ok(());
                };
                let Some(current) = self.current_index(&entry) else {
                    return Ok(());
                };
                let target = if forward {
                    if current + 1 >= self.tracks.len() {
                        return Ok(()); // already on the last track
                    }
                    current + 1
                } else if entry.current_time.unwrap_or(0.0) > RESTART_THRESHOLD_SECS {
                    current
                } else {
                    current.saturating_sub(1)
                };
                self.load(s, target)
            }
        }
    }

    /// Loads our album as a queue on the speaker, starting at `start`.
    fn load(&mut self, s: Session, start: usize) -> Result<()> {
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
        self.emit(PlayerEvent::Playing(self.album));
        Ok(())
    }

    fn poll(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let s = self.open()?;
        let entry = media_status(&s)?.map(|(_, e)| e);
        let ours = entry.as_ref().and_then(|e| self.current_index(e)).is_some();
        let event = match entry.map(|e| e.player_state) {
            Some(PlayerState::Playing | PlayerState::Buffering) if ours => {
                PlayerEvent::Playing(self.album)
            }
            Some(PlayerState::Paused) if ours => PlayerEvent::Paused(self.album),
            _ => PlayerEvent::Stopped,
        };
        if event == PlayerEvent::Stopped {
            self.active = false;
        }
        self.emit(event);
        Ok(())
    }

    fn open(&self) -> Result<Session> {
        // Fail fast if the speaker is unreachable (rust_cast has no connect timeout).
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("cannot resolve {}", self.host))?;
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .with_context(|| format!("speaker {addr} not reachable"))?;

        let device = CastDevice::connect_without_host_verification(self.host.clone(), self.port)?;
        device.connection.connect(RECEIVER)?;
        let status = device.receiver.get_status()?;
        let app = status
            .applications
            .into_iter()
            .find(|a| a.app_id == DEFAULT_MEDIA_RECEIVER);
        Ok(Session { device, app })
    }

    /// Position of the currently playing track in our tracklist, or `None` if it isn't ours.
    fn current_index(&self, entry: &StatusEntry) -> Option<usize> {
        let id = &entry.media.as_ref()?.content_id;
        self.tracks.iter().position(|t| &t.url == id)
    }

    fn emit(&mut self, event: PlayerEvent) {
        if self.last != Some(event) {
            self.last = Some(event);
            let _ = self.events.send(event);
        }
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

/// Drops commands made pointless by later ones (e.g. a kid mashing buttons
/// while the speaker was slow to answer).
fn coalesce(cmds: Vec<PlayerCmd>) -> Vec<PlayerCmd> {
    let last_play = cmds
        .iter()
        .rposition(|c| matches!(c, PlayerCmd::PlayAlbum { .. }));
    let last_volume = cmds
        .iter()
        .rposition(|c| matches!(c, PlayerCmd::SetVolume(_)));
    cmds.into_iter()
        .enumerate()
        .filter(|(i, c)| {
            let after_play = last_play.is_none_or(|p| *i >= p);
            let volume_ok = !matches!(c, PlayerCmd::SetVolume(_)) || Some(*i) == last_volume;
            // Volume changes still matter even if they came before the last album press.
            (after_play || matches!(c, PlayerCmd::SetVolume(_))) && volume_ok
        })
        .map(|(_, c)| c)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failed_command_reports_stopped() {
        // Nothing listens on this port, so every command fails at once.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (tx, events) = mpsc::channel();
        let player = spawn("127.0.0.1".into(), port, tx);

        for _ in 0..2 {
            player.send(PlayerCmd::TogglePause).unwrap();
            assert_eq!(
                events.recv_timeout(Duration::from_secs(5)),
                Ok(PlayerEvent::Stopped)
            );
        }
    }

    fn album() -> PlayerCmd {
        PlayerCmd::PlayAlbum {
            album: 0,
            tracks: vec![],
            volume: 0.2,
        }
    }

    #[test]
    fn keeps_only_last_album_and_volume() {
        let out = coalesce(vec![
            PlayerCmd::SetVolume(0.1),
            album(),
            PlayerCmd::Next,
            PlayerCmd::SetVolume(0.2),
            album(),
            PlayerCmd::Next,
        ]);
        let kinds: Vec<String> = out
            .iter()
            .map(|c| {
                format!("{c:?}")
                    .split(['(', ' '])
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(kinds, ["SetVolume", "PlayAlbum", "Next"]);
    }
}
