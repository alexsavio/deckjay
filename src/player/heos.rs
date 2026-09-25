//! Denon / Marantz HEOS speakers, through the HEOS CLI on TCP port 1255.
//!
//! One connection stays open, as the CLI spec asks. A command that finds it
//! closed or reset is sent again on a new one; after a timeout, the next call
//! reconnects. It drives the player whose `ip` is the speaker host, else the
//! first one `get_players` lists. `browse/play_stream`
//! plays a single URL and there is no queue for plain URLs, so this player
//! walks the album itself: while an album is active it polls the play state
//! every second, and a `stop` after the track was seen playing starts the
//! next track, or ends the album after the last one. A `stop` before that is
//! the stream still loading, for up to [`LOAD_TIMEOUT`]. This cannot tell a
//! track that ended from a stop pressed in the HEOS app: both move to the
//! next track.
//!
//! The CLI has no command that tells the place in a track. While an item
//! that reports progress plays, change events are on and the place comes
//! from `event/player_now_playing_progress`, which the speaker sends every
//! few seconds.

mod cli;

use std::io::{self, ErrorKind};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, info, warn};

pub use self::cli::players;
use self::cli::{Cli, IO_TIMEOUT, Message, NowPlaying};
use super::progress::Place;
use super::{Emitter, PlayerCmd, PlayerEvent, Speaker, TrackInfo};
use crate::library::ItemId;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long a new track may report `stop` before we give up on it.
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// A last track that stops this close to its length has ended; earlier, it
/// was stopped in the HEOS app. Progress events come every ~5 s.
const END_MARGIN: Duration = Duration::from_secs(10);

pub(super) struct HeosPlayer {
    host: String,
    port: u16,
    conn: Option<Connection>,
    /// Id of the item we started last.
    item: ItemId,
    /// Tracks of the album we started last.
    tracks: Vec<TrackInfo>,
    /// Whether that album reports progress.
    progress: bool,
    /// `None` while no album is active.
    current: Option<Current>,
    load_timeout: Duration,
    io_timeout: Duration,
}

/// The track of our album the speaker was told to play.
#[derive(Clone, Copy)]
struct Current {
    track: usize,
    phase: Phase,
    /// The latest progress event for this track.
    seen: Option<NowPlaying>,
}

#[derive(Clone, Copy)]
enum Phase {
    /// Sent to the speaker at this time, not seen playing yet.
    Loading(Instant),
    Started,
}

enum PlayState {
    Play,
    Pause,
    Stop,
}

impl HeosPlayer {
    pub(super) fn new(host: String, port: u16) -> HeosPlayer {
        HeosPlayer {
            host,
            port,
            conn: None,
            item: ItemId(0),
            tracks: Vec::new(),
            progress: false,
            current: None,
            load_timeout: LOAD_TIMEOUT,
            io_timeout: IO_TIMEOUT,
        }
    }
}

impl Speaker for HeosPlayer {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        match cmd {
            PlayerCmd::Play {
                item,
                content,
                volume,
            } => {
                let list = content.into_tracks()?;
                events.begin(item, list.progress);
                self.item = item;
                self.tracks = list.tracks;
                self.progress = list.progress;
                self.current = None;
                self.set_volume(volume)?;
                self.play_track(list.start.track, events)
            }
            PlayerCmd::SetVolume(volume) => self.set_volume(volume),
            PlayerCmd::TogglePause => self.toggle_pause(events),
            PlayerCmd::Next => match self.current {
                Some(c) if c.track + 1 < self.tracks.len() => self.play_track(c.track + 1, events),
                _ => Ok(()),
            },
            PlayerCmd::Prev => match self.current {
                Some(c) => self.play_track(c.track.saturating_sub(1), events),
                None => Ok(()),
            },
        }
    }

    fn poll(&mut self, events: &mut Emitter) -> Result<()> {
        if self.current.is_none() {
            return Ok(());
        }
        let state = self.play_state()?;
        self.report_place(events);
        let Some(current) = self.current else {
            return Ok(());
        };
        let started = Some(Current {
            phase: Phase::Started,
            ..current
        });
        match state {
            PlayState::Play => {
                self.current = started;
                events.emit(PlayerEvent::Playing(self.item));
            }
            PlayState::Pause => {
                self.current = started;
                events.emit(PlayerEvent::Paused(self.item));
            }
            PlayState::Stop => match current.phase {
                Phase::Loading(since) if since.elapsed() < self.load_timeout => {}
                Phase::Loading(_) => {
                    warn!(track = current.track, "the track did not start playing");
                    self.end(events);
                }
                Phase::Started if current.track + 1 < self.tracks.len() => {
                    if let Err(err) = self.play_track(current.track + 1, events) {
                        self.end(events);
                        return Err(err);
                    }
                }
                Phase::Started => {
                    if ended(current.seen) {
                        events.finished();
                    }
                    self.end(events);
                }
            },
        }
        Ok(())
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.current.map(|_| POLL_INTERVAL)
    }

    fn reset(&mut self) {
        self.current = None;
    }
}

