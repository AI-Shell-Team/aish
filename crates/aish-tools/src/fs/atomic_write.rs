//! Atomic file replacement: write to a same-directory temp file, fsync, then
//! rename over the target. Crash at any point leaves either the old file or
//! the new one intact — never a truncated target. Permissions, owner, group
//! and extended attributes (including POSIX ACLs) of an existing target are
//! carried over to the replacement, and writing through a symlink replaces
//! the link's destination rather than the link itself. File capabilities
//! (`security.capability`) are intentionally not carried over: changing the
//! content clears them, matching in-place edit semantics.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::snapshot_store::{SnapshotTag, StreamingTagHasher};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Follow a chain of symlinks to the path that should actually be replaced.
///
/// `rename` operates on the directory entry it is given, so renaming a temp
/// file onto a symlink replaces the link with a regular file. Resolving the
/// chain first keeps the link intact and updates its destination instead
/// (issue #688). Relative targets are resolved against the link's directory.
fn resolve_write_target(target: &Path) -> std::io::Result<PathBuf> {
    const MAX_SYMLINK_DEPTH: usize = 40;

    let mut current = target.to_path_buf();
    for _ in 0..MAX_SYMLINK_DEPTH {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&current)?;
                current = if link.is_absolute() {
                    link
                } else {
                    current
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(link)
                };
            }
            // Regular file, missing target (dangling link), or any other
            // file type: this is the entry to write.
            _ => return Ok(current),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "too many levels of symbolic links",
    ))
}

/// Write `contents` to `target` atomically (temp file + fsync + rename).
///
/// Preserves the permissions of an existing `target`; a new file keeps the
/// process umask defaults. Falls back to direct write when the rename cannot
/// be prepared (e.g. `/proc`-like pseudo filesystems) so behavior matches
/// `std::fs::write` on filesystems where atomic replace is unavailable.
pub fn atomic_write(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let resolved = resolve_write_target(target)?;
    let target = resolved.as_path();
    let tmp = match temp_path(target) {
        Some(p) => p,
        // No usable sibling directory (e.g. bare filename resolved against
        // cwd-less process): fall back to a direct, non-atomic write.
        None => return std::fs::write(target, contents),
    };

    // Snapshot target metadata before touching anything so the renamed
    // replacement keeps the original mode bits, owner and group.
    let prior_meta = existing_metadata(target)?;

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
///
/// Returns the snapshot tag of the exact bytes written. The splice streams
/// the output anyway, so hashing it here avoids a second full read just to
/// refresh the session's file anchor (issues #692, #696).
pub fn atomic_write_splice(
    target: &Path,
    before: u64,
    middle: &[u8],
    after: u64,
) -> std::io::Result<SnapshotTag> {
    let resolved = resolve_write_target(target)?;
    let target = resolved.as_path();
    let tmp = match temp_path(target) {
        Some(p) => p,
        None => {
            std::fs::write(target, middle)?;
            return Ok(SnapshotTag::from_bytes(middle));
        }
    };
    let prior_meta = existing_metadata(target)?;

    let write_result = (|| -> std::io::Result<SnapshotTag> {
        use std::io::{Seek, SeekFrom};
        let mut src = std::fs::File::open(target)?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(meta) = &prior_meta {
            apply_metadata(&mut file, meta)?;
        }

        let mut hasher = StreamingTagHasher::new();
        copy_exactly(&mut src, &mut file, before, &mut hasher)?;
        file.write_all(middle)?;
        hasher.update(middle);
        src.seek(SeekFrom::Start(after))?;
        copy_remaining(&mut src, &mut file, &mut hasher)?;

        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, target)?;
        if let Some(dir) = temp_dir_for(target) {
            let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
        }
        Ok(hasher.finalize())
    })();

    match write_result {
        Ok(tag) => Ok(tag),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Copy exactly `count` bytes from `src` to `dst` in bounded chunks so the
/// untouched regions of a huge file never sit in memory. The same bytes are
/// fed to the tag hasher, so no second pass is needed.
fn copy_exactly(
    src: &mut std::fs::File,
    dst: &mut std::fs::File,
    count: u64,
    hasher: &mut StreamingTagHasher,
) -> std::io::Result<()> {
    use std::io::Read;
    let mut remaining = count;
    let mut chunk = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = std::cmp::min(remaining, chunk.len() as u64) as usize;
        src.read_exact(&mut chunk[..want])?;
        dst.write_all(&chunk[..want])?;
        hasher.update(&chunk[..want]);
        remaining -= want as u64;
    }
    Ok(())
}

/// Copy `src` to EOF in bounded chunks, hashing every byte written.
fn copy_remaining(
    src: &mut std::fs::File,
    dst: &mut std::fs::File,
    hasher: &mut StreamingTagHasher,
) -> std::io::Result<u64> {
    use std::io::Read;
    let mut chunk = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut chunk)?;
        if n == 0 {
            return Ok(total);
        }
        dst.write_all(&chunk[..n])?;
        hasher.update(&chunk[..n]);
        total += n as u64;
    }
}

