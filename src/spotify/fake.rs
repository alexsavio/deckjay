//! A fake Spotify accounts service and Web API on 127.0.0.1 that answers with
//! canned JSON and records every request.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::Response;
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};

use super::api::Endpoints;

pub(crate) const CLIENT_ID: &str = "client-1";
pub(crate) const CODE: &str = "the-code";
pub(crate) const REFRESH_TOKEN: &str = "refresh-1";

/// One request as the fake saw it.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: String,
    pub auth: Option<String>,
    pub content_length: Option<String>,
}

impl Request {
    /// A form or query parameter, decoded.
    pub fn param(&self, name: &str) -> Option<String> {
        let source = if self.path == "/api/token" {
            &self.body
        } else {
            &self.query
        };
        form(source).remove(name)
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

/// A canned answer that replaces the normal one.
pub(crate) struct Canned {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: String,
}

/// What the fake answers and what it was sent.
pub(crate) struct Script {
    pub requests: Vec<Request>,
    /// The only refresh token the accounts service accepts.
    pub refresh_token: String,
    /// Each refresh answers with a new refresh token.
    pub rotate: bool,
    /// Each refresh fails with `invalid_grant`.
    pub revoked: bool,
    pub expires_in: u64,
    /// Access tokens are `access-1`, `access-2`, …; only the newest is accepted.
    pub issued: u32,
    rotations: u32,
    pub display_name: Value,
    pub devices: Value,
    /// `None` answers `GET /me/player` with 204.
    pub player: Option<Value>,
    pub images: Value,
    /// Used, in order, for requests to their path before the normal answer.
    pub canned: Vec<(String, Canned)>,
}

impl Default for Script {
    fn default() -> Script {
        Script {
            requests: Vec::new(),
            refresh_token: REFRESH_TOKEN.into(),
            rotate: false,
            revoked: false,
            expires_in: 3600,
            issued: 0,
            rotations: 0,
            display_name: json!("Parent"),
            devices: json!([
                {"id": "dev-den", "is_active": false, "is_private_session": false,
                 "is_restricted": false, "name": "Den", "supports_volume": true,
                 "type": "AVR", "volume_percent": 30},
                {"id": "dev-kitchen", "is_active": true, "is_private_session": false,
                 "is_restricted": false, "name": "Kitchen Speaker", "supports_volume": false,
                 "type": "Speaker", "volume_percent": null},
                {"id": null, "is_active": false, "is_private_session": false,
                 "is_restricted": true, "name": "Old TV", "supports_volume": false,
                 "type": "TV", "volume_percent": null}
            ]),
            player: None,
            images: json!([
                {"url": "https://i.scdn.co/image/big", "width": 640, "height": 640},
                {"url": "https://i.scdn.co/image/any", "width": null, "height": null}
            ]),
            canned: Vec::new(),
        }
    }
}

pub(crate) struct Fake {
    pub url: String,
    script: Arc<Mutex<Script>>,
}

impl Fake {
    pub fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let script = Arc::new(Mutex::new(Script::default()));
        let app = Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&script));
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        Fake { url, script }
    }

    pub fn endpoints(&self) -> Endpoints {
        Endpoints {
            api: format!("{}/v1", self.url),
            accounts: self.url.clone(),
        }
    }

    pub fn script(&self) -> MutexGuard<'_, Script> {
        self.script.lock().unwrap()
    }

    pub fn requests_to(&self, path: &str) -> Vec<Request> {
        self.script()
            .requests
            .iter()
            .filter(|request| request.path == path)
            .cloned()
            .collect()
    }

    pub fn token_requests(&self) -> Vec<Request> {
        self.requests_to("/api/token")
    }

    pub fn can(&self, path: &str, status: u16, retry_after: Option<u64>, body: &str) {
        self.script().canned.push((
            path.into(),
            Canned {
                status,
                retry_after,
                body: body.into(),
            },
        ));
    }
}

async fn handle(
    State(script): State<Arc<Mutex<Script>>>,
    request: axum::extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    let request = Request {
        method: parts.method.to_string(),
        path: parts.uri.path().into(),
        query: parts.uri.query().unwrap_or("").into(),
        body: String::from_utf8(body.to_vec()).unwrap(),
        auth: parts
            .headers
            .get(header::AUTHORIZATION)
            .map(|value| value.to_str().unwrap().into()),
        content_length: parts
            .headers
            .get(header::CONTENT_LENGTH)
            .map(|value| value.to_str().unwrap().into()),
    };
    let canned = script.lock().unwrap().answer(&request);
    let mut reply = Response::builder()
        .status(canned.status)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(seconds) = canned.retry_after {
        reply = reply.header(header::RETRY_AFTER, seconds);
    }
    reply.body(Body::from(canned.body)).unwrap()
}

