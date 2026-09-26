//! The episode cache on disk, one folder per feed:
//!
//! ```text
//! <cache_dir>/<slug>/feed.json                              manifest
//!                    20250107-der-elefant-1a2b3c4d.mp3      date, title, episode id
//!                    p-9f8e7d6c.jpg                         hash of the picture URL
//!                    .20250107-der-elefant-1a2b3c4d.mp3.part   unfinished download
//! ```
//!
//! Names come from dates, ASCII slugs and hashes only, never from feed text
//! as is, and the sweep deletes nothing that does not match these patterns.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike};
use serde::{Deserialize, Serialize};

use super::feed::{content_type, hash_hex};
use super::plan::Stored;

pub const MANIFEST: &str = "feed.json";

const MAX_SLUG_CHARS: usize = 40;
const HASH_CHARS: usize = 8;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The feed's own title.
    #[serde(default)]
    pub title: String,
    /// Picture file name.
    #[serde(default)]
    pub picture: Option<String>,
    /// The published episodes, newest first.
    #[serde(default)]
    pub episodes: Vec<Stored>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Manifest,
    Episode,
    Picture,
    /// An unfinished download or manifest.
    Part,
}

/// `slug` becomes a folder name, so only `[a-z0-9-]` gets through.
pub fn feed_dir(cache_dir: &Path, slug: &str) -> Result<PathBuf> {
    let valid = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid {
        bail!("invalid podcast folder name {slug:?}: use 1 to 64 of a-z, 0-9 and -");
    }
    Ok(cache_dir.join(slug))
}

/// A missing manifest is an empty cache. `None` (logged) when it cannot be
/// read or parsed: the folder may then hold cached files no manifest lists.
pub fn load(dir: &Path) -> Option<Manifest> {
    let path = dir.join(MANIFEST);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Some(Manifest::default()),
        Err(err) => {
            tracing::warn!("cannot read {}: {err}", path.display());
            return None;
        }
    };
    match serde_json::from_str::<Manifest>(&text) {
        Ok(manifest) => Some(sanitized(manifest)),
        Err(err) => {
            tracing::warn!(
                "ignoring the broken podcast manifest {}: {err}",
                path.display()
            );
            None
        }
    }
}

/// The manifest is read back as file names, so names that this module would
/// never write are dropped.
fn sanitized(manifest: Manifest) -> Manifest {
    let picture = manifest.picture.filter(|p| kind(p) == Some(Kind::Picture));
    let episodes = manifest
        .episodes
        .into_iter()
        .filter(|e| kind(&e.file) == Some(Kind::Episode))
        .map(|e| Stored {
            picture: e.picture.filter(|p| kind(p) == Some(Kind::Picture)),
            ..e
        })
        .collect();
    Manifest {
        title: manifest.title,
        picture,
        episodes,
    }
}

/// Written to a temporary file, synced and renamed, so a power cut leaves the
/// old or the new manifest, never half of one.
pub fn save(dir: &Path, manifest: &Manifest) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(MANIFEST);
    let temp = dir.join(part_name(MANIFEST));
    let json = serde_json::to_vec_pretty(manifest)?;
    let write = || -> io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        // Makes the rename itself durable; not every platform can open a folder.
        if let Ok(folder) = File::open(dir) {
            let _ = folder.sync_all();
        }
        Ok(())
    };
    write().with_context(|| format!("cannot write {}", path.display()))
}

/// The manifest's episodes whose file is there, with the size on disk.
pub fn present(dir: &Path, manifest: &Manifest) -> Vec<Stored> {
    manifest
        .episodes
        .iter()
        .filter_map(|episode| {
            let meta = fs::metadata(dir.join(&episode.file)).ok()?;
            meta.is_file().then(|| Stored {
                bytes: meta.len(),
                ..episode.clone()
            })
        })
        .collect()
}

/// `<yyyymmdd>-<title slug>-<first 8 of id>.<ext>`; `00000000` without a date.
pub fn episode_name(published: Option<i64>, title: &str, id: &str, ext: &str) -> String {
    let date = published
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
        .filter(|date| (1000..=9999).contains(&date.year()))
        .map_or_else(
            || "00000000".to_string(),
            |date| date.format("%Y%m%d").to_string(),
        );
    let hash = id.get(..HASH_CHARS).unwrap_or(id);
    format!("{date}-{}-{hash}.{ext}", slug(title))
}

pub fn picture_name(url: &str) -> String {
    format!("p-{}.jpg", &hash_hex(url)[..HASH_CHARS])
}

pub fn part_name(name: &str) -> String {
    format!(".{name}.part")
}

/// Whether `name` is the file of episode `id`, which holds the start of the
/// id; this finds the playing episode even among files no manifest lists.
pub fn is_file_of(name: &str, id: &str) -> bool {
    episode_hash(name).is_some_and(|hash| id.get(..HASH_CHARS) == Some(hash))
}

fn episode_hash(name: &str) -> Option<&str> {
    if kind(name) != Some(Kind::Episode) {
        return None;
    }
    let stem = name.rsplit_once('.')?.0;
    stem.get(stem.len() - HASH_CHARS..)
}