/// Outcome of a conditional replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalWriteOutcome {
    /// The target matched `expected` at commit time and was replaced.
    Written,
    /// The target no longer matched `expected`; it was left untouched.
    Conflict,
}

/// Atomically replace `target` with `contents`, but only if the target still
/// contains `expected` at commit time.
///
/// On Linux this uses `renameat2(RENAME_EXCHANGE)`: the new content is written
/// to a sibling temp file, then the two paths are swapped atomically. The
/// swapped-out entry (the target as it existed at the instant of the swap) is
/// compared with `expected`. On match the old entry is removed; on mismatch
/// the swap is reverted, preserving the external change and returning
/// [`ConditionalWriteOutcome::Conflict`].
///
/// This is a commit-time conditional replace, so it does not rely on a
/// read-then-rename window. On filesystems/targets where `RENAME_EXCHANGE` is
/// unavailable, it falls back to a best-effort read-compare-write.
pub fn atomic_write_if_unchanged(
    target: &Path,
    expected: &[u8],
    contents: &[u8],
) -> std::io::Result<ConditionalWriteOutcome> {
    #[cfg(target_os = "linux")]
    {
        atomic_write_if_unchanged_linux(target, expected, contents)
    }
    #[cfg(not(target_os = "linux"))]
    {
        atomic_write_if_unchanged_fallback(target, expected, contents)
    }
}

#[cfg(target_os = "linux")]
fn atomic_write_if_unchanged_linux(
    target: &Path,
    expected: &[u8],
    contents: &[u8],
) -> std::io::Result<ConditionalWriteOutcome> {
    let resolved = resolve_write_target(target)?;
    let target = resolved.as_path();
    let tmp = match temp_path(target) {
        Some(p) => p,
        None => return atomic_write_if_unchanged_fallback(target, expected, contents),
    };
    let prior_meta = existing_metadata(target)?;

    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(meta) = &prior_meta {
            apply_metadata(&mut file, meta)?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = rename_exchange(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        // The filesystem may not implement RENAME_EXCHANGE. Fall back to a
        // best-effort conditional write rather than failing the edit.
        if matches!(
            e.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
        ) {
            return atomic_write_if_unchanged_fallback(target, expected, contents);
        }
        return Err(e);
    }

    // The swap put the previous target at `tmp`. Compare exactly the bytes
    // that were at the target when the exchange committed.
    let matched = match std::fs::metadata(&tmp) {
        Ok(meta) if meta.len() == expected.len() as u64 => std::fs::read(&tmp)
            .map(|old| old == expected)
            .unwrap_or(false),
        _ => false,
    };

    if matched {
        // Our contents are now at `target`; discard the old entry.
        let _ = std::fs::remove_file(&tmp);
        sync_parent_dir(target);
        return Ok(ConditionalWriteOutcome::Written);
    }

    // Conflict: put the external content back at `target` and discard our
    // replacement from the temp path.
    match rename_exchange(&tmp, target) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            sync_parent_dir(target);
            Ok(ConditionalWriteOutcome::Conflict)
        }
        Err(e) => {
            // Leave `tmp` in place: it still holds the external content.
            Err(e)
        }
    }
}

