//! The saved Spotify sign-in, `<state_dir>/spotify-token.json`: the app's
//! client id and the refresh token. Access tokens live only in memory.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "spotify-token.json";

/// Two saves in one process must not share a temp file.
static SAVES: AtomicU32 = AtomicU32::new(0);

/// A token. `Debug` prints `***` and there is no `Display`, so it cannot end
/// up in a log line or an error message by accident.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    /// Only for building a request.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenFile {
    /// A refresh token only works together with the app it was issued to.
    pub client_id: String,
    pub refresh_token: Secret,
}

impl TokenFile {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join(FILE_NAME)
    }

    pub fn load(state_dir: &Path) -> Result<TokenFile> {
        let path = TokenFile::path(state_dir);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => bail!(
                "not signed in to Spotify ({} is missing): run `deckjay spotify-login`",
                path.display()
            ),
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
        };
        serde_json::from_str(&text)
            .with_context(|| format!("{} is damaged: run `deckjay spotify-login`", path.display()))
    }

    /// Atomic: a crash leaves the old file or the new one, never half of one.
    /// Unix mode 0600, because the refresh token controls the account's playback.
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("cannot create {}", state_dir.display()))?;
        let path = TokenFile::path(state_dir);
        let temp = state_dir.join(format!(
            ".{FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            SAVES.fetch_add(1, Ordering::Relaxed)
        ));
        let json = serde_json::to_string_pretty(self)?;
        let written = write_new(&temp, json.as_bytes()).and_then(|()| fs::rename(&temp, &path));
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(err).with_context(|| format!("cannot save {}", path.display()));
        }
        sync_dir(state_dir);
        Ok(())
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Makes the rename itself survive a power cut. Best effort: the file is
/// already complete either way.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(dir) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(refresh: &str) -> TokenFile {
        TokenFile {
            client_id: "client-1".into(),
            refresh_token: Secret::new(refresh),
        }
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_saved_token_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        token("refresh-1").save(dir.path()).unwrap();
        let loaded = TokenFile::load(dir.path()).unwrap();
        assert_eq!(loaded, token("refresh-1"));
        assert_eq!(loaded.refresh_token.expose(), "refresh-1");
        assert_eq!(loaded.client_id, "client-1");
    }

    #[test]
    fn the_file_is_plain_json_with_two_keys() {
        let dir = tempfile::tempdir().unwrap();
        token("refresh-1").save(dir.path()).unwrap();
        let text = fs::read_to_string(TokenFile::path(dir.path())).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"client_id": "client-1", "refresh_token": "refresh-1"})
        );
    }

    #[test]
    fn saving_leaves_only_the_token_file() {
        let dir = tempfile::tempdir().unwrap();
        token("refresh-1").save(dir.path()).unwrap();
        token("refresh-2").save(dir.path()).unwrap();
        assert_eq!(entries(dir.path()), [FILE_NAME]);
        assert_eq!(TokenFile::load(dir.path()).unwrap(), token("refresh-2"));
    }

    #[test]
    fn saving_creates_the_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state").join("deeper");
        token("refresh-1").save(&state_dir).unwrap();
        assert_eq!(TokenFile::load(&state_dir).unwrap(), token("refresh-1"));
    }

    #[cfg(unix)]
    #[test]
    fn only_the_owner_can_read_the_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = TokenFile::path(dir.path());
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        token("refresh-1").save(dir.path()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_missing_file_asks_for_spotify_login() {
        let dir = tempfile::tempdir().unwrap();
        let err = TokenFile::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("run `deckjay spotify-login`"), "{err}");
    }

    #[test]
    fn a_damaged_file_asks_for_spotify_login() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(TokenFile::path(dir.path()), "{\"client_id\": ").unwrap();
        let err = format!("{:#}", TokenFile::load(dir.path()).unwrap_err());
        assert!(err.contains("run `deckjay spotify-login`"), "{err}");
    }

    #[test]
    fn debug_output_hides_the_token() {
        let shown = format!("{:?}", token("refresh-very-secret"));
        assert!(!shown.contains("very-secret"), "{shown}");
        assert!(shown.contains("***"), "{shown}");
    }
}
