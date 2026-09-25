//! `type = "spotify"` sources: Spotify playlists.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

use super::source::{Color, Look};

/// One `[[source.playlist]]` table, checked, with its URI normalised and its
/// picture resolved.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Playlist {
    pub name: String,
    /// `spotify:playlist:<id>`; the file may also hold a share link.
    pub uri: String,
    pub picture: Option<PathBuf>,
    pub color: Option<Color>,
}

/// A Spotify `[[source]]` table as written in the file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSpotify {
    pub(super) name: Option<String>,
    picture: Option<PathBuf>,
    color: Option<Color>,
    #[serde(default)]
    playlist: Vec<Playlist>,
}

impl RawSpotify {
    pub(super) fn look(&self) -> Look {
        Look {
            picture: self.picture.clone(),
            color: self.color,
        }
    }

    /// `name` is the source's name, for the error messages.
    pub(super) fn playlists(&self, name: &str, base: &Path) -> Result<Vec<Playlist>> {
        if self.playlist.is_empty() {
            bail!(
                "spotify source {name} has no playlist; add one:\n\n\
                 [[source.playlist]]\nname = \"...\"\nuri = \"https://open.spotify.com/playlist/...\""
            );
        }
        let mut names = std::collections::HashSet::new();
        self.playlist
            .iter()
            .map(|playlist| {
                if playlist.name.trim().is_empty() {
                    bail!("spotify source {name}: a playlist has an empty name");
                }
                if !names.insert(playlist.name.as_str()) {
                    bail!(
                        "spotify source {name}: two playlists are named {:?}",
                        playlist.name
                    );
                }
                Ok(Playlist {
                    uri: crate::spotify::normalize_playlist(&playlist.uri)?,
                    picture: playlist.picture.as_ref().map(|p| base.join(p)),
                    ..playlist.clone()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlists(text: &str) -> Result<Vec<Playlist>> {
        let raw: RawSpotify = toml::from_str(text)?;
        raw.playlists("spotify", Path::new("/srv/deck"))
    }

    #[test]
    fn normalises_share_links() {
        let got = playlists(
            "[[playlist]]\nname = \"Bedtime\"\n\
             uri = \"https://open.spotify.com/playlist/37i9dQZF1DX0XUsuxWHRQd?si=abc\"\n\
             picture = \"p.png\"\n",
        )
        .unwrap();
        assert_eq!(got[0].uri, "spotify:playlist:37i9dQZF1DX0XUsuxWHRQd");
        assert_eq!(
            got[0].picture.as_deref(),
            Some(Path::new("/srv/deck/p.png"))
        );
    }

    #[test]
    fn rejects_bad_playlists() {
        let one = "[[playlist]]\nname = \"A\"\nuri = \"spotify:playlist:abc\"\n";
        for (text, needle) in [
            ("", "no playlist"),
            (
                "[[playlist]]\nname = \"A\"\nuri = \"spotify:album:abc\"\n",
                "not a Spotify playlist",
            ),
            (
                "[[playlist]]\nname = \" \"\nuri = \"spotify:playlist:abc\"\n",
                "empty name",
            ),
            (&format!("{one}{one}") as &str, "two playlists"),
            (
                "[[playlist]]\nname = \"A\"\nurl = \"spotify:playlist:abc\"\n",
                "url",
            ),
        ] {
            let err = playlists(text).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{text}: {err:#}");
        }
    }
}