pub fn kind(name: &str) -> Option<Kind> {
    if name == MANIFEST {
        return Some(Kind::Manifest);
    }
    if let Some(inner) = name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".part"))
    {
        return kind(inner).filter(|&k| k != Kind::Part).map(|_| Kind::Part);
    }
    if is_picture_name(name) {
        return Some(Kind::Picture);
    }
    is_episode_name(name).then_some(Kind::Episode)
}

fn is_hash(text: &[u8]) -> bool {
    text.len() == HASH_CHARS
        && text
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

fn is_picture_name(name: &str) -> bool {
    name.strip_prefix("p-")
        .and_then(|rest| rest.strip_suffix(".jpg"))
        .is_some_and(|hash| is_hash(hash.as_bytes()))
}

fn is_episode_name(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let ext_ok = ext
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && content_type(ext).is_some();
    let b = stem.as_bytes();
    let n = b.len();
    ext_ok
        && (8 + 1 + 1 + 1 + HASH_CHARS..=8 + 1 + MAX_SLUG_CHARS + 1 + HASH_CHARS).contains(&n)
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[n - HASH_CHARS - 1] == b'-'
        && is_hash(&b[n - HASH_CHARS..])
        && b[9..n - HASH_CHARS - 1]
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Lowercase ASCII letters and digits joined by `-`, at most 40 characters.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(fold) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_SLUG_CHARS);
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "episode".to_string()
    } else {
        out.to_string()
    }
}

/// Spells common Latin letters in ASCII, so German titles stay readable.
fn fold(c: char) -> Vec<char> {
    let ascii = match c {
        'ä' | 'Ä' => "ae",
        'ö' | 'Ö' => "oe",
        'ü' | 'Ü' => "ue",
        'ß' => "ss",
        'à' | 'á' | 'â' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Å' => "a",
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "i",
        'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ø' => "o",
        'ù' | 'ú' | 'û' | 'Ù' | 'Ú' | 'Û' => "u",
        'ç' | 'Ç' => "c",
        'ñ' | 'Ñ' => "n",
        _ => return vec![c],
    };
    ascii.chars().collect()
}

/// Files in `dir` that match the cache's patterns, are not in `keep`, and are
/// not the manifest.
pub fn unwanted(dir: &Path, keep: &HashSet<String>) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| match kind(name) {
            None | Some(Kind::Manifest) => false,
            Some(_) => !keep.contains(name),
        })
        .collect();
    names.sort();
    names
}

