//! Internet radio: one live stream, tried again when it drops at once.

use super::*;
use crate::player::tests::{head, station, web};

/// A station whose key is a `.pls` naming the stream `/live.mp3`; returns
/// its URL and the stream's.
fn station_urls() -> (String, String) {
    let base = web(|path, stream| {
        if path == "/kids.pls" {
            let live = format!("http://{}/live.mp3", stream.local_addr().unwrap());
            head(stream, 200, "audio/x-scpls");
            let _ = stream.write_all(format!("[playlist]\nFile1={live}\n").as_bytes());
        } else {
            head(stream, 200, "audio/mpeg");
            let _ = stream.write_all(b"not really audio");
        }
    });
    (format!("{base}/kids.pls"), format!("{base}/live.mp3"))
}

fn play_station(url: String) -> PlayerCmd {
    PlayerCmd::Play {
        item: ITEM,
        content: station(url),
        volume: 0.2,
    }
}

/// A rig playing a station; returns the stream URL.
fn tuned() -> (Rig, String) {
    let mut rig = Rig::new(ONE_PLAYER);
    let (url, live) = station_urls();
    rig.send(play_station(url)).unwrap();
    (rig, live)
}

#[test]
fn a_station_streams_the_url_its_playlist_names() {
    let (rig, live) = tuned();
    assert_eq!(
        rig.fake.commands(),
        [
            "system/register_for_change_events?enable=off",
            "player/get_players",
            "player/set_volume?pid=7&level=20",
            "player/clear_queue?pid=7",
            format!("browse/play_stream?pid=7&url={live}").as_str(),
        ]
    );
    assert_eq!(rig.events(), [Playing(ITEM)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));
}

#[test]
fn a_station_that_cannot_be_reached_sends_nothing() {
    let mut rig = Rig::new(ONE_PLAYER);
    let gone = web(|_, stream| head(stream, 404, "text/html"));
    let err = rig
        .send(play_station(format!("{gone}/kids.mp3")))
        .unwrap_err();
    assert!(format!("{err:#}").contains("404"), "{err:#}");
    assert_eq!(rig.fake.commands(), Vec::<String>::new());
    assert_eq!(rig.events(), []);
}

#[test]
fn a_station_keeps_playing_while_it_loads_and_plays() {
    let (mut rig, live) = tuned();
    rig.poll_with("stop");
    rig.poll_with("unknown");
    rig.poll_with("play");
    rig.poll_with("play");
    assert_eq!(rig.fake.streamed(), [live]);
    assert_eq!(rig.events(), [Playing(ITEM)]);
}

#[test]
fn a_station_that_drops_right_after_starting_is_tried_up_to_three_times() {
    let (mut rig, live) = tuned();
    for _ in 0..3 {
        rig.poll_with("play");
        rig.poll_with("stop");
    }
    assert_eq!(rig.fake.streamed(), [live.clone(), live.clone(), live]);
    assert_eq!(
        rig.events(),
        [Playing(ITEM), Stopped],
        "a retry stays playing"
    );
    assert_eq!(rig.player.poll_interval(), None);
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
}

#[test]
fn a_lasting_stop_ends_the_station() {
    let (mut rig, live) = tuned();
    rig.player.drop_window = Duration::ZERO;
    rig.poll_with("play");
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [live]);
    assert_eq!(rig.events(), [Playing(ITEM), Stopped]);
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
}

#[test]
fn a_station_that_never_starts_ends() {
    let (mut rig, live) = tuned();
    rig.player.load_timeout = Duration::ZERO;
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [live]);
    assert_eq!(rig.events(), [Playing(ITEM), Stopped]);
}

#[test]
fn next_and_prev_do_nothing_on_a_station() {
    let (mut rig, live) = tuned();
    let before = rig.fake.commands().len();
    rig.send(PlayerCmd::Next).unwrap();
    rig.send(PlayerCmd::Prev).unwrap();
    assert_eq!(rig.fake.commands().len(), before);
    assert_eq!(rig.fake.streamed(), [live]);
    assert_eq!(rig.events(), [Playing(ITEM)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));
}

#[test]
fn pause_pauses_the_station_and_play_rejoins_it_live() {
    let (mut rig, live) = tuned();
    rig.poll_with("play");
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=pause"
    );
    rig.poll_with("pause");
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.fake.streamed(), [live.clone(), live]);
    assert_eq!(rig.events(), [Playing(ITEM), Paused(ITEM), Playing(ITEM)]);
}

#[test]
fn a_stop_after_a_pause_ends_the_station_instead_of_sending_it_again() {
    let (mut rig, live) = tuned();
    rig.poll_with("play");
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [live]);
    assert_eq!(rig.events(), [Playing(ITEM), Paused(ITEM), Stopped]);
    assert_eq!(rig.player.poll_interval(), None);
}

#[test]
fn a_station_the_receiver_cannot_pause_is_stopped_and_rejoined() {
    let (mut rig, live) = tuned();
    rig.poll_with("play");
    rig.fake.script().fail = Some("player/set_play_state?pid=7&state=pause");
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
    assert_eq!(rig.player.poll_interval(), None, "a stop is not a drop");

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.fake.streamed(), [live.clone(), live]);
    assert_eq!(rig.events(), [Playing(ITEM), Paused(ITEM), Playing(ITEM)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));
}

#[test]
fn a_station_after_an_album_does_not_walk_the_album() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    let (url, live) = station_urls();
    rig.send(play_station(url)).unwrap();
    rig.poll_with("play");
    rig.player.drop_window = Duration::ZERO;
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [track_url(0), live]);
    assert_eq!(rig.events(), [Playing(ITEM), Playing(ITEM), Stopped]);
}

#[test]
fn stop_stops_the_receiver_and_forgets_the_station() {
    let (mut rig, _) = tuned();
    rig.player.halt(&mut rig.emitter);
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
    assert_eq!(rig.player.poll_interval(), None);
    assert_eq!(
        rig.events(),
        [Playing(ITEM)],
        "the caller reports what follows"
    );

    let before = rig.fake.commands().len();
    rig.player.halt(&mut rig.emitter);
    assert_eq!(rig.fake.commands().len(), before, "nothing of ours plays");
}

#[test]
fn stop_stops_an_album_too() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("play");
    rig.player.halt(&mut rig.emitter);
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
    assert_eq!(rig.player.poll_interval(), None);
}

#[test]
fn stop_after_the_receiver_went_away_only_forgets_the_item() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.player.io_timeout = Duration::from_millis(100);
    rig.send(play_station(station_urls().0)).unwrap();
    rig.fake.script().mute = Some("player/set_play_state");
    rig.player.halt(&mut rig.emitter);
    assert_eq!(rig.player.poll_interval(), None);
}
