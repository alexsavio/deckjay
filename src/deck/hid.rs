//! USB Stream Decks through `hidapi`.

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

use super::Backend;

pub(super) struct HidDeck {
    device: Arc<StreamDeck>,
    reader: Arc<DeviceStateReader>,
    kind: Kind,
}

impl HidDeck {
    /// `reset` clears the deck to the Elgato logo first.
    pub(super) fn open(hid: &mut HidApi, reset: bool) -> Result<Option<HidDeck>> {
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
        if reset {
            device.reset()?;
        }
        let reader = device.get_reader();
        Ok(Some(HidDeck {
            device,
            reader,
            kind,
        }))
    }
}

impl Backend for HidDeck {
    fn name(&self) -> String {
        format!("{:?}", self.kind)
    }

    fn layout(&self) -> (usize, usize) {
        (
            self.kind.row_count() as usize,
            self.kind.column_count() as usize,
        )
    }

    fn key_size(&self) -> u32 {
        self.kind.key_image_format().size.0 as u32
    }

    fn set_brightness(&self, percent: u8) -> Result<()> {
        Ok(self.device.set_brightness(percent)?)
    }

    fn encode(&self, image: RgbImage) -> Result<Vec<u8>> {
        Ok(convert_image(self.kind, DynamicImage::ImageRgb8(image))?)
    }

    fn write_image(&self, key: usize, data: &[u8]) -> Result<()> {
        Ok(self.device.write_image(key as u8, data)?)
    }

    fn flush(&self) -> Result<()> {
        Ok(self.device.flush()?)
    }

    fn clear(&self) -> Result<()> {
        self.device.clear_all_button_images()?;
        Ok(self.device.flush()?)
    }

    fn pressed_keys(&self, timeout: Duration) -> Result<Vec<usize>> {
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