/// A file that is already gone counts as deleted.
pub fn delete(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Folders in the cache that belong to no configured feed.
pub fn foreign_folders(cache_dir: &Path, slugs: &HashSet<&str>) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !slugs.contains(name))
        })
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    folders
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1a2b3c4d5e6f7a8b";

    fn stored(file: &str) -> Stored {
        Stored {
            id: ID.into(),
            title: "Der Elefant".into(),
            published: Some(1_736_229_600),
            file: file.into(),
            content_type: "audio/mpeg".into(),
            bytes: 3,
            picture: Some("p-0011aabb.jpg".into()),
        }
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"abc").unwrap();
    }

    #[test]
    fn episode_names_are_date_slug_and_hash() {
        let name = episode_name(Some(1_736_229_600), "Der Elefant & die Maus!", ID, "mp3");
        assert_eq!(name, "20250107-der-elefant-die-maus-1a2b3c4d.mp3");
        assert_eq!(kind(&name), Some(Kind::Episode));
        assert!(is_file_of(&name, ID));
        assert!(!is_file_of(&name, "1a2b3c4e5e6f7a8b"));
        assert!(!is_file_of(&part_name(&name), ID));
    }

    #[test]
    fn titles_become_short_ascii_slugs() {
        assert_eq!(
            slug("Hören über Bäume, Straße"),
            "hoeren-ueber-baeume-strasse"
        );
        assert_eq!(slug("Café Niño"), "cafe-nino");
        assert_eq!(slug("日本語"), "episode");
        assert_eq!(slug(""), "episode");
        let long = slug(&"abc ".repeat(30));
        assert!(
            long.len() <= MAX_SLUG_CHARS && !long.ends_with('-'),
            "{long}"
        );
    }

    #[test]
    fn hostile_titles_stay_inside_the_folder() {
        for title in ["../../etc/passwd", "a/b\\c", "\0nul", ".hidden", "   "] {
            let name = episode_name(None, title, ID, "m4a");
            assert!(name.starts_with("00000000-"), "{name}");
            assert!(!name.contains('/') && !name.contains('\\') && !name.contains(".."));
            assert_eq!(kind(&name), Some(Kind::Episode), "{name}");
        }
    }

    #[test]
    fn classifies_cache_files() {
        let picture = picture_name("https://example.org/a.jpg");
        assert_eq!(picture.len(), "p-12345678.jpg".len());
        for (name, expected) in [
            ("feed.json", Some(Kind::Manifest)),
            (picture.as_str(), Some(Kind::Picture)),
            ("20250107-x-1a2b3c4d.opus", Some(Kind::Episode)),
            (".20250107-x-1a2b3c4d.opus.part", Some(Kind::Part)),
            (".p-0011aabb.jpg.part", Some(Kind::Part)),
            (".feed.json.part", Some(Kind::Part)),
            ("..feed.json.part.part", None),
            ("20250107-x-1a2b3c4d.txt", None),
            ("20250107-X-1a2b3c4d.mp3", None),
            ("20250107--1a2b3c4d.mp3", None),
            ("2025010-x-1a2b3c4d.mp3", None),
            ("20250107-x-1a2b3c4g.mp3", None),
            ("p-0011AABB.jpg", None),
            ("cover.jpg", None),
            (".DS_Store", None),
        ] {
            assert_eq!(kind(name), expected, "{name}");
        }
    }

    #[test]
    fn manifest_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("maus");
        let manifest = Manifest {
            title: "Die Maus".into(),
            picture: Some("p-0011aabb.jpg".into()),
            episodes: vec![stored("20250107-der-elefant-1a2b3c4d.mp3")],
        };

        save(&folder, &manifest).unwrap();

        assert_eq!(load(&folder), Some(manifest));
        assert!(!folder.join(part_name(MANIFEST)).exists());
    }

    #[test]
    fn a_missing_manifest_is_empty_and_a_corrupt_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Some(Manifest::default()));
        fs::write(dir.path().join(MANIFEST), b"{\"title\": ").unwrap();
        assert_eq!(load(dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_manifest_is_none() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &Manifest::default()).unwrap();
        let path = dir.path().join(MANIFEST);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

        let loaded = load(dir.path());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(loaded, None);
    }

    #[test]
    fn manifest_names_this_module_would_not_write_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut evil = stored("../../secret.mp3");
        evil.id = "x".into();
        let mut good = stored("20250107-der-elefant-1a2b3c4d.mp3");
        good.picture = Some("../p-0011aabb.jpg".into());
        let manifest = Manifest {
            title: "T".into(),
            picture: Some("/etc/passwd".into()),
            episodes: vec![evil, good],
        };
        save(dir.path(), &manifest).unwrap();

        let loaded = load(dir.path()).unwrap();

        assert_eq!(loaded.picture, None);
        assert_eq!(loaded.episodes.len(), 1);
        assert_eq!(loaded.episodes[0].picture, None);
    }

    #[test]
    fn present_keeps_episodes_whose_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "20250107-a-1a2b3c4d.mp3");
        let manifest = Manifest {
            episodes: vec![
                stored("20250107-a-1a2b3c4d.mp3"),
                stored("20250106-b-1a2b3c4d.mp3"),
            ],
            ..Manifest::default()
        };

        let present = present(dir.path(), &manifest);

        assert_eq!(present.len(), 1);
        assert_eq!(present[0].file, "20250107-a-1a2b3c4d.mp3");
        assert_eq!(present[0].bytes, 3);
    }

    #[test]
    fn sweep_lists_only_unreferenced_cache_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in [
            "feed.json",
            "20250107-kept-1a2b3c4d.mp3",
            "20250101-old-0a0b0c0d.mp3",
            "p-0011aabb.jpg",
            "p-99887766.jpg",
            ".20250108-next-2a2b3c4d.mp3.part",
            ".20241201-stale-3a3b3c4d.mp3.part",
            ".feed.json.part",
            "notes.txt",
            ".DS_Store",
            "cover.jpg",
        ] {
            touch(root, name);
        }
        fs::create_dir(root.join("20250102-dir-4a4b4c4d.mp3")).unwrap();
        let keep: HashSet<String> = [
            "20250107-kept-1a2b3c4d.mp3",
            "p-0011aabb.jpg",
            ".20250108-next-2a2b3c4d.mp3.part",
        ]
        .map(String::from)
        .into();

        assert_eq!(
            unwanted(root, &keep),
            [
                ".20241201-stale-3a3b3c4d.mp3.part",
                ".feed.json.part",
                "20250101-old-0a0b0c0d.mp3",
                "p-99887766.jpg",
            ]
        );
    }

    #[test]
    fn delete_ignores_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a");
        delete(&dir.path().join("a")).unwrap();
        delete(&dir.path().join("a")).unwrap();
        assert!(!dir.path().join("a").exists());
    }

    #[test]
    fn feed_folders_need_safe_names() {
        let root = Path::new("/cache");
        assert_eq!(
            feed_dir(root, "die-maus-2").unwrap(),
            root.join("die-maus-2")
        );
        for bad in ["", "..", "Die Maus", "a/b", "ä", &"x".repeat(65)] {
            assert!(feed_dir(root, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn finds_folders_of_feeds_that_are_gone() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("maus")).unwrap();
        fs::create_dir(dir.path().join("old")).unwrap();
        touch(dir.path(), "stray.txt");
        let slugs = HashSet::from(["maus"]);
        assert_eq!(
            foreign_folders(dir.path(), &slugs),
            [dir.path().join("old")]
        );
    }
}
