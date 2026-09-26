//! Plays albums on the speaker, on a thread of its own. Three kinds of
//! speaker are supported: Chromecast ([`cast`]), Denon HEOS ([`heos`]) and
//! this computer's sound output ([`local`]). All take [`PlayerCmd`]s and
//! report [`PlayerEvent`]s, so the UI does not know which one it drives.
//! [`progress`] decides when an item reports how far it got.

mod cast;
pub mod heos;
pub mod local;
mod progress;
mod router;
pub mod spotify;

use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, warn};

use self::progress::Place;
use crate::library::ItemId;

#[derive(Debug)]
pub struct TrackInfo {
    /// Where the speaker downloads the track from.
    pub url: String,
    /// The file itself, for local playback.
    pub path: PathBuf,
    pub content_type: String,
    pub title: String,
    pub album: String,
    pub cover_url: Option<String>,
}

/// Where playback begins: a track, and a position inside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Start {
    pub track: usize,
    pub position: Duration,
}

/// An internet radio station.
#[derive(Clone, Debug)]
pub struct Station {
    pub url: String,
    /// The MIME type of the stream, when known.
    pub content_type: Option<String>,
    pub name: String,
    pub cover_url: Option<String>,
}

/// A Spotify playlist, played through Spotify Connect.
#[derive(Clone, Debug)]
pub struct Playlist {
    /// `spotify:playlist:<id>`
    pub uri: String,
    pub name: String,
}

/// What [`PlayerCmd::Play`] plays.
#[derive(Debug)]
pub enum Content {
    /// Files the speaker plays one after the other.
    Tracks {
        tracks: Vec<TrackInfo>,
        /// A track out of range starts the first track from its beginning.
        start: Start,
        /// Whether to send [`PlayerEvent::Progress`] and
        /// [`PlayerEvent::Finished`], for items that resume.
        progress: bool,
    },
    Stream(Station),
    Spotify(Playlist),
}

/// [`Content::Tracks`] taken apart; `start.track` is always one of `tracks`,
/// unless there are none.
#[derive(Debug)]
struct TrackList {
    tracks: Vec<TrackInfo>,
    start: Start,
    progress: bool,
}

impl Content {
    /// The tracks to play; a stream or a playlist is an error.
    fn into_tracks(self) -> Result<TrackList> {
        match self {
            Content::Tracks {
                tracks,
                start,
                progress,
            } => {
                let start = if start.track < tracks.len() {
                    start
                } else {
                    Start::default()
                };
                Ok(TrackList {
                    tracks,
                    start,
                    progress,
                })
            }
            Content::Stream(station) => {
                bail!("internet radio ({}) is not supported yet", station.name)
            }
            Content::Spotify(playlist) => {
                bail!("Spotify ({}) is not supported yet", playlist.name)
            }
        }
    }
}

#[derive(Debug)]
pub enum PlayerCmd {
    /// `item` is an id chosen by the caller; it is echoed back in events.
    Play {
        item: ItemId,
        content: Content,
        /// The current volume: it replaces any `SetVolume` sent before.
        volume: f32,
    },
    /// Pauses if playing, resumes if paused, restarts the album if nothing is loaded.
    TogglePause,
    /// Does nothing on the last track.
    Next,
    /// Goes to the previous track. On Chromecast it restarts the current track
    /// instead once that has played for a few seconds.
    Prev,
    /// Moves the place in the current track by this many seconds, back when
    /// negative: 0 at the most, the next track's start past the end. Only
    /// for tracks; HEOS and Spotify ignore it.
    Seek(i32),
    /// 0.0 to 1.0.
    SetVolume(f32),
    /// The power key: stops what plays (an audiobook reports its place
    /// first) and puts the speaker in standby where its protocol allows.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The item with this id is playing.
    Playing(ItemId),
    Paused(ItemId),
    /// Nothing playing any more (album finished, stopped elsewhere, or failed).
    Stopped,
    /// How far an item that asked for progress got: `track` indexes its
    /// tracks, `duration` is that track's length when known.
    Progress {
        item: ItemId,
        track: usize,
        position: Duration,
        duration: Option<Duration>,
    },
    /// An item that asked for progress played its last track to the end;
    /// `Stopped` follows.
    Finished(ItemId),
}

/// A speaker whose status polls keep failing for this long is taken to be
/// gone. A shorter network drop only pauses the reports; the poll interval
/// differs per speaker (1 s on HEOS, 4 s on Cast), so a count would not.
const POLL_FAILURE_GRACE: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Where the album plays, and what the player needs to reach it.
#[derive(Debug, Clone)]
pub enum Output {
    Cast {
        host: String,
        port: u16,
    },
    Heos {
        host: String,
        port: u16,
    },
    /// This computer's sound output; `device` is part of its name.
    Local {
        device: Option<String>,
    },
}

