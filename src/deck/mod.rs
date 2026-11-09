//! The key grid the kids press: a USB Stream Deck or the web simulator.
//! [`Deck`] caches encoded key images, only re-sends keys that changed, and
//! reports key presses; a `Backend` talks to the device itself.

mod hid;
mod remote;

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hidapi::HidApi;
use image::RgbImage;

use crate::simulator::Info;
use crate::ui::Face;

// Bounds on a deck's grid: the largest real deck has 36 keys, and key
// images are drawn at 4x, so a bogus grid could exhaust a Pi's memory.
const MAX_KEYS: usize = 64;
const KEY_SIZES: RangeInclusive<u32> = 16..=512;

/// A key grid the player can draw: the one rule for the USB deck, the
/// simulator and `deckjay check`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    /// Side length in pixels of a (square) key image.
    pub key_size: u32,
}

impl Grid {
    pub fn new(rows: usize, cols: usize, key_size: u32) -> Result<Grid> {
        Grid::check_shape(rows, cols)?;
        if !KEY_SIZES.contains(&key_size) {
            bail!(
                "the deck has keys of {key_size} px; the player needs keys of {} to {} px",
                KEY_SIZES.start(),
                KEY_SIZES.end()
            );
        }
        Ok(Grid {
            rows,
            cols,
            key_size,
        })
    }

    /// The rows and columns part of the rule; the layout needs no key size.
    pub fn check_shape(rows: usize, cols: usize) -> Result<()> {
        let too_many_keys = rows.checked_mul(cols).is_none_or(|n| n > MAX_KEYS);
        // The layout needs two keys above the control row: an item and "more".
        let too_few_keys = rows < 2 || (rows - 1).checked_mul(cols).is_none_or(|n| n < 2);
        if too_few_keys || too_many_keys {
            bail!(
                "the deck has {rows}x{cols} keys; the player needs at least 2 rows with 2 keys \
                 above the bottom row and at most {MAX_KEYS} keys"
            );
        }
        Ok(())
    }
}

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
    /// Removes every key image at once.
    fn clear(&self) -> Result<()>;
    /// Waits up to `timeout` for input and returns the keys that were pressed down.
    fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>>;
    /// Shows `text` beside the keys, where the device has room for it: the
    /// simulator page does, a USB deck does not.
    fn notice(&self, text: Option<&str>) -> Result<()> {
        let _ = text;
        Ok(())
    }
}

pub struct Deck {
    backend: Box<dyn Backend>,
    /// Encoded image bytes per face, so each image is only converted once.
    encoded: HashMap<Face, Vec<u8>>,
    /// What each key currently shows.
    shown: Vec<Option<Face>>,
    noticed: Option<String>,
}

impl Deck {
    /// Opens the first Stream Deck with a key grid, if one is plugged in.
    pub fn open_usb(hid: &mut HidApi) -> Result<Option<Deck>> {
        Ok(hid::HidDeck::open(hid, true)?.map(Deck::new))
    }

    /// As [`Deck::open_usb`], but leaves what the keys show: a reset would
    /// light the Elgato logo on a deck about to go dark.
    pub fn open_usb_as_is(hid: &mut HidApi) -> Result<Option<Deck>> {
        Ok(hid::HidDeck::open(hid, false)?.map(Deck::new))
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
            noticed: None,
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
        self.backend
            .set_brightness(percent)
            .with_context(|| format!("cannot set the brightness to {percent}%"))
    }

    /// Queues `face` for `key`; `render` is only called if the image isn't
    /// cached yet. Returns false when the key already showed `face`.
    pub fn show(
        &mut self,
        key: usize,
        face: &Face,
        render: impl FnOnce() -> RgbImage,
    ) -> Result<bool> {
        if self.shown[key].as_ref() == Some(face) {
            return Ok(false);
        }
        if !self.encoded.contains_key(face) {
            let data = self
                .backend
                .encode(render())
                .with_context(|| format!("cannot encode the image of key {key}"))?;
            self.encoded.insert(*face, data);
        }
        self.backend
            .write_image(key, &self.encoded[face])
            .with_context(|| format!("cannot send key {key}"))?;
        self.shown[key] = Some(*face);
        Ok(true)
    }

    /// Forgets the cached images of every face that fails `keep`, e.g. of
    /// items no longer in the library. Keys that show such a face are sent
    /// again on their next [`Deck::show`].
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
        self.backend.flush().context("cannot flush the deck")
    }

    /// Turns the deck dark: no key images, no notice, brightness 0. The next
    /// [`Deck::show`] sends every key again.
    pub fn blank(&mut self) -> Result<()> {
        self.backend.clear().context("cannot clear the deck")?;
        self.shown.fill(None);
        self.noticed = None;
        self.set_brightness(0)
    }

    pub fn notice(&mut self, text: Option<&str>) -> Result<()> {
        if self.noticed.as_deref() != text {
            self.backend
                .notice(text)
                .context("cannot show the notice")?;
            self.noticed = text.map(str::to_owned);
        }
        Ok(())
    }

    /// Waits up to `timeout` for input and returns the keys that were pressed down.
    pub fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>> {
        self.backend
            .pressed_keys(timeout)
            .context("cannot read key presses")
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
        clears: usize,
        brightness: Option<u8>,
        notices: Vec<Option<String>>,
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

        fn set_brightness(&self, percent: u8) -> Result<()> {
            self.0.borrow_mut().brightness = Some(percent);
            Ok(())
        }

        fn notice(&self, text: Option<&str>) -> Result<()> {
            self.0.borrow_mut().notices.push(text.map(str::to_owned));
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

        fn clear(&self) -> Result<()> {
            self.0.borrow_mut().clears += 1;
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
    fn a_notice_is_sent_once_per_change_and_blank_forgets_it() {
        let (mut deck, log) = fake_deck();
        deck.notice(Some("no sound output")).unwrap();
        deck.notice(Some("no sound output")).unwrap();
        deck.notice(None).unwrap();
        deck.notice(None).unwrap();
        deck.notice(Some("gone")).unwrap();
        deck.blank().unwrap();
        deck.notice(Some("gone")).unwrap();
        assert_eq!(
            log.borrow().notices,
            [
                Some("no sound output".to_string()),
                None,
                Some("gone".to_string()),
                Some("gone".to_string())
            ]
        );
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
    fn blank_clears_dims_and_sends_every_key_again_after() {
        let (mut deck, log) = fake_deck();
        deck.set_brightness(60).unwrap();
        deck.show(0, &Face::Play, tile).unwrap();

        deck.blank().unwrap();
        assert_eq!(log.borrow().clears, 1);
        assert_eq!(log.borrow().brightness, Some(0));

        deck.show(0, &Face::Play, tile).unwrap();
        assert_eq!(log.borrow().writes, [0, 0], "the key shows its face again");
        assert_eq!(log.borrow().encodes, 1, "from the cached image");
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
            new: false,
            trouble: false,
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

    #[test]
    fn a_grid_is_refused_without_overflowing() {
        assert!(Grid::new(usize::MAX, 2, 72).is_err());
        assert!(Grid::new(2, usize::MAX, 72).is_err());
    }

    #[test]
    fn the_largest_and_smallest_grids_are_accepted() {
        assert!(Grid::new(2, 2, 16).is_ok());
        assert!(Grid::new(8, 8, 512).is_ok());
        assert!(Grid::new(8, 9, 512).is_err());
    }
}
