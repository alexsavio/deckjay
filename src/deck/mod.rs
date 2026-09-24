//! The key grid the kids press: a USB Stream Deck or the web simulator.
//! [`Deck`] caches encoded key images, only re-sends keys that changed, and
//! reports key presses; a `Backend` talks to the device itself.

mod hid;
mod remote;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use hidapi::HidApi;
use image::RgbImage;

use crate::simulator::Info;
use crate::ui::Face;

trait Backend {
    fn name(&self) -> String;
    /// (rows, columns)
    fn layout(&self) -> (usize, usize);
    /// Side length in pixels of a (square) key image.
    fn key_size(&self) -> u32;
    fn set_brightness(&self, percent: u8) -> Result<()>;
    /// Converts a key image into the bytes `write_image` sends.
    fn encode(&self, image: RgbImage) -> Result<Vec<u8>>;
    fn write_image(&self, key: usize, data: &[u8]) -> Result<()>;
    fn flush(&self) -> Result<()>;
    /// Waits up to `timeout` for input and returns the keys that were pressed down.
    fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>>;
}

pub struct Deck {
    backend: Box<dyn Backend>,
    /// Encoded image bytes per face, so each image is only converted once.
    encoded: HashMap<Face, Vec<u8>>,
    /// What each key currently shows.
    shown: Vec<Option<Face>>,
}

impl Deck {
    /// Opens the first Stream Deck with a key grid, if one is plugged in.
    pub fn open_usb(hid: &mut HidApi) -> Result<Option<Deck>> {
        Ok(hid::HidDeck::open(hid)?.map(Deck::new))
    }

    /// Connects to the simulator at `url` and clears its keys; `None` while
    /// nothing answers there.
    pub fn open_simulator(url: &str) -> Result<Option<Deck>> {
        Ok(remote::RemoteDeck::open(url)?.map(Deck::new))
    }

    /// Asks the simulator at `url` for its grid without changing it; `None`
    /// while nothing answers there.
    pub fn simulator_info(url: &str) -> Result<Option<Info>> {
        remote::fetch_info(&remote::agent(), url)
    }

    fn new(backend: impl Backend + 'static) -> Deck {
        let (rows, cols) = backend.layout();
        Deck {
            backend: Box::new(backend),
            encoded: HashMap::new(),
            shown: vec![None; rows * cols],
        }
    }

    pub fn name(&self) -> String {
        self.backend.name()
    }

    /// (rows, columns)
    pub fn layout(&self) -> (usize, usize) {
        self.backend.layout()
    }

    pub fn key_size(&self) -> u32 {
        self.backend.key_size()
    }

    pub fn set_brightness(&self, percent: u8) -> Result<()> {
        self.backend.set_brightness(percent)
    }

    /// Queues `face` for `key`; `render` is only called if the image isn't cached yet.
    pub fn show(
        &mut self,
        key: usize,
        face: &Face,
        render: impl FnOnce() -> RgbImage,
    ) -> Result<()> {
        if self.shown[key].as_ref() == Some(face) {
            return Ok(());
        }
        if !self.encoded.contains_key(face) {
            let data = self.backend.encode(render())?;
            self.encoded.insert(face.clone(), data);
        }
        self.backend.write_image(key, &self.encoded[face])?;
        self.shown[key] = Some(face.clone());
        Ok(())
    }

    /// Sends all queued images to the device.
    pub fn flush(&self) -> Result<()> {
        self.backend.flush()
    }

    /// Waits up to `timeout` for input and returns the keys that were pressed down.
    pub fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>> {
        self.backend.pressed_keys(timeout)
    }
}
