//! Scans the folder of a source. Every folder at the top is one item: its
//! audio files and those one folder down (e.g. `CD1/`, `CD2/`). Every audio
//! file at the top is an item of its own.
//!
//! ```text
//! music/
//!   01 Animal Songs/
//!     cover.jpg
//!     01 The Elephant.mp3
//!     02 Five Little Ducks.mp3
//!   02 Bedtime Stories/
//!     CD1/01 The Moon.mp3
//!     CD2/01 The Stars.mp3
//!   03 Lullaby.mp3
//!   03 Lullaby.jpg          <- the cover of 03 Lullaby.mp3
//! ```
//!
//! Items and tracks are sorted by name, so number prefixes control the order.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Item, ItemKey, Media, Track};
use crate::config::Source;

/// Cover file names (without extension), best match first.
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "album"];

/// `key.png` or `key.jpg` in a folder: a picture for the deck only, never
/// sent to the speaker as the cover.
const KEY_PICTURE: &str = "key";

/// The items of `source`, in name order.
pub(super) fn scan(source: &Source) -> Result<Vec<Item>> {
    let entries = usable_entries(&source.path).with_context(|| {
        format!(
            "cannot read the folder of source {}: {}",
            source.name,
            source.path.display()
        )
    })?;
    let top_files: Vec<PathBuf> = entries.iter().filter(|p| p.is_file()).cloned().collect();
    let items: Vec<Item> = entries
        .iter()
        .filter_map(|entry| {
            if entry.is_dir() {
                folder_item(source, entry)
            } else {
                file_item(source, entry, &top_files)
            }
        })
        .collect();
    for name in source.items.keys() {
        let key = format!("{}/{name}", source.name);
        if !items.iter().any(|i| i.key.0 == key || i.name == *name) {
            tracing::warn!(
                "[source.item.{name:?}] of source {} matches no item",
                source.name
            );
        }
    }
    Ok(items)
}

fn folder_item(source: &Source, dir: &Path) -> Option<Item> {
    let entries = usable_entries(dir)
        .inspect_err(|err| tracing::warn!("skipping {}: {err}", dir.display()))
        .ok()?;
    let top_files: Vec<PathBuf> = entries.iter().filter(|p| p.is_file()).cloned().collect();
    let mut files = top_files.clone();
    for sub in entries.iter().filter(|p| p.is_dir()) {
        match usable_entries(sub) {
            Ok(inner) => files.extend(inner.into_iter().filter(|p| p.is_file())),
            Err(err) => tracing::warn!("skipping {}: {err}", sub.display()),
        }
    }
    // Whole paths sort a folder's own files first, then CD1/ before CD2/.
    files.sort();

    let tracks = tracks(source, &files);
    if tracks.is_empty() {
        tracing::warn!("skipping {}: no playable audio files", dir.display());
        return None;
    }
    let name = file_name(dir);
    let cover = find_cover(&top_files).or_else(|| find_cover(&files));
    let key_picture = top_files
        .iter()
        .find(|f| is_image(f) && has_stem(f, KEY_PICTURE))
        .cloned();
    let mut item = item(source, &name, name.clone(), tracks, cover);
    item.picture = item.picture.or(key_picture);
    Some(item)
}

fn file_item(source: &Source, file: &Path, top_files: &[PathBuf]) -> Option<Item> {
    let tracks = tracks(source, &[file.to_path_buf()]);
    if tracks.is_empty() {
        return None;
    }
    let stem = file.file_stem()?.to_string_lossy().into_owned();
    let cover = top_files
        .iter()
        .find(|f| is_image(f) && f.file_stem().is_some_and(|s| s.to_string_lossy() == stem))
        .cloned();
    Some(item(source, &file_name(file), stem, tracks, cover))
}

/// `key_name` is the entry's name in the source folder. The picture and
/// colour come from the source's `[source.item]` table for it, if any.
fn item(
    source: &Source,
    key_name: &str,
    name: String,
    tracks: Vec<Track>,
    cover: Option<PathBuf>,
) -> Item {
    let look = source
        .items
        .get(key_name)
        .or_else(|| source.items.get(&name))
        .cloned()
        .unwrap_or_default();
    Item {
        kind: source.kind.into(),
        key: ItemKey(format!("{}/{key_name}", source.name)),
        name,
        media: Media::Tracks(tracks),
        cover_rel: cover.as_deref().and_then(|c| served_path(source, c)),
        cover,
        picture: look.picture,
        color: look.color,
    }
}

