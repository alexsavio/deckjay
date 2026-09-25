use std::sync::mpsc;

use super::layout::control_row;
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

fn controls(l: &Layout, keys: std::ops::Range<usize>) -> Vec<Option<Control>> {
    keys.map(|key| match l.action(key, 0) {
        Some(Action::Control(c)) => Some(c),
        None => None,
        other => panic!("key {key} is {other:?}"),
    })
    .collect()
}

#[test]
fn six_keys_mini() {
    use Control::{PlayPause, VolumeDown, VolumeUp};
    let l = Layout::new(2, 3, 3);
    assert_eq!((l.pages, l.has_more_key), (1, false));
    assert_eq!(l.action(2, 0), Some(Action::Album(2)));
    assert_eq!(
        controls(&l, 3..6),
        [Some(PlayPause), Some(VolumeDown), Some(VolumeUp)]
    );

    let l = Layout::new(2, 3, 4);
    assert_eq!((l.albums_per_page, l.pages), (2, 2));
    assert_eq!(l.action(2, 0), Some(Action::More));
    assert_eq!(l.action(0, 1), Some(Action::Album(2)));
    assert_eq!(l.action(1, 1), Some(Action::Album(3)));
    assert_eq!(Layout::new(2, 3, 17).pages, 9);
}

#[test]
fn eight_keys_neo_and_plus() {
    use Control::{Next, PlayPause, VolumeDown, VolumeUp};
    let l = Layout::new(2, 4, 4);
    assert_eq!((l.pages, l.has_more_key), (1, false));
    assert_eq!(
        controls(&l, 4..8),
        [
            Some(PlayPause),
            Some(Next),
            Some(VolumeDown),
            Some(VolumeUp)
        ]
    );
    assert_eq!(l.action(8, 0), None);
    assert_eq!(Layout::new(2, 4, 25).pages, 9);
}

#[test]
fn thirty_two_keys_with_paging() {
    use Control::{Next, PlayPause, Prev, VolumeDown, VolumeUp};
    let l = Layout::new(4, 8, 30);
    assert_eq!((l.album_slots, l.albums_per_page, l.pages), (24, 23, 2));
    assert_eq!(l.action(23, 0), Some(Action::More));
    assert_eq!(l.action(0, 1), Some(Action::Album(23)));
    assert_eq!(l.action(6, 1), Some(Action::Album(29)));
    assert_eq!(l.action(7, 1), None);
    assert_eq!(
        controls(&l, 24..32),
        [
            Some(Prev),
            Some(PlayPause),
            Some(Next),
            None,
            None,
            None,
            Some(VolumeDown),
            Some(VolumeUp)
        ]
    );
    assert_eq!(l.action(32, 0), None);
}

#[test]
fn fifteen_keys_at_the_paging_edges() {
    let l = Layout::new(3, 5, 0);
    assert_eq!(l.pages, 1);
    assert_eq!(l.action(0, 0), None);

    let l = Layout::new(3, 5, 10);
    assert_eq!((l.pages, l.has_more_key), (1, false));
    assert_eq!(l.action(9, 0), Some(Action::Album(9)));

    let l = Layout::new(3, 5, 11);
    assert_eq!(l.pages, 2);
    assert_eq!(l.action(0, 1), Some(Action::Album(9)));
    assert_eq!(l.action(1, 1), Some(Action::Album(10)));
    assert_eq!(l.action(2, 1), None);
    assert_eq!(l.action(15, 0), None);
}

/// A `Ui` with `albums` empty albums; `volume` is extra config lines.
fn test_ui(albums: usize, volume: &str) -> (Ui, Receiver<PlayerCmd>, Sender<PlayerEvent>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let text = format!("music_dir = \"music\"\nspeaker_host = \"192.168.1.50\"\n{volume}");
    std::fs::write(&path, text).unwrap();
    let cfg = Config::load(&path).unwrap();
    let albums = (0..albums)
        .map(|i| Album {
            name: format!("album {i}"),
            tracks: vec![],
            cover: None,
            cover_rel: None,
        })
        .collect();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let ui = Ui::new(&cfg, albums, "http://host/music".into(), cmd_tx, event_rx);
    (ui, cmd_rx, event_tx)
}

#[test]
fn an_event_queued_before_a_press_does_not_undo_it() {
    let (mut ui, _cmds, events) = test_ui(3, "");
    let layout = Layout::new(3, 5, 3);
    events.send(PlayerEvent::Playing(1)).unwrap();
    assert!(ui.update(&layout, &[0]));
    assert_eq!(ui.current, Some(0));
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
    let layout = Layout::new(3, 5, 1);
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
    let layout = Layout::new(3, 5, 11);
    ui.press(&layout, 9);
    assert_eq!(ui.page, 1);
    ui.press(&layout, 9);
    assert_eq!(ui.page, 0);
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
    let layout = Layout::new(3, 5, 1);
    for _ in 0..10 {
        ui.press(&layout, VOLUME_UP);
    }
    assert_eq!(volumes_sent(&cmds), [0.25, 0.3, 0.35, 0.4]);
    assert_eq!(volume_level(&ui, &layout), 20);
}

#[test]
fn a_zero_max_volume_sends_nothing() {
    let (mut ui, cmds, _events) = test_ui(1, "max_volume = 0.0\n");
    let layout = Layout::new(3, 5, 1);
    ui.press(&layout, VOLUME_UP);
    ui.press(&layout, VOLUME_DOWN);
    assert!(volumes_sent(&cmds).is_empty());
    assert_eq!(volume_level(&ui, &layout), 0);
}
