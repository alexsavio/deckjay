//! The client against the fake accounts service and Web API.

use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::spotify::auth::LOGIN_EXPIRED;
use crate::spotify::fake::{self, Fake};

const DEVICES: &str = "/v1/me/player/devices";
const NO_ACTIVE_DEVICE: &str = r#"{"error":{"status":404,"message":"Player command failed: No active device found","reason":"NO_ACTIVE_DEVICE"}}"#;

/// A signed-in client and the fake it talks to.
struct Rig {
    fake: Fake,
    dir: TempDir,
    client: Client,
}

impl Rig {
    fn new() -> Rig {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        TokenFile {
            client_id: fake::CLIENT_ID.into(),
            refresh_token: Secret::new(fake::REFRESH_TOKEN),
        }
        .save(dir.path())
        .unwrap();
        let client = Client::load(dir.path(), fake.endpoints()).unwrap();
        Rig { fake, dir, client }
    }

    fn api_error(err: &anyhow::Error) -> &ApiError {
        err.downcast_ref::<ApiError>()
            .unwrap_or_else(|| panic!("not an ApiError: {err:#}"))
    }
}

#[test]
fn the_first_call_gets_an_access_token_and_later_calls_reuse_it() {
    let mut rig = Rig::new();
    rig.client.devices().unwrap();
    rig.client.devices().unwrap();

    let tokens = rig.fake.token_requests();
    assert_eq!(tokens.len(), 1);
    assert_eq!(
        tokens[0].param("refresh_token").as_deref(),
        Some(fake::REFRESH_TOKEN)
    );
    for request in rig.fake.requests_to(DEVICES) {
        assert_eq!(request.method, "GET");
        assert_eq!(request.auth.as_deref(), Some("Bearer access-1"));
    }
}

#[test]
fn a_token_that_ends_within_a_minute_is_replaced() {
    let mut rig = Rig::new();
    rig.fake.script().expires_in = 30;
    rig.client.devices().unwrap();
    rig.client.devices().unwrap();
    assert_eq!(rig.fake.token_requests().len(), 2);
}

#[test]
fn the_access_token_from_the_sign_in_is_used_first() {
    let rig = Rig::new();
    rig.fake.script().issued = 7;
    let mut client = Client::load(rig.dir.path(), rig.fake.endpoints())
        .unwrap()
        .with_access(Secret::new("access-7"), Duration::from_secs(3600));
    client.devices().unwrap();
    assert!(rig.fake.token_requests().is_empty());
}

#[test]
fn a_rotated_refresh_token_is_saved_and_used() {
    let mut rig = Rig::new();
    {
        let mut script = rig.fake.script();
        script.rotate = true;
        script.expires_in = 30;
    }
    rig.client.devices().unwrap();
    let saved = TokenFile::load(rig.dir.path()).unwrap();
    assert_eq!(saved.refresh_token.expose(), "refresh-2");
    assert_eq!(saved.client_id, fake::CLIENT_ID);

    rig.client.devices().unwrap();
    let tokens = rig.fake.token_requests();
    assert_eq!(
        tokens[1].param("refresh_token").as_deref(),
        Some("refresh-2")
    );
    assert_eq!(
        TokenFile::load(rig.dir.path())
            .unwrap()
            .refresh_token
            .expose(),
        "refresh-3"
    );
}

#[test]
fn a_401_gets_a_new_access_token_and_retries_once() {
    let mut rig = Rig::new();
    rig.fake.can(DEVICES, 401, None, "");
    assert_eq!(rig.client.devices().unwrap().len(), 3);

    let auths: Vec<_> = rig
        .fake
        .requests_to(DEVICES)
        .into_iter()
        .map(|request| request.auth.unwrap())
        .collect();
    assert_eq!(auths, ["Bearer access-1", "Bearer access-2"]);
}

#[test]
fn a_second_401_is_an_error() {
    let mut rig = Rig::new();
    rig.fake.can(DEVICES, 401, None, "");
    rig.fake.can(DEVICES, 401, None, "");
    let err = rig.client.devices().unwrap_err();
    assert_eq!(Rig::api_error(&err).status, 401);
    assert_eq!(rig.fake.requests_to(DEVICES).len(), 2);
}

#[test]
fn a_revoked_login_names_spotify_login() {
    let mut rig = Rig::new();
    rig.fake.script().revoked = true;
    let err = rig.client.devices().unwrap_err();
    assert_eq!(err.to_string(), LOGIN_EXPIRED);
    assert_eq!(
        Rig::api_error(&err).reason.as_deref(),
        Some("invalid_grant")
    );
    assert!(rig.fake.requests_to(DEVICES).is_empty());
}

#[test]
fn a_short_retry_after_is_waited_out() {
    let mut rig = Rig::new();
    rig.fake.can(DEVICES, 429, Some(1), "");
    let started = Instant::now();
    rig.client.devices().unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(rig.fake.requests_to(DEVICES).len(), 2);
}