impl Script {
    fn answer(&mut self, request: &Request) -> Canned {
        self.requests.push(request.clone());
        if let Some(index) = self
            .canned
            .iter()
            .position(|(path, _)| *path == request.path)
        {
            return self.canned.remove(index).1;
        }
        if request.path == "/api/token" {
            return self.token(&form(&request.body));
        }
        let fresh = format!("Bearer access-{}", self.issued);
        if self.issued == 0 || request.auth.as_deref() != Some(&fresh) {
            return reply(
                401,
                &json!({"error": {"status": 401, "message": "Invalid access token"}}),
            );
        }
        self.api(&request.method, &request.path)
    }

    fn token(&mut self, form: &HashMap<String, String>) -> Canned {
        let field = |name: &str| form.get(name).map_or("", String::as_str);
        if field("client_id") != CLIENT_ID {
            return oauth_error("invalid_client", "Invalid client");
        }
        match field("grant_type") {
            "authorization_code" if field("code") != CODE => {
                oauth_error("invalid_grant", "Invalid authorization code")
            }
            "authorization_code" if field("code_verifier").is_empty() => {
                oauth_error("invalid_request", "code_verifier required")
            }
            "authorization_code" => {
                let refresh = self.refresh_token.clone();
                self.issue(Some(refresh))
            }
            "refresh_token" if self.revoked || field("refresh_token") != self.refresh_token => {
                oauth_error("invalid_grant", "Refresh token revoked")
            }
            "refresh_token" if self.rotate => {
                self.rotations += 1;
                self.refresh_token = format!("refresh-{}", self.rotations + 1);
                let refresh = self.refresh_token.clone();
                self.issue(Some(refresh))
            }
            "refresh_token" => self.issue(None),
            _ => oauth_error("unsupported_grant_type", "grant_type not supported"),
        }
    }

    fn issue(&mut self, refresh_token: Option<String>) -> Canned {
        self.issued += 1;
        let mut body = json!({
            "access_token": format!("access-{}", self.issued),
            "token_type": "Bearer",
            "expires_in": self.expires_in,
            "scope": "user-read-playback-state user-modify-playback-state",
        });
        if let Some(refresh_token) = refresh_token {
            body["refresh_token"] = json!(refresh_token);
        }
        reply(200, &body)
    }

    fn api(&self, method: &str, path: &str) -> Canned {
        match (method, path) {
            ("GET", "/v1/me") => reply(
                200,
                &json!({"display_name": self.display_name, "id": "parent-1"}),
            ),
            ("GET", "/v1/me/player/devices") => reply(200, &json!({"devices": self.devices})),
            ("GET", "/v1/me/player") => match &self.player {
                Some(player) => reply(200, player),
                None => empty(204),
            },
            (
                "PUT",
                "/v1/me/player"
                | "/v1/me/player/play"
                | "/v1/me/player/pause"
                | "/v1/me/player/volume",
            )
            | ("POST", "/v1/me/player/next" | "/v1/me/player/previous") => empty(204),
            ("GET", images)
                if images.starts_with("/v1/playlists/") && images.ends_with("/images") =>
            {
                reply(200, &self.images)
            }
            _ => reply(
                404,
                &json!({"error": {"status": 404, "message": "Service not found"}}),
            ),
        }
    }
}

fn reply(status: u16, body: &Value) -> Canned {
    Canned {
        status,
        retry_after: None,
        body: body.to_string(),
    }
}

fn empty(status: u16) -> Canned {
    Canned {
        status,
        retry_after: None,
        body: String::new(),
    }
}

fn oauth_error(error: &str, description: &str) -> Canned {
    reply(
        400,
        &json!({"error": error, "error_description": description}),
    )
}

pub(crate) fn form(text: &str) -> HashMap<String, String> {
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| {
                percent_decode_str(&s.replace('+', " "))
                    .decode_utf8()
                    .unwrap()
                    .into_owned()
            };
            (decode(key), decode(value))
        })
        .collect()
}
