//! The one way deckjay saves a file: write a temp file beside it, sync, rename,
//! sync the folder. A crash leaves the old file or the new one, never half of
//! one, and at worst a `.part` temp file. The podcast cache sweep and
//! [`remove_stale_temps`] delete those.

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

/// Deletes the temp files that `write_atomic` of an earlier process left
/// beside `path` (a crash or a kill between the write and the rename). A temp
/// file with this process's id stays: another thread may be writing it.
/// So a temp left by an earlier process with the same id (pid 1 in Docker)
/// stays too: the next save to that name fails once with `AlreadyExists`,
/// its error path removes the temp, and the save after it works. No data is
/// lost. Best effort; call it before a save, not in a loop.
pub fn remove_stale_temps(path: &Path) {
    let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return;
    };
    let prefix = format!(
        ".{}.",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        let Some((pid, count)) = rest
            .strip_suffix(".part")
            .and_then(|ids| ids.split_once('.'))
        else {
            continue;
        };
        let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        if digits(count) && digits(pid) && pid.parse() != Ok(std::process::id()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub(crate) fn temp_name(path: &Path) -> PathBuf {
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

    #[test]
    fn only_the_temps_of_another_process_beside_the_target_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x");
        let own = temp_name(&target);
        let own_name = own.file_name().unwrap().to_str().unwrap().to_owned();
        let other_pid = std::process::id().wrapping_add(1);
        let gone = format!(".x.{other_pid}.0.part");
        let kept_files = [
            "x".to_owned(),
            own_name,
            ".x.12.part".into(),
            ".x.1.2.3.part".into(),
            ".x..part".into(),
            "..x.1.2.part".into(),
            ".x.1.2.part.bak".into(),
            ".x.bad.1.2.part".into(),
            ".x.+5.1.part".into(),
            ".y.1.2.part".into(),
        ];
        for name in kept_files.iter().chain([&gone]) {
            fs::write(dir.path().join(name), b"data").unwrap();
        }
        fs::create_dir(dir.path().join(".x.5.6.part")).unwrap();

        remove_stale_temps(&target);

        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut want: Vec<String> = kept_files.to_vec();
        want.push(".x.5.6.part".into());
        want.sort();
        assert_eq!(left, want);
    }
}
