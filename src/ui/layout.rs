//! Which key shows what: albums on top, controls in the bottom row, and a
//! "more" key when the albums do not fit.

use crate::library::ItemId;

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
    More,
    Control(Control),
}

pub(super) struct Layout {
    pub(super) cols: usize,
    pub(super) album_slots: usize,
    pub(super) albums_per_page: usize,
    pub(super) pages: usize,
    pub(super) has_more_key: bool,
    controls: Vec<Option<Control>>,
    /// The items in the order the album keys show them, page after page.
    items: Vec<ItemId>,
}

impl Layout {
    pub(super) fn new(rows: usize, cols: usize, items: &[ItemId]) -> Layout {
        // With one album key, paging would turn it into the "more" key and
        // hide every album; the deck backends only open bigger grids.
        debug_assert!(
            rows >= 2 && (rows - 1) * cols >= 2,
            "no room for albums on a {rows}x{cols} deck"
        );
        let album_slots = (rows - 1) * cols;
        let has_more_key = items.len() > album_slots;
        let albums_per_page = if has_more_key {
            album_slots - 1
        } else {
            album_slots
        };
        let pages = items.len().div_ceil(albums_per_page.max(1)).max(1);
        Layout {
            cols,
            album_slots,
            albums_per_page,
            pages,
            has_more_key,
            controls: control_row(cols),
            items: items.to_vec(),
        }
    }

    pub(super) fn action(&self, key: usize, page: usize) -> Option<Action> {
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
        self.items.get(index).copied().map(Action::Item)
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
