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

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use anyhow::Result;
use image::{Rgb, RgbImage};
use tracing::{info, warn};

use crate::config::Config;
use crate::deck::Deck;
use crate::icons;
use crate::library::{self, Album};
use crate::player::{PlayerCmd, PlayerEvent, TrackInfo};

/// Volume bar resolution; keeps the number of distinct cached key images small.
const VOLUME_LEVELS: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Prev,
    PlayPause,
    Next,
    VolumeDown,
    VolumeUp,
}

/// Everything a key can show. Used as the cache key for rendered images.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Blank,
    Album { index: usize, current: bool },
    More { page: usize, pages: usize },
    Play,
    Pause,
    Prev,
    Next,
    Volume { up: bool, level: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Album(usize),
    More,
    Control(Control),
}

pub struct Layout {
    cols: usize,
    album_slots: usize,
    albums_per_page: usize,
    pages: usize,
    has_more_key: bool,
    controls: Vec<Option<Control>>,
    album_count: usize,
}

impl Layout {
    pub fn new(rows: usize, cols: usize, album_count: usize) -> Layout {
        let album_slots = (rows - 1) * cols;
        let has_more_key = album_count > album_slots;
        let albums_per_page = if has_more_key {
            album_slots - 1
        } else {
            album_slots
        };
        let pages = album_count.div_ceil(albums_per_page.max(1)).max(1);
        Layout {
            cols,
            album_slots,
            albums_per_page,
            pages,
            has_more_key,
            controls: control_row(cols),
            album_count,
        }
    }

    fn action(&self, key: usize, page: usize) -> Option<Action> {
        if key >= self.album_slots {
            return self
                .controls
                .get(key - self.album_slots)
                .copied()
                .flatten()
                .map(Action::Control);
        }
        if self.has_more_key && key == self.album_slots - 1 {
            return Some(Action::More);
        }
        let index = page * self.albums_per_page + key;
        (index < self.album_count).then_some(Action::Album(index))
    }
}

/// Controls for the bottom row, depending on how wide the deck is.
fn control_row(cols: usize) -> Vec<Option<Control>> {
    use Control::{Next, PlayPause, Prev, VolumeDown, VolumeUp};
    let mut row = vec![None; cols];
    match cols {
        0 => {}
        1 => row[0] = Some(PlayPause),
        2 => row.copy_from_slice(&[Some(PlayPause), Some(VolumeUp)]),
        3 => row.copy_from_slice(&[Some(PlayPause), Some(VolumeDown), Some(VolumeUp)]),
        4 => row.copy_from_slice(&[
            Some(PlayPause),
            Some(Next),
            Some(VolumeDown),
            Some(VolumeUp),
        ]),
        _ => {
            row[..3].copy_from_slice(&[Some(Prev), Some(PlayPause), Some(Next)]);
            row[cols - 2] = Some(VolumeDown);
            row[cols - 1] = Some(VolumeUp);
        }
    }
    row
}

pub struct Ui {
    albums: Vec<Album>,
    base_url: String,
    player: Sender<PlayerCmd>,
    events: Receiver<PlayerEvent>,

    max_volume: f32,
    volume_step: f32,
    volume: f32,

    page: usize,
    /// Album currently loaded on the speaker (highlighted with a frame).
    current: Option<usize>,
    playing: bool,

    tiles: Vec<RgbImage>,
    tile_size: u32,
}

impl Ui {
    pub fn new(
        cfg: &Config,
        albums: Vec<Album>,
        base_url: String,
        player: Sender<PlayerCmd>,
        events: Receiver<PlayerEvent>,
    ) -> Ui {
        Ui {
            albums,
            base_url,
            player,
            events,
            max_volume: cfg.max_volume,
            volume_step: cfg.volume_step,
            volume: cfg.start_volume,
            page: 0,
            current: None,
            playing: false,
            tiles: Vec::new(),
            tile_size: 0,
        }
    }