fn tracks(source: &Source, files: &[PathBuf]) -> Vec<Track> {
    files
        .iter()
        .filter_map(|f| {
            Some(Track {
                content_type: content_type(f)?,
                path: f.clone(),
                rel_path: served_path(source, f)?,
                title: f.file_stem()?.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

/// `<source name>/<path in the source folder>`: where the web server serves `path`.
fn served_path(source: &Source, path: &Path) -> Option<PathBuf> {
    Some(Path::new(&source.name).join(path.strip_prefix(&source.path).ok()?))
}

/// Entries that are not hidden and have UTF-8 names, sorted by name.
fn usable_entries(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| !is_hidden(p) && has_utf8_name(p))
        .collect();
    paths.sort();
    Ok(paths)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The file server only opens UTF-8 paths, so a track with any other name
/// could never play.
fn has_utf8_name(p: &Path) -> bool {
    let utf8 = p.file_name().and_then(OsStr::to_str).is_some();
    if !utf8 {
        tracing::warn!("skipping {}: the name is not UTF-8", p.display());
    }
    utf8
}

/// Dot files, such as macOS `._` resource forks and `.DS_Store`.
fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

fn extension(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Formats the Chromecast default receiver can play.
pub(super) fn content_type(p: &Path) -> Option<&'static str> {
    Some(match extension(p).as_str() {
        "mp3" => "audio/mpeg",
        "m4a" | "m4b" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        _ => return None,
    })
}

fn is_image(p: &Path) -> bool {
    matches!(extension(p).as_str(), "jpg" | "jpeg" | "png")
}

/// Prefers cover.jpg / folder.png / ..., falls back to any image in the
/// folder except the deck's key picture.
fn find_cover(files: &[PathBuf]) -> Option<PathBuf> {
    let images: Vec<&PathBuf> = files
        .iter()
        .filter(|f| is_image(f) && !has_stem(f, KEY_PICTURE))
        .collect();
    COVER_NAMES
        .iter()
        .find_map(|name| images.iter().find(|f| has_stem(f, name)))
        .or_else(|| images.first())
        .map(|p| (*p).clone())
}

fn has_stem(p: &Path, stem: &str) -> bool {
    p.file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(stem))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SourceKind;
    use crate::library::Kind;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    fn music(root: &Path) -> Vec<Item> {
        scan(&Source::plain("music", SourceKind::Music, root)).unwrap()
    }

    fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }

    fn rel_paths(item: &Item) -> Vec<&Path> {
        item.tracks().iter().map(|t| t.rel_path.as_path()).collect()
    }

    #[test]
    fn scans_albums_and_tracks_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("02 Bedtime/01 Moon.mp3"));
        touch(&root.join("01 Animals/02 Ducks.flac"));
        touch(&root.join("01 Animals/01 Elephant.mp3"));
        touch(&root.join("01 Animals/notes.txt"));

        let albums = music(root);

        let names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["01 Animals", "02 Bedtime"]);
        let titles: Vec<&str> = albums[0]
            .tracks()
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(titles, ["01 Elephant", "02 Ducks"]);
        assert_eq!(
            albums[0].tracks()[1].rel_path,
            Path::new("music/01 Animals/02 Ducks.flac")
        );
        assert_eq!(albums[0].tracks()[1].content_type, "audio/flac");
    }

    #[test]
    fn skips_hidden_entries_and_folders_without_audio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Songs/._01 Song.mp3"));
        touch(&root.join("Songs/01 Song.mp3"));
        touch(&root.join(".Trash/01 Song.mp3"));
        touch(&root.join("Only Covers/cover.jpg"));

        let albums = music(root);

        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Songs");
        assert_eq!(albums[0].tracks().len(), 1);
    }

    #[test]
    fn prefers_named_cover_over_other_images() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/aaa.png"));
        touch(&root.join("Album/Folder.JPG"));

        let album = &music(root)[0];

        assert_eq!(
            album.cover.as_deref(),
            Some(root.join("Album/Folder.JPG").as_path())
        );
        assert_eq!(
            album.cover_rel.as_deref(),
            Some(Path::new("music/Album/Folder.JPG"))
        );
    }

    #[test]
    fn falls_back_to_any_image_as_cover() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/scan.jpeg"));

        let album = &music(root)[0];

        assert_eq!(
            album.cover_rel.as_deref(),
            Some(Path::new("music/Album/scan.jpeg"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn skips_an_unreadable_album_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Songs/01 Song.mp3"));
        let locked = root.join("lost+found");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let albums = scan(&Source::plain("music", SourceKind::Music, root));

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<String> = albums.unwrap().into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["Songs"]);
    }

    #[cfg(unix)]
    #[test]
    fn only_utf8_names_are_usable() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let latin1 = Path::new(OsStr::from_bytes(b"/music/Chansons d'\xe9t\xe9"));
        assert!(!has_utf8_name(latin1));
        assert!(has_utf8_name(Path::new("/music/Chansons d'été")));
    }

    #[test]
    fn a_missing_folder_is_an_error_naming_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let err = scan(&Source::plain(
            "books",
            SourceKind::Audiobook,
            &dir.path().join("nope"),
        ))
        .unwrap_err();
        assert!(err.to_string().contains("source books"), "{err:#}");
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type(Path::new("a.MP3")), Some("audio/mpeg"));
        assert_eq!(content_type(Path::new("book.m4b")), Some("audio/mp4"));
        assert_eq!(content_type(Path::new("a.txt")), None);
    }

    #[test]
    fn tracks_one_folder_down_belong_to_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Book/CD2/01 Stars.mp3"));
        touch(&root.join("Book/CD1/02 Night.mp3"));
        touch(&root.join("Book/CD1/01 Moon.mp3"));
        touch(&root.join("Book/00 Intro.mp3"));
        touch(&root.join("Book/CD1/Extra/01 Deep.mp3"));
        touch(&root.join("Book/CD1/cover.png"));

        let items = music(root);

        assert_eq!(names(&items), ["Book"]);
        assert_eq!(
            rel_paths(&items[0]),
            [
                Path::new("music/Book/00 Intro.mp3"),
                Path::new("music/Book/CD1/01 Moon.mp3"),
                Path::new("music/Book/CD1/02 Night.mp3"),
                Path::new("music/Book/CD2/01 Stars.mp3"),
            ]
        );
        assert_eq!(
            items[0].cover_rel.as_deref(),
            Some(Path::new("music/Book/CD1/cover.png"))
        );
    }

    #[test]
    fn a_folder_with_audio_only_one_level_down_is_an_item() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Box/CD1/01.mp3"));

        assert_eq!(
            rel_paths(&music(root)[0]),
            [Path::new("music/Box/CD1/01.mp3")]
        );
    }

    #[test]
    fn a_loose_audio_file_is_an_item_with_the_picture_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("02 Rain.wav"));
        touch(&root.join("01 Thunder.mp3"));
        touch(&root.join("01 Thunder.jpg"));
        touch(&root.join("notes.txt"));
        touch(&root.join("03 Wind/01.mp3"));

        let items = music(root);

        assert_eq!(names(&items), ["01 Thunder", "02 Rain", "03 Wind"]);
        let thunder = &items[0];
        assert_eq!(thunder.key.0, "music/01 Thunder.mp3");
        assert_eq!(rel_paths(thunder), [Path::new("music/01 Thunder.mp3")]);
        assert_eq!(
            thunder.cover_rel.as_deref(),
            Some(Path::new("music/01 Thunder.jpg"))
        );
        assert_eq!(items[1].cover, None);
    }

    #[test]
    fn items_take_their_kind_and_key_from_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Pippi/01.m4b"));

        let items = scan(&Source::plain("books", SourceKind::Audiobook, root)).unwrap();

        assert_eq!(items[0].kind, Kind::Audiobook);
        assert_eq!(items[0].key.0, "books/Pippi");
        assert_eq!(rel_paths(&items[0]), [Path::new("books/Pippi/01.m4b")]);
    }

    #[test]
    fn a_key_picture_is_for_the_deck_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/Key.PNG"));
        touch(&root.join("Only Key/01.mp3"));
        touch(&root.join("Only Key/key.jpg"));

        let items = music(root);

        assert_eq!(items[0].picture, Some(root.join("Album/Key.PNG")));
        assert_eq!(items[0].cover, None);
        assert_eq!(items[1].picture, Some(root.join("Only Key/key.jpg")));
        assert_eq!(items[1].cover_rel, None);
    }

    #[test]
    fn item_tables_set_pictures_and_colours_by_name() {
        use crate::config::{Color, Look};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/key.png"));
        touch(&root.join("Rain.mp3"));
        touch(&root.join("Wind.mp3"));
        let mut source = Source::plain("music", SourceKind::Music, root);
        let look = |picture: Option<&str>, color| Look {
            picture: picture.map(PathBuf::from),
            color,
        };
        source.items = [
            ("Album".to_string(), look(Some("/pics/a.png"), None)),
            ("Rain.mp3".to_string(), look(None, Some(Color([1, 2, 3])))),
            ("Wind".to_string(), look(None, Some(Color([4, 5, 6])))),
        ]
        .into();

        let items = scan(&source).unwrap();

        assert_eq!(items[0].picture.as_deref(), Some(Path::new("/pics/a.png")));
        assert_eq!(items[1].color, Some(Color([1, 2, 3])));
        assert_eq!(items[2].color, Some(Color([4, 5, 6])));
    }
}