#[cfg(test)]
pub fn spawn(output: Output, events: Sender<PlayerEvent>) -> Sender<PlayerCmd> {
    spawn_with(output, None, events)
}

/// `spotify` plays Spotify playlists; without it they fail.
pub fn spawn_with(
    output: Output,
    spotify: Option<spotify::Connect>,
    events: Sender<PlayerEvent>,
) -> Sender<PlayerCmd> {
    let (tx, rx) = mpsc::channel();
    let name = match &output {
        Output::Cast { .. } => "cast",
        Output::Heos { .. } => "heos",
        Output::Local { .. } => "local",
    };
    let emitter = Emitter::new(events);
    thread::Builder::new()
        .name(name.into())
        // Built on the player thread: a sound card stream cannot move between
        // threads on every platform.
        .spawn(move || {
            let speaker: Box<dyn Speaker> = match output {
                Output::Cast { host, port } => Box::new(cast::CastPlayer::new(host, port)),
                Output::Heos { host, port } => Box::new(heos::HeosPlayer::new(host, port)),
                Output::Local { device } => Box::new(local::LocalPlayer::new(device)),
            };
            let spotify = spotify.map(|connect| -> Box<dyn Speaker> {
                Box::new(spotify::SpotifyPlayer::new(
                    connect,
                    crate::spotify::api::Endpoints::default(),
                ))
            });
            run(
                Box::new(router::Router::new(speaker, spotify)),
                &rx,
                emitter,
            );
        })
        .expect("failed to start the player thread");
    tx
}

/// One speaker protocol. The methods run on the player thread.
trait Speaker {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()>;
    /// Checks the speaker while an album is active and reports what changed.
    fn poll(&mut self, events: &mut Emitter) -> Result<()>;
    /// `None` while no album is active, so there is nothing to poll.
    fn poll_interval(&self) -> Option<Duration>;
    /// How long polls may keep failing before the album is given up. A
    /// speaker whose poll failures are final (a sound card that went away)
    /// gives up sooner.
    fn poll_failure_grace(&self) -> Duration {
        POLL_FAILURE_GRACE
    }
    /// Forgets the album after a failed command.
    fn reset(&mut self);
    /// Stops what plays and forgets it, before another speaker takes over.
    fn stop(&mut self, _events: &mut Emitter) -> Result<()> {
        self.reset();
        Ok(())
    }
    /// Puts the device in standby, where its protocol has a command for it.
    fn standby(&mut self) -> Result<()> {
        Ok(())
    }
    /// The power key: stops whatever the device plays, also what another app
    /// or an earlier run of kids-deck started.
    fn stop_everything(&mut self, events: &mut Emitter) -> Result<()> {
        self.stop(events)
    }
}

