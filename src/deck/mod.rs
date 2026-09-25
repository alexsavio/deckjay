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
            self.encoded.insert(*face, data);
        }
        self.backend.write_image(key, &self.encoded[face])?;
        self.shown[key] = Some(*face);
        Ok(())
    }

    /// Forgets the cached images of every face that fails `keep`, e.g. of
    /// items no longer in the library. Keys that show such a face are sent
    /// again on their next [`Deck::show`].
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the library does not change at runtime yet")
    )]
    pub fn retain(&mut self, keep: impl Fn(&Face) -> bool) {
        self.encoded.retain(|face, _| keep(face));
        for shown in &mut self.shown {
            if shown.as_ref().is_some_and(|face| !keep(face)) {
                *shown = None;
            }
        }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::bail;

    use super::*;
    use crate::library::ItemId;

    #[derive(Default)]
    struct Log {
        encodes: usize,
        writes: Vec<usize>,
        unplugged: bool,
    }

    struct FakeBackend(Rc<RefCell<Log>>);

    impl Backend for FakeBackend {
        fn name(&self) -> String {
            "fake".into()
        }

        fn layout(&self) -> (usize, usize) {
            (2, 3)
        }

        fn key_size(&self) -> u32 {
            4
        }

        fn set_brightness(&self, _: u8) -> Result<()> {
            Ok(())
        }

        fn encode(&self, _: RgbImage) -> Result<Vec<u8>> {
            self.0.borrow_mut().encodes += 1;
            Ok(Vec::new())
        }

        fn write_image(&self, key: usize, _: &[u8]) -> Result<()> {
            let mut log = self.0.borrow_mut();
            if log.unplugged {
                bail!("unplugged");
            }
            log.writes.push(key);
            Ok(())
        }

        fn flush(&self) -> Result<()> {
            Ok(())
        }

        fn pressed_keys(&self, _: Duration) -> Result<Vec<usize>> {
            Ok(Vec::new())
        }
    }

    fn fake_deck() -> (Deck, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        (Deck::new(FakeBackend(Rc::clone(&log))), log)
    }

    fn tile() -> RgbImage {
        RgbImage::new(4, 4)
    }

    #[test]
    fn a_key_is_only_sent_when_its_face_changes() {
        let (mut deck, log) = fake_deck();
        deck.show(0, &Face::Play, tile).unwrap();
        deck.show(0, &Face::Play, tile).unwrap();
        deck.show(0, &Face::Pause, tile).unwrap();
        assert_eq!(log.borrow().writes, [0, 0]);
    }

    #[test]
    fn a_face_is_rendered_and_encoded_once_for_every_key() {
        let (mut deck, log) = fake_deck();
        deck.show(0, &Face::Blank, tile).unwrap();
        deck.show(1, &Face::Blank, || panic!("Blank rendered twice"))
            .unwrap();
        deck.show(0, &Face::Play, tile).unwrap();
        deck.show(0, &Face::Blank, || panic!("Blank rendered twice"))
            .unwrap();
        assert_eq!(log.borrow().encodes, 2);
        assert_eq!(log.borrow().writes, [0, 1, 0, 0]);
    }

    #[test]
    fn a_failed_write_is_sent_again_on_the_next_show() {
        let (mut deck, log) = fake_deck();
        log.borrow_mut().unplugged = true;
        assert!(deck.show(5, &Face::Play, tile).is_err());

        log.borrow_mut().unplugged = false;
        deck.show(5, &Face::Play, tile).unwrap();
        assert_eq!(log.borrow().writes, [5]);
    }

    #[test]
    fn retain_forgets_dropped_faces_and_sends_their_keys_again() {
        let (mut deck, log) = fake_deck();
        let gone = Face::Item {
            id: ItemId(7),
            current: false,
            progress: None,
        };
        deck.show(0, &gone, tile).unwrap();
        deck.show(1, &Face::Play, tile).unwrap();

        deck.retain(|face| *face != gone);

        deck.show(1, &Face::Play, || panic!("Play was dropped"))
            .unwrap();
        deck.show(0, &gone, tile).unwrap();
        assert_eq!(
            log.borrow().encodes,
            3,
            "only the dropped face is encoded again"
        );
        assert_eq!(log.borrow().writes, [0, 1, 0]);
    }
}
