//! Filesystem helpers with the durability guarantees a long-lived server needs.
//!
//! [`write_atomic`] is used for every piece of state this CLI persists
//! (credentials, saved issue views). A partially written file after a crash or
//! a full disk would be worse than no file at all — an agent would then fail
//! to parse its own config on the next invocation, with no way to recover
//! except manual deletion. Writing to a sibling temp file and renaming over the
//! target makes the update atomic on POSIX filesystems.
//!
//! Secrets (`credentials.toml`) are created mode `0600` so another process on a
//! shared host cannot read API keys out of the config directory.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Write `contents` to `path` atomically, creating parent directories as needed.
///
/// On Unix the file is created with mode `0o600`. The temp file lives in the
/// destination directory so the final `rename` stays on the same filesystem.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent directory: {}", path.display()),
        )
    })?;
    fs::create_dir_all(dir)?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "state".to_string());
    let temp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Best-effort read of a file, with "not found" and "unreadable" collapsed into
/// `None`. Used for optional state where absence is normal.
pub fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.toml");
        write_atomic(&path, "hello = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello = 1\n");
    }

    #[test]
    fn replaces_existing_file_wholesale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        write_atomic(&path, "aaaaaaaaaa").unwrap();
        write_atomic(&path, "b").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "b");
    }

    #[cfg(unix)]
    #[test]
    fn creates_file_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        write_atomic(&path, "default = \"acme\"\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        write_atomic(&path, "x").unwrap();
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["state.toml".to_string()]);
    }
}