#[test]
fn a_long_retry_after_fails_at_once_and_holds_back_later_calls() {
    let mut rig = Rig::new();
    rig.fake.can(DEVICES, 429, Some(60), "");
    let started = Instant::now();
    let err = rig.client.devices().unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    let api = Rig::api_error(&err);
    assert_eq!(api.status, 429);
    assert_eq!(api.retry_after, Some(Duration::from_secs(60)));
    assert!(err.to_string().contains("try again in 60 s"), "{err}");

    let err = rig.client.pause("dev-den").unwrap_err();
    let wait = Rig::api_error(&err).retry_after.unwrap();
    assert!(wait > Duration::from_secs(58) && wait <= Duration::from_secs(60));
    assert_eq!(rig.fake.requests_to(DEVICES).len(), 1);
    assert!(rig.fake.requests_to("/v1/me/player/pause").is_empty());
}

#[test]
fn a_second_429_is_not_waited_out() {
    let mut rig = Rig::new();
    rig.fake.can(DEVICES, 429, Some(1), "");
    rig.fake.can(DEVICES, 429, Some(1), "");
    let err = rig.client.devices().unwrap_err();
    assert_eq!(Rig::api_error(&err).status, 429);
    assert_eq!(rig.fake.requests_to(DEVICES).len(), 2);
}

#[test]
fn a_server_error_on_a_read_is_retried_once() {
    let mut rig = Rig::new();
    rig.fake.can(DEVICES, 503, None, "upstream down");
    rig.client.devices().unwrap();
    assert_eq!(rig.fake.requests_to(DEVICES).len(), 2);
}

#[test]
fn a_server_error_on_a_command_is_not_retried() {
    let mut rig = Rig::new();
    rig.fake
        .can("/v1/me/player/next", 502, None, "<html>Bad Gateway</html>");
    let err = rig.client.next("dev-den").unwrap_err();
    let api = Rig::api_error(&err);
    assert_eq!(api.status, 502);
    assert_eq!(api.reason, None);
    assert_eq!(api.message, "<html>Bad Gateway</html>");
    assert_eq!(rig.fake.requests_to("/v1/me/player/next").len(), 1);
}

#[test]
fn a_web_api_error_carries_its_reason() {
    let mut rig = Rig::new();
    rig.fake
        .can("/v1/me/player/play", 404, None, NO_ACTIVE_DEVICE);
    let err = rig.client.play("dev-den", None).unwrap_err();
    assert_eq!(
        *Rig::api_error(&err),
        ApiError {
            status: 404,
            reason: Some("NO_ACTIVE_DEVICE".into()),
            message: "Player command failed: No active device found".into(),
            retry_after: None,
        }
    );
    assert_eq!(
        err.to_string(),
        "Spotify answered 404 NO_ACTIVE_DEVICE: Player command failed: No active device found"
    );
}

#[test]
fn an_oauth_error_body_is_read_too() {
    let api = ApiError::parse(
        400,
        r#"{"error":"invalid_client","error_description":"Invalid client"}"#,
        None,
    );
    assert_eq!(api.reason.as_deref(), Some("invalid_client"));
    assert_eq!(api.message, "Invalid client");
}

#[test]
fn devices_are_read_with_their_details() {
    let mut rig = Rig::new();
    let devices = rig.client.devices().unwrap();
    let den = &devices[0];
    assert_eq!(den.id.as_deref(), Some("dev-den"));
    assert_eq!(den.name, "Den");
    assert_eq!(den.kind, "AVR");
    assert!(!den.is_active && !den.is_restricted && den.supports_volume);
    assert_eq!(den.volume_percent, Some(30));

    let kitchen = &devices[1];
    assert!(kitchen.is_active && !kitchen.supports_volume);
    assert_eq!(kitchen.volume_percent, None);

    let tv = &devices[2];
    assert_eq!(tv.id, None);
    assert!(tv.is_restricted);
}

#[test]
fn nothing_playing_is_none() {
    let mut rig = Rig::new();
    assert_eq!(rig.client.player().unwrap(), None);
}

#[test]
fn the_playback_state_is_read() {
    let mut rig = Rig::new();
    rig.fake.script().player = Some(json!({
        "device": {"id": "dev-den", "name": "Den", "type": "AVR"},
        "is_playing": true,
        "progress_ms": 12_345,
        "context": {"type": "playlist", "uri": "spotify:playlist:37i9dQZF1DX8Uebhn9wzrS"},
        "item": {"name": "Baby Shark", "type": "track"},
        "shuffle_state": false
    }));
    assert_eq!(
        rig.client.player().unwrap(),
        Some(PlaybackState {
            device_id: Some("dev-den".into()),
            is_playing: true,
            context_uri: Some("spotify:playlist:37i9dQZF1DX8Uebhn9wzrS".into()),
            progress_ms: Some(12_345),
            item_name: Some("Baby Shark".into()),
        })
    );
}

