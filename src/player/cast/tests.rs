use rust_cast::channels::media::{
    ExtendedPlayerState, ExtendedStatus, IdleReason, Media, PlayerState, StatusEntry, StreamType,
};

use super::*;

const ALBUM: usize = 5;

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
    poll_event(entry, &tracks(), ALBUM)
}

#[test]
fn our_track_playing_or_buffering_is_playing() {
    for state in [PlayerState::Playing, PlayerState::Buffering] {
        let e = entry(state, Some(&url(1)));
        assert_eq!(poll(Some(&e)), Some(PlayerEvent::Playing(ALBUM)));
    }
}

#[test]
fn our_track_paused_is_paused() {
    let e = entry(PlayerState::Paused, Some(&url(0)));
    assert_eq!(poll(Some(&e)), Some(PlayerEvent::Paused(ALBUM)));
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
