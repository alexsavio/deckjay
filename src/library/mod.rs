//! What the deck plays: items on shelves (one shelf per `[[source]]`), the
//! files the speaker may download, and the URLs it downloads them from.

pub mod podcast;
mod scan;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::config::{Color, Source, SourceKind};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// An internet radio station: a stream, or a playlist that names one.
    Stream { url: String },
    /// A Spotify playlist: `spotify:playlist:<id>`.
    Spotify { uri: String },
}

/// One thing a key plays, e.g. an album folder or a story file.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: Kind,
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
    /// What the key shows instead of the cover: a `[source.item]` picture
    /// or a `key.png` / `key.jpg` in the folder.
    pub picture: Option<PathBuf>,
    /// The background of the key when it has neither picture nor cover.
    pub color: Option<Color>,
}

impl Item {
    /// Empty for a stream.
    pub fn tracks(&self) -> &[Track] {
        match &self.media {
            Media::Tracks(tracks) => tracks,
            Media::Stream { .. } | Media::Spotify { .. } => &[],
        }
    }
}

/// Items the deck shows together.
#[derive(Debug, Clone)]
pub struct Shelf {
    pub name: String,
    pub kind: Kind,
    /// In the order the deck shows them.
    pub items: Vec<ItemId>,
    /// The shelf key's picture, from the `[[source]]` table.
    pub picture: Option<PathBuf>,
    pub color: Option<Color>,
}

/// What [`Library::refill`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Refilled {
    /// The shelf's list: items came, went or moved.
    pub moved: bool,
    /// Known items whose picture, cover, colour or name changed.
    pub restyled: Vec<ItemId>,
}

pub struct Library {
    /// `ItemId(n)` is `items[n]`; items are only ever added at the end.
    items: Vec<Item>,
    shelves: Vec<Shelf>,
}

impl Kind {
    /// Whether a press plays on from where the item stopped.
    pub fn resumes(self) -> bool {
        matches!(self, Kind::Audiobook | Kind::Podcast)
    }

    /// Whether ⏮ and ⏭ jump in the item instead of skipping tracks: the
    /// spoken kinds, where a track is a long chapter.
    pub fn seeks(self) -> bool {
        matches!(self, Kind::Audiobook | Kind::Podcast | Kind::Story)
    }
}

impl From<SourceKind> for Kind {
    fn from(kind: SourceKind) -> Kind {
        match kind {
            SourceKind::Music => Kind::Music,
            SourceKind::Audiobook => Kind::Audiobook,
            SourceKind::Story => Kind::Story,
            SourceKind::Podcast => Kind::Podcast,
            SourceKind::Radio => Kind::Radio,
            SourceKind::Spotify => Kind::Spotify,
        }
    }
}

impl Library {
    /// One shelf per source, in config order. A source whose folder cannot
    /// be read gets an empty shelf and a warning, so the others still play.
    /// Podcast shelves start with the episodes in their cache.
    pub fn scan(sources: &[Source]) -> Library {
        let mut library = Library {
            items: Vec::new(),
            shelves: Vec::new(),
        };
        for source in sources {
            let items = if let Some(settings) = podcast::settings(source) {
                podcast::items(source, &crate::podcasts::load_cached(&settings))
            } else if source.kind == SourceKind::Radio {
                stations(source)
            } else if source.kind == SourceKind::Spotify {
                playlists(source)
            } else {
                scan::scan(source).unwrap_or_else(|err| {
                    tracing::warn!("{err:#}");
                    Vec::new()
                })
            };
            let shelf = Shelf {
                name: source.name.clone(),
                kind: source.kind.into(),
                items: Vec::new(),
                picture: source.look.picture.clone(),
                color: source.look.color,
            };
            library.add_shelf(shelf, items);
        }
        library
    }

    /// `items` in this order on one music shelf.
    #[cfg(test)]
    pub fn music(items: Vec<Item>) -> Library {
        Library::with_shelves(vec![("music", Kind::Music, items)])
    }

    /// One shelf per `(name, kind, items)`, in this order.
    #[cfg(test)]
    pub fn with_shelves(shelves: Vec<(&str, Kind, Vec<Item>)>) -> Library {
        let mut library = Library {
            items: Vec::new(),
            shelves: Vec::new(),
        };
        for (name, kind, items) in shelves {
            let shelf = Shelf {
                name: name.into(),
                kind,
                items: Vec::new(),
                picture: None,
                color: None,
            };
            library.add_shelf(shelf, items);
        }
        library
    }

