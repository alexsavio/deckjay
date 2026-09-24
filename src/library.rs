//! Scans the music folder. Expected layout:
//!
//! ```text
//! music/
//!   01 Animal Songs/
//!     cover.jpg
//!     01 The Elephant.mp3
//!     02 Five Little Ducks.mp3
//!   02 Bedtime Stories/
//!     ...
//! ```
//!
//! Albums and tracks are sorted by name, so number prefixes control the order.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

#[derive(Debug, Clone)]
pub struct Track {
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

/// Cover file names (without extension), best match first.
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "album"];

pub fn scan(music_dir: &Path) -> Result<Vec<Album>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(music_dir)
        .with_context(|| format!("cannot read music folder {}", music_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && !is_hidden(p))
        .collect();
    dirs.sort();

    let mut albums = Vec::new();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && !is_hidden(p))
            .collect();
        files.sort();

        let tracks: Vec<Track> = files
            .iter()
            .filter_map(|f| {
                let content_type = content_type(f)?;
                Some(Track {
                    rel_path: f.strip_prefix(music_dir).ok()?.to_path_buf(),
                    title: f.file_stem()?.to_string_lossy().into_owned(),
                    content_type,
                })
            })
            .collect();

        if tracks.is_empty() {
            tracing::warn!("skipping {}: no playable audio files", dir.display());
            continue;
        }

        let cover = find_cover(&files);
        albums.push(Album {
            name: dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            tracks,
            cover_rel: cover
                .as_ref()
                .and_then(|c| c.strip_prefix(music_dir).ok().map(Path::to_path_buf)),
            cover,
        });
    }
    Ok(albums)
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
fn content_type(p: &Path) -> Option<&'static str> {
    Some(match extension(p).as_str() {
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
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

/// Prefers cover.jpg / folder.png / ..., falls back to any image in the folder.
fn find_cover(files: &[PathBuf]) -> Option<PathBuf> {
    let images: Vec<&PathBuf> = files.iter().filter(|f| is_image(f)).collect();
    COVER_NAMES
        .iter()
        .find_map(|name| {
            images.iter().find(|f| {
                f.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name))
            })
        })
        .or_else(|| images.first())
        .map(|p| (*p).clone())
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
    fn scans_albums_and_tracks_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("02 Bedtime/01 Moon.mp3"));
        touch(&root.join("01 Animals/02 Ducks.flac"));
        touch(&root.join("01 Animals/01 Elephant.mp3"));
        touch(&root.join("01 Animals/notes.txt"));

        let albums = scan(root).unwrap();

        let names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["01 Animals", "02 Bedtime"]);
        let titles: Vec<&str> = albums[0].tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["01 Elephant", "02 Ducks"]);
        assert_eq!(
            albums[0].tracks[1].rel_path,
            Path::new("01 Animals/02 Ducks.flac")
        );
        assert_eq!(albums[0].tracks[1].content_type, "audio/flac");
    }

    #[test]
    fn skips_hidden_entries_and_folders_without_audio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Songs/._01 Song.mp3"));
        touch(&root.join("Songs/01 Song.mp3"));
        touch(&root.join(".Trash/01 Song.mp3"));
        touch(&root.join("Only Covers/cover.jpg"));

        let albums = scan(root).unwrap();

        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Songs");
        assert_eq!(albums[0].tracks.len(), 1);
    }

    #[test]
    fn prefers_named_cover_over_other_images() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/aaa.png"));
        touch(&root.join("Album/Folder.JPG"));

        let album = &scan(root).unwrap()[0];

        assert_eq!(
            album.cover.as_deref(),
            Some(root.join("Album/Folder.JPG").as_path())
        );
        assert_eq!(
            album.cover_rel.as_deref(),
            Some(Path::new("Album/Folder.JPG"))
        );
    }

    #[test]
    fn falls_back_to_any_image_as_cover() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Album/01.mp3"));
        touch(&root.join("Album/scan.jpeg"));

        let album = &scan(root).unwrap()[0];

        assert_eq!(
            album.cover_rel.as_deref(),
            Some(Path::new("Album/scan.jpeg"))
        );
    }

    #[test]
    fn missing_music_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type(Path::new("a.MP3")), Some("audio/mpeg"));
        assert_eq!(content_type(Path::new("a.txt")), None);
    }
}
