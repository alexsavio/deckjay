//! Plays albums on the speaker, on a thread of its own. Two kinds of speaker
//! are supported: Chromecast ([`cast`]) and Denon HEOS ([`heos`]). Both take
//! [`PlayerCmd`]s and report [`PlayerEvent`]s, so the UI does not know which
//! one it drives.

mod cast;
pub mod heos;
pub mod local;

use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, warn};

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
#[expect(dead_code, reason = "no backend plays radio yet")]
pub struct Station {
    pub url: String,
    /// The MIME type of the stream, when known.
    pub content_type: Option<String>,
    pub name: String,
    pub cover_url: Option<String>,
}

/// A Spotify playlist, played through Spotify Connect.
#[derive(Clone, Debug)]
#[expect(dead_code, reason = "no backend plays Spotify yet")]
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
        #[expect(dead_code, reason = "every backend starts at the first track so far")]
        start: Start,
        /// Whether to report how far playback got, for items that resume.
        #[expect(dead_code, reason = "no backend reports progress yet")]
        progress: bool,
    },
    #[cfg_attr(not(test), expect(dead_code, reason = "radio items do not exist yet"))]
    Stream(Station),
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Spotify items do not exist yet")
    )]
    Spotify(Playlist),
}

impl Content {
    /// The tracks to play from the first; a stream or a playlist is an error.
    fn into_tracks(self) -> Result<Vec<TrackInfo>> {
        match self {
            Content::Tracks { tracks, .. } => Ok(tracks),
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
    /// 0.0 to 1.0.
    SetVolume(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The item with this id is playing.
    Playing(ItemId),
    Paused(ItemId),
    /// Nothing playing any more (album finished, stopped elsewhere, or failed).
    Stopped,
}

/// A speaker that fails this many status polls in a row is taken to be gone.
const MAX_FAILED_POLLS: u32 = 3;
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

pub fn spawn(output: Output, events: Sender<PlayerEvent>) -> Sender<PlayerCmd> {
    let (tx, rx) = mpsc::channel();
    let name = match &output {
        Output::Cast { .. } => "cast",
        Output::Heos { .. } => "heos",
        Output::Local { .. } => "local",
    };
    let emitter = Emitter {
        tx: events,
        last: None,
    };
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
            run(speaker, &rx, emitter);
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
    /// Forgets the album after a failed command.
    fn reset(&mut self);
}

fn run(mut speaker: Box<dyn Speaker>, rx: &Receiver<PlayerCmd>, mut events: Emitter) {
    let mut failed_polls = 0;
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
                    if let Err(err) = speaker.handle(cmd, &mut events) {
                        warn!("speaker command failed: {err:#}");
                        speaker.reset();
                        events.emit(PlayerEvent::Stopped);
                        // The rest were guesses from a state the UI no longer
                        // shows, and each would wait out its own timeout.
                        break;
                    }
                    failed_polls = 0;
                }
            }
            Err(RecvTimeoutError::Timeout) => match speaker.poll(&mut events) {
                Ok(()) => failed_polls = 0,
                Err(err) => {
                    debug!("status poll failed: {err:#}");
                    failed_polls += 1;
                    if failed_polls >= MAX_FAILED_POLLS {
                        warn!("the speaker stopped answering: {err:#}");
                        speaker.reset();
                        events.emit(PlayerEvent::Stopped);
                        failed_polls = 0;
                    }
                }
            },
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
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

struct Emitter {
    tx: Sender<PlayerEvent>,
    last: Option<PlayerEvent>,
}

impl Emitter {
    fn emit(&mut self, event: PlayerEvent) {
        if self.last != Some(event) {
            self.last = Some(event);
            let _ = self.tx.send(event);
        }
    }
}

/// Drops commands made pointless by later ones (e.g. a kid mashing buttons
/// while the speaker was slow to answer).
fn coalesce(cmds: Vec<PlayerCmd>) -> Vec<PlayerCmd> {
    let last_play = cmds
        .iter()
        .rposition(|c| matches!(c, PlayerCmd::Play { .. }));
    let last_volume = cmds
        .iter()
        .rposition(|c| matches!(c, PlayerCmd::SetVolume(_)));
    cmds.into_iter()
        .enumerate()
        .filter(|(i, c)| {
            let after_play = last_play.is_none_or(|p| *i >= p);
            after_play && (!matches!(c, PlayerCmd::SetVolume(_)) || Some(*i) == last_volume)
        })
        .map(|(_, c)| c)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn every_failed_command_reports_stopped() {
        // Nothing listens on this port, so every command fails at once.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let host = String::from("127.0.0.1");
        for output in [
            Output::Cast {
                host: host.clone(),
                port,
            },
            Output::Heos { host, port },
        ] {
            let (tx, events) = mpsc::channel();
            let player = spawn(output.clone(), tx);

            for _ in 0..2 {
                player.send(PlayerCmd::TogglePause).unwrap();
                assert_eq!(
                    events.recv_timeout(Duration::from_secs(5)),
                    Ok(PlayerEvent::Stopped),
                    "{output:?}"
                );
            }
        }
    }

    #[test]
    fn connect_tries_every_address() {
        // `localhost` resolves to ::1 first on macOS, and only IPv4 listens here.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        connect("localhost", port).unwrap();
    }

    #[test]
    fn volumes_are_clamped_to_the_speaker_range() {
        for (volume, expected) in [(-0.1, 0.0), (0.35, 0.35), (1.5, 1.0)] {
            assert!(
                (clamp_volume(volume) - expected).abs() < f32::EPSILON,
                "{volume}"
            );
        }
    }

    fn album() -> PlayerCmd {
        PlayerCmd::Play {
            item: ItemId(0),
            content: Content::Tracks {
                tracks: vec![],
                start: Start::default(),
                progress: false,
            },
            volume: 0.2,
        }
    }

    /// One [`Content`] of each kind that no backend plays yet.
    pub(super) fn unsupported() -> [Content; 2] {
        [
            Content::Stream(Station {
                url: "https://radio.example/kids.mp3".into(),
                content_type: Some("audio/mpeg".into()),
                name: "Kids Radio".into(),
                cover_url: None,
            }),
            Content::Spotify(Playlist {
                uri: "spotify:playlist:37i9dQZF1DX0XUsuxWHRQd".into(),
                name: "Bedtime".into(),
            }),
        ]
    }

    #[test]
    fn only_tracks_can_be_played_so_far() {
        let tracks = Content::Tracks {
            tracks: vec![],
            start: Start::default(),
            progress: false,
        };
        assert!(tracks.into_tracks().is_ok());
        for content in unsupported() {
            let err = content.into_tracks().unwrap_err();
            assert!(err.to_string().contains("not supported yet"), "{err:#}");
        }
    }

    #[test]
    fn unsupported_content_reports_stopped() {
        // A local player opens the sound card only for tracks, so this runs
        // without one.
        let (tx, events) = mpsc::channel();
        let player = spawn(Output::Local { device: None }, tx);
        for content in unsupported() {
            let cmd = PlayerCmd::Play {
                item: ItemId(1),
                content,
                volume: 0.2,
            };
            player.send(cmd).unwrap();
            assert_eq!(
                events.recv_timeout(Duration::from_secs(5)),
                Ok(PlayerEvent::Stopped)
            );
        }
    }

    #[test]
    fn keeps_the_last_album_and_only_the_last_volume_after_it() {
        use PlayerCmd::{Next, Play, SetVolume, TogglePause};
        let out = coalesce(vec![
            SetVolume(0.1),
            album(),
            Next,
            SetVolume(0.2),
            album(),
            Next,
        ]);
        assert!(matches!(out.as_slice(), [Play { .. }, Next]), "{out:?}");

        let out = coalesce(vec![album(), SetVolume(0.3), SetVolume(0.4)]);
        assert!(
            matches!(out.as_slice(), [Play { .. }, SetVolume(v)] if (v - 0.4).abs() < f32::EPSILON),
            "{out:?}"
        );

        let out = coalesce(vec![SetVolume(0.1), TogglePause, SetVolume(0.2)]);
        assert!(
            matches!(out.as_slice(), [TogglePause, SetVolume(v)] if (v - 0.2).abs() < f32::EPSILON),
            "{out:?}"
        );
    }

    enum Poll {
        Answer,
        Fail,
        /// Fails, then has no album left to poll, so `run` waits for a command.
        FailAndIdle,
    }

    /// A speaker that follows a script, to drive `run` without a network.
    struct Scripted {
        polls: VecDeque<Poll>,
        active: bool,
        fail_commands: bool,
        handled: Arc<AtomicUsize>,
        polled: Arc<AtomicUsize>,
        /// Gets a message each time the script goes idle.
        idle: Sender<()>,
    }

    impl Scripted {
        fn new(polls: Vec<Poll>) -> (Scripted, Receiver<()>) {
            let (idle, idle_rx) = mpsc::channel();
            let speaker = Scripted {
                polls: polls.into(),
                active: true,
                fail_commands: false,
                handled: Arc::default(),
                polled: Arc::default(),
                idle,
            };
            (speaker, idle_rx)
        }
    }

    impl Speaker for Scripted {
        fn handle(&mut self, _: PlayerCmd, _: &mut Emitter) -> Result<()> {
            self.handled.fetch_add(1, Ordering::SeqCst);
            anyhow::ensure!(!self.fail_commands, "the speaker is off");
            self.active = true;
            Ok(())
        }

        fn poll(&mut self, _: &mut Emitter) -> Result<()> {
            self.polled.fetch_add(1, Ordering::SeqCst);
            match self.polls.pop_front() {
                Some(Poll::Answer) => Ok(()),
                Some(Poll::Fail) => anyhow::bail!("no answer"),
                Some(Poll::FailAndIdle) | None => {
                    self.active = false;
                    self.idle.send(()).unwrap();
                    anyhow::bail!("no answer")
                }
            }
        }

        fn poll_interval(&self) -> Option<Duration> {
            self.active.then_some(Duration::from_millis(1))
        }

        fn reset(&mut self) {
            self.active = false;
        }
    }

    fn run_on_a_thread(
        speaker: Scripted,
    ) -> (
        Sender<PlayerCmd>,
        Receiver<PlayerEvent>,
        thread::JoinHandle<()>,
    ) {
        let (cmds, rx) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        let thread = thread::spawn(move || run(Box::new(speaker), &rx, Emitter { tx, last: None }));
        (cmds, events, thread)
    }

    #[test]
    fn three_failed_polls_in_a_row_stop_the_album() {
        use Poll::{Answer, Fail};
        let (speaker, _idle) = Scripted::new(vec![Fail, Fail, Answer, Fail, Fail, Fail]);
        let polled = Arc::clone(&speaker.polled);
        let (cmds, events, thread) = run_on_a_thread(speaker);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(PlayerEvent::Stopped)
        );
        drop(cmds);
        thread.join().unwrap();
        assert_eq!(
            polled.load(Ordering::SeqCst),
            6,
            "a poll that works restarts the count, and polling ends with the album"
        );
    }

    #[test]
    fn a_command_that_works_restarts_the_failed_poll_count() {
        use Poll::{Fail, FailAndIdle};
        let (speaker, idle) = Scripted::new(vec![Fail, FailAndIdle, Fail, FailAndIdle]);
        let (cmds, events, thread) = run_on_a_thread(speaker);
        idle.recv_timeout(Duration::from_secs(2)).unwrap();
        cmds.send(PlayerCmd::Next).unwrap();
        idle.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(cmds);
        thread.join().unwrap();
        assert_eq!(events.try_iter().collect::<Vec<_>>(), []);
    }

    #[test]
    fn a_failed_command_drops_the_rest_of_its_batch() {
        let (mut speaker, _idle) = Scripted::new(vec![]);
        speaker.active = false;
        speaker.fail_commands = true;
        let handled = Arc::clone(&speaker.handled);
        let (cmds, rx) = mpsc::channel();
        for _ in 0..3 {
            cmds.send(PlayerCmd::TogglePause).unwrap();
        }
        drop(cmds);
        let (tx, events) = mpsc::channel();
        run(Box::new(speaker), &rx, Emitter { tx, last: None });
        assert_eq!(handled.load(Ordering::SeqCst), 1);
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            [PlayerEvent::Stopped]
        );
    }
}
