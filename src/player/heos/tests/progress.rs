//! Places and the end of a book, from `event/player_now_playing_progress`.

use super::*;

fn book(n: usize) -> PlayerCmd {
    play(n, Start::default(), true)
}

fn at(track: usize, secs: u64, duration: Option<u64>) -> PlayerEvent {
    PlayerEvent::Progress {
        item: ITEM,
        track,
        position: Duration::from_secs(secs),
        duration: duration.map(Duration::from_secs),
    }
}

fn is_progress(event: &PlayerEvent) -> bool {
    matches!(
        event,
        PlayerEvent::Progress { .. } | PlayerEvent::Finished(_)
    )
}

#[test]
fn a_book_turns_change_events_on_and_reports_the_place() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.emitter.progress.interval = Duration::ZERO;
    rig.fake.script().progress = Some("pid='7'&cur_pos='12000'&duration='180000'");
    rig.send(book(3)).unwrap();
    assert_eq!(rig.events(), [at(0, 0, None), Playing(ITEM)]);

    rig.poll_with("play");
    assert_eq!(rig.events(), [at(0, 12, Some(180))]);
    let commands = rig.fake.commands();
    assert_eq!(
        commands[commands.len() - 3..],
        [
            format!("browse/play_stream?pid=7&url={}", track_url(0)).as_str(),
            "system/register_for_change_events?enable=on",
            "player/get_play_state?pid=7",
        ]
    );

    rig.send(PlayerCmd::Next).unwrap();
    assert_eq!(rig.events(), [at(1, 0, None), Playing(ITEM)]);
}

#[test]
fn pause_reports_the_latest_place_at_once() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(book(3)).unwrap();
    rig.events();
    rig.fake.script().progress = Some("pid=7&cur_pos=3000&duration=180000");
    rig.poll_with("play");
    assert_eq!(rig.events(), [], "under 5 s after the last report");

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [at(0, 3, Some(180)), Paused(ITEM)]);
}

#[test]
fn the_next_album_reports_the_books_latest_place_first() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(book(3)).unwrap();
    rig.poll_with("play");
    rig.events();
    rig.fake.script().progress = Some("pid=7&cur_pos=42000&duration=180000");
    // Reads the event; nothing asks for the place until the next album.
    rig.send(PlayerCmd::SetVolume(0.3)).unwrap();
    rig.send(play_album(2)).unwrap();
    assert_eq!(rig.events(), [at(0, 42, Some(180)), Playing(ITEM)]);
}

#[test]
fn the_end_of_the_last_track_is_finished_and_turns_events_off() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(book(1)).unwrap();
    rig.events();
    rig.fake.script().progress = Some("pid=7&cur_pos=178000&duration=180000");
    rig.poll_with("play");
    rig.poll_with("stop");
    assert_eq!(rig.events(), [PlayerEvent::Finished(ITEM), Stopped]);
    let commands = rig.fake.commands();
    assert_eq!(
        commands[commands.len() - 2..],
        [
            "system/register_for_change_events?enable=off",
            "player/set_play_state?pid=7&state=stop",
        ]
    );
}

#[test]
fn a_stop_long_before_the_end_of_the_last_track_keeps_the_place() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(book(1)).unwrap();
    rig.events();
    // A stop pressed in the HEOS app, a minute into three.
    rig.fake.script().progress = Some("pid=7&cur_pos=60000&duration=180000");
    rig.poll_with("play");
    rig.poll_with("stop");
    assert_eq!(rig.events(), [at(0, 60, Some(180)), Stopped]);
}

#[test]
fn without_a_length_every_stop_of_the_last_track_is_its_end() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(book(1)).unwrap();
    rig.events();
    // What the AVR-X1600H sends for an m4a with its index at the end.
    rig.fake.script().progress = Some("pid=7&cur_pos=60000&duration=0");
    rig.poll_with("play");
    rig.poll_with("stop");
    assert_eq!(rig.events(), [PlayerEvent::Finished(ITEM), Stopped]);
}

#[test]
fn another_players_progress_is_ignored() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.emitter.progress.interval = Duration::ZERO;
    rig.fake.script().progress = Some("pid=8&cur_pos=5000&duration=9000");
    rig.send(book(2)).unwrap();
    rig.events();
    rig.poll_with("play");
    assert_eq!(rig.events(), []);
}

#[test]
fn an_album_without_progress_keeps_events_off_and_reports_nothing() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.emitter.progress.interval = Duration::ZERO;
    rig.fake.script().progress = Some("pid=7&cur_pos=178000&duration=180000");
    rig.send(play_album(2)).unwrap();
    rig.poll_with("play");
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.poll_with("stop");
    rig.poll_with("play");
    rig.poll_with("stop");
    let events = rig.events();
    assert!(!events.iter().any(is_progress), "{events:?}");
    assert_eq!(events.last(), Some(&Stopped));
    assert!(
        !rig.fake.commands().iter().any(|c| c.ends_with("enable=on")),
        "{:?}",
        rig.fake.commands()
    );
}

#[test]
fn the_start_track_plays_first_and_one_out_of_range_is_the_first() {
    let mut rig = Rig::new(ONE_PLAYER);
    let third = Start {
        track: 2,
        position: Duration::ZERO,
    };
    rig.send(play(3, third, true)).unwrap();
    assert_eq!(rig.events(), [at(2, 0, None), Playing(ITEM)]);
    let beyond = Start {
        track: 3,
        position: Duration::from_secs(40),
    };
    rig.send(play(3, beyond, true)).unwrap();
    assert_eq!(rig.fake.streamed(), [track_url(2), track_url(0)]);
}

#[test]
fn a_place_inside_the_track_starts_that_track_from_its_beginning() {
    let mut rig = Rig::new(ONE_PLAYER);
    let inside = Start {
        track: 1,
        position: Duration::from_secs(90),
    };
    rig.send(play(3, inside, true)).unwrap();
    assert_eq!(rig.fake.streamed(), [track_url(1)]);
    assert_eq!(rig.events(), [at(1, 0, None), Playing(ITEM)]);
    let seeks = rig.fake.commands();
    assert!(
        seeks.iter().all(|c| !c.contains("seek")),
        "the CLI has no seek: {seeks:?}"
    );
}
