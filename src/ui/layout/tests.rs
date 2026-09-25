use super::*;

fn ids(range: std::ops::Range<u32>) -> Vec<ItemId> {
    range.map(ItemId).collect()
}

/// A layout for one shelf of `n` items, with ids 0 to n - 1.
fn layout(rows: usize, cols: usize, n: u32) -> Layout {
    Layout::new(rows, cols, vec![ids(0..n)])
}

/// Shelves of `sizes` items; ids count on from one shelf to the next.
fn shelves(rows: usize, cols: usize, sizes: &[u32]) -> Layout {
    let mut next = 0;
    let shelves = sizes
        .iter()
        .map(|&n| {
            next += n;
            ids(next - n..next)
        })
        .collect();
    Layout::new(rows, cols, shelves)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "compared with what Layout::action returns"
)]
fn item(id: u32) -> Option<Action> {
    Some(Action::Item(ItemId(id)))
}

fn controls(l: &Layout, keys: std::ops::Range<usize>) -> Vec<Option<Control>> {
    keys.map(|key| match l.action(key, 0, 0) {
        Some(Action::Control(c)) => Some(c),
        None => None,
        other => panic!("key {key} is {other:?}"),
    })
    .collect()
}

#[test]
fn fifteen_keys_without_paging() {
    let l = layout(3, 5, 7);
    assert_eq!(l.pages(0), 1);
    assert_eq!(l.action(0, 0, 0), item(0));
    assert_eq!(l.action(3, 0, 0), item(3));
    assert_eq!(l.action(4, 0, 0), Some(Action::Control(Control::Power)));
    assert_eq!(l.action(5, 0, 0), item(4));
    assert_eq!(l.action(7, 0, 0), item(6));
    assert_eq!(l.action(8, 0, 0), None);
    assert_eq!(l.action(10, 0, 0), Some(Action::Control(Control::Prev)));
    assert_eq!(l.action(14, 0, 0), Some(Action::Control(Control::VolumeUp)));
}

#[test]
fn fifteen_keys_with_paging() {
    let l = layout(3, 5, 20);
    assert_eq!(l.pages(0), 3); // 8 items per page, the power key and "more"
    assert_eq!(l.action(9, 0, 0), Some(Action::More));
    assert_eq!(l.action(0, 0, 1), item(8));
    assert_eq!(l.action(3, 0, 2), item(19));
    assert_eq!(l.action(5, 0, 2), None);
}

#[test]
fn fifteen_keys_at_the_paging_edges() {
    let l = layout(3, 5, 0);
    assert_eq!(l.pages(0), 1);
    assert_eq!(l.action(0, 0, 0), None);

    let l = layout(3, 5, 9);
    assert_eq!((l.pages(0), l.page_size(0)), (1, (9, false)));
    assert_eq!(l.action(9, 0, 0), item(8));

    let l = layout(3, 5, 10);
    assert_eq!(l.pages(0), 2);
    assert_eq!(l.action(0, 0, 1), item(8));
    assert_eq!(l.action(1, 0, 1), item(9));
    assert_eq!(l.action(2, 0, 1), None);
    assert_eq!(l.action(15, 0, 0), None);
}

#[test]
fn xl_controls_sit_at_both_ends() {
    let row = control_row(8);
    assert_eq!(row[1], Some(Control::PlayPause));
    assert_eq!(row[4], None);
    assert_eq!(row[7], Some(Control::VolumeUp));
}

#[test]
fn six_keys_mini() {
    use Control::{PlayPause, VolumeDown, VolumeUp};
    let l = layout(2, 3, 3);
    assert_eq!((l.pages(0), l.page_size(0)), (1, (3, false)));
    assert_eq!(l.action(2, 0, 0), item(2));
    assert_eq!(
        controls(&l, 3..6),
        [Some(PlayPause), Some(VolumeDown), Some(VolumeUp)]
    );

    let l = layout(2, 3, 4);
    assert_eq!((l.page_size(0), l.pages(0)), ((2, true), 2));
    assert_eq!(l.action(2, 0, 0), Some(Action::More));
    assert_eq!(l.action(0, 0, 1), item(2));
    assert_eq!(l.action(1, 0, 1), item(3));
    assert_eq!(layout(2, 3, 17).pages(0), 9);
}

#[test]
fn eight_keys_neo_and_plus() {
    use Control::{Next, PlayPause, VolumeDown, VolumeUp};
    let l = layout(2, 4, 4);
    assert_eq!((l.pages(0), l.page_size(0)), (1, (4, false)));
    assert_eq!(
        controls(&l, 4..8),
        [
            Some(PlayPause),
            Some(Next),
            Some(VolumeDown),
            Some(VolumeUp)
        ]
    );
    assert_eq!(l.action(8, 0, 0), None);
    assert_eq!(layout(2, 4, 25).pages(0), 9);
}

