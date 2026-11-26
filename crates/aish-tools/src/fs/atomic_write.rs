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

/// Fingerprint of a complete source file used for internal consistency
/// checks. The 16-bit [`SnapshotTag`] is for display and receipts; this wider
/// digest prevents a tag collision from hiding a concurrent edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub hash: u64,
    pub len: u64,
}

impl SourceFingerprint {
    /// Compute the fingerprint of a complete in-memory file.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = StreamingTagHasher::new();
        hasher.update(bytes);
        Self {
            hash: hasher.finalize_u64(),
            len: bytes.len() as u64,
        }
    }
}

/// Result of a successful splice.
#[derive(Debug)]
pub struct SpliceOutput {
    /// Tag of the complete new file.
    pub new_tag: SnapshotTag,
    /// Complete source file as observed while building the replacement, when
    /// the caller asked to capture it for rollback.
    pub prior: Option<Vec<u8>>,
}

/// Errors from [`atomic_write_splice`].
#[derive(Debug)]
pub enum SpliceError {
    /// The target changed between the caller's scan and the splice read.
    SourceChanged,
    /// Underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for SpliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpliceError::SourceChanged => f.write_str("source changed during edit"),
            SpliceError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SpliceError {}

impl From<std::io::Error> for SpliceError {
    fn from(e: std::io::Error) -> Self {
        SpliceError::Io(e)
    }
}

/// Atomically replace `target` with: original bytes `[0, before)` +
/// `middle` + original bytes `[after, len)`. Streamed variant of
/// [`atomic_write`] for line-range edits on very large files. The untouched
/// head and tail are copied straight from the source file, so only
/// `middle` is ever materialized.
///
/// While copying, the same source handle feeds:
/// - the complete output tag,
/// - a 64-bit source fingerprint checked before the rename,
/// - and, when `prior_limit` is set, the complete prior content for undo.
///
/// `original_window` is the source bytes in `[before, after)` that the caller
/// matched and replaced with `middle`. The live bytes in that range are read
/// from the same handle and compared with `original_window`, so a writer that
/// changes the edited line after the caller's window read is detected before
/// the replacement is published.
///
/// Concurrency guarantee: a change observed through the final pre-rename
/// identity/timestamp check aborts the edit. A save that lands after that
/// check but before the rename cannot be detected without cross-process
/// locking or a content CAS, and is last-writer-wins. Undo only contains the
/// snapshot read by this call.
pub fn atomic_write_splice(
    target: &Path,
    before: u64,
    original_window: &[u8],
    middle: &[u8],
    after: u64,
    expected_source: Option<SourceFingerprint>,
    prior_limit: Option<usize>,
) -> Result<SpliceOutput, SpliceError> {
    atomic_write_splice_with_hook(
        target,
        before,
        original_window,
        middle,
        after,
        expected_source,
        prior_limit,
        |_| {},
    )
}

/// Internal form with a pre-rename hook. Production passes a no-op closure;
/// tests use it to inject a concurrent write after the source read.
fn atomic_write_splice_with_hook<F: FnOnce(&Path)>(
    target: &Path,
    before: u64,
    original_window: &[u8],
    middle: &[u8],
    after: u64,
    expected_source: Option<SourceFingerprint>,
    prior_limit: Option<usize>,
    before_rename: F,
) -> Result<SpliceOutput, SpliceError> {
    if after < before || after - before != original_window.len() as u64 {
        return Err(SpliceError::SourceChanged);
    }

    let resolved = resolve_write_target(target)?;
    let target = resolved.as_path();
    let Some(tmp) = temp_path(target) else {
        return Err(SpliceError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cannot create a sibling temp file for this target",
        )));
    };
    let prior_meta = existing_metadata(target)?;

    let write_result = (|| -> Result<SpliceOutput, SpliceError> {
        let mut src = std::fs::File::open(target)?;
        let initial_meta = src.metadata()?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(meta) = &prior_meta {
            apply_metadata(&mut file, meta)?;
        }

        let mut new_hasher = StreamingTagHasher::new();
        let mut source_hasher = StreamingTagHasher::new();
        let mut prior = prior_limit.map(|_| Vec::new());

        copy_exactly(
            &mut src,
            &mut file,
            before,
            &mut new_hasher,
            &mut source_hasher,
            &mut prior,
            prior_limit,
        )?;
        read_live_window(
            &mut src,
            original_window,
            &mut source_hasher,
            &mut prior,
            prior_limit,
        )?;
        file.write_all(middle)?;
        new_hasher.update(middle);
        let tail_len = copy_remaining(
            &mut src,
            &mut file,
            &mut new_hasher,
            &mut source_hasher,
            &mut prior,
            prior_limit,
        )?;

        if let Some(expected) = expected_source {
            let source_len = before + original_window.len() as u64 + tail_len;
            if source_len != expected.len || source_hasher.finalize_u64() != expected.hash {
                return Err(SpliceError::SourceChanged);
            }
        }

        // Last chance to catch a concurrent save between the source read and
        // the rename. This is a best-effort guard: a writer that saves after
        // this check but before the rename cannot be detected without
        // cross-process CAS/locking, and is last-writer-wins.
        before_rename(target);
        if !target_matches_snapshot(&src, &initial_meta, target) {
            return Err(SpliceError::SourceChanged);
        }

        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, target)?;
        sync_parent_dir(target);
        Ok(SpliceOutput {
            new_tag: new_hasher.finalize(),
            prior,
        })
    })();

    match write_result {
        Ok(out) => Ok(out),
        Err(err) => {
            // Never leave the temp file behind; the target is untouched when
            // the source check or a write step failed.
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Append source bytes to the undo snapshot, aborting if the file grew beyond
/// the budget the caller reserved.
fn append_prior(
    prior: &mut Option<Vec<u8>>,
    limit: Option<usize>,
    bytes: &[u8],
) -> Result<(), SpliceError> {
    if let (Some(prior), Some(limit)) = (prior.as_mut(), limit) {
        if prior.len().saturating_add(bytes.len()) > limit {
            return Err(SpliceError::SourceChanged);
        }
        prior.extend_from_slice(bytes);
    }
    Ok(())
}

/// Copy exactly `count` bytes from `src` to `dst` in bounded chunks so the
/// untouched regions of a huge file never sit in memory.
fn copy_exactly(
    src: &mut std::fs::File,
    dst: &mut std::fs::File,
    count: u64,
    new_hasher: &mut StreamingTagHasher,
    source_hasher: &mut StreamingTagHasher,
    prior: &mut Option<Vec<u8>>,
    prior_limit: Option<usize>,
) -> Result<(), SpliceError> {
    use std::io::Read;
    let mut remaining = count;
    let mut chunk = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = std::cmp::min(remaining, chunk.len() as u64) as usize;
        src.read_exact(&mut chunk[..want])?;
        dst.write_all(&chunk[..want])?;
        new_hasher.update(&chunk[..want]);
        source_hasher.update(&chunk[..want]);
        append_prior(prior, prior_limit, &chunk[..want])?;
        remaining -= want as u64;
    }
    Ok(())
}

/// Read the live replacement window from the source handle and compare it with
/// the bytes the caller originally matched. This closes the window between the
/// caller's `read_window` and this splice.
fn read_live_window(
    src: &mut std::fs::File,
    original_window: &[u8],
    source_hasher: &mut StreamingTagHasher,
    prior: &mut Option<Vec<u8>>,
    prior_limit: Option<usize>,
) -> Result<(), SpliceError> {
    use std::io::Read;
    let mut chunk = [0u8; 64 * 1024];
    let mut offset = 0usize;
    while offset < original_window.len() {
        let want = std::cmp::min(original_window.len() - offset, chunk.len());
        src.read_exact(&mut chunk[..want])?;
        let bytes = &chunk[..want];
        if bytes != &original_window[offset..offset + want] {
            return Err(SpliceError::SourceChanged);
        }
        source_hasher.update(bytes);
        append_prior(prior, prior_limit, bytes)?;
        offset += want;
    }
    Ok(())
}

/// Copy `src` to EOF in bounded chunks, feeding the output hash, source hash
/// and optional prior capture. Returns the number of bytes copied.
fn copy_remaining(
    src: &mut std::fs::File,
    dst: &mut std::fs::File,
    new_hasher: &mut StreamingTagHasher,
    source_hasher: &mut StreamingTagHasher,
    prior: &mut Option<Vec<u8>>,
    prior_limit: Option<usize>,
) -> Result<u64, SpliceError> {
    use std::io::Read;
    let mut chunk = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut chunk)?;
        if n == 0 {
            return Ok(total);
        }
        dst.write_all(&chunk[..n])?;
        new_hasher.update(&chunk[..n]);
        source_hasher.update(&chunk[..n]);
        append_prior(prior, prior_limit, &chunk[..n])?;
        total += n as u64;
    }
}

