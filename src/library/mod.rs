//! What the deck plays: items on shelves (one shelf per `[[source]]`), the
//! files the speaker may download, and the URLs it downloads them from.

mod scan;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::config::{Source, SourceKind};

#[derive(Debug, Clone)]
pub struct Track {
    /// The file as found in the source folder, for local playback.
    pub path: PathBuf,
    /// `<source name>/<path in the source folder>`: the path the web server
    /// serves the file at, below `/music/`.
    pub rel_path: PathBuf,
    /// File name without the extension.
    pub title: String,
    pub content_type: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(dead_code, reason = "radio, podcast and Spotify sources come later")]
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

/// `<source name>/<name in the source folder>`, e.g. `music/01 Animal Songs`: the same
/// item gets the same key after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemKey(pub String);

#[derive(Debug, Clone)]
pub enum Media {
    /// Sorted by file name.
    Tracks(Vec<Track>),
}

/// One thing a key plays, e.g. an album folder or a story file.
#[derive(Debug, Clone)]
pub struct Item {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read once keys show what kind an item is")
    )]
    pub kind: Kind,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read once playback progress is saved")
    )]
    pub key: ItemKey,
    /// Folder name, or file name without the extension, including any
    /// number prefix.
    pub name: String,
    pub media: Media,
    /// Absolute path of the cover image, if one was found.
    pub cover: Option<PathBuf>,
    /// Where the web server serves the cover, like [`Track::rel_path`] (for
    /// the speaker's metadata).
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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read once the deck has shelf keys")
    )]
    pub name: String,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read once the deck has shelf keys")
    )]
    pub kind: Kind,
    /// In the order the deck shows them.
    pub items: Vec<ItemId>,
}

pub struct Library {
    /// `ItemId(n)` is `items[n]`; items are only ever added at the end.
    items: Vec<Item>,
    shelves: Vec<Shelf>,
}

impl From<SourceKind> for Kind {
    fn from(kind: SourceKind) -> Kind {
        match kind {
            SourceKind::Music => Kind::Music,
            SourceKind::Audiobook => Kind::Audiobook,
            SourceKind::Story => Kind::Story,
        }
    }
}

impl Library {
    /// One shelf per source, in config order. A source whose folder cannot
    /// be read gets an empty shelf and a warning, so the others still play.
    pub fn scan(sources: &[Source]) -> Library {
        let mut library = Library {
            items: Vec::new(),
            shelves: Vec::new(),
        };
        for source in sources {
            let items = scan::scan(source).unwrap_or_else(|err| {
                tracing::warn!("{err:#}");
                Vec::new()
            });
            library.add_shelf(source.name.clone(), source.kind.into(), items);
        }
        library
    }

    /// `items` in this order on one music shelf.
    #[cfg(test)]
    pub fn music(items: Vec<Item>) -> Library {
        let mut library = Library {
            items: Vec::new(),
            shelves: Vec::new(),
        };
        library.add_shelf("music".into(), Kind::Music, items);
        library
    }

    fn add_shelf(&mut self, name: String, kind: Kind, items: Vec<Item>) {
        let first = self.items.len();
        let ids = (first..first + items.len())
            .map(|i| ItemId(i as u32))
            .collect();
        self.items.extend(items);
        self.shelves.push(Shelf {
            name,
            kind,
            items: ids,
        });
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

    /// Paths below `/music/`: the only files the speaker may download.
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

    fn source(name: &str, kind: SourceKind, path: &Path) -> Source {
        Source {
            name: name.into(),
            kind,
            path: path.into(),
        }
    }

    #[test]
    fn served_files_are_the_tracks_and_covers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/cover.jpg"));
        touch(&root.join("Album/notes.txt"));
        touch(&root.join("Album/.hidden.mp3"));

        let files = Library::scan(&[source("music", SourceKind::Music, root)]).served_files();

        let expected: HashSet<PathBuf> = [
            PathBuf::from("music/Album/01.mp3"),
            PathBuf::from("music/Album/cover.jpg"),
        ]
        .into();
        assert_eq!(files, expected);
    }

    #[test]
    fn every_source_is_one_shelf_in_config_order() {
        let music = tempfile::tempdir().unwrap();
        touch(&music.path().join("02 Bedtime/01 Moon.mp3"));
        touch(&music.path().join("01 Animals/01 Elephant.mp3"));
        let books = tempfile::tempdir().unwrap();
        touch(&books.path().join("Pippi/01.mp3"));

        let library = Library::scan(&[
            source("books", SourceKind::Audiobook, books.path()),
            source("music", SourceKind::Music, music.path()),
        ]);

        let items: Vec<(ItemId, &str, &str)> = library
            .items()
            .map(|(id, item)| (id, item.name.as_str(), item.key.0.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                (ItemId(0), "Pippi", "books/Pippi"),
                (ItemId(1), "01 Animals", "music/01 Animals"),
                (ItemId(2), "02 Bedtime", "music/02 Bedtime"),
            ]
        );
        assert_eq!(library.item(ItemId(2)).name, "02 Bedtime");
        let shelves: Vec<(&str, Kind, &[ItemId])> = library
            .shelves()
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.items.as_slice()))
            .collect();
        assert_eq!(
            shelves,
            [
                ("books", Kind::Audiobook, &[ItemId(0)][..]),
                ("music", Kind::Music, &[ItemId(1), ItemId(2)][..]),
            ]
        );
    }

    #[test]
    fn a_source_that_cannot_be_read_is_an_empty_shelf() {
        let music = tempfile::tempdir().unwrap();
        touch(&music.path().join("Songs/01.mp3"));

        let library = Library::scan(&[
            source("usb", SourceKind::Story, &music.path().join("missing")),
            source("music", SourceKind::Music, music.path()),
        ]);

        assert_eq!(library.items().len(), 1);
        assert!(library.shelves()[0].items.is_empty());
        assert_eq!(library.shelves()[1].items, [ItemId(0)]);
    }
}
