use std::sync::mpsc::{self, Receiver};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::spotify::fake::{self, Fake};
use crate::spotify::token::{Secret, TokenFile};

const URI: &str = "spotify:playlist:abc";

/// A signed-in player that talks to the fake, and its events.
struct Rig {
    fake: Fake,
    _dir: TempDir,
    player: SpotifyPlayer,
    emitter: Emitter,
    events: Receiver<PlayerEvent>,
}

impl Rig {
    fn new(device: &str) -> Rig {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        TokenFile {
            client_id: fake::CLIENT_ID.into(),
            refresh_token: Secret::new(fake::REFRESH_TOKEN),
        }
        .save(dir.path())
        .unwrap();
        let connect = Connect {
            state_dir: dir.path().into(),
            device: device.into(),
        };
        let player = SpotifyPlayer::new(connect, fake.endpoints());
        let (tx, events) = mpsc::channel();
        Rig {
            fake,
            _dir: dir,
            player,
            emitter: Emitter::new(tx),
            events,
        }
    }

    fn send(&mut self, cmd: PlayerCmd) -> Result<()> {
        self.player.handle(cmd, &mut self.emitter)
    }

    fn play(&mut self) -> Result<()> {
        self.send(PlayerCmd::Play {
            item: ItemId(3),
            content: Content::Spotify(Playlist {
                uri: URI.into(),
                name: "Bedtime".into(),
            }),
            volume: 0.25,
        })
    }

    fn events(&self) -> Vec<PlayerEvent> {
        self.events.try_iter().collect()
    }

    /// `(method, path)` of every Web API request.
    fn calls(&self) -> Vec<(String, String)> {
        self.fake
            .script()
            .requests
            .iter()
            .filter(|r| r.path.starts_with("/v1/"))
            .map(|r| (r.method.clone(), r.path.clone()))
            .collect()
    }
}

fn call(method: &str, path: &str) -> (String, String) {
    (method.into(), path.into())
}

#[test]
fn a_playlist_plays_on_the_named_device() {
    let mut rig = Rig::new("den");
    rig.play().unwrap();

    assert_eq!(
        rig.calls(),
        [
            call("GET", "/v1/me/player/devices"),
            call("PUT", "/v1/me/player"),
            call("PUT", "/v1/me/player/volume"),
            call("PUT", "/v1/me/player/play"),
        ]
    );
    let transfer = &rig.fake.requests_to("/v1/me/player")[0];
    assert_eq!(
        transfer.json(),
        json!({"device_ids": ["dev-den"], "play": false})
    );
    let volume = &rig.fake.requests_to("/v1/me/player/volume")[0];
    assert_eq!(volume.param("volume_percent").as_deref(), Some("25"));
    let play = &rig.fake.requests_to("/v1/me/player/play")[0];
    assert_eq!(play.param("device_id").as_deref(), Some("dev-den"));
    assert_eq!(play.json()["context_uri"], URI);
    assert_eq!(rig.events(), [PlayerEvent::Playing(ItemId(3))]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_PLAYING));
}

#[test]
fn an_active_device_without_volume_control_gets_only_the_play() {
    let mut rig = Rig::new("kitchen");
    rig.play().unwrap();
    assert_eq!(
        rig.calls(),
        [
            call("GET", "/v1/me/player/devices"),
            call("PUT", "/v1/me/player/play"),
        ]
    );
}

#[test]
fn a_device_gone_idle_is_woken_once() {
    let mut rig = Rig::new("den");
    rig.fake.can(
        "/v1/me/player/play",
        404,
        None,
        r#"{"error": {"status": 404, "message": "Device not found", "reason": "NO_ACTIVE_DEVICE"}}"#,
    );
    rig.play().unwrap();
    assert_eq!(rig.fake.requests_to("/v1/me/player/play").len(), 2);
    assert_eq!(rig.fake.requests_to("/v1/me/player").len(), 2);
}

#[test]
fn an_unknown_device_names_the_ones_spotify_sees() {
    let mut rig = Rig::new("bathroom");
    let err = rig.play().unwrap_err();
    assert!(format!("{err:#}").contains("Den"), "{err:#}");
    assert!(rig.events().is_empty());
}

#[test]
fn without_a_login_the_error_says_how_to_sign_in() {
    let mut rig = Rig::new("den");
    std::fs::remove_file(TokenFile::path(&rig.player.connect.state_dir)).unwrap();
    let err = rig.play().unwrap_err();
    assert!(format!("{err:#}").contains("spotify-login"), "{err:#}");
}

#[test]
fn play_pause_toggles_on_the_device() {
    let mut rig = Rig::new("den");
    rig.play().unwrap();
    rig.events();
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.send(PlayerCmd::Next).unwrap();

    assert_eq!(
        rig.events(),
        [
            PlayerEvent::Paused(ItemId(3)),
            PlayerEvent::Playing(ItemId(3))
        ]
    );
    let plays = rig.fake.requests_to("/v1/me/player/play");
    assert_eq!(plays.len(), 2);
    assert_eq!(plays[1].body, "", "resume without a context");
    assert_eq!(rig.fake.requests_to("/v1/me/player/pause").len(), 1);
    assert_eq!(rig.fake.requests_to("/v1/me/player/next").len(), 1);
}

#[test]
fn a_poll_follows_our_playlist_until_something_else_plays() {
    let mut rig = Rig::new("den");
    rig.play().unwrap();
    rig.events();
    let state = |context: &str, playing: bool| {
        json!({"device": {"id": "dev-den"}, "is_playing": playing,
               "context": {"uri": context}, "progress_ms": 1000, "item": {"name": "Song"}})
    };

    rig.fake.script().player = Some(state(URI, false));
    rig.player.poll(&mut rig.emitter).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Paused(ItemId(3))]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_PAUSED));

    rig.fake.script().player = Some(state("spotify:album:other", true));
    rig.player.poll(&mut rig.emitter).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Stopped]);
    assert_eq!(rig.player.poll_interval(), None);
}

#[test]
fn stop_pauses_a_playing_device_and_forgets_it() {
    let mut rig = Rig::new("den");
    rig.play().unwrap();
    rig.player.stop(&mut rig.emitter).unwrap();
    assert_eq!(rig.fake.requests_to("/v1/me/player/pause").len(), 1);
    assert_eq!(rig.player.poll_interval(), None);
    rig.player.stop(&mut rig.emitter).unwrap();
    assert_eq!(rig.fake.requests_to("/v1/me/player/pause").len(), 1);
}

#[test]
fn the_power_key_pauses_our_device_whatever_it_plays() {
    let mut rig = Rig::new("den");
    let playing_on = |device: &str| {
        json!({"device": {"id": device}, "is_playing": true,
               "context": {"uri": "spotify:album:someone-else"}, "progress_ms": 1, "item": {"name": "Song"}})
    };

    rig.fake.script().player = Some(playing_on("dev-kitchen"));
    rig.player.stop_everything(&mut rig.emitter).unwrap();
    assert!(
        rig.fake.requests_to("/v1/me/player/pause").is_empty(),
        "another device"
    );

    rig.fake.script().player = Some(playing_on("dev-den"));
    rig.player.stop_everything(&mut rig.emitter).unwrap();
    let pauses = rig.fake.requests_to("/v1/me/player/pause");
    assert_eq!(pauses.len(), 1);
    assert_eq!(pauses[0].param("device_id").as_deref(), Some("dev-den"));
}
