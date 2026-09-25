//! `[[source]]` tables: where the deck's items come from and what kind they are.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

use super::playlist::{Playlist, RawSpotify};
use super::podcast::{Podcast, RawPodcast};
use super::radio::{RawRadio, Station};

/// One `[[source]]` table, checked and with its path resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Names the source in item keys and music URLs: letters, digits, `-`
    /// and `_` only. Defaults to the type (`music`, `audiobook`, `story`).
    pub name: String,
    pub kind: SourceKind,
    /// The folder to scan, or for a podcast source its cache folder; empty
    /// for a radio source. Relative paths are resolved against the folder
    /// the config file lives in.
    pub path: PathBuf,
    /// How the shelf key looks.
    pub look: Look,
    /// How single items look, by their name in the folder (with or without
    /// the file extension).
    pub items: BTreeMap<String, Look>,
    /// For `type = "podcast"`.
    pub podcast: Option<Podcast>,
    /// For `type = "radio"`.
    pub stations: Vec<Station>,
    /// For `type = "spotify"`.
    pub playlists: Vec<Playlist>,
}

/// A picture or a colour for a key; pictures are resolved like `path`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub picture: Option<PathBuf>,
    /// The background of a key without a picture.
    pub color: Option<Color>,
}

impl Look {
    fn resolve(self, base: &Path) -> Look {
        Look {
            picture: self.picture.map(|p| base.join(p)),
            color: self.color,
        }
    }
}

/// `#rrggbb` in the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Color(pub [u8; 3]);

impl TryFrom<String> for Color {
    type Error = String;

    fn try_from(text: String) -> Result<Color, String> {
        let invalid = || format!("colour {text:?} must look like \"#e8a33d\"");
        let hex = text.strip_prefix('#').ok_or_else(invalid)?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| invalid());
        Ok(Color([byte(0)?, byte(2)?, byte(4)?]))
    }
}

impl Source {
    /// Whether the web server serves files from `path`: not for radio and
    /// Spotify, which have no folder.
    pub fn serves_files(&self) -> bool {
        !matches!(self.kind, SourceKind::Radio | SourceKind::Spotify)
    }