#[test]
fn a_playback_state_without_context_or_item_is_read() {
    let mut rig = Rig::new();
    rig.fake.script().player = Some(json!({
        "device": {"id": null, "name": "Old TV"},
        "is_playing": false,
        "progress_ms": null,
        "context": null,
        "item": null
    }));
    let state = rig.client.player().unwrap().unwrap();
    assert_eq!(state.device_id, None);
    assert!(!state.is_playing);
    assert_eq!(state.context_uri, None);
    assert_eq!(state.progress_ms, None);
    assert_eq!(state.item_name, None);
}

#[test]
fn player_commands_send_what_the_web_api_expects() {
    let mut rig = Rig::new();
    rig.client.transfer("dev-den", false).unwrap();
    rig.client.set_volume("dev-den", 140).unwrap();
    rig.client
        .play("dev-den", Some("spotify:playlist:abc"))
        .unwrap();
    rig.client.play("dev-den", None).unwrap();
    rig.client.pause("dev-den").unwrap();
    rig.client.next("dev-den").unwrap();
    rig.client.previous("dev-den").unwrap();

    let sent: Vec<_> = rig
        .fake
        .script()
        .requests
        .iter()
        .filter(|request| request.path != "/api/token")
        .map(|request| {
            (
                format!("{} {}?{}", request.method, request.path, request.query),
                request.body.clone(),
            )
        })
        .collect();
    let with_device = "device_id=dev-den";
    assert_eq!(sent[0].0, "PUT /v1/me/player?");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sent[0].1).unwrap(),
        json!({"device_ids": ["dev-den"], "play": false})
    );
    assert_eq!(
        sent[1],
        (
            format!("PUT /v1/me/player/volume?volume_percent=100&{with_device}"),
            String::new()
        )
    );
    assert_eq!(sent[2].0, format!("PUT /v1/me/player/play?{with_device}"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sent[2].1).unwrap(),
        json!({"context_uri": "spotify:playlist:abc", "offset": {"position": 0}, "position_ms": 0})
    );
    assert_eq!(
        sent[3..],
        [
            (
                format!("PUT /v1/me/player/play?{with_device}"),
                String::new()
            ),
            (
                format!("PUT /v1/me/player/pause?{with_device}"),
                String::new()
            ),
            (
                format!("POST /v1/me/player/next?{with_device}"),
                String::new()
            ),
            (
                format!("POST /v1/me/player/previous?{with_device}"),
                String::new()
            ),
        ]
    );
    let play = &rig.fake.requests_to("/v1/me/player/play")[0];
    assert_eq!(play.json()["context_uri"], "spotify:playlist:abc");
    assert_eq!(play.param("device_id").as_deref(), Some("dev-den"));
    // Spotify answers 411 to a PUT or POST without a length.
    for path in ["/v1/me/player/pause", "/v1/me/player/next"] {
        let sent = &rig.fake.requests_to(path)[0];
        assert_eq!(sent.content_length.as_deref(), Some("0"), "{path}");
    }
}

#[test]
fn me_is_the_display_name_or_else_the_user_id() {
    let mut rig = Rig::new();
    assert_eq!(rig.client.me().unwrap(), "Parent");
    rig.fake.script().display_name = json!(null);
    assert_eq!(rig.client.me().unwrap(), "parent-1");
}

#[test]
fn playlist_images_are_read() {
    let mut rig = Rig::new();
    let images = rig
        .client
        .playlist_images("37i9dQZF1DX8Uebhn9wzrS")
        .unwrap();
    assert_eq!(
        images,
        [
            Image {
                url: "https://i.scdn.co/image/big".into(),
                width: Some(640),
                height: Some(640),
            },
            Image {
                url: "https://i.scdn.co/image/any".into(),
                width: None,
                height: None,
            },
        ]
    );
    assert_eq!(
        rig.fake
            .requests_to("/v1/playlists/37i9dQZF1DX8Uebhn9wzrS/images")
            .len(),
        1
    );
}

#[test]
fn debug_output_never_shows_a_token() {
    let mut rig = Rig::new();
    rig.client.devices().unwrap();
    let shown = format!("{:?}", rig.client);
    assert!(!shown.contains("access-1"), "{shown}");
    assert!(!shown.contains(fake::REFRESH_TOKEN), "{shown}");
    assert!(shown.contains("has_access_token: true"), "{shown}");
}

#[test]
fn the_real_endpoints_use_https() {
    let endpoints = Endpoints::default();
    assert_eq!(endpoints.api, "https://api.spotify.com/v1");
    assert_eq!(endpoints.accounts, "https://accounts.spotify.com");
}
