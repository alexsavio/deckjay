//! Outgoing HTTP(S) for podcast feeds, internet radio and Spotify.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used once radio, podcasts and Spotify are wired in"
    )
)]

use std::sync::Arc;
use std::time::Duration;

use ureq::Agent;
use ureq::tls::{TlsConfig, TlsProvider};

/// Sent with every request, so servers can tell who fetches their feeds.
pub const USER_AGENT: &str = concat!("kids-deck/", env!("CARGO_PKG_VERSION"));

/// `rust_cast` builds its TLS config from the process-wide default provider.
/// Installing aws-lc-rs up front keeps that choice explicit.
pub fn install_crypto() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn tls() -> TlsConfig {
    TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .build()
}

/// For API calls and feeds: the whole request must finish within `timeout`.
/// Non-2xx replies are returned, not turned into errors.
pub fn api_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .tls_config(tls())
        .user_agent(USER_AGENT)
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// For long downloads and live streams: only connecting and the reply headers
/// have a deadline, so a long body is never cut off.
pub fn stream_agent() -> Agent {
    Agent::config_builder()
        .tls_config(tls())
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustls_has_exactly_one_crypto_engine() {
        // Panics when both aws-lc-rs and ring are compiled in: that is what
        // rust_cast would hit on its first Cast command.
        let _ = rustls::ClientConfig::builder();
    }

    #[test]
    #[ignore = "needs the internet: cargo test -- --ignored"]
    fn fetches_an_https_page() {
        install_crypto();
        let reply = api_agent(Duration::from_secs(10))
            .get("https://example.com/")
            .call()
            .unwrap();
        assert_eq!(reply.status(), 200);
    }

    #[test]
    fn agents_build_with_tls() {
        install_crypto();
        let _ = api_agent(Duration::from_secs(5));
        let _ = stream_agent();
    }
}