    /// A source with no pictures or colours.
    #[cfg(test)]
    pub fn plain(name: &str, kind: SourceKind, path: &Path) -> Source {
        Source {
            name: name.into(),
            kind,
            path: path.into(),
            look: Look::default(),
            items: BTreeMap::new(),
            podcast: None,
            stations: Vec::new(),
            playlists: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Music,
    Audiobook,
    /// Stories and sound effects.
    Story,
    Podcast,
    Radio,
    Spotify,
}

impl SourceKind {
    fn name(self) -> &'static str {
        match self {
            SourceKind::Music => "music",
            SourceKind::Audiobook => "audiobook",
            SourceKind::Story => "story",
            SourceKind::Podcast => "podcast",
            SourceKind::Radio => "radio",
            SourceKind::Spotify => "spotify",
        }
    }
}

/// A `[[source]]` table as written in the file.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(super) enum RawSource {
    Music(Folder),
    Audiobook(Folder),
    Story(Folder),
    Podcast(RawPodcast),
    Radio(RawRadio),
    Spotify(RawSpotify),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Folder {
    name: Option<String>,
    path: PathBuf,
    picture: Option<PathBuf>,
    color: Option<Color>,
    #[serde(default)]
    item: BTreeMap<String, Look>,
}

/// Checks names and resolves relative paths against `base`; podcast caches
/// default to `<state_dir>/podcasts/<name>`.
pub(super) fn resolve(raw: Vec<RawSource>, base: &Path, state_dir: &Path) -> Result<Vec<Source>> {
    if raw.is_empty() {
        bail!(
            "there is no [[source]] table; add one after the other keys, e.g.\n\n\
             [[source]]\ntype = \"music\"\npath = \"music\""
        );
    }
    let mut names = HashSet::new();
    let mut caches = HashSet::new();
    raw.into_iter()
        .map(|raw| {
            let (kind, folder) = match raw {
                RawSource::Music(f) => (SourceKind::Music, f),
                RawSource::Audiobook(f) => (SourceKind::Audiobook, f),
                RawSource::Story(f) => (SourceKind::Story, f),
                RawSource::Podcast(p) => {
                    let source = podcast_source(&p, base, state_dir, &mut names)?;
                    if !caches.insert(source.path.clone()) {
                        bail!(
                            "two podcast sources use the cache_dir {}",
                            source.path.display()
                        );
                    }
                    return Ok(source);
                }
                RawSource::Radio(r) => {
                    let name = r
                        .name
                        .clone()
                        .unwrap_or_else(|| SourceKind::Radio.name().into());
                    check_unique(&name, &mut names)?;
                    return Ok(Source {
                        stations: r.stations(&name, base)?,
                        look: r.look().resolve(base),
                        name,
                        kind: SourceKind::Radio,
                        path: PathBuf::new(),
                        items: BTreeMap::new(),
                        podcast: None,
                        playlists: Vec::new(),
                    });
                }
                RawSource::Spotify(s) => {
                    let name = s
                        .name
                        .clone()
                        .unwrap_or_else(|| SourceKind::Spotify.name().into());
                    check_unique(&name, &mut names)?;
                    return Ok(Source {
                        playlists: s.playlists(&name, base)?,
                        look: s.look().resolve(base),
                        name,
                        kind: SourceKind::Spotify,
                        path: PathBuf::new(),
                        items: BTreeMap::new(),
                        podcast: None,
                        stations: Vec::new(),
                    });
                }
            };
            let name = folder.name.unwrap_or_else(|| kind.name().into());
            check_unique(&name, &mut names)?;
            Ok(Source {
                name,
                kind,
                path: base.join(folder.path),
                look: Look {
                    picture: folder.picture,
                    color: folder.color,
                }
                .resolve(base),
                items: folder
                    .item
                    .into_iter()
                    .map(|(name, look)| (name, look.resolve(base)))
                    .collect(),
                podcast: None,
                stations: Vec::new(),
                playlists: Vec::new(),
            })
        })
        .collect()
}

fn podcast_source(
    raw: &RawPodcast,
    base: &Path,
    state_dir: &Path,
    names: &mut HashSet<String>,
) -> Result<Source> {
    let name = raw
        .name
        .clone()
        .unwrap_or_else(|| SourceKind::Podcast.name().into());
    check_unique(&name, names)?;
    let path = match &raw.cache_dir {
        Some(dir) => base.join(dir),
        None => state_dir.join("podcasts").join(&name),
    };
    Ok(Source {
        podcast: Some(raw.check(&name)?),
        look: raw.look().resolve(base),
        name,
        kind: SourceKind::Podcast,
        path,
        items: BTreeMap::new(),
        stations: Vec::new(),
        playlists: Vec::new(),
    })
}

fn check_unique(name: &str, names: &mut HashSet<String>) -> Result<()> {
    check_name(name)?;
    if !names.insert(name.into()) {
        bail!(
            "two sources are named {name:?}; give each one a different \
             `name = \"...\"`"
        );
    }
    Ok(())
}

/// The name goes into URL paths and into the keys of saved progress.
fn check_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        bail!("source name {name:?} must use only letters, digits, - and _");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Sources {
        source: Vec<RawSource>,
    }

    fn sources(text: &str) -> Result<Vec<Source>> {
        let raw: Sources = toml::from_str(text)?;
        resolve(
            raw.source,
            Path::new("/srv/deck"),
            Path::new("/srv/deck/state"),
        )
    }

