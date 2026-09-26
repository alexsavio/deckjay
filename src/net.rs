//! Outgoing HTTP(S) for podcast feeds, internet radio and Spotify, and the
//! address the speaker downloads the music from ([`BaseUrl`]).

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};
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

/// Where the speaker downloads the music from: `http://<host>:<port>/music`.
pub enum BaseUrl {
    /// A whole URL: from `advertise_host`, or for local audio, which
    /// downloads nothing.
    Fixed(String),
    /// The host is this machine's address on the route to the speaker,
    /// looked up at each use: a new DHCP address reaches the next album, not
    /// the next restart.
    Towards(Route),
}

/// The route to the speaker. No packets are sent: connecting a UDP socket
/// only picks the route.
pub struct Route {
    speaker: String,
    port: u16,
    http_port: u16,
    /// The speaker's addresses, resolved once: a name lookup can block.
    addrs: Vec<SocketAddr>,
    /// The last URL found, for while the route cannot be found.
    last: Option<String>,
    /// The route could not be found last time; warned once.
    failing: bool,
}

impl BaseUrl {
    pub fn fixed(url: impl Into<String>) -> BaseUrl {
        BaseUrl::Fixed(url.into())
    }

    pub fn towards(speaker: &str, port: u16, http_port: u16) -> BaseUrl {
        BaseUrl::Towards(Route {
            speaker: speaker.into(),
            port,
            http_port,
            addrs: Vec::new(),
            last: None,
            failing: false,
        })
    }

    /// The URL, or why the route to the speaker cannot be found.
    pub fn try_get(&mut self) -> Result<String> {
        match self {
            BaseUrl::Fixed(url) => Ok(url.clone()),
            BaseUrl::Towards(route) => route.find(),
        }
    }

    /// The URL. While the route cannot be found, the last URL found, else
    /// one no speaker can use: the album then fails at the speaker, which
    /// the deck shows, and the log says why once.
    pub fn get(&mut self) -> String {
        match self {
            BaseUrl::Fixed(url) => url.clone(),
            BaseUrl::Towards(route) => match route.find() {
                Ok(url) => url,
                Err(err) => route.fallback(&err),
            },
        }
    }
}

impl Route {
    fn find(&mut self) -> Result<String> {
        let ip = self.local_ip().with_context(|| {
            format!(
                "cannot find a route to speaker_host {} (port {}); check it, or set advertise_host",
                self.speaker, self.port
            )
        })?;
        let url = format!("http://{ip}:{}/music", self.http_port);
        if self.failing {
            info!("the route to the speaker is back; serving music at {url}/");
            self.failing = false;
        }
        self.last = Some(url.clone());
        Ok(url)
    }

    fn local_ip(&mut self) -> Result<std::net::IpAddr> {
        if self.addrs.is_empty() {
            self.addrs = (self.speaker.as_str(), self.port)
                .to_socket_addrs()?
                .collect();
        }
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(self.addrs.as_slice())?;
        Ok(socket.local_addr()?.ip())
    }

    fn fallback(&mut self, err: &anyhow::Error) -> String {
        if !self.failing {
            warn!("{err:#}; the speaker cannot fetch music until the route is back");
            self.failing = true;
        }
        self.last
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/music", self.http_port))
    }
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

    #[test]
    fn the_route_to_the_speaker_gives_this_machines_address() {
        // The route to loopback: no network needed.
        let mut url = BaseUrl::towards("127.0.0.1", 8009, 8765);
        assert_eq!(url.try_get().unwrap(), "http://127.0.0.1:8765/music");
        assert_eq!(url.get(), "http://127.0.0.1:8765/music");
    }

    #[test]
    fn no_route_names_the_speaker_and_falls_back_to_the_last_url() {
        // An IPv4 socket cannot connect to an IPv6 address: fails without DNS or network.
        let mut url = BaseUrl::towards("::1", 8009, 8765);
        let err = url.try_get().unwrap_err();
        assert!(err.to_string().contains("speaker_host ::1"), "{err:#}");
        assert_eq!(
            url.get(),
            "http://127.0.0.1:8765/music",
            "no route found yet"
        );
        let BaseUrl::Towards(route) = &mut url else {
            unreachable!()
        };
        route.last = Some("http://10.0.0.2:8765/music".into());
        assert_eq!(url.get(), "http://10.0.0.2:8765/music");
    }

    #[test]
    fn a_fixed_url_is_returned_as_it_is() {
        let mut url = BaseUrl::fixed("http://host/music");
        assert_eq!(url.try_get().unwrap(), "http://host/music");
        assert_eq!(url.get(), "http://host/music");
    }
}