#[test]
fn thirty_two_keys_with_paging() {
    use Control::{Next, PlayPause, Power, Prev, VolumeDown, VolumeUp};
    let l = layout(4, 8, 30);
    assert_eq!(
        (l.item_keys, l.page_size(0), l.pages(0)),
        (24, (23, true), 2)
    );
    assert_eq!(l.action(23, 0, 0), Some(Action::More));
    assert_eq!(l.action(0, 0, 1), item(23));
    assert_eq!(l.action(6, 0, 1), item(29));
    assert_eq!(l.action(7, 0, 1), None);
    assert_eq!(
        controls(&l, 24..32),
        [
            Some(Prev),
            Some(PlayPause),
            Some(Next),
            None,
            None,
            Some(Power),
            Some(VolumeDown),
            Some(VolumeUp)
        ]
    );
    assert_eq!(l.action(32, 0, 0), None);
}

#[test]
fn no_shelves_are_one_empty_shelf() {
    let l = Layout::new(3, 5, Vec::new());
    assert_eq!((l.shelf_count(), l.pages(0)), (1, 1));
    assert_eq!(l.action(0, 0, 0), None);
    assert_eq!(l.action(9, 0, 0), None);
}

#[test]
fn several_shelves_put_the_shelf_key_last() {
    let l = shelves(3, 5, &[3, 2]);
    assert_eq!(l.shelf_count(), 2);
    assert_eq!(l.action(9, 0, 0), Some(Action::Shelf));
    assert_eq!(l.action(9, 1, 0), Some(Action::Shelf));
    assert_eq!(l.action(2, 0, 0), item(2));
    assert_eq!(l.action(3, 0, 0), None);
    assert_eq!(l.action(0, 1, 0), item(3));
    assert_eq!(l.action(1, 1, 0), item(4));
    assert_eq!(l.action(2, 1, 0), None);
}

#[test]
fn a_full_shelf_gets_more_just_before_the_shelf_key() {
    let l = shelves(3, 5, &[8, 9, 20]);
    assert_eq!((l.page_size(0), l.pages(0)), ((8, false), 1));
    assert_eq!(l.action(8, 0, 0), item(7));

    assert_eq!((l.page_size(1), l.pages(1)), ((7, true), 2));
    assert_eq!(l.action(8, 1, 0), Some(Action::More));
    assert_eq!(l.action(9, 1, 0), Some(Action::Shelf));
    assert_eq!(l.action(0, 1, 1), item(15));
    assert_eq!(l.action(1, 1, 1), item(16));
    assert_eq!(l.action(2, 1, 1), None);

    assert_eq!(l.pages(2), 3);
    assert_eq!(l.action(6, 2, 2), item(36));
    assert_eq!(l.action(7, 2, 2), None);
}

#[test]
fn xl_shelves() {
    let l = shelves(4, 8, &[30, 1]);
    assert_eq!(l.page_size(0), (22, true));
    assert_eq!(l.action(22, 0, 0), Some(Action::More));
    assert_eq!(l.action(23, 0, 0), Some(Action::Shelf));
    assert_eq!(l.action(7, 0, 1), item(29));
    assert_eq!(l.action(0, 1, 0), item(30));
}

#[test]
fn small_decks_flip_through_pages_then_shelves() {
    for (rows, cols) in [(2, 3), (2, 4)] {
        let l = shelves(rows, cols, &[5, 1]);
        let flip = l.item_keys - 1;
        assert_eq!(l.action(flip, 0, 0), Some(Action::Flip), "{rows}x{cols}");
        assert_eq!(l.action(flip, 1, 0), Some(Action::Flip), "{rows}x{cols}");
        assert_eq!(l.page_size(1), (flip, false), "{rows}x{cols}");
    }

    let l = shelves(2, 3, &[5, 1]);
    assert_eq!((l.pages(0), l.pages(1)), (3, 1));
    assert_eq!(l.action(0, 0, 2), item(4));
    assert_eq!(l.action(1, 0, 2), None);
    assert_eq!(l.flip(0, 0), (0, 1));
    assert_eq!(l.flip(0, 2), (1, 0));
    assert_eq!(l.flip(1, 0), (0, 0));
    assert_eq!(l.flip_step(0, 2), (2, 4));
    assert_eq!(l.flip_step(1, 0), (3, 4));
}

#[test]
fn one_shelf_on_a_small_deck_keeps_the_more_key() {
    let l = shelves(2, 3, &[5]);
    assert_eq!(l.action(2, 0, 0), Some(Action::More));
}

#[test]
fn the_power_key_takes_a_free_control_key_else_the_top_right_item_key() {
    let power = Some(Action::Control(Control::Power));
    let keys_with_power = |l: &Layout, keys: usize| -> Vec<usize> {
        (0..keys).filter(|&k| l.action(k, 0, 0) == power).collect()
    };
    assert_eq!(keys_with_power(&layout(3, 5, 20), 15), [4], "MK.2");
    assert_eq!(keys_with_power(&layout(4, 8, 40), 32), [29], "XL");
    assert_eq!(keys_with_power(&layout(2, 3, 9), 6), [0; 0], "Mini");
    assert_eq!(keys_with_power(&layout(2, 4, 9), 8), [0; 0], "Neo");
    assert_eq!(
        keys_with_power(&shelves(3, 5, &[20, 3]), 15),
        [4],
        "MK.2 with shelves"
    );
}