fn run(mut speaker: Box<dyn Speaker>, rx: &Receiver<PlayerCmd>, mut events: Emitter) {
    let mut failing_since: Option<Instant> = None;
    loop {
        let next = match speaker.poll_interval() {
            Some(interval) => rx.recv_timeout(interval),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(first) => {
                let pending: Vec<PlayerCmd> = std::iter::once(first).chain(rx.try_iter()).collect();
                // The UI already guessed the outcome of these commands, so
                // the next event must reach it even if it repeats the last.
                events.last = None;
                for cmd in coalesce(pending) {
                    debug!(?cmd, "speaker command");
                    let guessed = guessed(&cmd);
                    let done = match cmd {
                        PlayerCmd::Off => power_off(speaker.as_mut(), &mut events),
                        cmd => speaker.handle(cmd, &mut events),
                    };
                    match done {
                        Ok(()) => failing_since = None,
                        Err(err) if guessed => {
                            warn!("speaker command failed: {err:#}");
                            speaker.reset();
                            events.emit(PlayerEvent::Stopped);
                            // The rest were guesses from a state the UI no
                            // longer shows, and each would wait out its own
                            // timeout.
                            break;
                        }
                        // The UI shows nothing for it, and the album may well
                        // play on; the polls find out if the speaker is gone.
                        Err(err) => warn!("speaker command failed: {err:#}"),
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => match speaker.poll(&mut events) {
                Ok(()) => failing_since = None,
                Err(err) => {
                    debug!("status poll failed: {err:#}");
                    let since = *failing_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= speaker.poll_failure_grace() {
                        warn!("the speaker stopped answering: {err:#}");
                        speaker.reset();
                        events.emit(PlayerEvent::Stopped);
                        failing_since = None;
                    }
                }
            },
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Whether the UI shows the command's outcome before the speaker answers
/// (`Ui::press` sets the current item and play state at once). When such a
/// command fails, the guess must be undone; for the others the album goes on.
fn guessed(cmd: &PlayerCmd) -> bool {
    matches!(
        cmd,
        PlayerCmd::Play { .. } | PlayerCmd::TogglePause | PlayerCmd::Off
    )
}

/// Standby is tried even when the stop failed: a receiver's control port
/// can answer while its HEOS part does not. A failed standby is only logged.
fn power_off(speaker: &mut dyn Speaker, events: &mut Emitter) -> Result<()> {
    let stopped = speaker.stop_everything(events);
    if let Err(err) = speaker.standby() {
        warn!("cannot put the speaker in standby: {err:#}");
    }
    events.emit(PlayerEvent::Stopped);
    stopped
}

/// Both speaker protocols take 0.0 to 1.0.
fn clamp_volume(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0)
}

/// Like `TcpStream::connect`, with a timeout for each address `host` resolves to.
fn connect(host: &str, port: u16) -> Result<TcpStream> {
    let mut last_err = anyhow!("cannot resolve {host}");
    for addr in (host, port)
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {host}"))?
    {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_err = anyhow::Error::new(err).context(format!("speaker {addr} not reachable"));
            }
        }
    }
    Err(last_err)
}

/// Sends [`PlayerEvent`]s to the UI. Playing, paused and stopped drop
/// repeats of the last one; progress reports follow [`progress::Policy`].
struct Emitter {
    tx: Sender<PlayerEvent>,
    last: Option<PlayerEvent>,
    progress: progress::Policy,
}

impl Emitter {
    fn new(tx: Sender<PlayerEvent>) -> Emitter {
        Emitter {
            tx,
            last: None,
            progress: progress::Policy::new(),
        }
    }

    /// A pause or a stop first reports the item's latest place.
    fn emit(&mut self, event: PlayerEvent) {
        if matches!(event, PlayerEvent::Paused(_) | PlayerEvent::Stopped) {
            let report = self.progress.flush(Instant::now());
            self.send(report);
        }
        if self.last != Some(event) {
            self.last = Some(event);
            let _ = self.tx.send(event);
        }
    }

    /// `item` starts; the item it replaces reports its latest place first.
    fn begin(&mut self, item: ItemId, progress: bool) {
        let report = self.progress.begin(item, progress, Instant::now());
        self.send(report);
    }

    /// The item is at `at`; a `jump` is a track that started or moved.
    fn place(&mut self, at: Place, jump: bool) {
        let report = self.progress.offer(at, jump, Instant::now());
        self.send(report);
    }

    /// The item's last track played to its end by itself.
    fn finished(&mut self) {
        let report = self.progress.finish();
        self.send(report);
    }

    /// Where a restart of the item begins: its latest place if it resumes,
    /// else the first track.
    fn resume_point(&self) -> Start {
        self.progress.resume_point().unwrap_or_default()
    }

    /// Whether the item playing asked for progress.
    fn wants_progress(&self) -> bool {
        self.progress.wanted()
    }

    fn send(&self, report: Option<PlayerEvent>) {
        if let Some(event) = report {
            let _ = self.tx.send(event);
        }
    }
}

/// Drops commands made pointless by later ones (e.g. a kid mashing buttons
/// while the speaker was slow to answer), and adds up seeks in a row; a sum
/// of 0 goes.
fn coalesce(cmds: Vec<PlayerCmd>) -> Vec<PlayerCmd> {
    let last_play = cmds
        .iter()
        .rposition(|c| matches!(c, PlayerCmd::Play { .. }));
    let last_volume = cmds
        .iter()
        .rposition(|c| matches!(c, PlayerCmd::SetVolume(_)));
    let kept = cmds.into_iter().enumerate().filter(|(i, c)| {
        let after_play = last_play.is_none_or(|p| *i >= p);
        after_play && (!matches!(c, PlayerCmd::SetVolume(_)) || Some(*i) == last_volume)
    });
    let mut out: Vec<PlayerCmd> = Vec::new();
    for (_, cmd) in kept {
        match (out.last_mut(), cmd) {
            (Some(PlayerCmd::Seek(sum)), PlayerCmd::Seek(by)) => *sum = sum.saturating_add(by),
            (_, cmd) => out.push(cmd),
        }
    }
    out.retain(|cmd| !matches!(cmd, PlayerCmd::Seek(0)));
    out
}

#[cfg(test)]
mod tests;
