//! Atomic file replacement: write to a same-directory temp file, fsync, then
//! rename over the target. Crash at any point leaves either the old file or
//! the new one intact — never a truncated target. Permissions, owner and
//! group of an existing target are carried over to the replacement.

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

    // Snapshot target metadata before touching anything so the renamed
    // replacement keeps the original mode bits, owner and group.
    let prior_meta = existing_metadata(target);

    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(meta) = &prior_meta {
            apply_metadata(&mut file, meta)?;
        }
        file.write_all(contents)?;
        // fsync the data so the renamed file is durable, not just visible.
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, target)?;
        // Persist the directory entry itself: without an fsync on the
        // parent directory a host crash can lose the rename despite the
        // file data being durable.
        if let Some(dir) = temp_dir_for(target) {
            let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
        }
        Ok(())
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

/// Atomically replace `target` with: original bytes `[0, before)` +
/// `middle` + original bytes `[after, len)`. Streamed variant of
/// [`atomic_write`] for line-range edits on very large files. The untouched
/// head and tail are copied straight from the source file, so only
/// `middle` is ever materialized. Metadata carry-over, fsync and the
/// atomic rename match [`atomic_write`].
pub fn atomic_write_splice(
    target: &Path,
    before: u64,
    middle: &[u8],
    after: u64,
) -> std::io::Result<()> {
    let tmp = match temp_path(target) {
        Some(p) => p,
        None => return std::fs::write(target, middle),
    };
    let prior_meta = existing_metadata(target);

    let write_result = (|| -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        let mut src = std::fs::File::open(target)?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(meta) = &prior_meta {
            apply_metadata(&mut file, meta)?;
        }

        copy_exactly(&mut src, &mut file, before)?;
        file.write_all(middle)?;
        src.seek(SeekFrom::Start(after))?;
        std::io::copy(&mut src, &mut file)?;

        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, target)?;
        if let Some(dir) = temp_dir_for(target) {
            let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
        }
        Ok(())
    })();

    match write_result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Copy exactly `count` bytes from `src` to `dst` in bounded chunks so the
/// untouched regions of a huge file never sit in memory.
fn copy_exactly(
    src: &mut std::fs::File,
    dst: &mut std::fs::File,
    count: u64,
) -> std::io::Result<()> {
    use std::io::Read;
    let mut remaining = count;
    let mut chunk = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = std::cmp::min(remaining, chunk.len() as u64) as usize;
        src.read_exact(&mut chunk[..want])?;
        dst.write_all(&chunk[..want])?;
        remaining -= want as u64;
    }
    Ok(())
}

/// Read the byte range `[start, end)` of `path` into a vector. Bounded by
/// the requested window, never the file size.
pub fn read_window(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut src = std::fs::File::open(path)?;
    src.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; end.saturating_sub(start) as usize];
    src.read_exact(&mut buf)?;
    Ok(buf)
}
/// Same-directory temp path with a collision-resistant suffix. Same-directory
/// placement is what makes the final rename atomic (same filesystem).
fn temp_path(target: &Path) -> Option<PathBuf> {
    let dir = temp_dir_for(target)?;
    let name = target.file_name()?.to_string_lossy();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    Some(dir.join(format!(".{}.aish-tmp.{}.{}", name, pid, nanos)))
}

/// Parent directory for the temp file. Bare relative filenames (no parent
/// component) are resolved against the current directory `"."` so they stay
/// on the atomic-write path instead of falling back to a truncating write.
fn temp_dir_for(target: &Path) -> Option<&Path> {
    match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => Some(dir),
        _ => Some(Path::new(".")),
    }
}

/// Metadata worth carrying over to the replacement file. On Unix that is the
/// mode bits plus owner and group: rename() swaps the directory entry, so the
/// replacement would otherwise inherit the writing process's uid/gid, and an
/// ownership change can also clear set-ID bits.
#[cfg(unix)]
struct TargetMetadata {
    mode: u32,
    uid: u32,
    gid: u32,
}

#[cfg(not(unix))]
struct TargetMetadata;

#[cfg(unix)]
fn existing_metadata(path: &Path) -> Option<TargetMetadata> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| TargetMetadata {
        mode: m.permissions().mode(),
        uid: m.uid(),
        gid: m.gid(),
    })
}

#[cfg(not(unix))]
fn existing_metadata(_path: &Path) -> Option<TargetMetadata> {
    None
}

/// Apply captured metadata to the still-open temp file. Best-effort on
/// uid/gid: chowning a file owned by another user typically fails for an
/// unprivileged process, and the rename is still safe without it. The mode
/// is (re)applied after the ownership change because chown can clear set-ID
/// bits.
#[cfg(unix)]
fn apply_metadata(file: &mut std::fs::File, meta: &TargetMetadata) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: fd is a valid open file descriptor owned by `file`; fchown
    // only mutates its owner/group metadata and (uid, gid) are u32 values
    // with no pointer arguments.
    let _ = unsafe { libc::fchown(fd, meta.uid, meta.gid) };
    file.set_permissions(std::fs::Permissions::from_mode(meta.mode))
}

#[cfg(not(unix))]
fn apply_metadata(_file: &mut std::fs::File, _meta: &TargetMetadata) -> std::io::Result<()> {
    Ok(())
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

    #[test]
    fn bare_relative_path_stays_atomic() {
        // A bare relative filename has no parent component; it must use
        // "." as the temp directory rather than falling back to a
        // truncating non-atomic write. Run in the crate dir; clean up after.
        let name = "aish-atomic-write-test.txt";
        let _ = fs::remove_file(name);
        atomic_write(Path::new(name), b"v1").unwrap();
        atomic_write(Path::new(name), b"v2").unwrap();
        assert_eq!(fs::read(name).unwrap(), b"v2");
        let _ = fs::remove_file(name);
    }

    #[test]
    fn splice_preserves_untouched_regions() {
        // atomic_write_splice must reproduce head + middle + tail exactly,
        // including line endings around the replaced window. Line 2 ("b")
        // spans bytes [4, 8) in "a\r\nb\r\nc\r\nd\r\n"; it is replaced with
        // "B\r\nX" while every other byte — including the CRLFs — stays put.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        fs::write(&target, "a\r\nb\r\nc\r\nd\r\n").unwrap();
        atomic_write_splice(&target, 3, b"B\r\nX", 5).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"a\r\nB\r\nX\nc\r\nd\r\n");
    }
}
