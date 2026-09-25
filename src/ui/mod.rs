//! What the kids see and press.
//!
//! The bottom row holds the controls, every other key is an album cover.
//! If there are more albums than keys, the last album key becomes an orange
//! "more" arrow that flips through pages.
//!
//! ```text
//!  15 keys (MK.2 / Scissor Keys)       32 keys (XL)
//!  [A][A][A][A][A]                     [A][A][A][A][A][A][A][A]
//!  [A][A][A][A][>]  <- more            [A][A][A][A][A][A][A][A]
//!  [⏮][⏯][⏭][-][+]                    [A][A][A][A][A][A][A][>]
//!                                      [⏮][⏯][⏭][ ][ ][ ][-][+]
//! ```

mod layout;

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use anyhow::Result;
use image::{Rgb, RgbImage};
use tracing::{info, warn};

use self::layout::{Action, Control, Layout};
use crate::config::Config;
use crate::deck::Deck;
use crate::icons;
use crate::library::{self, ItemId, Library};
use crate::player::{PlayerCmd, PlayerEvent, TrackInfo};

/// Volume bar resolution; keeps the number of distinct cached key images small.
const VOLUME_LEVELS: f32 = 20.0;

/// Everything a key can show. Used as the cache key for rendered images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Blank,
    Item { id: ItemId, current: bool },
    More { page: usize, pages: usize },
    Play,
    Pause,
    Prev,
    Next,
    Volume { up: bool, level: u8 },
}

pub struct Ui {
    library: Library,
    base_url: String,
    player: Sender<PlayerCmd>,
    events: Receiver<PlayerEvent>,

    max_volume: f32,
    volume_step: f32,
    volume: f32,

    page: usize,
    /// Item currently loaded on the speaker (highlighted with a frame).
    current: Option<ItemId>,
    playing: bool,

    tiles: HashMap<ItemId, RgbImage>,
    tile_size: u32,
}

impl Ui {
    pub fn new(
        cfg: &Config,
        library: Library,
        base_url: String,
        player: Sender<PlayerCmd>,
        events: Receiver<PlayerEvent>,
    ) -> Ui {
        Ui {
            library,
            base_url,
            player,
            events,
            max_volume: cfg.max_volume,
            volume_step: cfg.volume_step,
            volume: cfg.start_volume,
            page: 0,
            current: None,
            playing: false,
            tiles: HashMap::new(),
            tile_size: 0,
        }
    }

    /// Drives a connected deck until it is unplugged (returns the error then).
    pub fn run(&mut self, deck: &mut Deck, brightness: u8) -> Result<()> {
        deck.set_brightness(brightness)?;
        let (rows, cols) = deck.layout();
        let layout = Layout::new(rows, cols, self.shelf_items());
        self.page = self.page.min(layout.pages - 1);
        self.prepare_tiles(deck.key_size());
        self.draw(deck, &layout)?;

        loop {
            let keys = deck.pressed_keys(Duration::from_millis(100))?;
            if self.update(&layout, &keys) {
                self.draw(deck, &layout)?;
            }
        }
    }

    /// Applies key presses and speaker events. Returns true if anything changed.
    fn update(&mut self, layout: &Layout, keys: &[usize]) -> bool {
        // Events are drained first: all of them predate the commands these
        // presses send, so they must not overwrite the presses' guesses.
        let mut changed = self.handle_events();
        for &key in keys {
            self.press(layout, key);
            changed = true;
        }
        changed
    }

    /// Applies status updates from the speaker. Returns true if anything changed.
    pub fn handle_events(&mut self) -> bool {
        let mut changed = false;
        for event in self.events.try_iter() {
            let (current, playing) = match event {
                PlayerEvent::Playing(item) => (Some(item), true),
                PlayerEvent::Paused(item) => (Some(item), false),
                PlayerEvent::Stopped => (None, false),
            };
            changed |= self.current != current || self.playing != playing;
            self.current = current;
            self.playing = playing;
        }
        changed
    }

    fn press(&mut self, layout: &Layout, key: usize) {
        let Some(action) = layout.action(key, self.page) else {
            return;
        };
        match action {
            Action::More => self.page = (self.page + 1) % layout.pages,
            Action::Item(id) if self.current == Some(id) => {
                self.send(PlayerCmd::TogglePause);
                self.playing = !self.playing;
            }
            Action::Item(id) => {
                info!(album = %self.library.item(id).name, "album pressed");
                let tracks = self.tracks(id);
                self.send(PlayerCmd::PlayAlbum {
                    album: id,
                    tracks,
                    volume: self.volume,
                });
                self.current = Some(id);
                self.playing = true;
            }
            Action::Control(control) => self.control(control),
        }
    }

