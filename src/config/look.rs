//! What every `[[source]]` table shares: how a key looks and the names it checks.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

/// A picture or a colour for a key; pictures are resolved like `path`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub picture: Option<PathBuf>,
    /// The background of a key without a picture.
    pub color: Option<Color>,
}

impl Look {
    pub(super) fn resolve(self, base: &Path) -> Look {
        Look {
            picture: self.picture.map(|p| base.join(p)),
            color: self.color,
        }
    }
}

/// The `picture` and `color` keys of a `[[source]]` table, not yet resolved.
pub(super) fn look(picture: Option<PathBuf>, color: Option<Color>) -> Look {
    Look { picture, color }
}

/// Every `what` (e.g. `"station"`) of `source` (e.g. `"radio source radio"`)
/// has a name, and no two share one.
pub(super) fn check_named<T>(
    what: &str,
    source: &str,
    items: &[T],
    name: impl Fn(&T) -> &str,
) -> Result<()> {
    let mut names = HashSet::new();
    for item in items {
        let name = name(item);
        if name.trim().is_empty() {
            bail!("{source}: a {what} has an empty name");
        }
        if !names.insert(name) {
            bail!("{source}: two {what}s are named {name:?}");
        }
    }
    Ok(())
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