impl HeosPlayer {
    /// Without an active album, whatever the speaker plays is not known to be
    /// ours, so the album starts over instead of pausing it.
    fn toggle_pause(&mut self, events: &mut Emitter) -> Result<()> {
        let state = self.play_state()?;
        self.report_place(events);
        match (self.current, state) {
            (Some(_), PlayState::Play) => {
                self.set_play_state("pause")?;
                events.emit(PlayerEvent::Paused(self.item));
            }
            (Some(_), PlayState::Pause) => {
                self.set_play_state("play")?;
                events.emit(PlayerEvent::Playing(self.item));
            }
            (
                Some(Current {
                    phase: Phase::Loading(since),
                    ..
                }),
                PlayState::Stop,
            ) if since.elapsed() < self.load_timeout => {
                events.emit(PlayerEvent::Playing(self.item));
            }
            _ if !self.tracks.is_empty() => {
                let start = events.resume_point();
                self.play_track(start.track, events)?;
            }
            _ => events.emit(PlayerEvent::Stopped),
        }
        Ok(())
    }

    fn play_track(&mut self, index: usize, events: &mut Emitter) -> Result<()> {
        let url = self
            .tracks
            .get(index)
            .ok_or_else(|| anyhow!("the album has no track {index}"))?
            .url
            .clone();
        // A Denon AVR-X1600H keeps earlier streams in a hidden queue that
        // `get_queue` does not list: without this, it plays them after our
        // track and never reports `stop`. It fails (eid 4) when the queue is
        // already empty, which is fine; that `fail` reply keeps the connection,
        // an I/O error does not.
        match self.call("player/clear_queue", &[]) {
            Ok(_) => {}
            Err(err) if self.conn.is_some() => debug!("clear_queue: {err:#}"),
            Err(err) => return Err(err),
        }
        self.call("browse/play_stream", &[("url", &url)])?;
        // Anything read up to this reply was about the stream before.
        self.take_progress();
        let track = &self.tracks[index];
        info!(album = %track.album, track = %track.title, "playing");
        self.current = Some(Current {
            track: index,
            phase: Phase::Loading(Instant::now()),
            seen: None,
        });
        events.place(Place::start_of(index, Duration::ZERO), true);
        events.emit(PlayerEvent::Playing(self.item));
        Ok(())
    }

    /// Offers the place from the latest progress event, if one came.
    fn report_place(&mut self, events: &mut Emitter) {
        let Some(seen) = self.take_progress() else {
            return;
        };
        let Some(current) = &mut self.current else {
            return;
        };
        current.seen = Some(seen);
        let at = Place {
            track: current.track,
            position: seen.position,
            duration: seen.duration,
        };
        events.place(at, false);
    }

    fn take_progress(&mut self) -> Option<NowPlaying> {
        self.conn.as_mut()?.cli.take_progress()
    }

    /// Change events carry the place, so they are on only while an album
    /// that reports progress is active.
    fn wants_events(&self) -> bool {
        self.progress && self.current.is_some()
    }