    fn control(&mut self, control: Control) {
        let active = self.current.is_some();
        match control {
            Control::PlayPause if active => {
                self.send(PlayerCmd::TogglePause);
                self.playing = !self.playing;
            }
            Control::Next if active => self.send(PlayerCmd::Next),
            Control::Prev if active => self.send(PlayerCmd::Prev),
            Control::VolumeUp | Control::VolumeDown => {
                let delta = if control == Control::VolumeUp {
                    self.volume_step
                } else {
                    -self.volume_step
                };
                // Snapped to 0.001 so f32 drift cannot give one volume two
                // bar levels (0.25 vs 0.24999999 round to 13 and 12).
                let volume =
                    (((self.volume + delta) * 1000.0).round() / 1000.0).clamp(0.0, self.max_volume);
                if (volume - self.volume).abs() > f32::EPSILON {
                    self.volume = volume;
                    self.send(PlayerCmd::SetVolume(volume));
                }
            }
            _ => {}
        }
    }

    fn send(&self, cmd: PlayerCmd) {
        if self.player.send(cmd).is_err() {
            warn!("player thread is gone");
        }
    }

    fn tracks(&self, id: ItemId) -> Vec<TrackInfo> {
        let album = self.library.item(id);
        let cover_url = album
            .cover_rel
            .as_ref()
            .map(|c| library::url_for(&self.base_url, c));
        album
            .tracks()
            .iter()
            .map(|t| TrackInfo {
                url: library::url_for(&self.base_url, &t.rel_path),
                path: t.path.clone(),
                content_type: t.content_type.to_string(),
                title: t.title.clone(),
                album: album.name.clone(),
                cover_url: cover_url.clone(),
            })
            .collect()
    }

    fn faces(&self, layout: &Layout) -> Vec<Face> {
        let total = layout.album_slots + layout.cols;
        (0..total)
            .map(|key| match layout.action(key, self.page) {
                None => Face::Blank,
                Some(Action::Item(id)) => Face::Item {
                    id,
                    current: self.current == Some(id),
                },
                Some(Action::More) => Face::More {
                    page: self.page,
                    pages: layout.pages,
                },
                Some(Action::Control(c)) => match c {
                    Control::PlayPause if self.playing => Face::Pause,
                    Control::PlayPause => Face::Play,
                    Control::Prev => Face::Prev,
                    Control::Next => Face::Next,
                    Control::VolumeDown | Control::VolumeUp => Face::Volume {
                        up: c == Control::VolumeUp,
                        level: (self.volume / self.max_volume.max(f32::EPSILON) * VOLUME_LEVELS)
                            .round() as u8,
                    },
                },
            })
            .collect()
    }

    fn render(&self, face: &Face, size: u32) -> RgbImage {
        match face {
            Face::Blank => icons::blank(size),
            Face::Item { id, current: false } => self.tiles[id].clone(),
            Face::Item { id, current: true } => icons::with_highlight(&self.tiles[id]),
            Face::More { page, pages } => icons::more(size, *page, *pages),
            Face::Play => icons::play(size),
            Face::Pause => icons::pause(size),
            Face::Prev => icons::prev(size),
            Face::Next => icons::next(size),
            Face::Volume { up, level } => {
                icons::volume(size, *up, f32::from(*level) / VOLUME_LEVELS)
            }
        }
    }

    fn draw(&self, deck: &mut Deck, layout: &Layout) -> Result<()> {
        let size = deck.key_size();
        for (key, face) in self.faces(layout).iter().enumerate() {
            deck.show(key, face, || self.render(face, size))?;
        }
        deck.flush()
    }

    /// The deck shows the first shelf's items.
    fn shelf_items(&self) -> &[ItemId] {
        self.library
            .shelves()
            .first()
            .map(|shelf| shelf.items.as_slice())
            .unwrap_or_default()
    }

    /// Loads and shrinks all covers once, so drawing stays fast on a Pi.
    fn prepare_tiles(&mut self, size: u32) {
        if self.tile_size == size && self.tiles.len() == self.library.items().len() {
            return;
        }
        self.tiles = self
            .library
            .items()
            .map(|(id, album)| {
                let tile = match &album.cover {
                    Some(path) => match image::open(path) {
                        Ok(img) => icons::thumbnail(&img, size),
                        Err(err) => {
                            warn!("cannot read cover {}: {err}", path.display());
                            icons::placeholder(&album.name, size)
                        }
                    },
                    None => icons::placeholder(&album.name, size),
                };
                (id, tile)
            })
            .collect();
        self.tile_size = size;
    }

    /// Renders what a deck would show, as one picture (for `--preview`).
    pub fn preview(&mut self, rows: usize, cols: usize, size: u32, demo_state: bool) -> RgbImage {
        let layout = Layout::new(rows, cols, self.shelf_items());
        self.prepare_tiles(size);
        if demo_state && let Some(&first) = self.shelf_items().first() {
            self.current = Some(first);
            self.playing = true;
        }
        let gap = (size / 6).max(4);
        let width = cols as u32 * (size + gap) + gap;
        let height = rows as u32 * (size + gap) + gap;
        let mut canvas = RgbImage::from_pixel(width, height, Rgb([45, 45, 50]));
        for (key, face) in self.faces(&layout).iter().enumerate() {
            let (row, col) = ((key / cols) as u32, (key % cols) as u32);
            let tile = self.render(face, size);
            image::imageops::replace(
                &mut canvas,
                &tile,
                i64::from(gap + col * (size + gap)),
                i64::from(gap + row * (size + gap)),
            );
        }
        canvas
    }
}

#[cfg(test)]
mod tests;
