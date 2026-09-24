//! Plays albums on the speaker, on a thread of its own. Two kinds of speaker
//! are supported: Chromecast ([`cast`]) and Denon HEOS ([`heos`]). Both take
//! [`PlayerCmd`]s and report [`PlayerEvent`]s, so the UI does not know which
//! one it drives.

mod cast;
pub mod heos;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, warn};

use crate::config::SpeakerType;

#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// Where the speaker downloads the track from.
    pub url: String,
    pub content_type: String,
    pub title: String,
    pub album: String,
    pub cover_url: Option<String>,
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
    /// Goes to the previous track. On Chromecast it restarts the current track
    /// instead once that has played for a few seconds.
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

/// How long the player thread sleeps between commands while no album plays.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

pub fn spawn(
    speaker_type: SpeakerType,
    host: String,
    port: u16,
    events: Sender<PlayerEvent>,
) -> Sender<PlayerCmd> {
    let (tx, rx) = mpsc::channel();
    let (name, speaker): (&str, Box<dyn Speaker + Send>) = match speaker_type {
        SpeakerType::Cast => ("cast", Box::new(cast::CastPlayer::new(host, port))),
        SpeakerType::Heos => ("heos", Box::new(heos::HeosPlayer::new(host, port))),
    };
    let emitter = Emitter {
        tx: events,
        last: None,
    };
    thread::Builder::new()
        .name(name.into())
        .spawn(move || run(speaker, &rx, emitter))
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

fn run(mut speaker: Box<dyn Speaker + Send>, rx: &Receiver<PlayerCmd>, mut events: Emitter) {
    loop {
        let timeout = speaker.poll_interval().unwrap_or(IDLE_WAIT);
        match rx.recv_timeout(timeout) {
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
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if speaker.poll_interval().is_some()
                    && let Err(err) = speaker.poll(&mut events)
                {
                    debug!("status poll failed: {err:#}");
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Sends events to the UI, skipping repeats of the last one.
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
        for speaker_type in [SpeakerType::Cast, SpeakerType::Heos] {
            // Nothing listens on this port, so every command fails at once.
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let (tx, events) = mpsc::channel();
            let player = spawn(speaker_type, "127.0.0.1".into(), port, tx);

            for _ in 0..2 {
                player.send(PlayerCmd::TogglePause).unwrap();
                assert_eq!(
                    events.recv_timeout(Duration::from_secs(5)),
                    Ok(PlayerEvent::Stopped),
                    "{speaker_type:?}"
                );
            }
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
