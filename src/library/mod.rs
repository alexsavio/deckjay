//! What the deck plays: items on shelves (for now one shelf with every album
//! folder of the music folder), the files the speaker may download, and the
//! URLs it downloads them from.

mod scan;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// The source name at the start of every [`ItemKey`] of the music folder.
const MUSIC: &str = "music";

#[derive(Debug, Clone)]
pub struct Track {
    /// The file as found in the music folder, for local playback.
    pub path: PathBuf,
    /// Path relative to the music folder.
    pub rel_path: PathBuf,
    /// File name without the extension.
    pub title: String,
    pub content_type: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(dead_code, reason = "only the music folder is scanned so far")]
pub enum Kind {
    Music,
    Audiobook,
    Story,
    Radio,
    Podcast,
    Spotify,
}

/// Names an item while the program runs: its index in [`Library`], never
/// reused. Commands, events and key faces carry it; it changes across
/// restarts, [`ItemKey`] does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(pub u32);

/// `<source>/<path in the source>`, e.g. `music/01 Animal Songs`: the same
/// item gets the same key after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemKey(pub String);

#[derive(Debug, Clone)]
pub enum Media {
    /// Sorted by file name.
    Tracks(Vec<Track>),
}

/// One thing a key plays: an album folder with at least one playable track.
#[derive(Debug, Clone)]
pub struct Item {
    #[expect(dead_code, reason = "read once keys show what kind an item is")]
    pub kind: Kind,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read once playback progress is saved")
    )]
    pub key: ItemKey,
    /// Folder name, including any number prefix.
    pub name: String,
    pub media: Media,
    /// Absolute path of the cover image, if one was found.
    pub cover: Option<PathBuf>,
    /// Cover path relative to the music folder (for the speaker's metadata).
    pub cover_rel: Option<PathBuf>,
}

impl Item {
    pub fn tracks(&self) -> &[Track] {
        let Media::Tracks(tracks) = &self.media;
        tracks
    }
}

/// Items the deck shows together.
#[derive(Debug, Clone)]
pub struct Shelf {
    #[expect(dead_code, reason = "read once the deck has shelf keys")]
    pub name: String,
    #[expect(dead_code, reason = "read once the deck has shelf keys")]
    pub kind: Kind,
    /// In the order the deck shows them.
    pub items: Vec<ItemId>,
}

pub struct Library {
    /// `ItemId(n)` is `items[n]`; items are only ever added at the end.
    items: Vec<Item>,
    shelves: Vec<Shelf>,
}

impl Library {
    /// Every album folder in `music_dir`, on one music shelf.
    pub fn scan(music_dir: &Path) -> Result<Library> {
        Ok(Library::music(scan::scan(music_dir)?))
    }

    /// `items` in this order on one music shelf, which exists even when empty.
    pub fn music(items: Vec<Item>) -> Library {
        let ids = (0..items.len()).map(|i| ItemId(i as u32)).collect();
        Library {
            items,
            shelves: vec![Shelf {
                name: MUSIC.into(),
                kind: Kind::Music,
                items: ids,
            }],
        }
    }

    pub fn item(&self, id: ItemId) -> &Item {
        &self.items[id.0 as usize]
    }

    pub fn items(&self) -> impl ExactSizeIterator<Item = (ItemId, &Item)> {
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| (ItemId(i as u32), item))
    }

    pub fn shelves(&self) -> &[Shelf] {
        &self.shelves
    }

    /// Paths relative to the music folder: the only files the speaker may download.
    pub fn served_files(&self) -> HashSet<PathBuf> {
        self.items
            .iter()
            .flat_map(|item| {
                item.tracks()
                    .iter()
                    .map(|t| t.rel_path.clone())
                    .chain(item.cover_rel.clone())
            })
            .collect()
    }
}

/// Everything except unreserved URL characters gets percent-encoded.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Builds `base/<segment>/<segment>` with every path segment URL-encoded.
pub fn url_for(base: &str, rel_path: &Path) -> String {
    let mut url = base.trim_end_matches('/').to_string();
    for part in rel_path.components() {
        url.push('/');
        url.extend(utf8_percent_encode(
            &part.as_os_str().to_string_lossy(),
            PATH_SEGMENT,
        ));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_each_segment() {
        let url = url_for(
            "http://10.0.0.2:8765/music/",
            Path::new("01 Tiere & Co/Die Kuh.mp3"),
        );
        assert_eq!(
            url,
            "http://10.0.0.2:8765/music/01%20Tiere%20%26%20Co/Die%20Kuh.mp3"
        );
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn served_files_are_the_tracks_and_covers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/cover.jpg"));
        touch(&root.join("Album/notes.txt"));
        touch(&root.join("Album/.hidden.mp3"));

        let files = Library::scan(root).unwrap().served_files();

        let expected: HashSet<PathBuf> = [
            PathBuf::from("Album/01.mp3"),
            PathBuf::from("Album/cover.jpg"),
        ]
        .into();
        assert_eq!(files, expected);
    }

    #[test]
    fn every_album_is_one_item_on_one_music_shelf() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("02 Bedtime/01 Moon.mp3"));
        touch(&root.join("01 Animals/01 Elephant.mp3"));

        let library = Library::scan(root).unwrap();

        let items: Vec<(ItemId, &str, &str)> = library
            .items()
            .map(|(id, item)| (id, item.name.as_str(), item.key.0.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                (ItemId(0), "01 Animals", "music/01 Animals"),
                (ItemId(1), "02 Bedtime", "music/02 Bedtime"),
            ]
        );
        assert_eq!(library.item(ItemId(1)).name, "02 Bedtime");
        let shelves = library.shelves();
        assert_eq!(shelves.len(), 1);
        assert_eq!(shelves[0].items, [ItemId(0), ItemId(1)]);
    }

    #[test]
    fn an_empty_music_folder_still_has_its_shelf() {
        let library = Library::music(Vec::new());
        assert_eq!(library.items().len(), 0);
        assert_eq!(library.shelves().len(), 1);
        assert!(library.shelves()[0].items.is_empty());
    }
}
