//! The one way deckjay saves a file: write a temp file beside it, sync, rename,
//! sync the folder. A crash leaves the old file or the new one, never half of
//! one, and at worst a `.part` temp file that the podcast sweep deletes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Two saves in one process must not share a temp file.
static SAVES: AtomicU32 = AtomicU32::new(0);

/// Writes `bytes` to `path` through a unique temp file in the same folder.
/// `mode` is the Unix permission of the new file (ignored elsewhere); `None`
/// leaves it to the umask. The temp file is removed when any step fails.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let temp = temp_name(path);
    let written = write_new(&temp, bytes, mode).and_then(|()| fs::rename(&temp, path));
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    sync_dir(path);
    Ok(())
}

/// Moves a finished `part` file to `dest` and syncs the folder. The caller
/// has synced the file and removes `part` when this fails.
pub fn publish(part: &Path, dest: &Path) -> io::Result<()> {
    fs::rename(part, dest)?;
    sync_dir(dest);
    Ok(())
}

fn temp_name(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        ".{name}.{}.{}.part",
        std::process::id(),
        SAVES.fetch_add(1, Ordering::Relaxed)
    ))
}

fn write_new(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Makes the rename itself survive a power cut. Best effort: the file is
/// already complete either way, and not every platform can open a folder.
fn sync_dir(file: &Path) {
    let dir = match file.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let _ = File::open(dir).and_then(|dir| dir.sync_all());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_saves_of_one_file_use_two_temp_names() {
        let path = Path::new("/state/state.json");
        assert_ne!(temp_name(path), temp_name(path));
        let name = temp_name(path);
        assert_eq!(name.parent(), path.parent());
        assert!(name.to_string_lossy().ends_with(".part"));
    }

    #[test]
    fn publish_moves_the_part_over_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let (part, dest) = (dir.path().join(".ep.part"), dir.path().join("ep"));
        fs::write(&part, b"new").unwrap();
        fs::write(&dest, b"old").unwrap();

        publish(&part, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!part.exists());
    }
}
