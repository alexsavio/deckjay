use rust_cast::channels::media::{
    ExtendedPlayerState, ExtendedStatus, IdleReason, Media, PlayerState, StatusEntry, StreamType,
};

use super::*;

const ITEM: ItemId = ItemId(5);

fn url(track: usize) -> String {
    format!(
        "http://10.0.0.2:8765/music/Album/{:02}%20Song.mp3",
        track + 1
    )
}

fn tracks() -> Vec<TrackInfo> {
    (0..3)
        .map(|i| TrackInfo {
            url: url(i),
            path: format!("Album/{i:02}.mp3").into(),
            content_type: "audio/mpeg".into(),
            title: format!("Song {i}"),
            album: "Album".into(),
            cover_url: None,
        })
        .collect()
}

/// A media status entry with only the fields kids-deck reads set.
fn entry(state: PlayerState, content_id: Option<&str>) -> StatusEntry {
    StatusEntry {
        media_session_id: 1,
        media: content_id.map(|id| Media {
            content_id: id.into(),
            stream_type: StreamType::Buffered,
            content_type: "audio/mpeg".into(),
            metadata: None,
            duration: None,
        }),
        playback_rate: 1.0,
        player_state: state,
        current_item_id: None,
        loading_item_id: None,
        preloaded_item_id: None,
        idle_reason: None,
        extended_status: None,
        current_time: None,
        supported_media_commands: 0,
    }
}

fn poll(entry: Option<&StatusEntry>) -> Option<PlayerEvent> {
    poll_event(entry, &tracks(), ITEM)
}

#[test]
fn our_track_playing_or_buffering_is_playing() {
    for state in [PlayerState::Playing, PlayerState::Buffering] {
        let e = entry(state, Some(&url(1)));
        assert_eq!(poll(Some(&e)), Some(PlayerEvent::Playing(ITEM)));
    }
}

#[test]
fn our_track_paused_is_paused() {
    let e = entry(PlayerState::Paused, Some(&url(0)));
    assert_eq!(poll(Some(&e)), Some(PlayerEvent::Paused(ITEM)));
}

#[test]
fn idle_after_the_last_track_or_no_media_is_stopped() {
    let mut finished = entry(PlayerState::Idle, Some(&url(2)));
    finished.idle_reason = Some(IdleReason::Finished);
    assert_eq!(poll(Some(&finished)), Some(PlayerEvent::Stopped));
    assert_eq!(poll(None), Some(PlayerEvent::Stopped));
}

#[test]
fn someone_elses_media_is_stopped() {
    let e = entry(PlayerState::Playing, Some("http://example.com/radio.mp3"));
    assert_eq!(poll(Some(&e)), Some(PlayerEvent::Stopped));
}

#[test]
fn idle_while_the_next_track_loads_keeps_the_album() {
    let mut next_item = entry(PlayerState::Idle, Some(&url(0)));
    next_item.idle_reason = Some(IdleReason::Finished);
    next_item.loading_item_id = Some(2);
    assert_eq!(poll(Some(&next_item)), None);

    let mut extended = entry(PlayerState::Idle, None);
    extended.extended_status = Some(ExtendedStatus {
        player_state: ExtendedPlayerState::Loading,
        media_session_id: Some(1),
        media: None,
    });
    assert_eq!(poll(Some(&extended)), None);
}

#[test]
fn next_goes_one_track_on_and_does_nothing_on_the_last() {
    let tracks = tracks();
    let first = entry(PlayerState::Playing, Some(&url(0)));
    assert_eq!(skip_target(&first, &tracks, true), Some(1));
    let last = entry(PlayerState::Playing, Some(&url(2)));
    assert_eq!(skip_target(&last, &tracks, true), None);
}

#[test]
fn prev_restarts_a_track_played_over_five_seconds_else_goes_back() {
    let tracks = tracks();
    let mut second = entry(PlayerState::Playing, Some(&url(1)));
    second.current_time = Some(6.0);
    assert_eq!(skip_target(&second, &tracks, false), Some(1));
    second.current_time = Some(2.0);
    assert_eq!(skip_target(&second, &tracks, false), Some(0));
    let first = entry(PlayerState::Playing, Some(&url(0)));
    assert_eq!(skip_target(&first, &tracks, false), Some(0));
}

#[test]
fn skipping_someone_elses_media_does_nothing() {
    let tracks = tracks();
    let e = entry(PlayerState::Playing, Some("http://example.com/radio.mp3"));
    assert_eq!(skip_target(&e, &tracks, true), None);
    assert_eq!(skip_target(&e, &tracks, false), None);
}

