//! `type = "radio"` sources: internet radio stations.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

use super::source::{Color, Look};

/// One `[[source.station]]` table, checked and with its picture resolved.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Station {
    pub name: String,
    /// A stream, or a `.pls` / `.m3u` playlist that names one.
    pub url: String,
    pub picture: Option<PathBuf>,
    pub color: Option<Color>,
}

/// A radio `[[source]]` table as written in the file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRadio {
    pub(super) name: Option<String>,
    picture: Option<PathBuf>,
    color: Option<Color>,
    #[serde(default)]
    station: Vec<Station>,
}

impl RawRadio {
    pub(super) fn look(&self) -> Look {
        Look {
            picture: self.picture.clone(),
            color: self.color,
        }
    }

    /// The stations with pictures resolved against `base`; `name` is the
    /// source's name, for the error messages.
    pub(super) fn stations(&self, name: &str, base: &Path) -> Result<Vec<Station>> {
        if self.station.is_empty() {
            bail!(
                "radio source {name} has no station; add one:\n\n\
                 [[source.station]]\nname = \"...\"\nurl = \"https://...\""
            );
        }
        let mut names = std::collections::HashSet::new();
        self.station
            .iter()
            .map(|station| {
                if station.name.trim().is_empty() {
                    bail!("radio source {name}: a station has an empty name");
                }
                if !names.insert(station.name.as_str()) {
                    bail!(
                        "radio source {name}: two stations are named {:?}",
                        station.name
                    );
                }
                if !(station.url.starts_with("https://") || station.url.starts_with("http://")) {
                    bail!(
                        "station {}: the url must start with https:// or http://",
                        station.name
                    );
                }
                Ok(Station {
                    picture: station.picture.as_ref().map(|p| base.join(p)),
                    ..station.clone()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stations(text: &str) -> Result<Vec<Station>> {
        let raw: RawRadio = toml::from_str(text)?;
        raw.stations("radio", Path::new("/srv/deck"))
    }

    #[test]
    fn reads_stations_and_resolves_pictures() {
        let got = stations(
            "[[station]]\nname = \"Kinder\"\nurl = \"https://example.org/kinder.mp3\"\n\
             picture = \"pictures/kinder.png\"\ncolor = \"#102030\"\n",
        )
        .unwrap();
        assert_eq!(
            got,
            [Station {
                name: "Kinder".into(),
                url: "https://example.org/kinder.mp3".into(),
                picture: Some("/srv/deck/pictures/kinder.png".into()),
                color: Some(Color([0x10, 0x20, 0x30])),
            }]
        );
    }

    #[test]
    fn rejects_bad_stations() {
        let one = "[[station]]\nname = \"A\"\nurl = \"https://a/x.mp3\"\n";
        for (text, needle) in [
            ("", "no station"),
            (
                "[[station]]\nname = \"A\"\nurl = \"rtsp://a/x\"\n",
                "https://",
            ),
            (
                "[[station]]\nname = \"\"\nurl = \"https://a/x\"\n",
                "empty name",
            ),
            (&format!("{one}{one}") as &str, "two stations"),
            (
                "[[station]]\nname = \"A\"\nurl = \"https://a\"\nstream = \"x\"\n",
                "stream",
            ),
        ] {
            let err = stations(text).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{text}: {err:#}");
        }
    }
}
