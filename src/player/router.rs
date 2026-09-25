//! Sends each command to the speaker that plays it: Spotify playlists to the
//! Spotify Connect device, everything else to the configured output. When a
//! press moves playback from one to the other, the one that played stops
//! first: they can be two different boxes.

use std::time::Duration;

use anyhow::{Result, bail};
use tracing::warn;

use super::{Content, Emitter, PlayerCmd, Speaker};

pub(super) struct Router {
    output: Box<dyn Speaker>,
    spotify: Option<Box<dyn Speaker>>,
    /// Whether the last `Play` went to Spotify.
    on_spotify: bool,
}

impl Router {
    pub(super) fn new(output: Box<dyn Speaker>, spotify: Option<Box<dyn Speaker>>) -> Router {
        Router {
            output,
            spotify,
            on_spotify: false,
        }
    }

    fn active(&mut self) -> &mut dyn Speaker {
        match (&mut self.spotify, self.on_spotify) {
            (Some(spotify), true) => spotify.as_mut(),
            _ => self.output.as_mut(),
        }
    }
}

impl Speaker for Router {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        if let PlayerCmd::Play { content, .. } = &cmd {
            let wants_spotify = matches!(content, Content::Spotify(_));
            if wants_spotify && self.spotify.is_none() {
                bail!("Spotify is not set up: add a [spotify] table with client_id and device");
            }
            if wants_spotify != self.on_spotify {
                if let Err(err) = self.active().stop(events) {
                    warn!("cannot stop the speaker that played: {err:#}");
                }
                self.on_spotify = wants_spotify;
            }
        }
        self.active().handle(cmd, events)
    }

    fn poll(&mut self, events: &mut Emitter) -> Result<()> {
        self.active().poll(events)
    }

    fn poll_interval(&self) -> Option<Duration> {
        match (&self.spotify, self.on_spotify) {
            (Some(spotify), true) => spotify.poll_interval(),
            _ => self.output.poll_interval(),
        }
    }

    fn reset(&mut self) {
        self.active().reset();
    }

    fn stop(&mut self, events: &mut Emitter) -> Result<()> {
        self.active().stop(events)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;

    use super::*;
    use crate::library::ItemId;
    use crate::player::{Playlist, Start};

    /// Records what it was asked to do, as `"<name> <call>"`.
    struct Recorder {
        name: &'static str,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Speaker for Recorder {
        fn handle(&mut self, cmd: PlayerCmd, _: &mut Emitter) -> Result<()> {
            let what = match cmd {
                PlayerCmd::Play { .. } => "play",
                PlayerCmd::TogglePause => "pause",
                PlayerCmd::Next => "next",
                PlayerCmd::Prev => "prev",
                PlayerCmd::SetVolume(_) => "volume",
            };
            self.log.borrow_mut().push(format!("{} {what}", self.name));
            Ok(())
        }

        fn poll(&mut self, _: &mut Emitter) -> Result<()> {
            Ok(())
        }

        fn poll_interval(&self) -> Option<Duration> {
            None
        }

        fn reset(&mut self) {}

        fn stop(&mut self, _: &mut Emitter) -> Result<()> {
            self.log.borrow_mut().push(format!("{} stop", self.name));
            Ok(())
        }
    }

    fn router(with_spotify: bool) -> (Router, Rc<RefCell<Vec<String>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let recorder = |name| -> Box<dyn Speaker> {
            Box::new(Recorder {
                name,
                log: Rc::clone(&log),
            })
        };
        let spotify = with_spotify.then(|| recorder("spotify"));
        (Router::new(recorder("output"), spotify), log)
    }

    fn tracks() -> PlayerCmd {
        PlayerCmd::Play {
            item: ItemId(0),
            content: Content::Tracks {
                tracks: Vec::new(),
                start: Start::default(),
                progress: false,
            },
            volume: 0.2,
        }
    }

    fn playlist() -> PlayerCmd {
        PlayerCmd::Play {
            item: ItemId(1),
            content: Content::Spotify(Playlist {
                uri: "spotify:playlist:abc".into(),
                name: "Bedtime".into(),
            }),
            volume: 0.2,
        }
    }

    #[test]
    fn each_play_goes_to_its_speaker_and_stops_the_other() {
        let (mut router, log) = router(true);
        let mut events = Emitter::new(mpsc::channel().0);
        for cmd in [
            tracks(),
            PlayerCmd::Next,
            playlist(),
            PlayerCmd::TogglePause,
            playlist(),
            tracks(),
        ] {
            router.handle(cmd, &mut events).unwrap();
        }
        assert_eq!(
            *log.borrow(),
            [
                "output play",
                "output next",
                "output stop",
                "spotify play",
                "spotify pause",
                "spotify play",
                "spotify stop",
                "output play",
            ]
        );
    }

    #[test]
    fn a_playlist_without_spotify_set_up_is_an_error() {
        let (mut router, log) = router(false);
        let mut events = Emitter::new(mpsc::channel().0);
        router.handle(tracks(), &mut events).unwrap();
        let err = router.handle(playlist(), &mut events).unwrap_err();
        assert!(err.to_string().contains("[spotify]"), "{err:#}");
        router.handle(PlayerCmd::Next, &mut events).unwrap();
        assert_eq!(*log.borrow(), ["output play", "output next"]);
    }
}