#[test]
fn spotify_is_refused_before_connecting() {
    // Nothing listens on this port, so a connection attempt would fail with
    // another error.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut player = CastPlayer::new("127.0.0.1".into(), port);
    let (tx, _events) = std::sync::mpsc::channel();
    let mut emitter = Emitter::new(tx);
    let cmd = PlayerCmd::Play {
        item: ITEM,
        content: crate::player::tests::unsupported(),
        volume: 0.2,
    };
    let err = player.handle(cmd, &mut emitter).unwrap_err();
    assert!(format!("{err:#}").contains("not supported yet"), "{err:#}");
}

fn timed(state: PlayerState, track: usize, secs: f32, duration: Option<f32>) -> StatusEntry {
    let mut e = entry(state, Some(&url(track)));
    e.current_time = Some(secs);
    e.media.as_mut().unwrap().duration = duration;
    e
}

#[test]
fn the_place_is_our_track_and_its_time_while_playing_or_paused() {
    let tracks = tracks();
    for state in [
        PlayerState::Playing,
        PlayerState::Buffering,
        PlayerState::Paused,
    ] {
        let e = timed(state, 1, 42.5, Some(300.0));
        assert_eq!(
            place(&e, &tracks),
            Some(Place {
                track: 1,
                position: Duration::from_secs_f32(42.5),
                duration: Some(Duration::from_secs(300)),
            })
        );
    }
}

#[test]
fn no_place_when_idle_foreign_or_without_a_time() {
    let tracks = tracks();
    let mut idle = timed(PlayerState::Idle, 2, 10.0, None);
    idle.idle_reason = Some(IdleReason::Finished);
    assert_eq!(place(&idle, &tracks), None);
    let mut foreign = entry(PlayerState::Playing, Some("http://example.com/radio.mp3"));
    foreign.current_time = Some(10.0);
    assert_eq!(place(&foreign, &tracks), None);
    let untimed = entry(PlayerState::Playing, Some(&url(0)));
    assert_eq!(place(&untimed, &tracks), None);
    for bad in [-1.0, f32::NAN, f32::INFINITY] {
        let e = timed(PlayerState::Playing, 0, bad, None);
        assert_eq!(place(&e, &tracks), None, "{bad}");
    }
}

#[test]
fn a_length_of_zero_or_nonsense_is_unknown() {
    let tracks = tracks();
    for duration in [None, Some(0.0), Some(-3.0), Some(f32::NAN)] {
        let e = timed(PlayerState::Playing, 0, 5.0, duration);
        assert_eq!(place(&e, &tracks).unwrap().duration, None, "{duration:?}");
    }
}

#[test]
fn finished_is_the_last_track_idle_because_it_ended() {
    let tracks = tracks();
    let ended = |track: usize, reason| {
        let mut e = entry(PlayerState::Idle, Some(&url(track)));
        e.idle_reason = reason;
        e
    };
    assert!(finished(&ended(2, Some(IdleReason::Finished)), &tracks));
    assert!(!finished(&ended(1, Some(IdleReason::Finished)), &tracks));
    for reason in [
        None,
        Some(IdleReason::Cancelled),
        Some(IdleReason::Interrupted),
        Some(IdleReason::Error),
    ] {
        assert!(!finished(&ended(2, reason), &tracks), "{reason:?}");
    }

    let mut next_loading = ended(2, Some(IdleReason::Finished));
    next_loading.loading_item_id = Some(4);
    assert!(!finished(&next_loading, &tracks));
    let mut no_media = entry(PlayerState::Idle, None);
    no_media.idle_reason = Some(IdleReason::Finished);
    assert!(!finished(&no_media, &tracks), "the track is unknown");
    let playing = entry(PlayerState::Playing, Some(&url(2)));
    assert!(!finished(&playing, &tracks));
}

#[test]
fn a_stop_seen_near_the_end_of_the_last_track_is_its_end() {
    let seen = |track, position, duration: Option<u64>| {
        Some(Place {
            track,
            position: Duration::from_secs(position),
            duration: duration.map(Duration::from_secs),
        })
    };
    assert!(near_end(seen(2, 296, Some(300)), 3));
    assert!(near_end(seen(2, 290, Some(300)), 3));
    assert!(!near_end(seen(2, 289, Some(300)), 3), "stopped earlier");
    assert!(!near_end(seen(1, 299, Some(300)), 3), "not the last track");
    assert!(!near_end(seen(2, 299, None), 3), "length unknown");
    assert!(!near_end(None, 3));
}
