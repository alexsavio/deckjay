//! Thin wrapper around the Stream Deck: finds the device, caches encoded key
//! images, only re-sends keys that changed, and reports key presses.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use elgato_streamdeck::images::convert_image;
use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{
    DeviceStateReader, DeviceStateUpdate, StreamDeck, list_devices, refresh_device_list,
};
use hidapi::HidApi;
use image::{DynamicImage, RgbImage};

use crate::ui::Face;

pub struct Deck {
    device: Arc<StreamDeck>,
    reader: Arc<DeviceStateReader>,
    kind: Kind,
    /// Encoded image bytes per face, so each image is only converted once.
    encoded: HashMap<Face, Vec<u8>>,
    /// What each key currently shows.
    shown: Vec<Option<Face>>,
}

impl Deck {
    /// Opens the first Stream Deck with a key grid, if one is plugged in.
    pub fn open(hid: &mut HidApi) -> Result<Option<Deck>> {
        refresh_device_list(hid)?;
        let Some((kind, serial)) = list_devices(hid)
            .into_iter()
            .find(|(kind, _)| kind.is_visual() && kind.row_count() >= 2)
        else {
            return Ok(None);
        };
        #[expect(
            clippy::arc_with_non_send_sync,
            reason = "StreamDeck::get_reader takes &Arc<Self>"
        )]
        let device = Arc::new(StreamDeck::connect(hid, kind, &serial)?);
        device.reset()?;
        let reader = device.get_reader();
        Ok(Some(Deck {
            device,
            reader,
            kind,
            encoded: HashMap::new(),
            shown: vec![None; kind.key_count() as usize],
        }))
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// (rows, columns)
    pub fn layout(&self) -> (usize, usize) {
        (
            self.kind.row_count() as usize,
            self.kind.column_count() as usize,
        )
    }

    /// Side length in pixels of a (square) key image.
    pub fn key_size(&self) -> u32 {
        self.kind.key_image_format().size.0 as u32
    }

    pub fn set_brightness(&self, percent: u8) -> Result<()> {
        Ok(self.device.set_brightness(percent)?)
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
            let data = convert_image(self.kind, DynamicImage::ImageRgb8(render()))?;
            self.encoded.insert(face.clone(), data);
        }
        self.device.write_image(key as u8, &self.encoded[face])?;
        self.shown[key] = Some(face.clone());
        Ok(())
    }

    /// Sends all queued images to the device.
    pub fn flush(&self) -> Result<()> {
        Ok(self.device.flush()?)
    }

    /// Waits up to `timeout` for input and returns the keys that were pressed down.
    pub fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>> {
        Ok(self
            .reader
            .read(Some(timeout))?
            .into_iter()
            .filter_map(|u| match u {
                DeviceStateUpdate::ButtonDown(key) => Some(key as usize),
                _ => None,
            })
            .collect())
    }
}
