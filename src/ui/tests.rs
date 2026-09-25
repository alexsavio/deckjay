use std::sync::mpsc;

use super::*;
use crate::library::{Item, ItemKey, Kind, Media, Track};

fn items(kind: Kind, names: std::ops::Range<usize>) -> Vec<Item> {
    names
        .map(|i| Item {
            kind,
            key: ItemKey(format!("music/album {i}")),
            name: format!("album {i}"),
            media: Media::Tracks(
                (1..=3)
                    .map(|t| Track {
                        path: format!("/m/album {i}/{t:02}.mp3").into(),
                        rel_path: format!("music/album {i}/{t:02}.mp3").into(),
                        title: format!("{t:02}"),
                        content_type: "audio/mpeg",
                    })
                    .collect(),
            ),
            cover: None,
            cover_rel: None,
            picture: None,
            color: None,
        })
        .collect()
}

/// A `Ui` over `library`; `volume` is extra config lines.
fn ui_with(library: Library, volume: &str) -> (Ui, Receiver<PlayerCmd>, Sender<PlayerEvent>) {
    ui_with_store(library, volume, Store::in_memory())
}

fn ui_with_store(
    library: Library,
    volume: &str,
    store: Store,
) -> (Ui, Receiver<PlayerCmd>, Sender<PlayerEvent>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let text = format!(
        "speaker_host = \"192.168.1.50\"\n{volume}\n[[source]]\ntype = \"music\"\npath = \"music\"\n"
    );
    std::fs::write(&path, text).unwrap();
    let cfg = Config::load(&path).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let ui = Ui::new(
        &cfg,
        library,
        "http://host/music".into(),
        cmd_tx,
        event_rx,
        store,
    );
    (ui, cmd_rx, event_tx)
}

/// A `Ui` with `albums` empty albums on one shelf; `volume` is extra config lines.
fn test_ui(albums: usize, volume: &str) -> (Ui, Receiver<PlayerCmd>, Sender<PlayerEvent>) {
    ui_with(Library::music(items(Kind::Music, 0..albums)), volume)
}

/// Shelves of `sizes` empty items; ids count on from shelf to shelf.
fn shelves_ui(sizes: &[usize]) -> Ui {
    let mut next = 0;
    let shelves = sizes
        .iter()
        .map(|&n| {
            next += n;
            ("shelf", Kind::Story, items(Kind::Story, next - n..next))
        })
        .collect();
    ui_with(Library::with_shelves(shelves), "").0
}

fn first_item(ui: &Ui, layout: &Layout) -> Option<ItemId> {
    match ui.faces(layout)[0] {
        Face::Item { id, .. } => Some(id),
        _ => None,
    }
}

#[test]
fn an_event_queued_before_a_press_does_not_undo_it() {
    let (mut ui, _cmds, events) = test_ui(3, "");
    let layout = ui.layout(3, 5);
    events.send(PlayerEvent::Playing(ItemId(1))).unwrap();
    assert!(ui.update(&layout, &[0]));
    assert_eq!(ui.current, Some(ItemId(0)));
    assert!(ui.playing);
}

const VOLUME_DOWN: usize = 13;
const VOLUME_UP: usize = 14;

fn volume_level(ui: &Ui, layout: &Layout) -> u8 {
    match ui.faces(layout)[VOLUME_UP] {
        Face::Volume { level, .. } => level,
        ref face => panic!("not a volume key: {face:?}"),
    }
}

#[test]
fn a_volume_shows_the_same_level_going_up_and_down() {
    let (mut ui, _cmds, _events) = test_ui(1, "max_volume = 0.4\nvolume_step = 0.05\n");
    let layout = ui.layout(3, 5);
    let mut going_up = Vec::new();
    for _ in 0..4 {
        ui.press(&layout, VOLUME_UP);
        going_up.push(volume_level(&ui, &layout));
    }
    let mut going_down = Vec::new();
    for _ in 0..4 {
        going_down.push(volume_level(&ui, &layout));
        ui.press(&layout, VOLUME_DOWN);
    }
    going_down.reverse();
    assert_eq!(going_up, going_down);
}

#[test]
fn more_wraps_to_the_first_page() {
    let (mut ui, _cmds, _events) = test_ui(11, "");
    let layout = ui.layout(3, 5);
    ui.press(&layout, 9);
    assert_eq!(ui.page(), 1);
    ui.press(&layout, 9);
    assert_eq!(ui.page(), 0);
}

fn volumes_sent(cmds: &Receiver<PlayerCmd>) -> Vec<f32> {
    cmds.try_iter()
        .filter_map(|cmd| match cmd {
            PlayerCmd::SetVolume(v) => Some(v),
            _ => None,
        })
        .collect()
}

#[test]
fn the_volume_stops_at_max_volume() {
    let (mut ui, cmds, _events) = test_ui(1, "max_volume = 0.4\nvolume_step = 0.05\n");
    let layout = ui.layout(3, 5);
    for _ in 0..10 {
        ui.press(&layout, VOLUME_UP);
    }
    assert_eq!(volumes_sent(&cmds), [0.25, 0.3, 0.35, 0.4]);
    assert_eq!(volume_level(&ui, &layout), 20);
}

#[test]
fn a_zero_max_volume_sends_nothing() {
    let (mut ui, cmds, _events) = test_ui(1, "max_volume = 0.0\n");
    let layout = ui.layout(3, 5);
    ui.press(&layout, VOLUME_UP);
    ui.press(&layout, VOLUME_DOWN);
    assert!(volumes_sent(&cmds).is_empty());
    assert_eq!(volume_level(&ui, &layout), 0);
}

