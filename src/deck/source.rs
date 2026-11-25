//! Where the deck comes from: a USB Stream Deck or the simulator, found again
//! each time it is plugged in.

use std::hash::Hash;

use anyhow::Result;
use hidapi::HidApi;
use tracing::{info, warn};

use super::Deck;

enum Kind {
    Usb(HidApi),
    Simulator(String),
}

/// Opens the deck, again and again: it can be unplugged and plugged back in.
pub struct Source {
    kind: Kind,
    waiting_logged: bool,
}

impl Source {
    /// The simulator at `simulator`, else a USB deck.
    pub fn new(simulator: Option<&str>) -> Result<Source> {
        let kind = match simulator {
            Some(url) => Kind::Simulator(url.to_owned()),
            None => Kind::Usb(elgato_streamdeck::new_hidapi()?),
        };
        Ok(Source {
            kind,
            waiting_logged: false,
        })
    }

    /// The deck if one is there now. Says once that it waits, and warns about
    /// every failure to open.
    pub fn open<F: Copy + Eq + Hash>(&mut self) -> Option<Deck<F>> {
        let opened = match &mut self.kind {
            Kind::Usb(hid) => Deck::open_usb(hid),
            Kind::Simulator(url) => Deck::open_simulator(url),
        };
        match opened {
            Ok(Some(deck)) => {
                info!("deck connected: {}", deck.name());
                self.waiting_logged = false;
                Some(deck)
            }
            Ok(None) => {
                if !self.waiting_logged {
                    info!("{}", self.waiting_message());
                    self.waiting_logged = true;
                }
                None
            }
            Err(err) => {
                warn!("cannot open the deck: {err:#}");
                None
            }
        }
    }

    fn waiting_message(&self) -> String {
        match &self.kind {
            Kind::Usb(_) => "waiting for a Stream Deck to be plugged in…".into(),
            Kind::Simulator(url) => format!("waiting for the deck simulator at {url}…"),
        }
    }
}

/// For systemd's `ExecStopPost`, which runs however deckjay ended, a crash
/// included.
pub fn blank_usb() -> Result<()> {
    let mut hid = elgato_streamdeck::new_hidapi()?;
    if let Some(mut deck) = Deck::<()>::open_usb_as_is(&mut hid)? {
        deck.blank()?;
        info!("{} is dark", deck.name());
    } else {
        info!("no Stream Deck to turn off");
    }
    Ok(())
}