/// Whether the target still refers to the same inode and timestamp snapshot
/// captured when the splice source was opened.
fn target_matches_snapshot(
    src: &std::fs::File,
    initial: &std::fs::Metadata,
    target: &Path,
) -> bool {
    let Ok(current) = src.metadata() else {
        return false;
    };
    let Ok(target_meta) = std::fs::metadata(target) else {
        return false;
    };
    metadata_unchanged(initial, &current) && same_file(initial, &target_meta)
}

#[cfg(unix)]
fn metadata_unchanged(initial: &std::fs::Metadata, current: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    initial.len() == current.len()
        && initial.mtime() == current.mtime()
        && initial.mtime_nsec() == current.mtime_nsec()
        && initial.ctime() == current.ctime()
        && initial.ctime_nsec() == current.ctime_nsec()
}

#[cfg(not(unix))]
fn metadata_unchanged(initial: &std::fs::Metadata, current: &std::fs::Metadata) -> bool {
    initial.len() == current.len() && initial.modified().ok() == current.modified().ok()
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    true
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
        atomic_write_splice(&target, 3, b"b\r", b"B\r\nX", 5, None, None).unwrap();
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
        atomic_write_splice(&link, 2, b"b", b"B", 3, None, None).unwrap();

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
    fn splice_returns_complete_tags_and_prior() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        let prior = "alpha\nbeta\ngamma\n";
        fs::write(&target, prior).unwrap();

        let out = atomic_write_splice(
            &target,
            6,
            b"beta",
            b"BETA",
            10,
            Some(SourceFingerprint::from_bytes(prior.as_bytes())),
            Some(prior.len()),
        )
        .unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, b"alpha\nBETA\ngamma\n");
        assert_eq!(
            out.new_tag,
            SnapshotTag::from_bytes(&written),
            "the returned tag must describe the whole resulting file"
        );
        assert_eq!(
            out.prior.as_deref(),
            Some(prior.as_bytes()),
            "the captured prior must be the complete source snapshot"
        );
    }

    #[test]
    fn splice_rejects_changed_source_before_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        let original = "alpha\nbeta\ngamma\n";
        fs::write(&target, original).unwrap();

        // Caller scanned a different version of the file.
        let err = atomic_write_splice(
            &target,
            6,
            b"beta",
            b"BETA",
            10,
            Some(SourceFingerprint::from_bytes(b"alpha\nDRIFTED\ngamma\n")),
            Some(original.len()),
        )
        .unwrap_err();

        assert!(matches!(err, SpliceError::SourceChanged));
        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[test]
    fn splice_rejects_changed_live_window() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        // The caller scanned "a\nb\nc\n" and passes "b" as the matched
        // window, but another writer has already changed that line to "x".
        let expected_source = SourceFingerprint::from_bytes(b"a\nb\nc\n");
        fs::write(&target, "a\nx\nc\n").unwrap();

        let err = atomic_write_splice(&target, 2, b"b", b"B", 3, Some(expected_source), Some(16))
            .unwrap_err();

        assert!(matches!(err, SpliceError::SourceChanged));
        assert_eq!(fs::read_to_string(&target).unwrap(), "a\nx\nc\n");
    }

    #[test]
    fn splice_enforces_prior_capture_limit() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        let content = "a\nb\nc\n";
        fs::write(&target, content).unwrap();

        // The source fits the expected fingerprint, but the caller reserved
        // too little room for the undo snapshot. Abort instead of growing the
        // capture beyond the budget.
        let err = atomic_write_splice(
            &target,
            2,
            b"b",
            b"B",
            3,
            Some(SourceFingerprint::from_bytes(content.as_bytes())),
            Some(4),
        )
        .unwrap_err();

        assert!(matches!(err, SpliceError::SourceChanged));
        assert_eq!(fs::read_to_string(&target).unwrap(), content);
    }

    #[test]
    fn splice_detects_write_after_source_read() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        let original = "a\nb\nc\n";
        fs::write(&target, original).unwrap();

        // Inject a concurrent save after the source bytes were read but
        // before the rename. The final identity/timestamp guard must reject
        // the edit and leave the external content intact.
        let err = atomic_write_splice_with_hook(
            &target,
            2,
            b"b",
            b"B",
            3,
            Some(SourceFingerprint::from_bytes(original.as_bytes())),
            Some(16),
            |path| {
                fs::write(path, "a\nb\nGAMMA\n").unwrap();
            },
        )
        .unwrap_err();

        assert!(matches!(err, SpliceError::SourceChanged));
        assert_eq!(fs::read_to_string(&target).unwrap(), "a\nb\nGAMMA\n");
    }
}
