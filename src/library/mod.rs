//! The music folder: albums with their tracks and covers, the files the
//! speaker may download, and the URLs it downloads them from.

mod scan;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

pub use self::scan::scan;

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

/// One album folder with at least one playable track.
#[derive(Debug, Clone)]
pub struct Album {
    /// Folder name, including any number prefix.
    pub name: String,
    /// Tracks sorted by file name.
    pub tracks: Vec<Track>,
    /// Absolute path of the cover image, if one was found.
    pub cover: Option<PathBuf>,
    /// Cover path relative to the music folder (for the speaker's metadata).
    pub cover_rel: Option<PathBuf>,
}

/// Paths relative to the music folder: the only files the speaker may download.
pub fn served_files(albums: &[Album]) -> HashSet<PathBuf> {
    albums
        .iter()
        .flat_map(|a| {
            a.tracks
                .iter()
                .map(|t| t.rel_path.clone())
                .chain(a.cover_rel.clone())
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

    #[test]
    fn served_files_are_the_tracks_and_covers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/cover.jpg"));
        touch(&root.join("Album/notes.txt"));
        touch(&root.join("Album/.hidden.mp3"));

        let files = served_files(&scan(root).unwrap());

        let expected: HashSet<PathBuf> = [
            PathBuf::from("Album/01.mp3"),
            PathBuf::from("Album/cover.jpg"),
        ]
        .into();
        assert_eq!(files, expected);
    }
}