    /// Puts `items` on shelf `shelf` in this order. Items with a known key
    /// keep their id (their data is updated); new ones get new ids.
    pub fn refill(&mut self, shelf: usize, items: Vec<Item>) -> Refilled {
        let mut restyled = Vec::new();
        let known: HashMap<ItemKey, ItemId> = self
            .items()
            .map(|(id, item)| (item.key.clone(), id))
            .collect();
        let ids: Vec<ItemId> = items
            .into_iter()
            .map(|item| {
                if let Some(&id) = known.get(&item.key) {
                    let old = &self.items[id.0 as usize];
                    let same_look = (&old.name, &old.cover, &old.picture, old.color)
                        == (&item.name, &item.cover, &item.picture, item.color);
                    if !same_look {
                        restyled.push(id);
                    }
                    self.items[id.0 as usize] = item;
                    id
                } else {
                    self.items.push(item);
                    ItemId((self.items.len() - 1) as u32)
                }
            })
            .collect();
        let moved = self.shelves[shelf].items != ids;
        self.shelves[shelf].items = ids;
        Refilled { moved, restyled }
    }

    /// Adds `shelf` with `items` (its own item list is replaced).
    fn add_shelf(&mut self, mut shelf: Shelf, items: Vec<Item>) {
        let first = self.items.len();
        shelf.items = (first..first + items.len())
            .map(|i| ItemId(i as u32))
            .collect();
        self.items.extend(items);
        self.shelves.push(shelf);
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

/// One item per station of a radio source.
fn stations(source: &Source) -> Vec<Item> {
    source
        .stations
        .iter()
        .map(|station| Item {
            kind: Kind::Radio,
            key: ItemKey(format!("{}/{}", source.name, station.name)),
            name: station.name.clone(),
            media: Media::Stream {
                url: station.url.clone(),
            },
            cover: None,
            cover_rel: None,
            picture: station.picture.clone(),
            color: station.color,
        })
        .collect()
}

/// One item per playlist of a Spotify source, with the covers kept in its
/// folder.
fn playlists(source: &Source) -> Vec<Item> {
    source
        .playlists
        .iter()
        .map(|playlist| Item {
            kind: Kind::Spotify,
            key: ItemKey(format!("{}/{}", source.name, playlist.name)),
            name: playlist.name.clone(),
            media: Media::Spotify {
                uri: playlist.uri.clone(),
            },
            cover: Some(crate::spotify::covers::path(&source.path, &playlist.uri))
                .filter(|cover| cover.is_file()),
            cover_rel: None,
            picture: playlist.picture.clone(),
            color: playlist.color,
        })
        .collect()
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
        Source::plain(name, kind, path)
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

    #[test]
    fn every_station_of_a_radio_source_is_an_item() {
        use crate::config::Station;
        let mut radio = Source::plain("radio", SourceKind::Radio, Path::new(""));
        radio.stations = vec![Station {
            name: "Kinder".into(),
            url: "https://example.org/kinder.mp3".into(),
            picture: Some("/pics/kinder.png".into()),
            color: None,
        }];

        let library = Library::scan(&[radio]);

        let item = library.item(ItemId(0));
        assert_eq!(
            (item.kind, item.key.0.as_str(), item.picture.as_deref()),
            (
                Kind::Radio,
                "radio/Kinder",
                Some(Path::new("/pics/kinder.png"))
            )
        );
        assert!(
            matches!(&item.media, Media::Stream { url } if url == "https://example.org/kinder.mp3")
        );
        assert!(item.tracks().is_empty());
        assert!(library.served_files().is_empty());
    }

    #[test]
    fn every_playlist_of_a_spotify_source_is_an_item() {
        use crate::config::Playlist;
        let mut spotify = Source::plain("spotify", SourceKind::Spotify, Path::new(""));
        spotify.playlists = vec![Playlist {
            name: "Bedtime".into(),
            uri: "spotify:playlist:abc".into(),
            picture: None,
            color: None,
        }];

        let library = Library::scan(&[spotify]);

        let item = library.item(ItemId(0));
        assert_eq!(
            (item.kind, item.key.0.as_str()),
            (Kind::Spotify, "spotify/Bedtime")
        );
        assert!(matches!(&item.media, Media::Spotify { uri } if uri == "spotify:playlist:abc"));
    }
}
