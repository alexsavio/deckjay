//! Which key shows what: items on top, controls in the bottom row, and the
//! keys that move between pages and shelves in the last item keys.

use crate::library::ItemId;

/// Below this many item keys, a shelf key plus a "more" key would leave too
/// few keys for items, so one flip key does both jobs.
const MIN_ITEM_KEYS_FOR_SHELF_KEY: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Control {
    Prev,
    PlayPause,
    Next,
    VolumeDown,
    VolumeUp,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action {
    Item(ItemId),
    /// The next page of the shelf.
    More,
    /// The next shelf.
    Shelf,
    /// The next page, or after the last page the next shelf.
    Flip,
    Control(Control),
}

/// What the last item key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nav {
    /// One shelf: every item key shows items, the last one "more" if needed.
    Items,
    Shelf,
    Flip,
}

pub(super) struct Layout {
    pub(super) cols: usize,
    /// Keys above the control row.
    pub(super) item_keys: usize,
    nav: Nav,
    controls: Vec<Option<Control>>,
    /// The items of each shelf, in the order the keys show them.
    shelves: Vec<Vec<ItemId>>,
}

impl Layout {
    /// No shelves count as one empty shelf.
    pub(super) fn new(rows: usize, cols: usize, shelves: Vec<Vec<ItemId>>) -> Layout {
        // With one item key, paging would turn it into the "more" key and
        // hide every item; the deck backends only open bigger grids.
        debug_assert!(
            rows >= 2 && (rows - 1) * cols >= 2,
            "no room for items on a {rows}x{cols} deck"
        );
        let item_keys = (rows - 1) * cols;
        let shelves = if shelves.is_empty() {
            vec![Vec::new()]
        } else {
            shelves
        };
        let nav = if shelves.len() == 1 {
            Nav::Items
        } else if item_keys < MIN_ITEM_KEYS_FOR_SHELF_KEY {
            Nav::Flip
        } else {
            Nav::Shelf
        };
        Layout {
            cols,
            item_keys,
            nav,
            controls: control_row(cols),
            shelves,
        }
    }

    pub(super) fn shelf_count(&self) -> usize {
        self.shelves.len()
    }

    /// How many items one page of `shelf` shows, and whether it has a
    /// "more" key (right after the items).
    fn page_size(&self, shelf: usize) -> (usize, bool) {
        let free = match self.nav {
            Nav::Items => self.item_keys,
            Nav::Shelf | Nav::Flip => self.item_keys - 1,
        };
        if self.nav != Nav::Flip && self.shelves[shelf].len() > free {
            (free - 1, true)
        } else {
            (free, false)
        }
    }

    pub(super) fn pages(&self, shelf: usize) -> usize {
        let (size, _) = self.page_size(shelf);
        self.shelves[shelf].len().div_ceil(size.max(1)).max(1)
    }

    pub(super) fn action(&self, key: usize, shelf: usize, page: usize) -> Option<Action> {
        if key >= self.item_keys {
            return self
                .controls
                .get(key - self.item_keys)
                .copied()
                .flatten()
                .map(Action::Control);
        }
        if key == self.item_keys - 1 {
            match self.nav {
                Nav::Shelf => return Some(Action::Shelf),
                Nav::Flip => return Some(Action::Flip),
                Nav::Items => {}
            }
        }
        let (size, more) = self.page_size(shelf);
        if more && key == size {
            return Some(Action::More);
        }
        if key >= size {
            return None;
        }
        self.shelves[shelf]
            .get(page * size + key)
            .copied()
            .map(Action::Item)
    }

    /// Where the flip key goes from `page` of `shelf`: `(shelf, page)`.
    pub(super) fn flip(&self, shelf: usize, page: usize) -> (usize, usize) {
        if page + 1 < self.pages(shelf) {
            (shelf, page + 1)
        } else {
            ((shelf + 1) % self.shelf_count(), 0)
        }
    }

    /// `page` of `shelf` counted over the pages of all shelves, and the
    /// number of those pages: what the flip key's dots show.
    pub(super) fn flip_step(&self, shelf: usize, page: usize) -> (usize, usize) {
        let before: usize = (0..shelf).map(|s| self.pages(s)).sum();
        let all = (0..self.shelf_count()).map(|s| self.pages(s)).sum();
        (before + page, all)
    }
}

/// Controls for the bottom row, depending on how wide the deck is.
pub(super) fn control_row(cols: usize) -> Vec<Option<Control>> {
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

#[cfg(test)]
mod tests;