fn atomic_write_if_unchanged_fallback(
    target: &Path,
    expected: &[u8],
    contents: &[u8],
) -> std::io::Result<ConditionalWriteOutcome> {
    let resolved = resolve_write_target(target)?;
    let target = resolved.as_path();
    match std::fs::read(target) {
        Ok(current) if current == expected => {
            atomic_write(target, contents)?;
            Ok(ConditionalWriteOutcome::Written)
        }
        Ok(_) => Ok(ConditionalWriteOutcome::Conflict),
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "linux")]
fn rename_exchange(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let right = CString::new(right.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))?;

    // SAFETY: both pointers are valid NUL-terminated strings that outlive the
    // call; renameat2 has no other preconditions.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn sync_parent_dir(target: &Path) {
    if let Some(dir) = temp_dir_for(target) {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
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
    /// Extended attributes, which on Linux carry POSIX ACLs, security
    /// labels and user metadata. Captured before the replacement and
    /// re-applied afterwards (issue #691).
    #[cfg(target_os = "linux")]
    xattrs: Vec<(std::ffi::CString, Vec<u8>)>,
}

#[cfg(not(unix))]
struct TargetMetadata;

#[cfg(unix)]
fn existing_metadata(path: &Path) -> std::io::Result<Option<TargetMetadata>> {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        // Only a genuinely missing target means "new file". Any other
        // failure leaves us unable to capture the ACL, so fail before the
        // replacement instead of silently dropping it (issue #691).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(TargetMetadata {
        mode: meta.permissions().mode(),
        uid: meta.uid(),
        gid: meta.gid(),
        #[cfg(target_os = "linux")]
        xattrs: existing_xattrs(path)?,
    }))
}

#[cfg(not(unix))]
fn existing_metadata(_path: &Path) -> std::io::Result<Option<TargetMetadata>> {
    Ok(None)
}

/// Extended attribute name carrying the POSIX access ACL.
#[cfg(target_os = "linux")]
const ACL_ACCESS_XATTR: &[u8] = b"system.posix_acl_access";

/// Collect the attributes that should survive an atomic replacement.
///
/// The access ACL is captured separately and first: a race in the bulk
/// `listxattr` enumeration (for example, `ERANGE` after another process adds
/// an attribute) must never be mistaken for "there is no ACL". Confirmed
/// absence (`ENODATA`) and filesystems without xattr support (`ENOTSUP`) are
/// fine; any other capture failure aborts the write while the original file
/// is still intact (issue #691).
#[cfg(target_os = "linux")]
fn existing_xattrs(path: &Path) -> std::io::Result<Vec<(std::ffi::CString, Vec<u8>)>> {
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;

    let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Ok(Vec::new());
    };
    let mut attrs = Vec::new();

    // Capture the access ACL independently of best-effort enumeration.
    let acl_name = std::ffi::CString::new(ACL_ACCESS_XATTR).expect("static ACL name");
    match get_xattr(&cpath, &acl_name) {
        Ok(Some(value)) => attrs.push((acl_name, value)),
        // ENODATA / ENOTSUP are mapped to Ok(None): confirmed absence, or a
        // filesystem that cannot store ACLs.
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Enumerate the remaining attributes. `listxattr` can race with another
    // process adding an attribute and return ERANGE for the second call;
    // retry instead of treating that as "no attributes".
    let mut names = Vec::new();
    for attempt in 0..4 {
        let len = unsafe { libc::listxattr(cpath.as_ptr(), ptr::null_mut(), 0) };
        if len < 0 {
            let err = std::io::Error::last_os_error();
            if is_xattr_unsupported(&err) {
                return Ok(attrs);
            }
            return Err(err);
        }
        if len == 0 {
            names.clear();
            break;
        }
        let mut buf = vec![0u8; len as usize];
        let got = unsafe { libc::listxattr(cpath.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if got >= 0 {
            buf.truncate(got as usize);
            names = buf;
            break;
        }
        let err = std::io::Error::last_os_error();
        if is_xattr_unsupported(&err) {
            return Ok(attrs);
        }
        if err.raw_os_error() != Some(libc::ERANGE) || attempt == 3 {
            return Err(err);
        }
    }

    for name in names.split(|&b| b == 0).filter(|name| !name.is_empty()) {
        if name == ACL_ACCESS_XATTR {
            // Already captured above; a concurrent change here is not a
            // reason to duplicate or drop the required ACL entry.
            continue;
        }
        // Linux clears file capabilities when a file's content is written
        // (file_remove_privs), so copying an old capability onto changed
        // content would either be silently dropped or, worse, grant the new
        // content privileges the old one had. Keep in-place edit semantics.
        if name == b"security.capability" {
            continue;
        }
        let Ok(cname) = std::ffi::CString::new(name) else {
            continue;
        };
        // The attribute disappeared between listxattr and getxattr, or
        // cannot be read: non-ACL attributes stay best-effort.
        if let Ok(Some(value)) = get_xattr(&cpath, &cname) {
            attrs.push((cname, value));
        }
    }

    Ok(attrs)
}

/// Read one named xattr.
///
/// Returns `Ok(None)` for confirmed absence (`ENODATA`) or a filesystem
/// without xattr support (`ENOTSUP`); other errors are propagated.
#[cfg(target_os = "linux")]
fn get_xattr(path: &std::ffi::CStr, name: &std::ffi::CStr) -> std::io::Result<Option<Vec<u8>>> {
    use std::ptr;

    let len = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), ptr::null_mut(), 0) };
    if len < 0 {
        let err = std::io::Error::last_os_error();
        return if is_xattr_missing(&err) {
            Ok(None)
        } else {
            Err(err)
        };
    }
    let mut value = vec![0u8; len as usize];
    if len == 0 {
        return Ok(Some(value));
    }
    let got = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if got < 0 {
        return Err(std::io::Error::last_os_error());
    }
    value.truncate(got as usize);
    Ok(Some(value))
}

