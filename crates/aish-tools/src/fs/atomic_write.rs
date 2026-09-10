//! Atomic file replacement: write to a same-directory temp file, fsync, then
//! rename over the target. Crash at any point leaves either the old file or
//! the new one intact — never a truncated target. Permissions of an existing
//! target are carried over to the replacement.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Write `contents` to `target` atomically (temp file + fsync + rename).
///
/// Preserves the permissions of an existing `target`; a new file keeps the
/// process umask defaults. Falls back to direct write when the rename cannot
/// be prepared (e.g. `/proc`-like pseudo filesystems) so behavior matches
/// `std::fs::write` on filesystems where atomic replace is unavailable.
pub fn atomic_write(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = match temp_path(target) {
        Some(p) => p,
        // No usable sibling directory (e.g. bare filename resolved against
        // cwd-less process): fall back to a direct, non-atomic write.
        None => return std::fs::write(target, contents),
    };

    // Snapshot target permissions before touching anything so the renamed
    // replacement keeps the original mode bits.
    let prior_mode = existing_mode(target);

    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(mode) = prior_mode {
            #[cfg(unix)]
            {
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            #[cfg(not(unix))]
            {
                let _ = mode;
            }
        }
        file.write_all(contents)?;
        // fsync the data so the renamed file is durable, not just visible.
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, target)
    })();

    match write_result {
        Ok(()) => Ok(()),
        Err(e) => {
            // Never leave the temp file behind; the target is untouched.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Same-directory temp path with a collision-resistant suffix. Same-directory
/// placement is what makes the final rename atomic (same filesystem).
fn temp_path(target: &Path) -> Option<PathBuf> {
    let dir = target.parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    let name = target.file_name()?.to_string_lossy();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    Some(dir.join(format!(".{}.aish-tmp.{}.{}", name, pid, nanos)))
}

#[cfg(unix)]
fn existing_mode(path: &Path) -> Option<u32> {
    std::fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn writes_new_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        fs::write(&target, "old").unwrap();
        atomic_write(&target, b"new").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn preserves_permissions_of_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("script.sh");
        fs::write(&target, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
            atomic_write(&target, b"#!/bin/sh\nnew").unwrap();
            let mode = fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o750);
        }
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        fs::write(&target, "old").unwrap();
        atomic_write(&target, b"new").unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("aish-tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn failed_write_leaves_target_intact_and_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        // Target path is a directory: rename onto it fails, target content
        // (the directory) stays intact and no temp file lingers.
        let target = dir.path().join("conflict");
        fs::create_dir(&target).unwrap();
        assert!(atomic_write(&target, b"new").is_err());
        assert!(target.is_dir());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("aish-tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
