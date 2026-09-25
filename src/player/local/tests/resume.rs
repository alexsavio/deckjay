//! Books: where playback starts, and the places and the end it reports.

use super::*;

fn book(tracks: Vec<TrackInfo>, start: Start) -> PlayerCmd {
    PlayerCmd::Play {
        item: ITEM,
        content: Content::Tracks {
            tracks,
            start,
            progress: true,
        },
        volume: 1.0,
    }
}

fn from_track(track: usize) -> Start {
    Start {
        track,
        position: Duration::ZERO,
    }
}

/// The `Progress` events among `events`, as (track, position).
fn places(events: &[PlayerEvent]) -> Vec<(usize, Duration)> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::Progress {
                item: ITEM,
                track,
                position,
                ..
            } => Some((*track, *position)),
            _ => None,
        })
        .collect()
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

#[test]
fn the_place_counts_what_the_sound_card_played_not_what_was_decoded() {
    let mut rig = Rig::new();
    rig.emitter.progress.interval = Duration::ZERO;
    let tracks = vec![rig.track("1.wav", 3.0, 1000)];
    rig.send(book(tracks, Start::default())).unwrap();
    assert_eq!(
        rig.events(),
        [
            PlayerEvent::Progress {
                item: ITEM,
                track: 0,
                position: Duration::ZERO,
                duration: Some(secs(3.0)),
            },
            PlayerEvent::Playing(ITEM),
        ]
    );

    let heard = rig.listen(0.5);
    // By now the decoder is about 1.5 s ahead of the card.
    thread::sleep(Duration::from_millis(50));
    rig.poll().unwrap();
    // Every sample of the track is loud; silence is the decoder catching up.
    let frames = heard.iter().filter(|s| **s != 0.0).count() / 2;
    let played = secs(frames as f64 / f64::from(RATE));
    assert_eq!(places(&rig.events()), [(0, played)]);
    assert!((secs(0.5)..secs(0.6)).contains(&played), "{played:?}");
}

#[test]
fn a_new_track_reports_at_once_and_the_natural_end_is_finished() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 0.2, 1000), rig.track("2.wav", 0.2, 2000)];
    rig.send(book(tracks, Start::default())).unwrap();
    assert_eq!(places(&rig.events()), [(0, Duration::ZERO)]);

    rig.finish_track();
    assert_eq!(places(&rig.events()), [(1, Duration::ZERO)]);
    rig.finish_track();
    assert_eq!(
        rig.events(),
        [PlayerEvent::Finished(ITEM), PlayerEvent::Stopped]
    );
}

#[test]
fn next_on_the_last_track_and_a_new_album_are_not_the_end() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 1.0, 1000), rig.track("2.wav", 1.0, 2000)];
    rig.send(book(tracks, Start::default())).unwrap();
    rig.send(PlayerCmd::Next).unwrap();
    rig.send(PlayerCmd::Next).unwrap();
    assert_eq!(
        places(&rig.events()),
        [(0, Duration::ZERO), (1, Duration::ZERO)]
    );

    rig.listen(0.1);
    let at = rig.engine().position();
    let tracks = vec![rig.track("3.wav", 1.0, 3000)];
    rig.play_album(tracks, 1.0).unwrap();
    let events = rig.events();
    assert_eq!(places(&events), [(1, at)], "the book's last place");
    assert!(!events.contains(&PlayerEvent::Finished(ITEM)), "{events:?}");
}

#[test]
fn pause_reports_the_place_at_once() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 1.0, 1000)];
    rig.send(book(tracks, Start::default())).unwrap();
    rig.events();
    rig.listen(0.3);
    rig.poll().unwrap();
    assert_eq!(rig.events(), [], "under 5 s after the last report");

    rig.send(PlayerCmd::TogglePause).unwrap();
    let at = rig.engine().position();
    assert!(at >= secs(0.3), "{at:?}");
    assert_eq!(
        rig.events(),
        [
            PlayerEvent::Progress {
                item: ITEM,
                track: 0,
                position: at,
                duration: Some(secs(1.0)),
            },
            PlayerEvent::Paused(ITEM),
        ]
    );
}

#[test]
fn an_album_without_progress_reports_no_place_and_no_end() {
    let mut rig = Rig::new();
    rig.emitter.progress.interval = Duration::ZERO;
    let tracks = vec![rig.track("1.wav", 0.2, 1000), rig.track("2.wav", 0.2, 2000)];
    rig.play_album(tracks, 1.0).unwrap();
    rig.listen(0.1);
    rig.poll().unwrap();
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.send(PlayerCmd::TogglePause).unwrap();
    rig.finish_track();
    rig.finish_track();
    assert_eq!(
        rig.events(),
        [
            PlayerEvent::Playing(ITEM),
            PlayerEvent::Paused(ITEM),
            PlayerEvent::Playing(ITEM),
            PlayerEvent::Stopped,
        ]
    );
}

#[test]
fn the_start_track_plays_first() {
    let mut rig = Rig::new();
    let tracks = vec![
        rig.track("1.wav", 0.2, 1000),
        rig.track("2.wav", 0.2, 2000),
        rig.track("3.wav", 0.2, 3000),
    ];
    rig.send(book(tracks, from_track(2))).unwrap();
    assert_eq!(rig.player.current, Some(2));
    assert_eq!(levels(&rig.listen(0.1), 1.0), [3000]);
    assert_eq!(places(&rig.events()), [(2, Duration::ZERO)]);
}
