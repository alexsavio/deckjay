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

/// Like the `steps.*` fixtures: 3 s of 440 Hz whose amplitude names the
/// second (0.2, 0.4, 0.6).
fn steps_wav(dir: &Path) -> PathBuf {
    let path = dir.join("steps.wav");
    let samples: Vec<i16> = (0..3 * RATE)
        .map(|i| {
            let t = f64::from(i) / f64::from(RATE);
            let amplitude = 0.2 * (1.0 + t.floor());
            (amplitude * (std::f64::consts::TAU * 440.0 * t).sin() * 32_767.0) as i16
        })
        .collect();
    write_wav(&path, RATE, 1, &samples);
    path
}

/// Everything `source` decodes from where it stands.
fn rest_of(source: &mut Source) -> Vec<f32> {
    let (mut rest, mut packet) = (Vec::new(), Vec::new());
    while source.next(&mut packet).is_some() {
        rest.extend_from_slice(&packet);
    }
    rest
}

#[test]
fn a_seek_lands_near_the_target_and_tells_where_in_every_format() {
    let dir = TempDir::new().unwrap();
    let m4b = dir.path().join("steps.m4b");
    std::fs::copy(fixture("steps.m4a"), &m4b).unwrap();
    let files = ["steps.mp3", "steps.m4a", "steps.flac", "steps.ogg"]
        .map(fixture)
        .into_iter()
        .chain([m4b, steps_wav(dir.path())]);
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (rate, channels, whole) = decode_all(&path);
        let frames = |s: f64| (s * f64::from(rate)) as usize * channels;
        let length = (whole.len() / channels) as f64 / f64::from(rate);
        for target in [0.5, 1.5, 2.5] {
            let mut source = Source::open(&path).unwrap();
            let landed = source.seek(secs(target)).unwrap();
            assert_eq!(source.start(), landed, "{name}");
            let landed = landed.as_secs_f64();
            assert!((landed - target).abs() < 0.3, "{name} {target}: {landed}");

            let rest = rest_of(&mut source);
            // The rest is as long as the whole track from `landed` on.
            let rest_length = (rest.len() / channels) as f64 / f64::from(rate);
            let gap = length - landed - rest_length;
            assert!(gap.abs() < 0.15, "{name} {target}: {gap} s off");
            // And as loud as that spot of the whole track, once the decoder
            // has settled.
            let heard = peak(&rest[frames(0.2)..frames(0.3)]);
            let there = peak(&whole[frames(landed + 0.2)..frames(landed + 0.3)]);
            assert!(
                (heard - there).abs() < 0.2 * there,
                "{name} {target}: {heard} after the seek, {there} in the whole"
            );
        }
    }
}

#[test]
fn a_seek_past_the_end_plays_the_track_from_its_beginning() {
    let dir = TempDir::new().unwrap();
    let path = steps_wav(dir.path());
    let mut source = Source::open_at(&path, secs(10.0)).unwrap();
    assert_eq!(source.start(), Duration::ZERO);
    assert_eq!(rest_of(&mut source).len(), 3 * RATE as usize);
}

#[test]
fn a_book_resumes_inside_its_start_track_and_counts_on_from_there() {
    let mut rig = Rig::new();
    rig.emitter.progress.interval = Duration::ZERO;
    let path = rig.dir.path().join("2.wav");
    let one_second = RATE as usize;
    let samples = [vec![1000; one_second], vec![2000; one_second]].concat();
    write_wav(&path, RATE, 1, &samples);
    let tracks = vec![rig.track("1.wav", 0.2, 500), track_info(path)];
    let start = Start {
        track: 1,
        position: secs(1.5),
    };
    rig.send(book(tracks, start)).unwrap();

    let landed = rig.engine().position();
    assert!((secs(1.2)..=secs(1.5)).contains(&landed), "{landed:?}");
    assert_eq!(places(&rig.events()), [(1, landed)]);
    let heard = rig.listen(landed.as_secs_f64() + 0.2);
    assert_eq!(levels(&heard, 1.0), [2000]);
    rig.poll().unwrap();
    let played = secs((heard.iter().filter(|s| **s != 0.0).count() / 2) as f64 / f64::from(RATE));
    assert_eq!(places(&rig.events()), [(1, landed + played)]);
}

#[test]
fn a_book_past_the_end_of_its_track_starts_that_track_over() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 1.0, 1000), rig.track("2.wav", 1.0, 2000)];
    let start = Start {
        track: 0,
        position: secs(30.0),
    };
    rig.send(book(tracks, start)).unwrap();
    assert_eq!(places(&rig.events()), [(0, Duration::ZERO)]);
    assert_eq!(levels(&rig.listen(0.1), 1.0), [1000]);
}

#[test]
fn a_restart_after_a_failure_goes_on_where_the_book_got_to() {
    let mut rig = Rig::new();
    let path = rig.dir.path().join("1.wav");
    let one_second = RATE as usize;
    let samples = [vec![1000; one_second], vec![2000; one_second]].concat();
    write_wav(&path, RATE, 1, &samples);
    rig.send(book(vec![track_info(path)], Start::default()))
        .unwrap();
    rig.events();
    rig.listen(1.2);
    rig.poll().unwrap();
    // As `player::run` handles a failed command.
    rig.player.reset();
    rig.emitter.emit(PlayerEvent::Stopped);
    let [(0, got_to), ..] = places(&rig.events())[..] else {
        panic!("no place before Stopped");
    };
    assert!(got_to >= secs(1.2), "{got_to:?}");

    rig.send(PlayerCmd::TogglePause).unwrap();
    let resumed = rig.engine().position();
    assert!(resumed <= got_to && resumed > secs(1.0), "{resumed:?}");
    assert_eq!(
        levels(&rig.listen(resumed.as_secs_f64() + 0.1), 1.0),
        [2000]
    );
}
