//! Writes that a reader cannot catch half-done.
//!
//! A cache entry or a config file written in place is observable while it is being written: a
//! reader that arrives mid-write sees a truncated file, and a crash mid-write leaves it that way.
//! For the image cache that is worse than a miss, because [`crate::markdown`] treats an existing
//! file as a hit - a half-downloaded image would be served forever.
//!
//! Writing to a temporary file *in the same directory* and renaming it into place makes the swap
//! atomic: a reader sees the old file or the new one, never a prefix of the new one, and a failed
//! write leaves the destination untouched. The file is not `fsync`ed: this is about what another
//! process can *observe*, not about what survives a power cut, and these are caches.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::{CliError, Result};

/// A sibling path to write into before [`commit`].
///
/// In the same directory on purpose: a rename across filesystems is a copy, and stops being atomic.
pub fn staging_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.subsec_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()))
}

/// Move a staged write into place, replacing whatever was there.
pub fn commit(staged: &Path, path: &Path) -> Result<()> {
    fs::rename(staged, path).map_err(|error| {
        // A rename that failed leaves the staging file behind; it is debris, not a cache entry.
        let _ = fs::remove_file(staged);
        CliError::cli(format!("Failed to replace {}", path.display())).cause(error)
    })
}

/// Write `bytes` to `path` so that no reader can observe a partial file.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let staged = staging_path(path);
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&staged)?;
        file.write_all(bytes)?;
        file.flush()
    })();

    if let Err(error) = written {
        let _ = fs::remove_file(&staged);
        return Err(CliError::cli(format!("Failed to write {}", staged.display())).cause(error));
    }

    commit(&staged, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("linear-atomic-{}-{name}", std::process::id()))
    }

    #[test]
    fn a_write_replaces_the_whole_file_and_leaves_no_staging_copy() {
        let path = scratch("replace.txt");
        fs::write(&path, b"the old content, longer than the new one").expect("seed");

        write(&path, b"new").expect("write");

        assert_eq!(fs::read_to_string(&path).expect("read"), "new");
        let debris: Vec<String> = fs::read_dir(path.parent().expect("dir"))
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("replace.txt") && name.ends_with(".tmp"))
            .collect();
        assert!(debris.is_empty(), "staging files left behind: {debris:?}");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_failed_commit_cleans_up_its_staging_file() {
        // The failure a swap can actually have: the rename does not land. What matters is that the
        // staging file does not survive as debris next to the cache.
        let staged = scratch("debris.txt.tmp");
        fs::write(&staged, b"half a picture").expect("stage");
        let unreachable = PathBuf::from("/nonexistent-directory-for-this-test/entry");

        let error = commit(&staged, &unreachable).expect_err("the rename cannot land");
        assert!(error.to_string().contains("Failed to replace"), "{error:?}");
        assert!(
            !staged.exists(),
            "the staging file should have been cleaned up"
        );
    }

    #[test]
    fn the_staging_path_is_a_sibling() {
        let path = PathBuf::from("/tmp/somewhere/image.png");
        let staged = staging_path(&path);
        assert_eq!(
            staged.parent(),
            path.parent(),
            "a rename across a filesystem is a copy"
        );
        assert_ne!(staged, path);
    }
}
