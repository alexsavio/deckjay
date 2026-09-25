//! Key pictures of items and shelves, loaded and shrunk once per key size.

use std::collections::HashSet;
use std::path::Path;

use image::{Rgb, RgbImage};
use tracing::warn;

use super::Ui;
use crate::config::Color;
use crate::icons::{self, Decor};
use crate::library::{Item, Kind};

impl Ui {
    /// The picture of a shelf key: the shelf's picture, else the glyph of
    /// its kind on its colour.
    pub(super) fn shelf_tile(&self, shelf: usize, size: u32) -> RgbImage {
        let shelf = &self.library.shelves()[shelf];
        shelf
            .picture
            .as_deref()
            .and_then(|path| load_tile(path, size))
            .unwrap_or_else(|| {
                icons::glyph_placeholder(glyph(shelf.kind), color(shelf.color, &shelf.name), size)
            })
    }

    /// Loads and shrinks the pictures of items that have no tile yet, so
    /// drawing stays fast on a Pi. Returns true when every tile was made
    /// again (a new key size, or badges that come or go).
    pub(super) fn prepare_tiles(&mut self, size: u32) -> bool {
        // Badges tell the shelves apart; with one kind there is nothing to tell.
        let kinds: HashSet<Kind> = self
            .deck_shelves
            .iter()
            .map(|&shelf| self.library.shelves()[shelf].kind)
            .collect();
        let badges = kinds.len() > 1;
        let remade = self.tile_size != size || self.badges != badges;
        if remade {
            self.tiles.clear();
            self.tile_size = size;
            self.badges = badges;
        }
        for (id, item) in self.library.items() {
            self.tiles
                .entry(id)
                .or_insert_with(|| item_tile(item, size, badges));
        }
        remade
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