    /// Drives a connected deck until it is unplugged (returns the error then).
    pub fn run(&mut self, deck: &mut Deck, brightness: u8) -> Result<()> {
        deck.set_brightness(brightness)?;
        let (rows, cols) = deck.layout();
        let layout = Layout::new(rows, cols, self.albums.len());
        self.page = self.page.min(layout.pages - 1);
        self.prepare_tiles(deck.key_size());
        self.draw(deck, &layout)?;

        loop {
            let mut changed = false;
            for key in deck.pressed_keys(Duration::from_millis(100))? {
                self.press(&layout, key);
                changed = true;
            }
            changed |= self.handle_events();
            if changed {
                self.draw(deck, &layout)?;
            }
        }
    }

    /// Applies status updates from the speaker. Returns true if anything changed.
    pub fn handle_events(&mut self) -> bool {
        let mut changed = false;
        for event in self.events.try_iter() {
            let (current, playing) = match event {
                PlayerEvent::Playing(album) => (Some(album), true),
                PlayerEvent::Paused(album) => (Some(album), false),
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
            Action::Album(index) if self.current == Some(index) => {
                self.send(PlayerCmd::TogglePause);
                self.playing = !self.playing;
            }
            Action::Album(index) => {
                info!(album = %self.albums[index].name, "album pressed");
                let tracks = self.tracks(index);
                self.send(PlayerCmd::PlayAlbum {
                    album: index,
                    tracks,
                    volume: self.volume,
                });
                self.current = Some(index);
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
                let volume = (self.volume + delta).clamp(0.0, self.max_volume);
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

    fn tracks(&self, index: usize) -> Vec<TrackInfo> {
        let album = &self.albums[index];
        let cover_url = album
            .cover_rel
            .as_ref()
            .map(|c| library::url_for(&self.base_url, c));
        album
            .tracks
            .iter()
            .map(|t| TrackInfo {
                url: library::url_for(&self.base_url, &t.rel_path),
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
                Some(Action::Album(index)) => Face::Album {
                    index,
                    current: self.current == Some(index),
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
            Face::Album {
                index,
                current: false,
            } => self.tiles[*index].clone(),
            Face::Album {
                index,
                current: true,
            } => icons::with_highlight(&self.tiles[*index]),
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

    /// Loads and shrinks all covers once, so drawing stays fast on a Pi.
    fn prepare_tiles(&mut self, size: u32) {
        if self.tile_size == size && self.tiles.len() == self.albums.len() {
            return;
        }
        self.tiles = self
            .albums
            .iter()
            .map(|album| match &album.cover {
                Some(path) => match image::open(path) {
                    Ok(img) => icons::thumbnail(&img, size),
                    Err(err) => {
                        warn!("cannot read cover {}: {err}", path.display());
                        icons::placeholder(&album.name, size)
                    }
                },
                None => icons::placeholder(&album.name, size),
            })
            .collect();
        self.tile_size = size;
    }

    /// Renders what a deck would show, as one picture (for `--preview`).
    pub fn preview(&mut self, rows: usize, cols: usize, size: u32, demo_state: bool) -> RgbImage {
        let layout = Layout::new(rows, cols, self.albums.len());
        self.prepare_tiles(size);
        if demo_state && !self.albums.is_empty() {
            self.current = Some(0);
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
mod tests {
    use super::*;

    #[test]
    fn fifteen_keys_without_paging() {
        let l = Layout::new(3, 5, 7);
        assert_eq!(l.pages, 1);
        assert_eq!(l.action(0, 0), Some(Action::Album(0)));
        assert_eq!(l.action(6, 0), Some(Action::Album(6)));
        assert_eq!(l.action(7, 0), None);
        assert_eq!(l.action(10, 0), Some(Action::Control(Control::Prev)));
        assert_eq!(l.action(14, 0), Some(Action::Control(Control::VolumeUp)));
    }

    #[test]
    fn fifteen_keys_with_paging() {
        let l = Layout::new(3, 5, 20);
        assert_eq!(l.pages, 3); // 9 albums per page
        assert_eq!(l.action(9, 0), Some(Action::More));
        assert_eq!(l.action(0, 1), Some(Action::Album(9)));
        assert_eq!(l.action(1, 2), Some(Action::Album(19)));
        assert_eq!(l.action(2, 2), None);
    }

    #[test]
    fn xl_controls_sit_at_both_ends() {
        let row = control_row(8);
        assert_eq!(row[1], Some(Control::PlayPause));
        assert_eq!(row[4], None);
        assert_eq!(row[7], Some(Control::VolumeUp));
    }
}
