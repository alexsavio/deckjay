//! The whole sign-in against the fake accounts service, with the test as the
//! browser or as the person pasting the address.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::thread::JoinHandle;

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::spotify::auth::{AUTHORIZE_URL, challenge};
use crate::spotify::fake::{self, Fake};

/// Output the test can read while the sign-in still runs.
#[derive(Clone, Default)]
struct Shared(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "never printed {needle:?}:\n{text}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A sign-in running on its own thread.
struct Login {
    fake: Fake,
    dir: TempDir,
    out: Shared,
    done: Option<JoinHandle<Result<()>>>,
    /// The query of the printed authorize URL, decoded.
    params: HashMap<String, String>,
}

impl Login {
    fn start(answer: impl BufRead + Send + 'static, timeout: Duration) -> Login {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        let out = Shared::default();
        let done = {
            let (state_dir, endpoints, mut out) =
                (dir.path().to_path_buf(), fake.endpoints(), out.clone());
            thread::spawn(move || {
                let listen = "127.0.0.1:0".parse().unwrap();
                sign_in(
                    fake::CLIENT_ID,
                    &state_dir,
                    listen,
                    answer,
                    endpoints,
                    &mut out,
                    timeout,
                )
            })
        };
        let text = out.wait_for("Waiting for the browser");
        let url = text
            .lines()
            .find(|line| line.starts_with(AUTHORIZE_URL))
            .unwrap();
        let params = fake::form(url.split_once('?').unwrap().1);
        Login {
            fake,
            dir,
            out,
            done: Some(done),
            params,
        }
    }

    fn redirect_uri(&self) -> &str {
        &self.params["redirect_uri"]
    }

    fn state(&self) -> &str {
        &self.params["state"]
    }

    /// Where Spotify sends the browser after "Agree".
    fn answer_url(&self, state: &str) -> String {
        format!("{}?code={}&state={state}", self.redirect_uri(), fake::CODE)
    }

    fn result(&mut self) -> Result<()> {
        self.done.take().unwrap().join().unwrap()
    }

    fn saved(&self) -> Result<TokenFile> {
        TokenFile::load(self.dir.path())
    }
}

/// The browser: the status and the page.
fn browse(url: &str) -> (u16, String) {
    let mut reply = crate::net::api_agent(Duration::from_secs(5))
        .get(url)
        .call()
        .unwrap();
    let page = reply.body_mut().read_to_string().unwrap();
    (reply.status().as_u16(), page)
}

#[test]
fn the_browser_brings_the_code_back() {
    let mut login = Login::start(io::empty(), Duration::from_secs(10));
    assert_eq!(login.params["client_id"], fake::CLIENT_ID);
    assert_eq!(login.params["response_type"], "code");
    assert_eq!(login.params["code_challenge_method"], "S256");
    assert!(login.redirect_uri().starts_with("http://127.0.0.1:"));

    let root = login.redirect_uri().strip_suffix("/callback").unwrap();
    assert_eq!(browse(&format!("{root}/favicon.ico")).0, 404);
    let (status, page) = browse(&login.answer_url(login.state()));
    assert_eq!(status, 200);
    assert!(page.contains("close this tab"), "{page}");
    login.result().unwrap();

    assert_eq!(
        login.saved().unwrap(),
        TokenFile {
            client_id: fake::CLIENT_ID.into(),
            refresh_token: Secret::new(fake::REFRESH_TOKEN),
        }
    );
    let exchanges = login.fake.token_requests();
    assert_eq!(
        exchanges.len(),
        1,
        "the sign-in's access token is used as is"
    );
    assert_eq!(
        exchanges[0].param("redirect_uri").as_deref(),
        Some(login.redirect_uri())
    );
    let verifier = exchanges[0].param("code_verifier").unwrap();
    assert_eq!(challenge(&verifier), login.params["code_challenge"]);

    let text = login.out.text();
    for line in [
        "Spotify account: Parent\n",
        "  Den (AVR)\n",
        "  Kitchen Speaker (Speaker, active)\n",
        "  Old TV (TV, restricted: kids-deck cannot control it)\n",
    ] {
        assert!(text.contains(line), "{line:?} missing in:\n{text}");
    }
    assert!(!text.contains("access-1"), "{text}");
    assert!(!text.contains(fake::REFRESH_TOKEN), "{text}");
}

#[test]
fn a_pasted_address_brings_the_code_back() {
    let (reader, mut writer) = io::pipe().unwrap();
    let mut login = Login::start(BufReader::new(reader), Duration::from_secs(10));
    writeln!(writer, "what do I paste?").unwrap();
    login
        .out
        .wait_for("That is not the address from the browser");
    writeln!(writer, "  {}#_=_  ", login.answer_url(login.state())).unwrap();
    login.result().unwrap();
    assert_eq!(
        login.saved().unwrap().refresh_token.expose(),
        fake::REFRESH_TOKEN
    );
}

#[test]
fn an_answer_for_another_sign_in_is_refused() {
    let mut login = Login::start(io::empty(), Duration::from_secs(10));
    let (status, page) = browse(&login.answer_url("forged"));
    assert_eq!(status, 400);
    assert!(page.contains("did not work"), "{page}");
    let err = login.result().unwrap_err().to_string();
    assert!(err.contains("state does not match"), "{err}");
    assert!(login.fake.token_requests().is_empty());
    assert!(login.saved().is_err());
}

#[test]
fn a_denied_sign_in_saves_nothing() {
    let (reader, mut writer) = io::pipe().unwrap();
    let mut login = Login::start(BufReader::new(reader), Duration::from_secs(10));
    let denied = format!(
        "{}?error=access_denied&state={}",
        login.redirect_uri(),
        login.state()
    );
    writeln!(writer, "{denied}").unwrap();
    let err = login.result().unwrap_err().to_string();
    assert!(err.contains("not allowed"), "{err}");
    assert!(login.fake.token_requests().is_empty());
    assert!(login.saved().is_err());
}

#[test]
fn without_an_answer_it_gives_up_and_stops_listening() {
    let mut login = Login::start(io::empty(), Duration::from_millis(300));
    let err = login.result().unwrap_err().to_string();
    assert!(err.contains("no answer from Spotify"), "{err}");
    assert!(err.contains("kids-deck spotify-login"), "{err}");
    let address = login.redirect_uri().strip_prefix("http://").unwrap();
    let address = address.strip_suffix("/callback").unwrap();
    assert!(TcpStream::connect(address).is_err());
    assert_eq!(minutes_or_seconds(TIMEOUT), "5 minutes");
}

#[test]
fn a_busy_port_is_reported() {
    let busy = TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let err = run(
        fake::CLIENT_ID,
        dir.path(),
        busy.local_addr().unwrap(),
        io::empty(),
        Endpoints::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("cannot listen on"), "{err}");
}

#[test]
fn the_account_check_says_when_no_device_is_awake() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let token = TokenFile {
        client_id: fake::CLIENT_ID.into(),
        refresh_token: Secret::new(fake::REFRESH_TOKEN),
    };
    let mut client = Client::new(token, dir.path(), fake.endpoints());
    fake.script().devices = json!([]);
    fake.can(
        "/v1/me",
        403,
        None,
        r#"{"error":{"status":403,"message":"Forbidden"}}"#,
    );

    let mut out = Vec::new();
    print_account(&mut client, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("Cannot read the Spotify account: Spotify answered 403: Forbidden"),
        "{text}"
    );
    assert!(text.contains("Asleep? Open the Spotify app"), "{text}");
}

#[test]
fn only_a_get_of_the_callback_path_is_an_answer() {
    assert_eq!(
        callback_target("GET /callback?code=c&state=s HTTP/1.1\r\n"),
        Some("code=c&state=s")
    );
    assert_eq!(callback_target("GET /callback HTTP/1.1\r\n"), Some(""));
    for line in [
        "GET /favicon.ico HTTP/1.1\r\n",
        "GET /callback/x?code=c HTTP/1.1\r\n",
        "POST /callback?code=c HTTP/1.1\r\n",
        "",
    ] {
        assert_eq!(callback_target(line), None, "{line:?}");
    }
}

#[test]
fn a_pasted_address_needs_the_callback_path() {
    assert_eq!(
        pasted_query("http://127.0.0.1:8898/callback?code=c&state=s"),
        Some("code=c&state=s")
    );
    assert_eq!(
        pasted_query("127.0.0.1:8898/callback?code=c&state=s#_=_"),
        Some("code=c&state=s")
    );
    for line in [
        "http://127.0.0.1:8898/callback",
        "http://127.0.0.1:8898/other?code=c",
        "code=c&state=s",
    ] {
        assert_eq!(pasted_query(line), None, "{line:?}");
    }
}
