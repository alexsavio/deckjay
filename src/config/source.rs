//! `[[source]]` tables: where the deck's items come from and what kind they are.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

/// One `[[source]]` table, checked and with its path resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Names the source in item keys and music URLs: letters, digits, `-`
    /// and `_` only. Defaults to the type (`music`, `audiobook`, `story`).
    pub name: String,
    pub kind: SourceKind,
    /// The folder to scan. Relative paths are resolved against the folder
    /// the config file lives in.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Music,
    Audiobook,
    /// Stories and sound effects.
    Story,
}

impl SourceKind {
    fn name(self) -> &'static str {
        match self {
            SourceKind::Music => "music",
            SourceKind::Audiobook => "audiobook",
            SourceKind::Story => "story",
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Folder {
    name: Option<String>,
    path: PathBuf,
}

/// Checks names and resolves relative paths against `base`.
pub(super) fn resolve(raw: Vec<RawSource>, base: &Path) -> Result<Vec<Source>> {
    if raw.is_empty() {
        bail!(
            "there is no [[source]] table; add one after the other keys, e.g.\n\n\
             [[source]]\ntype = \"music\"\npath = \"music\""
        );
    }
    let mut names = HashSet::new();
    raw.into_iter()
        .map(|raw| {
            let (kind, folder) = match raw {
                RawSource::Music(f) => (SourceKind::Music, f),
                RawSource::Audiobook(f) => (SourceKind::Audiobook, f),
                RawSource::Story(f) => (SourceKind::Story, f),
            };
            let name = folder.name.unwrap_or_else(|| kind.name().into());
            check_name(&name)?;
            if !names.insert(name.clone()) {
                bail!(
                    "two sources are named {name:?}; give each one a different \
                     `name = \"...\"`"
                );
            }
            Ok(Source {
                name,
                kind,
                path: base.join(folder.path),
            })
        })
        .collect()
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
        resolve(raw.source, Path::new("/srv/deck"))
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
                },
                Source {
                    name: "books".into(),
                    kind: SourceKind::Audiobook,
                    path: "/mnt/usb/books".into(),
                },
                Source {
                    name: "story".into(),
                    kind: SourceKind::Story,
                    path: "/srv/deck/sounds".into(),
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
        let err = resolve(Vec::new(), Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("[[source]]"), "{err:#}");
    }
}
