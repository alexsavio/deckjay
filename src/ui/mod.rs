//! What the kids see and press.
//!
//! The bottom row holds the controls, every other key is an item (an album
//! cover, a book, a story). Each source is a shelf of items; the deck shows
//! one shelf at a time. With several shelves, the last item key shows the
//! next shelf and switches to it. If a shelf has more items than keys, an
//! orange "more" arrow pages through them. Decks with fewer than 6 item keys
//! (Mini, Neo, Plus) have one "flip" key instead: it pages, and after the
//! last page goes to the next shelf.
//!
//! ```text
//!  one shelf (MK.2)       several shelves (MK.2)    several shelves (Mini)
//!  [A][A][A][A][P]        [A][A][A][A][P]           [A][A][F]
//!  [A][A][A][A][>]        [A][A][A][>][S]           [⏯][-][+]
//!  [⏮][⏯][⏭][-][+]        [⏮][⏯][⏭][-][+]
//!  P = power              > = more, S = shelf        F = flip
//!
//! The power key takes a free control key (XL), else the top-right item key
//! (MK.2); the Mini, Neo and Plus have none. It stops everything and dims
//! the deck; the next press only lights it again.
//! ```

mod layout;
mod podcasts;
mod tiles;

pub use self::podcasts::PodcastThread;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use image::{Rgb, RgbImage};
use tracing::{info, warn};

use self::layout::{Action, Control, Layout};
use crate::config::Config;
use crate::deck::Deck;
use crate::icons::{self, Decor};
use crate::library::{self, ItemId, Kind, Library, Media};
use crate::player::{self, PlayerCmd, PlayerEvent, Start, TrackInfo};
use crate::state::Store;

/// Volume bar resolution; keeps the number of distinct cached key images small.
const VOLUME_LEVELS: f32 = 20.0;