    #[test]
    fn a_source_is_named_after_its_type_unless_it_has_a_name() {
        let got = sources(
            "[[source]]\ntype = \"music\"\npath = \"music\"\n\
             [[source]]\ntype = \"audiobook\"\nname = \"books\"\npath = \"/mnt/usb/books\"\n\
             [[source]]\ntype = \"story\"\npath = \"sounds\"\n",
        )
        .unwrap();
        assert_eq!(
            got,
            [
                Source {
                    name: "music".into(),
                    kind: SourceKind::Music,
                    path: "/srv/deck/music".into(),
                    look: Look::default(),
                    items: BTreeMap::new(),
                    podcast: None,
                    stations: Vec::new(),
                    playlists: Vec::new(),
                },
                Source {
                    name: "books".into(),
                    kind: SourceKind::Audiobook,
                    path: "/mnt/usb/books".into(),
                    look: Look::default(),
                    items: BTreeMap::new(),
                    podcast: None,
                    stations: Vec::new(),
                    playlists: Vec::new(),
                },
                Source {
                    name: "story".into(),
                    kind: SourceKind::Story,
                    path: "/srv/deck/sounds".into(),
                    look: Look::default(),
                    items: BTreeMap::new(),
                    podcast: None,
                    stations: Vec::new(),
                    playlists: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn an_unknown_key_in_a_source_is_an_error() {
        let err = sources("[[source]]\ntype = \"music\"\npath = \"m\"\npth = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("pth"), "{err:#}");
    }

    #[test]
    fn an_unknown_type_is_an_error() {
        let err = sources("[[source]]\ntype = \"cassette\"\npath = \"m\"\n").unwrap_err();
        assert!(err.to_string().contains("cassette"), "{err:#}");
    }

    #[test]
    fn a_folder_source_needs_a_path() {
        let err = sources("[[source]]\ntype = \"story\"\n").unwrap_err();
        assert!(err.to_string().contains("path"), "{err:#}");
    }

    #[test]
    fn two_sources_with_one_name_are_an_error() {
        let err = sources(
            "[[source]]\ntype = \"music\"\npath = \"a\"\n\
             [[source]]\ntype = \"music\"\npath = \"b\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("\"music\""), "{err:#}");
    }

    #[test]
    fn a_name_that_does_not_fit_in_a_url_is_an_error() {
        for name in ["", "my music", "a/b", "ä", ".."] {
            let text = format!("[[source]]\ntype = \"music\"\nname = \"{name}\"\npath = \"m\"\n");
            let err = sources(&text).unwrap_err();
            assert!(err.to_string().contains("source name"), "{name}: {err:#}");
        }
    }

    #[test]
    fn no_source_is_an_error() {
        let err = resolve(Vec::new(), Path::new("/"), Path::new("/state")).unwrap_err();
        assert!(err.to_string().contains("[[source]]"), "{err:#}");
    }

    #[test]
    fn a_source_and_its_items_have_pictures_and_colours() {
        let got = sources(
            "[[source]]\ntype = \"music\"\npath = \"music\"\n\
             picture = \"pictures/music.png\"\ncolor = \"#E8a33d\"\n\
             [source.item.\"01 Animal Songs\"]\npicture = \"/pics/animals.png\"\n\
             [source.item.\"Rain.mp3\"]\ncolor = \"#000000\"\n",
        )
        .unwrap();
        let music = &got[0];
        assert_eq!(
            music.look,
            Look {
                picture: Some("/srv/deck/pictures/music.png".into()),
                color: Some(Color([0xe8, 0xa3, 0x3d])),
            }
        );
        assert_eq!(
            music.items["01 Animal Songs"].picture.as_deref(),
            Some(Path::new("/pics/animals.png"))
        );
        assert_eq!(music.items["Rain.mp3"].color, Some(Color([0, 0, 0])));
    }

    #[test]
    fn a_colour_must_be_six_hex_digits() {
        for color in ["e8a33d", "#e8a33", "#e8a33dd", "#g8a33d", "red"] {
            let text = format!("[[source]]\ntype = \"music\"\npath = \"m\"\ncolor = \"{color}\"\n");
            let err = sources(&text).unwrap_err();
            assert!(err.to_string().contains("#e8a33d"), "{color}: {err:#}");
        }
    }

    #[test]
    fn an_unknown_key_in_an_item_is_an_error() {
        let err = sources(
            "[[source]]\ntype = \"music\"\npath = \"m\"\n[source.item.\"A\"]\ncolour = \"#000000\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("colour"), "{err:#}");
    }

    #[test]
    fn a_podcast_source_caches_in_the_state_folder_unless_told_otherwise() {
        let feed = "[[source.feed]]\nname = \"Maus\"\nurl = \"https://example.org/maus.xml\"\n";
        let got = sources(&format!(
            "[[source]]\ntype = \"podcast\"\n{feed}\
             [[source]]\ntype = \"podcast\"\nname = \"news\"\ncache_dir = \"/mnt/usb/news\"\n{feed}"
        ))
        .unwrap();
        assert_eq!(
            (got[0].name.as_str(), got[0].kind, got[0].path.as_path()),
            (
                "podcast",
                SourceKind::Podcast,
                Path::new("/srv/deck/state/podcasts/podcast")
            )
        );
        assert_eq!(got[0].podcast.as_ref().unwrap().feeds[0].name, "Maus");
        assert_eq!(got[1].path, Path::new("/mnt/usb/news"));
    }

    #[test]
    fn two_podcast_sources_cannot_share_a_cache() {
        let feed = "[[source.feed]]\nname = \"Maus\"\nurl = \"https://example.org/maus.xml\"\n";
        let err = sources(&format!(
            "[[source]]\ntype = \"podcast\"\nname = \"a\"\ncache_dir = \"c\"\n{feed}\
             [[source]]\ntype = \"podcast\"\nname = \"b\"\ncache_dir = \"c\"\n{feed}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("cache_dir"), "{err:#}");
    }
}
