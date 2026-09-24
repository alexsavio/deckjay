//! The web Stream Deck simulator ([`crate::simulator`]) over HTTP.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use image::{ImageFormat, RgbImage};
use ureq::Agent;

use super::Backend;
use crate::simulator::{Brightness, Info};

/// Time allowed for one request, on top of a long-poll's own wait.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct RemoteDeck {
    agent: Agent,
    base: String,
    info: Info,
}

pub(super) fn agent() -> Agent {
    Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into()
}

/// `None` when nothing answers at `url`, so the caller keeps waiting as it
/// does for an unplugged deck. HTTP errors are real errors: wrong URL.
pub(super) fn fetch_info(agent: &Agent, url: &str) -> Result<Option<Info>> {
    let base = url.trim_end_matches('/');
    let mut reply = match agent.get(format!("{base}/api/info")).call() {
        Ok(reply) => reply,
        Err(ureq::Error::StatusCode(code)) => {
            bail!("{base}/api/info answered HTTP {code}: is this the deck simulator?")
        }
        Err(err) => {
            tracing::debug!("deck simulator not reachable: {err}");
            return Ok(None);
        }
    };
    let body = reply.body_mut().read_to_string()?;
    let info = serde_json::from_str(&body).with_context(|| format!("bad reply from {base}"))?;
    Ok(Some(info))
}

impl RemoteDeck {
    pub(super) fn open(url: &str) -> Result<Option<RemoteDeck>> {
        let agent = agent();
        let Some(info) = fetch_info(&agent, url)? else {
            return Ok(None);
        };
        if info.rows < 2 || info.cols == 0 {
            bail!(
                "the simulator has {}x{} keys; at least 2 rows are needed",
                info.rows,
                info.cols
            );
        }
        let base = url.trim_end_matches('/').to_string();
        agent.post(format!("{base}/api/reset")).send_empty()?;
        Ok(Some(RemoteDeck { agent, base, info }))
    }
}

impl Backend for RemoteDeck {
    fn name(&self) -> String {
        format!(
            "simulator {}x{} at {}",
            self.info.rows, self.info.cols, self.base
        )
    }

    fn layout(&self) -> (usize, usize) {
        (self.info.rows, self.info.cols)
    }

    fn key_size(&self) -> u32 {
        self.info.key_size
    }

    fn set_brightness(&self, percent: u8) -> Result<()> {
        let body = serde_json::to_string(&Brightness { percent })?;
        self.agent
            .put(format!("{}/api/brightness", self.base))
            .header("content-type", "application/json")
            .send(body)?;
        Ok(())
    }

    fn encode(&self, image: RgbImage) -> Result<Vec<u8>> {
        let mut png = Vec::new();
        image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
        Ok(png)
    }

    fn write_image(&self, key: usize, data: &[u8]) -> Result<()> {
        self.agent
            .put(format!("{}/api/keys/{key}", self.base))
            .header("content-type", "image/png")
            .send(data)?;
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>> {
        let wait_ms = timeout.as_millis();
        let body = self
            .agent
            .get(format!("{}/api/presses?wait_ms={wait_ms}", self.base))
            .config()
            .timeout_global(Some(timeout + REQUEST_TIMEOUT))
            .build()
            .call()?
            .body_mut()
            .read_to_string()?;
        Ok(serde_json::from_str(&body)?)
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;
    use crate::deck::Deck;
    use crate::icons;
    use crate::simulator::{self, Model};
    use crate::ui::Face;

    fn start_simulator(info: Info) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || simulator::serve(listener, info));
        url
    }

    #[test]
    fn nothing_listening_means_keep_waiting() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}");
        assert!(Deck::open_simulator(&url).unwrap().is_none());
        assert!(Deck::simulator_info(&url).unwrap().is_none());
    }

    #[test]
    fn opens_with_the_simulated_grid() {
        let url = start_simulator(Model::Xl.info());
        let deck = Deck::open_simulator(&url).unwrap().unwrap();
        assert_eq!(deck.layout(), (4, 8));
        assert_eq!(deck.key_size(), 96);
    }

    #[test]
    fn rejects_a_grid_with_one_row() {
        let url = start_simulator(Info {
            rows: 1,
            cols: 3,
            key_size: 72,
        });
        assert!(Deck::open_simulator(&url).is_err());
    }

    #[test]
    fn a_wrong_url_is_an_error_not_a_wait() {
        let url = start_simulator(Model::Mk2.info());
        assert!(Deck::open_simulator(&format!("{url}/nope")).is_err());
    }

    #[test]
    fn shown_keys_reach_the_simulator_as_png() {
        let url = start_simulator(Model::Mk2.info());
        let mut deck = Deck::open_simulator(&url).unwrap().unwrap();

        deck.show(4, &Face::Play, || icons::play(72)).unwrap();
        deck.flush().unwrap();

        let png = ureq::get(format!("{url}/api/keys/4"))
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (72, 72));
    }

    #[test]
    fn clicks_in_the_page_become_key_presses() {
        let url = start_simulator(Model::Mk2.info());
        let deck = Deck::open_simulator(&url).unwrap().unwrap();

        ureq::post(format!("{url}/api/press/7"))
            .send_empty()
            .unwrap();

        assert_eq!(deck.pressed_keys(Duration::from_secs(2)).unwrap(), [7]);
        assert!(
            deck.pressed_keys(Duration::from_millis(10))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn opening_clears_keys_left_from_an_earlier_run() {
        let url = start_simulator(Model::Mk2.info());
        let mut deck = Deck::open_simulator(&url).unwrap().unwrap();
        deck.show(0, &Face::Play, || icons::play(72)).unwrap();

        Deck::open_simulator(&url).unwrap().unwrap();

        assert!(ureq::get(format!("{url}/api/keys/0")).call().is_err());
    }
}
