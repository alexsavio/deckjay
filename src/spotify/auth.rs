//! OAuth 2 Authorization Code with PKCE (RFC 7636) against Spotify's accounts
//! service. PKCE needs no client secret, so nothing secret ships with the program.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use aws_lc_rs::{digest, rand};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::Deserialize;
use ureq::Agent;

use super::api::{ApiError, Reply};
use super::token::Secret;

pub const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
pub const LOGIN_EXPIRED: &str = "the Spotify login expired: run `kids-deck spotify-login`";
pub const SCOPES: &str = "user-read-playback-state user-modify-playback-state \
                          playlist-read-private playlist-read-collaborative";

/// Everything but the RFC 3986 unreserved characters is encoded.
const QUERY_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub struct Pkce {
    pub verifier: Secret,
    pub challenge: String,
}

impl Pkce {
    /// 64 random bytes give an 86-character verifier; RFC 7636 allows 43 to 128.
    pub fn new() -> Result<Pkce> {
        let verifier = random_base64url::<64>()?;
        Ok(Pkce {
            challenge: challenge(&verifier),
            verifier: Secret::new(verifier),
        })
    }
}

/// `S256`: base64url (no padding) of the verifier's SHA-256.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// Ties the browser's answer to this sign-in, so a forged redirect is refused.
pub fn new_state() -> Result<String> {
    random_base64url::<16>()
}

fn random_base64url<const N: usize>() -> Result<String> {
    let mut bytes = [0; N];
    rand::fill(&mut bytes).map_err(|_| anyhow!("the system gave no random numbers"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The page where the account owner allows kids-deck to control playback.
pub fn authorize_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let query: Vec<String> = [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge),
        ("state", state),
        ("scope", SCOPES),
    ]
    .iter()
    .map(|(key, value)| format!("{key}={}", utf8_percent_encode(value, QUERY_VALUE)))
    .collect();
    format!("{AUTHORIZE_URL}?{}", query.join("&"))
}

/// The sign-in code from the query of the redirect back to kids-deck.
pub fn parse_callback(query: &str, expected_state: &str) -> Result<Secret> {
    let param = |name: &str| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| decode(value))
        })
    };
    if param("state").as_deref() != Some(expected_state) {
        bail!(
            "this answer belongs to another sign-in (its state does not match): \
             run `kids-deck spotify-login` again"
        );
    }
    match param("error").as_deref() {
        Some("access_denied") => {
            bail!("kids-deck was not allowed to use the Spotify account (access_denied)")
        }
        Some(error) => bail!("the Spotify sign-in failed: {error}"),
        None => {}
    }
    match param("code") {
        Some(code) if !code.is_empty() => Ok(Secret::new(code)),
        _ => bail!("the answer from Spotify has no sign-in code"),
    }
}

/// Form encoding: `+` is a space.
fn decode(value: &str) -> String {
    percent_decode_str(&value.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

pub struct Tokens {
    pub access: Secret,
    pub expires_in: Duration,
    /// Spotify may send a new refresh token; `None` means the old one stays valid.
    pub refresh: Option<Secret>,
}

/// Trades the sign-in code for tokens. `redirect_uri` must be the one in the
/// authorize URL.
pub fn exchange_code(
    agent: &Agent,
    accounts: &str,
    client_id: &str,
    code: &Secret,
    redirect_uri: &str,
    verifier: &Secret,
) -> Result<Tokens> {
    request_tokens(
        agent,
        accounts,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.expose()),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier.expose()),
        ],
    )
}

/// A revoked or expired refresh token gives an error that names
/// `kids-deck spotify-login`, with the [`ApiError`] (`invalid_grant`) beneath it.
pub fn refresh(
    agent: &Agent,
    accounts: &str,
    client_id: &str,
    refresh_token: &Secret,
) -> Result<Tokens> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.expose()),
        ("client_id", client_id),
    ];
    request_tokens(agent, accounts, &form).map_err(|err| {
        let expired = err
            .downcast_ref::<ApiError>()
            .is_some_and(|api| api.reason.as_deref() == Some("invalid_grant"));
        if expired {
            err.context(LOGIN_EXPIRED)
        } else {
            err
        }
    })
}

