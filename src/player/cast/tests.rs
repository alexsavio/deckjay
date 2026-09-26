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
    poll_event(entry, &tracks(), None, ITEM)
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

const LIVE: &str = "http://radio.example/kids.mp3?listener=1";

fn live_poll(entry: Option<&StatusEntry>) -> Option<PlayerEvent> {
    poll_event(entry, &[], Some(LIVE), ITEM)
}

fn on_air(state: PlayerState, content_id: &str) -> StatusEntry {
    let mut e = entry(state, Some(content_id));
    e.media.as_mut().unwrap().stream_type = StreamType::Live;
    e
}

#[test]
fn our_station_playing_buffering_or_paused_is_ours() {
    for state in [PlayerState::Playing, PlayerState::Buffering] {
        let e = on_air(state, LIVE);
        assert_eq!(live_poll(Some(&e)), Some(PlayerEvent::Playing(ITEM)));
    }
    let paused = on_air(PlayerState::Paused, LIVE);
    assert_eq!(live_poll(Some(&paused)), Some(PlayerEvent::Paused(ITEM)));
}

#[test]
fn another_stream_an_idle_receiver_or_an_album_is_not_our_station() {
    let other = on_air(PlayerState::Playing, "http://radio.example/other.mp3");
    assert_eq!(live_poll(Some(&other)), Some(PlayerEvent::Stopped));
    let mut ended = on_air(PlayerState::Idle, LIVE);
    ended.idle_reason = Some(IdleReason::Error);
    assert_eq!(live_poll(Some(&ended)), Some(PlayerEvent::Stopped));
    assert_eq!(live_poll(None), Some(PlayerEvent::Stopped));
    let album = entry(PlayerState::Playing, Some(&url(0)));
    assert_eq!(live_poll(Some(&album)), Some(PlayerEvent::Stopped));
}

#[test]
fn a_station_has_no_place_no_end_and_no_next_track() {
    let mut e = on_air(PlayerState::Playing, LIVE);
    e.current_time = Some(1234.0);
    assert_eq!(place(&e, &[]), None);
    assert_eq!(skip_target(&e, &[], true), None);
    assert_eq!(skip_target(&e, &[], false), None);
    let mut idle = on_air(PlayerState::Idle, LIVE);
    idle.idle_reason = Some(IdleReason::Finished);
    assert!(!finished(&idle, &[]));
}

fn station(content_type: Option<&str>, cover_url: Option<&str>) -> Station {
    Station {
        url: "http://radio.example/kids.pls".into(),
        content_type: content_type.map(Into::into),
        name: "Kids Radio".into(),
        cover_url: cover_url.map(Into::into),
    }
}

fn resolved(content_type: Option<&str>) -> crate::radio::Stream {
    crate::radio::Stream {
        url: LIVE.into(),
        content_type: content_type.map(Into::into),
    }
}

#[test]
fn a_station_loads_as_one_live_item_named_after_it() {
    let cover = "http://10.0.0.2:8765/covers/kids.png";
    let media = Live::new(&station(None, Some(cover)), resolved(Some("audio/aac"))).to_media();
    assert_eq!(media.content_id, LIVE);
    assert_eq!(media.stream_type, StreamType::Live);
    assert_eq!(media.content_type, "audio/aac");
    assert_eq!(media.duration, None);
    let Some(Metadata::Generic(metadata)) = media.metadata else {
        panic!("{:?}", media.metadata);
    };
    assert_eq!(metadata.title.as_deref(), Some("Kids Radio"));
    assert_eq!(metadata.images, [Image::new(cover.into())]);
}

#[test]
fn a_station_type_comes_from_the_stream_then_the_station_then_mp3() {
    let content_type = |station_type, stream_type| {
        Live::new(&station(station_type, None), resolved(stream_type))
            .to_media()
            .content_type
    };
    let hls = crate::radio::HLS;
    assert_eq!(content_type(Some("audio/ogg"), Some(hls)), hls);
    assert_eq!(content_type(Some("audio/ogg"), None), "audio/ogg");
    assert_eq!(content_type(None, None), "audio/mpeg");
}

#[test]
fn stop_ends_only_a_session_of_ours() {
    let tracks = tracks();
    let album = entry(PlayerState::Paused, Some(&url(2)));
    assert!(ours(&album, &tracks, None));
    let station = on_air(PlayerState::Playing, LIVE);
    assert!(ours(&station, &[], Some(LIVE)));
    let other = on_air(PlayerState::Playing, "http://radio.example/other.mp3");
    assert!(!ours(&other, &tracks, Some(LIVE)));
    assert!(!ours(&entry(PlayerState::Idle, None), &tracks, Some(LIVE)));
}

#[test]
fn stop_without_an_item_of_ours_does_not_connect() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut player = CastPlayer::new("127.0.0.1".into(), port);
    let (tx, _events) = std::sync::mpsc::channel();
    player.halt(&mut Emitter::new(tx));
    listener.set_nonblocking(true).unwrap();
    assert!(listener.accept().is_err(), "nobody connected");
}

#[test]
fn stop_with_the_receiver_gone_only_forgets_the_item() {
    // Nothing listens on this port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut player = CastPlayer::new("127.0.0.1".into(), port);
    player.active = true;
    let (tx, events) = std::sync::mpsc::channel();
    player.halt(&mut Emitter::new(tx));
    assert_eq!(player.poll_interval(), None);
    assert_eq!(events.try_iter().count(), 0);
}

fn app(app_id: &str, name: &str) -> Application {
    Application {
        app_id: app_id.into(),
        session_id: format!("session-{app_id}"),
        transport_id: format!("transport-{app_id}"),
        namespaces: Vec::new(),
        display_name: name.into(),
        status_text: String::new(),
    }
}

#[test]
fn the_power_key_quits_every_app_but_the_idle_screen() {
    let apps = [
        app("E8C28D3C", "Backdrop"),
        app("CC1AD845", "Default Media Receiver"),
        app("CC32E753", "Spotify"),
    ];
    let quit: Vec<&str> = apps_to_quit(&apps)
        .iter()
        .map(|a| a.display_name.as_str())
        .collect();
    assert_eq!(quit, ["Default Media Receiver", "Spotify"]);
}