    /// A Denon AVR-X1600H keeps retrying a finished URL stream, and sometimes
    /// plays it again, until it is told to stop.
    fn end(&mut self, events: &mut Emitter) {
        self.current = None;
        if let Err(err) = self.set_play_state("stop") {
            warn!("cannot stop the speaker: {err:#}");
        }
        events.emit(PlayerEvent::Stopped);
    }

    fn set_volume(&mut self, volume: f32) -> Result<()> {
        let level = (super::clamp_volume(volume) * 100.0).round() as u8;
        self.call("player/set_volume", &[("level", &level.to_string())])?;
        Ok(())
    }

    fn set_play_state(&mut self, state: &str) -> Result<()> {
        self.call("player/set_play_state", &[("state", state)])?;
        Ok(())
    }

    fn play_state(&mut self) -> Result<PlayState> {
        let message = self.call("player/get_play_state", &[])?;
        match message.get("state") {
            Some("play") => Ok(PlayState::Play),
            Some("pause") => Ok(PlayState::Pause),
            // A Denon AVR-X1600H says `unknown` before a stream starts and
            // after it ends.
            Some("stop" | "unknown") => Ok(PlayState::Stop),
            other => bail!("unexpected HEOS play state {other:?}"),
        }
    }

    /// Sends `command` for our player (`pid` is added) and returns the reply's message.
    fn call(&mut self, command: &str, args: &[(&str, &str)]) -> Result<Message> {
        let reused = self.conn.is_some();
        match self.call_once(command, args) {
            // The CLI resets idle connections when it recovers from a hang, and
            // a restarted receiver has none: the old socket fails at once, not
            // with a timeout. Every command sent here can safely go twice.
            Err(err) if reused && connection_lost(&err) => {
                debug!("HEOS connection lost, reconnecting: {err:#}");
                self.call_once(command, args)
            }
            result => result,
        }
    }

    fn call_once(&mut self, command: &str, args: &[(&str, &str)]) -> Result<Message> {
        let mut conn = match self.conn.take() {
            Some(conn) => conn,
            None => Connection::open(&self.host, self.port, self.io_timeout)?,
        };
        let events = self.wants_events();
        if conn.events != events {
            conn.cli.set_change_events(events)?;
            conn.events = events;
        }
        let pid = conn.pid.to_string();
        let args: Vec<(&str, &str)> = [("pid", pid.as_str())]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        // On an I/O error `conn` is dropped here, so the next call reconnects.
        let reply = conn.cli.exchange(command, &args)?;
        self.conn = Some(conn);
        Ok(reply.checked(command)?.message())
    }
}

struct Connection {
    cli: Cli,
    pid: i64,
    /// Whether change events are on.
    events: bool,
}

impl Connection {
    fn open(host: &str, port: u16, io_timeout: Duration) -> Result<Connection> {
        let mut cli = Cli::connect(host, port, io_timeout)?;
        let peer = cli.peer_ip();
        let players = cli.players()?;
        let player = players
            .iter()
            .find(|p| {
                p.ip.as_deref()
                    .is_some_and(|ip| ip == host || Some(ip) == peer.as_deref())
            })
            .or_else(|| players.first())
            .with_context(|| format!("the HEOS system at {host} has no players"))?;
        info!(pid = player.pid, name = %player.name, model = %player.model, "using HEOS player");
        let pid = player.pid;
        cli.follow(pid);
        Ok(Connection {
            cli,
            pid,
            events: false,
        })
    }
}

/// Whether the last track, stopped after `seen`, played to its end. With no
/// length known, every stop counts as the end.
fn ended(seen: Option<NowPlaying>) -> bool {
    match seen {
        Some(NowPlaying {
            position,
            duration: Some(duration),
        }) => position + END_MARGIN >= duration,
        _ => true,
    }
}

/// The socket failed at once (reset, closed, broken pipe) rather than timing
/// out; a read timeout is `WouldBlock` on Unix and `TimedOut` on Windows.
fn connection_lost(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .is_some_and(|e| !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

#[cfg(test)]
mod tests;
