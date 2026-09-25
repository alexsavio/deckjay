//! What the kids see and press.
//!
//! The bottom row holds the controls, every other key is an item (an album
//! cover, a book, a story). Each source is a shelf of items; the deck shows
//! one shelf at a time. With several shelves, the last item key shows the
//! next shelf and switches to it. If a shelf has more items than keys, an
//! orange "more" arrow pages through them. Decks with 4 item keys or fewer
//! have one "flip" key instead: it pages, and after the last page goes to
//! the next shelf.
//!
//! ```text
//!  one shelf (MK.2)       several shelves (MK.2)    several shelves (Mini)
//!  [A][A][A][A][A]        [A][A][A][A][A]           [A][A][F]
//!  [A][A][A][A][>]        [A][A][A][>][S]           [⏯][-][+]
//!  [⏮][⏯][⏭][-][+]        [⏮][⏯][⏭][-][+]
//!                         > = more, S = shelf        F = flip
//! ```

mod layout;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use image::{Rgb, RgbImage};
use tracing::{info, warn};

use self::layout::{Action, Control, Layout};
use crate::config::{Color, Config};
use crate::deck::Deck;
use crate::icons::{self, Decor};
use crate::library::{self, Item, ItemId, Kind, Library};
use crate::player::{self, PlayerCmd, PlayerEvent, Start, TrackInfo};
use crate::state::Store;

/// Volume bar resolution; keeps the number of distinct cached key images small.
const VOLUME_LEVELS: f32 = 20.0;

/// Everything a key can show. Used as the cache key for rendered images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Blank,
    /// `progress`: how much of an item that resumes is done, in tenths.
    Item {
        id: ItemId,
        current: bool,
        progress: Option<u8>,
    },
    More {
        page: usize,
        pages: usize,
    },
    /// The shelf the key switches to: its index in the library, and its
    /// position among the shelves on the deck.
    Shelf {
        shelf: usize,
        position: usize,
        count: usize,
    },
    /// `step` of `steps` pages over all shelves.
    Flip {
        step: usize,
        steps: usize,
    },
    Play,
    Pause,
    Prev,
    Next,
    Volume {
        up: bool,
        level: u8,
    },
}

pub struct Ui {
    library: Library,
    base_url: String,
    player: Sender<PlayerCmd>,
    events: Receiver<PlayerEvent>,

    max_volume: f32,
    volume_step: f32,
    volume: f32,