/// Everything a key can show. Used as the cache key for rendered images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Blank,
    /// `progress`: how much of an item that resumes is done, in tenths.
    /// `new`: a podcast episode that never played.
    Item {
        id: ItemId,
        current: bool,
        progress: Option<u8>,
        new: bool,
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
    Power,
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
    /// The saved shelf, while it is not on the deck yet (a podcast shelf
    /// whose cache is empty at start); the deck moves there when it fills,
    /// unless a key moved the deck first.
    waiting_for: Option<String>,
    /// Item currently loaded on the speaker (highlighted with a frame).
    current: Option<ItemId>,
    playing: bool,
    /// Where the deck was and how far items got, across restarts.
    store: Store,

    tiles: HashMap<ItemId, RgbImage>,
    tile_size: u32,
    /// Whether the tiles carry kind badges.
    badges: bool,
    /// Items whose look changed since the deck last drew them.
    restyled: HashSet<ItemId>,
    /// The power key dimmed the deck.
    asleep: bool,

    /// One per podcast source.
    podcasts: Vec<podcasts::PodcastThread>,
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
        let deck_shelves = non_empty(&library);
        let mut pages: Vec<usize> = deck_shelves
            .iter()
            .map(|&i| store.page(&shelves[i].name))
            .collect();
        if pages.is_empty() {
            pages.push(0);
        }
        let saved = store.shelf().map(|name| {
            (
                name,
                deck_shelves.iter().position(|&i| shelves[i].name == name),
            )
        });
        let shelf = saved.and_then(|(_, position)| position).unwrap_or(0);
        let waiting_for = match saved {
            Some((name, None)) => Some(name.to_string()),
            _ => None,
        };
        Ui {
            waiting_for,
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
            badges: false,
            restyled: HashSet::new(),
            asleep: false,
            podcasts: Vec::new(),
        }
    }

    /// Drives a connected deck until it is unplugged (returns the error then).
    pub fn run(&mut self, deck: &mut Deck, brightness: u8) -> Result<()> {
        let mut dimmed = self.asleep;
        deck.set_brightness(if dimmed { 0 } else { brightness })?;
        let (rows, cols) = deck.layout();
        let mut layout = self.layout(rows, cols);
        self.fit(&layout);
        self.prepare_tiles(deck.key_size());
        self.draw(deck, &layout)?;

        loop {
            let mut keys = deck.pressed_keys(Duration::from_millis(100))?;
            self.wake(&mut keys);
            let mut changed = self.update(&layout, &keys);
            if self.take_snapshots() {
                layout = self.layout(rows, cols);
                self.fit(&layout);
                let remade = self.prepare_tiles(deck.key_size());
                let shown: HashSet<ItemId> = self
                    .deck_shelves
                    .iter()
                    .flat_map(|&shelf| self.library.shelves()[shelf].items.iter().copied())
                    .collect();
                // The deck caches images by face, and a remade tile keeps its face.
                let restyled = std::mem::take(&mut self.restyled);
                deck.retain(|face| match face {
                    Face::Item { id, .. } => {
                        !remade && shown.contains(id) && !restyled.contains(id)
                    }
                    _ => true,
                });
                changed = true;
            }
            if changed {
                self.draw(deck, &layout)?;
            }
            if self.asleep != dimmed {
                dimmed = self.asleep;
                deck.set_brightness(if dimmed { 0 } else { brightness })?;
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
        self.pin_playing();
        changed
    }

    /// Applies status updates from the speaker. Returns true if anything changed.
    pub fn handle_events(&mut self) -> bool {
        let mut changed = false;
        let events: Vec<PlayerEvent> = self.events.try_iter().collect();
        for event in events {
            let (current, playing) = match event {
                PlayerEvent::Playing(item) => (Some(item), true),
                PlayerEvent::Paused(item) => (Some(item), false),
                PlayerEvent::Stopped => (None, false),
                PlayerEvent::Progress {
                    item,
                    track,
                    position,
                    duration,
                } => {
                    changed |= self.record_progress(item, track, position, duration);
                    continue;
                }
                PlayerEvent::Finished(item) => {
                    let key = &self.library.item(item).key.0;
                    self.store.finish(key);
                    self.store.save_now(Instant::now());
                    changed = true;
                    continue;
                }
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
                let content = self.content(id);
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
            Control::Power => self.power_off(),
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

    /// What pressing `id` plays: its tracks, from where it stopped if it
    /// resumes, or its stream.
    fn content(&self, id: ItemId) -> player::Content {
        let item = self.library.item(id);
        if let Media::Spotify { uri } = &item.media {
            info!(item = %item.name, "playlist pressed");
            return player::Content::Spotify(player::Playlist {
                uri: uri.clone(),
                name: item.name.clone(),
            });
        }
        if let Media::Stream { url } = &item.media {
            info!(item = %item.name, "station pressed");
            return player::Content::Stream(player::Station {
                url: url.clone(),
                content_type: None,
                name: item.name.clone(),
                cover_url: None,
            });
        }
        let resumes = item.kind.resumes();
        let start = if resumes {
            self.resume(id)
        } else {
            Start::default()
        };
        info!(item = %item.name, track = start.track, position = ?start.position, "item pressed");
        player::Content::Tracks {
            tracks: self.tracks(id),
            start,
            progress: resumes,
        }
    }

    /// Stops everything and dims the deck; the next press only wakes it.
    fn power_off(&mut self) {
        info!("power key: everything stops");
        self.send(PlayerCmd::Off);
        self.current = None;
        self.playing = false;
        self.store.save_now(Instant::now());
        self.asleep = true;
    }

    /// The press that wakes a dimmed deck does nothing else: a child sees
    /// the keys before they choose.
    fn wake(&mut self, keys: &mut Vec<usize>) {
        if self.asleep && !keys.is_empty() {
            self.asleep = false;
            keys.clear();
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

    /// Saves how far `id` got. Returns whether its key changes.
    fn record_progress(
        &mut self,
        id: ItemId,
        track: usize,
        position: Duration,
        duration: Option<Duration>,
    ) -> bool {
        let item = self.library.item(id);
        let Some(file) = item.tracks().get(track).map(|t| t.rel_path.clone()) else {
            return false;
        };
        if !item.kind.resumes() {
            return false;
        }
        let before = (self.progress_steps(id), self.is_new(id));
        let key = item.key.0.clone();
        self.store
            .set_progress(&key, track, &file, position, duration);
        before != (self.progress_steps(id), self.is_new(id))
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

    fn is_new(&self, id: ItemId) -> bool {
        let item = self.library.item(id);
        item.kind == Kind::Podcast && self.store.progress(&item.key.0).is_none()
    }

    /// Recomputes the shelves on the deck after the library changed; the
    /// shelf on the deck and the pages stay where they were.
    fn reshelve(&mut self) {
        let current = self.deck_shelves.get(self.shelf).copied();
        let pages: HashMap<usize, usize> = self
            .deck_shelves
            .iter()
            .copied()
            .zip(self.pages.iter().copied())
            .collect();
        self.deck_shelves = non_empty(&self.library);
        let shelves = self.library.shelves();
        self.pages = self
            .deck_shelves
            .iter()
            .map(|&shelf| {
                pages
                    .get(&shelf)
                    .copied()
                    .unwrap_or_else(|| self.store.page(&shelves[shelf].name))
            })
            .collect();
        if self.pages.is_empty() {
            self.pages.push(0);
        }
        let waited = self.waiting_for.as_deref().and_then(|name| {
            self.deck_shelves
                .iter()
                .position(|&s| shelves[s].name == name)
        });
        if waited.is_some() {
            self.waiting_for = None;
        }
        self.shelf = waited
            .or_else(|| {
                current.and_then(|current| self.deck_shelves.iter().position(|&s| s == current))
            })
            .unwrap_or(0);
    }

    /// Saves the shelf on the deck and its page.
    fn remember_place(&mut self) {
        self.waiting_for = None;
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
                    new: self.is_new(id),
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
                    Control::Power => Face::Power,
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
                new: false,
            } => self.tiles[id].clone(),
            Face::Item {
                id,
                current,
                progress,
                new,
            } => icons::decorate(
                &self.tiles[id],
                Decor {
                    current: *current,
                    progress: *progress,
                    new: *new,
                    badge: None,
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
            Face::Power => icons::power(size),
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

/// The library shelves with items: the ones the deck shows.
fn non_empty(library: &Library) -> Vec<usize> {
    library
        .shelves()
        .iter()
        .enumerate()
        .filter(|(_, shelf)| !shelf.items.is_empty())
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests;
