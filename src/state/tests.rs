use super::*;

fn progress(track: usize, file: &str, position_ms: u64) -> Progress {
    Progress {
        track,
        file: file.into(),
        position_ms,
        duration_ms: None,
        finished: false,
        saved: 0,
    }
}

#[test]
fn a_missing_file_starts_empty_and_saves_to_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let mut store = Store::open(&state_dir);
    assert_eq!(store.shelf(), None);
    assert_eq!(store.page("music"), 0);

    store.set_shelf("books");
    store.set_page("music", 2);
    store.set_progress(
        "books/Pippi",
        3,
        Path::new("books/Pippi/04.mp3"),
        Duration::from_millis(61_500),
        Some(Duration::from_secs(300)),
    );
    store.save_now(Instant::now());

    let again = Store::open(&state_dir);
    assert_eq!(again.shelf(), Some("books"));
    assert_eq!(again.page("music"), 2);
    let saved = again.progress("books/Pippi").unwrap();
    assert_eq!(
        (
            saved.track,
            saved.file.as_str(),
            saved.position_ms,
            saved.duration_ms
        ),
        (3, "books/Pippi/04.mp3", 61_500, Some(300_000))
    );
    assert!(!state_dir.join("state.json.tmp").exists());
}

#[test]
fn saves_wait_ten_seconds_after_the_last_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut store = Store::open(dir.path());
    let start = Instant::now();

    store.set_page("music", 1);
    store.save_if_due(start);
    assert!(path.exists(), "the first change is saved at once");

    store.set_page("music", 2);
    store.save_if_due(start + Duration::from_secs(9));
    assert_eq!(Store::open(dir.path()).page("music"), 1);

    store.save_if_due(start + Duration::from_secs(10));
    assert_eq!(Store::open(dir.path()).page("music"), 2);
}

#[test]
fn nothing_is_written_without_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path());
    store.set_page("music", 0);
    store.save_now(Instant::now());
    store.save_if_due(Instant::now());
    assert!(!dir.path().join("state.json").exists());
}

#[test]
fn a_broken_file_is_kept_aside_and_the_store_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state.json"), "{ not json").unwrap();

    let mut store = Store::open(dir.path());

    assert_eq!(store.shelf(), None);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("state.json.bad")).unwrap(),
        "{ not json"
    );
    store.set_shelf("music");
    store.save_now(Instant::now());
    assert_eq!(Store::open(dir.path()).shelf(), Some("music"));
}

#[test]
fn a_file_from_a_newer_version_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, r#"{"version": 99, "shelf": "future"}"#).unwrap();

    let mut store = Store::open(dir.path());
    store.set_shelf("music");
    store.save_now(Instant::now());

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"version": 99, "shelf": "future"}"#
    );
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_written_keeps_the_state_in_memory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

    let mut store = Store::open(&dir.path().join("state"));
    store.set_shelf("music");
    store.save_now(Instant::now());

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(store.shelf(), Some("music"));
    assert!(!dir.path().join("state").exists());
}

#[test]
fn finishing_marks_the_item_to_start_over() {
    let mut store = Store::in_memory();
    store.set_progress(
        "books/A",
        2,
        Path::new("books/A/03.mp3"),
        Duration::from_secs(5),
        None,
    );
    store.finish("books/A");
    store.finish("books/B");

    let files = [Path::new("books/A/01.mp3"), Path::new("books/A/03.mp3")];
    assert_eq!(
        store.progress("books/A").unwrap().resume_at(&files),
        (0, Duration::ZERO)
    );
    assert!(store.progress("books/B").unwrap().finished);
}

#[test]
fn resume_finds_the_track_by_its_file() {
    let files = [
        Path::new("b/A/00 Intro.mp3"),
        Path::new("b/A/01.mp3"),
        Path::new("b/A/02.mp3"),
    ];
    let at = |p: Progress| p.resume_at(&files);
    assert_eq!(
        at(progress(2, "b/A/02.mp3", 1500)),
        (2, Duration::from_millis(1500))
    );
    assert_eq!(
        at(progress(1, "b/A/02.mp3", 1500)),
        (2, Duration::from_millis(1500)),
        "a file was added before the saved track"
    );
    assert_eq!(
        at(progress(1, "b/A/gone.mp3", 1500)),
        (0, Duration::ZERO),
        "the saved track is gone"
    );
}

#[test]
fn done_counts_tracks_and_the_position_in_the_track() {
    let mut p = progress(1, "x", 30_000);
    assert!((p.done(4) - 0.25).abs() < f32::EPSILON);
    p.duration_ms = Some(60_000);
    assert!((p.done(4) - 0.375).abs() < f32::EPSILON);
    p.duration_ms = Some(0);
    assert!((p.done(4) - 0.25).abs() < f32::EPSILON);
    p.finished = true;
    assert!((p.done(4) - 1.0).abs() < f32::EPSILON);
    assert!((progress(0, "x", 0).done(0)).abs() < f32::EPSILON);
}

#[cfg(unix)]
#[test]
fn a_folder_that_becomes_writable_gets_the_state_that_waited() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mode =
        |m| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o500);
    let mut store = Store::open(dir.path());
    store.set_shelf("books");
    store.save_now(Instant::now());
    store.save_now(Instant::now());
    mode(0o700);
    assert!(!dir.path().join("state.json").exists());

    store.save_now(Instant::now());

    assert_eq!(Store::open(dir.path()).shelf(), Some("books"));
}