    /// The library shelves the deck shows: those with items.
    deck_shelves: Vec<usize>,
    /// Index into `deck_shelves` of the shelf on the deck.
    shelf: usize,
    /// The page each deck shelf is on.
    pages: Vec<usize>,
    /// Item currently loaded on the speaker (highlighted with a frame).
    current: Option<ItemId>,
    playing: bool,
    /// Where the deck was and how far items got, across restarts.
    store: Store,

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
        store: Store,
    ) -> Ui {
        let shelves = library.shelves();
        let deck_shelves: Vec<usize> = shelves
            .iter()
            .enumerate()
            .filter(|(_, shelf)| !shelf.items.is_empty())
            .map(|(i, _)| i)
            .collect();
        let mut pages: Vec<usize> = deck_shelves
            .iter()
            .map(|&i| store.page(&shelves[i].name))
            .collect();
        if pages.is_empty() {
            pages.push(0);
        }
        let shelf = store
            .shelf()
            .and_then(|name| deck_shelves.iter().position(|&i| shelves[i].name == name))
            .unwrap_or(0);
        Ui {
            pages,
            deck_shelves,
            shelf,
            store,
            library,
            base_url,
            player,
            events,
            max_volume: cfg.max_volume,
            volume_step: cfg.volume_step,
            volume: cfg.start_volume,
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
        let layout = self.layout(rows, cols);
        self.fit(&layout);
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
            if !playing {
                self.store.save_now(Instant::now());
            }
            changed |= self.current != current || self.playing != playing;
            self.current = current;
            self.playing = playing;
        }
        self.store.save_if_due(Instant::now());
        changed
    }

    fn press(&mut self, layout: &Layout, key: usize) {
        let Some(action) = layout.action(key, self.shelf, self.page()) else {
            return;
        };
        match action {
            Action::More => {
                self.pages[self.shelf] = (self.page() + 1) % layout.pages(self.shelf);
                self.remember_place();
            }
            Action::Shelf => {
                self.shelf = (self.shelf + 1) % layout.shelf_count();
                self.remember_place();
            }
            Action::Flip => {
                let (shelf, page) = layout.flip(self.shelf, self.page());
                self.shelf = shelf;
                self.pages[shelf] = page;
                self.remember_place();
            }
            Action::Item(id) if self.current == Some(id) => {
                self.send(PlayerCmd::TogglePause);
                self.playing = !self.playing;
            }
            Action::Item(id) => {
                let item = self.library.item(id);
                let resumes = item.kind.resumes();
                let start = if resumes {
                    self.resume(id)
                } else {
                    Start::default()
                };
                info!(item = %item.name, track = start.track, position = ?start.position, "item pressed");
                let content = player::Content::Tracks {
                    tracks: self.tracks(id),
                    start,
                    progress: resumes,
                };
                self.send(PlayerCmd::Play {
                    item: id,
                    content,
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

    /// Where `id` stopped last time, from the store.
    fn resume(&self, id: ItemId) -> Start {
        let item = self.library.item(id);
        let Some(progress) = self.store.progress(&item.key.0) else {
            return Start::default();
        };
        let files: Vec<&Path> = item.tracks().iter().map(|t| t.rel_path.as_path()).collect();
        let (track, position) = progress.resume_at(&files);
        Start { track, position }
    }

    /// How much of `id` is done, in tenths, for items that resume.
    fn progress_steps(&self, id: ItemId) -> Option<u8> {
        let item = self.library.item(id);
        if !item.kind.resumes() {
            return None;
        }
        let progress = self.store.progress(&item.key.0)?;
        Some((progress.done(item.tracks().len()) * 10.0).round() as u8)
    }

    /// Saves the shelf on the deck and its page.
    fn remember_place(&mut self) {
        if let Some(&shelf) = self.deck_shelves.get(self.shelf) {
            let name = &self.library.shelves()[shelf].name;
            self.store.set_shelf(name);
            self.store.set_page(name, self.pages[self.shelf]);
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
        let total = layout.item_keys + layout.cols;
        (0..total)
            .map(|key| match layout.action(key, self.shelf, self.page()) {
                None => Face::Blank,
                Some(Action::Item(id)) => Face::Item {
                    id,
                    current: self.current == Some(id),
                    progress: self.progress_steps(id),
                },
                Some(Action::More) => Face::More {
                    page: self.page(),
                    pages: layout.pages(self.shelf),
                },
                Some(Action::Shelf) => {
                    let next = (self.shelf + 1) % layout.shelf_count();
                    Face::Shelf {
                        shelf: self.deck_shelves[next],
                        position: next,
                        count: layout.shelf_count(),
                    }
                }
                Some(Action::Flip) => {
                    let (step, steps) = layout.flip_step(self.shelf, self.page());
                    Face::Flip { step, steps }
                }
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
            Face::Item {
                id,
                current: false,
                progress: None,
            } => self.tiles[id].clone(),
            Face::Item {
                id,
                current,
                progress,
            } => icons::decorate(
                &self.tiles[id],
                Decor {
                    current: *current,
                    progress: *progress,
                    ..Decor::default()
                },
            ),
            Face::More { page, pages } => icons::more(size, *page, *pages),
            Face::Shelf {
                shelf,
                position,
                count,
            } => icons::shelf(&self.shelf_tile(*shelf, size), *position, *count),
            Face::Flip { step, steps } => icons::flip(size, *step, *steps),
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

    fn layout(&self, rows: usize, cols: usize) -> Layout {
        let shelves = self.library.shelves();
        let items = self
            .deck_shelves
            .iter()
            .map(|&i| shelves[i].items.clone())
            .collect();
        Layout::new(rows, cols, items)
    }

    /// Keeps the shelf and the pages inside `layout`, e.g. after a smaller
    /// deck was plugged in.
    fn fit(&mut self, layout: &Layout) {
        self.shelf = self.shelf.min(layout.shelf_count() - 1);
        for (shelf, page) in self.pages.iter_mut().enumerate() {
            *page = (*page).min(layout.pages(shelf) - 1);
        }
    }

    fn page(&self) -> usize {
        self.pages[self.shelf]
    }

    /// The picture of a shelf key: the shelf's picture, else the glyph of
    /// its kind on its colour.
    fn shelf_tile(&self, shelf: usize, size: u32) -> RgbImage {
        let shelf = &self.library.shelves()[shelf];
        shelf
            .picture
            .as_deref()
            .and_then(|path| load_tile(path, size))
            .unwrap_or_else(|| {
                icons::glyph_placeholder(glyph(shelf.kind), color(shelf.color, &shelf.name), size)
            })
    }

    /// Loads and shrinks all pictures once, so drawing stays fast on a Pi.
    fn prepare_tiles(&mut self, size: u32) {
        if self.tile_size == size && self.tiles.len() == self.library.items().len() {
            return;
        }
        // Badges tell the shelves apart; with one kind there is nothing to tell.
        let kinds: HashSet<Kind> = self
            .deck_shelves
            .iter()
            .map(|&shelf| self.library.shelves()[shelf].kind)
            .collect();
        let badges = kinds.len() > 1;
        self.tiles = self
            .library
            .items()
            .map(|(id, item)| (id, item_tile(item, size, badges)))
            .collect();
        self.tile_size = size;
    }

    /// Renders what a deck would show, one panel per shelf from top to
    /// bottom, as one picture (for `--preview`).
    pub fn preview(&mut self, rows: usize, cols: usize, size: u32, demo_state: bool) -> RgbImage {
        let layout = self.layout(rows, cols);
        self.prepare_tiles(size);
        let first = self
            .deck_shelves
            .first()
            .and_then(|&shelf| self.library.shelves()[shelf].items.first());
        if demo_state && let Some(&first) = first {
            self.current = Some(first);
            self.playing = true;
        }
        let gap = (size / 6).max(4);
        let width = cols as u32 * (size + gap) + gap;
        let height = rows as u32 * (size + gap) + gap;
        let shelves = layout.shelf_count() as u32;
        let mut canvas = RgbImage::from_pixel(width, height * shelves, Rgb([45, 45, 50]));
        for shelf in 0..layout.shelf_count() {
            self.shelf = shelf;
            let top = shelf as u32 * height;
            for (key, face) in self.faces(&layout).iter().enumerate() {
                let (row, col) = ((key / cols) as u32, (key % cols) as u32);
                let tile = self.render(face, size);
                image::imageops::replace(
                    &mut canvas,
                    &tile,
                    i64::from(gap + col * (size + gap)),
                    i64::from(top + gap + row * (size + gap)),
                );
            }
        }
        self.shelf = 0;
        canvas
    }
}

/// The item's picture, else its cover (with a badge for its kind when
/// `badges`), else the glyph of its kind on its colour.
fn item_tile(item: &Item, size: u32, badges: bool) -> RgbImage {
    let picture = [&item.picture, &item.cover]
        .into_iter()
        .flatten()
        .find_map(|path| load_tile(path, size));
    match picture {
        Some(tile) if badges => icons::decorate(
            &tile,
            Decor {
                badge: Some(glyph(item.kind)),
                ..Decor::default()
            },
        ),
        Some(tile) => tile,
        None => icons::glyph_placeholder(glyph(item.kind), color(item.color, &item.name), size),
    }
}

fn load_tile(path: &Path, size: u32) -> Option<RgbImage> {
    match image::open(path) {
        Ok(img) => Some(icons::thumbnail(&img, size)),
        Err(err) => {
            warn!("cannot read picture {}: {err}", path.display());
            None
        }
    }
}

/// The configured colour, else one derived from `name`.
fn color(configured: Option<Color>, name: &str) -> Rgb<u8> {
    configured.map_or_else(|| icons::name_color(name), |Color(rgb)| Rgb(rgb))
}

fn glyph(kind: Kind) -> icons::Glyph {
    match kind {
        Kind::Music => icons::Glyph::Note,
        Kind::Audiobook => icons::Glyph::Book,
        Kind::Story => icons::Glyph::Star,
        Kind::Radio => icons::Glyph::Waves,
        Kind::Podcast => icons::Glyph::Mic,
        Kind::Spotify => icons::Glyph::Spotify,
    }
}

#[cfg(test)]
mod tests;