#[cfg(target_os = "linux")]
fn is_xattr_missing(err: &std::io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::ENODATA) | Some(libc::ENOTSUP)
    )
}

#[cfg(target_os = "linux")]
fn is_xattr_unsupported(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(libc::ENOTSUP)
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
    file.set_permissions(std::fs::Permissions::from_mode(meta.mode))?;
    apply_xattrs(file, meta)?;
    Ok(())
}

/// Re-apply captured extended attributes to the replacement file. Best-effort
/// for the same reason as `fchown`: some attributes (e.g. `security.*`) need
/// privileges the process may not have, and a failed copy must not abort a
/// content write. POSIX ACLs live in `system.posix_acl_*`, so they are
/// restored by this pass (issue #691).
#[cfg(target_os = "linux")]
fn apply_xattrs(file: &std::fs::File, meta: &TargetMetadata) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    apply_xattr_list(&meta.xattrs, |name, value| {
        // SAFETY: fd is a valid open file descriptor owned by `file`; name is
        // NUL-terminated and value points to `value.len()` readable bytes.
        let rc =
            unsafe { libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    })
}

/// Apply captured attributes through `set_value`.
///
/// Losing `system.posix_acl_access` changes who may access the file, so a
/// failure to restore it is fatal and must abort the write before the
/// original is replaced (issue #691). Every other attribute stays
/// best-effort: e.g. `security.*` may require privileges the caller lacks.
#[cfg(target_os = "linux")]
fn apply_xattr_list(
    attrs: &[(std::ffi::CString, Vec<u8>)],
    mut set_value: impl FnMut(&std::ffi::CString, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for (name, value) in attrs {
        let required = name.as_bytes() == b"system.posix_acl_access";
        match set_value(name, value) {
            Ok(()) => {}
            Err(e) if required => return Err(e),
            Err(_) => {}
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn apply_xattrs(_file: &std::fs::File, _meta: &TargetMetadata) -> std::io::Result<()> {
    Ok(())
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
    #[cfg(unix)]
    #[test]
    fn write_through_symlink_preserves_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.conf");
        let link = dir.path().join("link.conf");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink("real.conf", &link).unwrap();

        atomic_write(&link, b"new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink must not be replaced by a regular file"
        );
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("real.conf"));
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn splice_through_symlink_preserves_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        fs::write(&real, "a\nb\nc\n").unwrap();
        std::os::unix::fs::symlink("real.txt", &link).unwrap();

        // Replace "b" with "B" through the link.
        atomic_write_splice(&link, 2, b"B", 3).unwrap();

        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"a\nB\nc\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_user_extended_attributes() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("attrs.conf");
        fs::write(&target, "old").unwrap();

        let cpath = CString::new(target.as_os_str().as_bytes()).unwrap();
        let cname = CString::new("user.aish_test").unwrap();
        let value = b"keep-me";
        let rc = unsafe {
            libc::setxattr(
                cpath.as_ptr(),
                cname.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if rc != 0 {
            eprintln!("skipping: filesystem does not support user xattrs");
            return;
        }

        atomic_write(&target, b"new").unwrap();

        let mut buf = [0u8; 32];
        let n = unsafe {
            libc::getxattr(
                cpath.as_ptr(),
                cname.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        assert_eq!(n, value.len() as isize, "extended attribute was lost");
        assert_eq!(&buf[..n as usize], value);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_posix_acl_when_tools_available() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("acl.conf");
        fs::write(&target, "old").unwrap();
        let path = target.to_str().unwrap();

        let Ok(out) = Command::new("setfacl")
            .args(["-m", "u:65534:r", path])
            .output()
        else {
            eprintln!("skipping: setfacl not installed");
            return;
        };
        if !out.status.success() {
            eprintln!(
                "skipping: setfacl failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }

        atomic_write(&target, b"new").unwrap();

        let Ok(out) = Command::new("getfacl").args(["-c", "-p", path]).output() else {
            eprintln!("skipping: getfacl not installed");
            return;
        };
        let acl = String::from_utf8_lossy(&out.stdout);
        assert!(
            acl.contains("user:65534:r--") || acl.contains("user:nobody:r--"),
            "POSIX ACL was not preserved, getfacl output:\n{acl}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn acl_failure_aborts_but_other_xattr_failures_are_best_effort() {
        use std::ffi::CString;

        let acl = CString::new("system.posix_acl_access").unwrap();
        let user = CString::new("user.aish_test").unwrap();

        let attrs = vec![(user, b"v".to_vec()), (acl, b"a".to_vec())];
        let err = apply_xattr_list(&attrs, |_, _| {
            Err(std::io::Error::other("simulated fsetxattr failure"))
        });
        assert!(
            err.is_err(),
            "failure to restore the access ACL must abort the write"
        );

        let attrs = vec![(CString::new("user.aish_test").unwrap(), b"v".to_vec())];
        assert!(
            apply_xattr_list(&attrs, |_, _| {
                Err(std::io::Error::other("simulated fsetxattr failure"))
            })
            .is_ok(),
            "non-ACL attributes must remain best-effort"
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn get_xattr_distinguishes_absence_from_value() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("xattr.conf");
        fs::write(&target, "x").unwrap();
        let cpath = CString::new(target.as_os_str().as_bytes()).unwrap();

        let missing = CString::new("user.aish_missing").unwrap();
        assert!(get_xattr(&cpath, &missing).unwrap().is_none());

        let name = CString::new("user.aish_present").unwrap();
        let value = b"hello";
        let rc = unsafe {
            libc::setxattr(
                cpath.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if rc != 0 {
            eprintln!("skipping: filesystem does not support user xattrs");
            return;
        }
        assert_eq!(
            get_xattr(&cpath, &name).unwrap().as_deref(),
            Some(&value[..])
        );
    }
    #[cfg(unix)]
    #[test]
    fn metadata_errors_other_than_not_found_are_propagated() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.conf");
        assert!(existing_metadata(&missing).unwrap().is_none());

        // `parent` is a regular file, so looking up `parent/child` fails
        // with ENOTDIR rather than NotFound.
        let parent = dir.path().join("parent");
        fs::write(&parent, "x").unwrap();
        assert!(existing_metadata(&parent.join("child")).is_err());
    }

    #[test]
    fn splice_returns_tag_of_complete_output() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        fs::write(&target, "alpha\nbeta\ngamma\n").unwrap();

        let tag = atomic_write_splice(&target, 6, b"BETA", 10).unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, b"alpha\nBETA\ngamma\n");
        assert_eq!(
            tag,
            SnapshotTag::from_bytes(&written),
            "the returned tag must describe the whole resulting file"
        );
    }

    #[test]
    fn conditional_write_replaces_when_snapshot_matches() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        fs::write(&target, "old").unwrap();

        let outcome = atomic_write_if_unchanged(&target, b"old", b"new").unwrap();
        assert_eq!(outcome, ConditionalWriteOutcome::Written);
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn conditional_write_preserves_external_change_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        fs::write(&target, "external").unwrap();

        // The caller's snapshot says "old", but the target already changed.
        let outcome = atomic_write_if_unchanged(&target, b"old", b"new").unwrap();
        assert_eq!(outcome, ConditionalWriteOutcome::Conflict);
        assert_eq!(fs::read(&target).unwrap(), b"external");
    }

    #[cfg(unix)]
    #[test]
    fn conditional_write_through_symlink_keeps_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink("real.txt", &link).unwrap();

        let outcome = atomic_write_if_unchanged(&link, b"old", b"new").unwrap();
        assert_eq!(outcome, ConditionalWriteOutcome::Written);
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }
}