fn request_tokens(agent: &Agent, accounts: &str, form: &[(&str, &str)]) -> Result<Tokens> {
    #[derive(Deserialize)]
    struct Answer {
        access_token: Secret,
        expires_in: u64,
        refresh_token: Option<Secret>,
    }

    let reply = agent
        .post(format!("{accounts}/api/token"))
        .send_form(form.iter().copied())
        .context("cannot reach the Spotify accounts service")?;
    let reply = Reply::read(reply)?;
    if !reply.is_success() {
        return Err(reply.error().into());
    }
    let answer: Answer = reply.json()?;
    Ok(Tokens {
        access: answer.access_token,
        expires_in: Duration::from_secs(answer.expires_in),
        refresh: answer.refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::fake::{self, Fake};

    const UNRESERVED: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

    #[test]
    fn challenge_matches_rfc_7636_appendix_b() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_long_random_and_unreserved() {
        let first = Pkce::new().unwrap();
        let second = Pkce::new().unwrap();
        let verifier = first.verifier.expose();
        assert_eq!(verifier.len(), 86);
        assert!(
            verifier.chars().all(|c| UNRESERVED.contains(c)),
            "{verifier}"
        );
        assert_ne!(verifier, second.verifier.expose());
        assert_eq!(first.challenge, challenge(verifier));
        assert_eq!(first.challenge.len(), 43);
    }

    #[test]
    fn a_state_is_16_random_bytes() {
        let state = new_state().unwrap();
        assert_eq!(state.len(), 22);
        assert!(state.chars().all(|c| UNRESERVED.contains(c)), "{state}");
        assert_ne!(state, new_state().unwrap());
    }

    #[test]
    fn the_authorize_url_encodes_every_value() {
        let url = authorize_url(
            "client-1",
            "http://127.0.0.1:8898/callback",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "st_ate~1",
        );
        assert_eq!(
            url,
            "https://accounts.spotify.com/authorize?client_id=client-1&response_type=code\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A8898%2Fcallback\
             &code_challenge_method=S256\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &state=st_ate~1\
             &scope=user-read-playback-state%20user-modify-playback-state\
             %20playlist-read-private%20playlist-read-collaborative"
        );
    }

    #[test]
    fn a_callback_gives_its_code() {
        let code = parse_callback("code=AQD%2Fx+y&state=s1", "s1").unwrap();
        assert_eq!(code.expose(), "AQD/x y");
    }

    #[test]
    fn a_callback_with_another_state_is_refused() {
        for query in ["code=c&state=s2", "code=c", "code=c&state="] {
            let err = parse_callback(query, "s1").unwrap_err().to_string();
            assert!(err.contains("state does not match"), "{query}: {err}");
        }
    }

    #[test]
    fn a_denied_sign_in_says_so() {
        let err = parse_callback("error=access_denied&state=s1", "s1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not allowed"), "{err}");
        assert!(err.contains("access_denied"), "{err}");
    }

    #[test]
    fn other_errors_are_named() {
        let err = parse_callback("state=s1&error=server_error", "s1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("server_error"), "{err}");
    }

    #[test]
    fn a_callback_without_a_code_is_refused() {
        for query in ["state=s1", "state=s1&code="] {
            let err = parse_callback(query, "s1").unwrap_err().to_string();
            assert!(err.contains("no sign-in code"), "{query}: {err}");
        }
    }

    fn agent() -> Agent {
        crate::net::api_agent(Duration::from_secs(5))
    }

    #[test]
    fn a_code_is_exchanged_with_its_verifier() {
        let fake = Fake::start();
        let tokens = exchange_code(
            &agent(),
            &fake.url,
            fake::CLIENT_ID,
            &Secret::new(fake::CODE),
            "http://127.0.0.1:8898/callback",
            &Secret::new("the-verifier"),
        )
        .unwrap();
        assert_eq!(tokens.access.expose(), "access-1");
        assert_eq!(tokens.expires_in, Duration::from_secs(3600));
        assert_eq!(tokens.refresh.unwrap().expose(), fake::REFRESH_TOKEN);

        let sent = fake.token_requests().pop().unwrap();
        assert_eq!(sent.method, "POST");
        for (name, value) in [
            ("grant_type", "authorization_code"),
            ("code", fake::CODE),
            ("redirect_uri", "http://127.0.0.1:8898/callback"),
            ("client_id", fake::CLIENT_ID),
            ("code_verifier", "the-verifier"),
        ] {
            assert_eq!(sent.param(name).as_deref(), Some(value), "{name}");
        }
    }

    #[test]
    fn a_wrong_code_is_an_api_error() {
        let fake = Fake::start();
        let err = exchange_code(
            &agent(),
            &fake.url,
            fake::CLIENT_ID,
            &Secret::new("stale"),
            "http://127.0.0.1:8898/callback",
            &Secret::new("the-verifier"),
        )
        .err()
        .unwrap();
        let api = err.downcast_ref::<ApiError>().unwrap();
        assert_eq!(api.status, 400);
        assert_eq!(api.reason.as_deref(), Some("invalid_grant"));
        assert_eq!(api.message, "Invalid authorization code");
    }

    #[test]
    fn a_refresh_without_a_new_refresh_token_keeps_the_old_one() {
        let fake = Fake::start();
        let tokens = refresh(
            &agent(),
            &fake.url,
            fake::CLIENT_ID,
            &Secret::new(fake::REFRESH_TOKEN),
        )
        .unwrap();
        assert_eq!(tokens.access.expose(), "access-1");
        assert!(tokens.refresh.is_none());

        let sent = fake.token_requests().pop().unwrap();
        assert_eq!(sent.param("grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(
            sent.param("refresh_token").as_deref(),
            Some(fake::REFRESH_TOKEN)
        );
        assert_eq!(sent.param("client_id").as_deref(), Some(fake::CLIENT_ID));
    }

    #[test]
    fn a_refresh_may_rotate_the_refresh_token() {
        let fake = Fake::start();
        fake.script().rotate = true;
        let tokens = refresh(
            &agent(),
            &fake.url,
            fake::CLIENT_ID,
            &Secret::new(fake::REFRESH_TOKEN),
        )
        .unwrap();
        assert_eq!(tokens.refresh.unwrap().expose(), "refresh-2");
    }

    #[test]
    fn a_revoked_refresh_token_asks_for_spotify_login() {
        let fake = Fake::start();
        fake.script().revoked = true;
        let err = refresh(
            &agent(),
            &fake.url,
            fake::CLIENT_ID,
            &Secret::new(fake::REFRESH_TOKEN),
        )
        .err()
        .unwrap();
        assert_eq!(err.to_string(), LOGIN_EXPIRED);
        let api = err.downcast_ref::<ApiError>().unwrap();
        assert_eq!(api.reason.as_deref(), Some("invalid_grant"));
    }

    #[test]
    fn other_token_errors_are_not_a_login_expiry() {
        let fake = Fake::start();
        let err = refresh(&agent(), &fake.url, "other-app", &Secret::new("x"))
            .err()
            .unwrap();
        assert!(!err.to_string().contains("spotify-login"), "{err}");
        assert_eq!(
            err.downcast_ref::<ApiError>().unwrap().reason.as_deref(),
            Some("invalid_client")
        );
    }
}