#[test]
fn the_shelf_key_shows_the_next_shelf_and_each_shelf_keeps_its_page() {
    let mut ui = shelves_ui(&[20, 3]);
    let layout = ui.layout(3, 5);
    assert_eq!(
        ui.faces(&layout)[9],
        Face::Shelf {
            shelf: 1,
            position: 1,
            count: 2
        }
    );

    ui.press(&layout, 8);
    assert_eq!(first_item(&ui, &layout), Some(ItemId(8)));
    ui.press(&layout, 9);
    assert_eq!(first_item(&ui, &layout), Some(ItemId(20)));
    assert_eq!(
        ui.faces(&layout)[9],
        Face::Shelf {
            shelf: 0,
            position: 0,
            count: 2
        }
    );
    ui.press(&layout, 9);
    assert_eq!(first_item(&ui, &layout), Some(ItemId(8)));
}

#[test]
fn empty_shelves_are_not_on_the_deck() {
    let (mut ui, _cmds, _events) = ui_with(
        Library::with_shelves(vec![
            ("music", Kind::Music, items(Kind::Music, 0..2)),
            ("usb", Kind::Story, Vec::new()),
            ("books", Kind::Audiobook, items(Kind::Audiobook, 2..3)),
        ]),
        "",
    );
    let layout = ui.layout(3, 5);
    assert_eq!(layout.shelf_count(), 2);
    ui.press(&layout, 9);
    assert_eq!(first_item(&ui, &layout), Some(ItemId(2)));
    assert!(matches!(ui.faces(&layout)[9], Face::Shelf { shelf: 0, .. }));
}

#[test]
fn the_flip_key_pages_then_goes_to_the_next_shelf() {
    let mut ui = shelves_ui(&[5, 1]);
    let layout = ui.layout(2, 3);
    let mut seen = Vec::new();
    for _ in 0..5 {
        let Face::Flip { step, steps } = ui.faces(&layout)[2] else {
            panic!("key 2 is not the flip key");
        };
        seen.push((first_item(&ui, &layout), step, steps));
        ui.press(&layout, 2);
    }
    assert_eq!(
        seen,
        [
            (Some(ItemId(0)), 0, 4),
            (Some(ItemId(2)), 1, 4),
            (Some(ItemId(4)), 2, 4),
            (Some(ItemId(5)), 3, 4),
            (Some(ItemId(0)), 0, 4),
        ]
    );
}

#[test]
fn a_smaller_deck_keeps_the_page_inside_its_shelf() {
    let mut ui = shelves_ui(&[20, 1]);
    let big = ui.layout(4, 8);
    let small = ui.layout(3, 5);
    ui.pages[0] = 5;
    ui.fit(&small);
    assert_eq!(ui.page(), small.pages(0) - 1);
    ui.fit(&big);
    assert_eq!(ui.page(), 0);
}

fn played(cmds: &Receiver<PlayerCmd>) -> Vec<(ItemId, Start, bool)> {
    cmds.try_iter()
        .filter_map(|cmd| match cmd {
            PlayerCmd::Play {
                item,
                content:
                    player::Content::Tracks {
                        start, progress, ..
                    },
                ..
            } => Some((item, start, progress)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_audiobook_starts_where_it_stopped_and_music_from_the_start() {
    let (mut ui, cmds, _events) = ui_with(
        Library::with_shelves(vec![
            ("music", Kind::Music, items(Kind::Music, 0..1)),
            ("books", Kind::Audiobook, items(Kind::Audiobook, 1..2)),
        ]),
        "",
    );
    ui.store.set_progress(
        "music/album 1",
        1,
        Path::new("music/album 1/02.mp3"),
        Duration::from_secs(42),
        None,
    );
    ui.store.set_progress(
        "music/album 0",
        2,
        Path::new("music/album 0/03.mp3"),
        Duration::from_secs(7),
        None,
    );
    let layout = ui.layout(3, 5);

    ui.press(&layout, 0);
    ui.press(&layout, 9);
    ui.press(&layout, 0);

    assert_eq!(
        played(&cmds),
        [
            (ItemId(0), Start::default(), false),
            (
                ItemId(1),
                Start {
                    track: 1,
                    position: Duration::from_secs(42)
                },
                true
            ),
        ]
    );
}

#[test]
fn an_audiobook_key_shows_how_much_is_done() {
    let (mut ui, _cmds, _events) = ui_with(
        Library::with_shelves(vec![(
            "books",
            Kind::Audiobook,
            items(Kind::Audiobook, 0..2),
        )]),
        "",
    );
    ui.store.set_progress(
        "music/album 0",
        1,
        Path::new("music/album 0/02.mp3"),
        Duration::from_secs(30),
        Some(Duration::from_secs(60)),
    );
    let faces = ui.faces(&ui.layout(3, 5));
    assert_eq!(
        faces[0],
        Face::Item {
            id: ItemId(0),
            current: false,
            progress: Some(5)
        }
    );
    assert_eq!(
        faces[1],
        Face::Item {
            id: ItemId(1),
            current: false,
            progress: None
        }
    );
}

#[test]
fn the_deck_comes_back_to_the_saved_shelf_and_pages() {
    let dir = tempfile::tempdir().unwrap();
    let library = || {
        Library::with_shelves(vec![
            ("music", Kind::Music, items(Kind::Music, 0..20)),
            ("books", Kind::Audiobook, items(Kind::Audiobook, 20..21)),
        ])
    };
    let (mut ui, _cmds, _events) = ui_with_store(library(), "", Store::open(dir.path()));
    let layout = ui.layout(3, 5);
    ui.press(&layout, 8);
    ui.press(&layout, 9);
    ui.store.save_now(Instant::now());

    let (again, _cmds, _events) = ui_with_store(library(), "", Store::open(dir.path()));
    assert_eq!((again.shelf, again.pages.clone()), (1, vec![1, 0]));
}
