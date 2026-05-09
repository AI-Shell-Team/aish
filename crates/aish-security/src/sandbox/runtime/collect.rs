use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::runtime::overlay::{OverlayMountRecord, OverlayPlan};
use crate::sandbox::types::{FsChange, FsChangeKind, SandboxChangeDetail, SandboxLimits};

const SINGLE_REPO_SCAFFOLD_ROOTS: [&str; 7] = ["bin", "dev", "etc", "lib", "lib64", "proc", "usr"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CollectedChanges {
    pub(crate) changes: Vec<FsChange>,
    pub(crate) truncated: bool,
}

pub(crate) fn collect_changes(
    plan: &OverlayPlan,
    limits: SandboxLimits,
) -> Result<CollectedChanges, SandboxError> {
    let mut by_path = BTreeMap::new();
    for overlay in &plan.overlays {
        collect_overlay_changes(overlay, &mut by_path)?;
    }

    let mut changes: Vec<_> = by_path
        .into_values()
        .filter(|change| should_report_change(plan, change))
        .collect();
    let truncated = changes.len() > limits.changes_max;
    if truncated {
        changes.truncate(limits.changes_max);
    }

    Ok(CollectedChanges { changes, truncated })
}

fn collect_overlay_changes(
    overlay: &OverlayMountRecord,
    by_path: &mut BTreeMap<String, FsChange>,
) -> Result<(), SandboxError> {
    if !path_exists(&overlay.upperdir) {
        return Ok(());
    }

    let mut upper_files = BTreeSet::new();
    let mut opaque_dirs = Vec::new();
    walk_upper_dir(
        overlay,
        Path::new(""),
        by_path,
        &mut upper_files,
        &mut opaque_dirs,
    )?;

    for rel_dir in opaque_dirs {
        collect_opaque_dir_deletions(overlay, &rel_dir, &upper_files, by_path)?;
    }

    Ok(())
}

fn walk_upper_dir(
    overlay: &OverlayMountRecord,
    rel_dir: &Path,
    by_path: &mut BTreeMap<String, FsChange>,
    upper_files: &mut BTreeSet<PathBuf>,
    opaque_dirs: &mut Vec<PathBuf>,
) -> Result<(), SandboxError> {
    let dir_path = overlay.upperdir.join(rel_dir);
    if is_opaque_dir(&dir_path) {
        opaque_dirs.push(rel_dir.to_path_buf());
    }

    let entries = read_dir_sorted(&dir_path)?;
    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|error| collect_error("read_type", &entry.path(), error))?;
        let rel_path = if rel_dir.as_os_str().is_empty() {
            PathBuf::from(entry.file_name())
        } else {
            rel_dir.join(entry.file_name())
        };
        let upper_path = overlay.upperdir.join(&rel_path);

        if file_type.is_dir() {
            collect_dir_change(overlay, &rel_path, &upper_path, by_path)?;
            walk_upper_dir(overlay, &rel_path, by_path, upper_files, opaque_dirs)?;
            continue;
        }

        collect_entry_change(overlay, &rel_path, &upper_path, by_path, upper_files)?;
    }

    Ok(())
}

fn collect_dir_change(
    overlay: &OverlayMountRecord,
    rel_path: &Path,
    upper_path: &Path,
    by_path: &mut BTreeMap<String, FsChange>,
) -> Result<(), SandboxError> {
    let lower_path = overlay.lowerdir.join(rel_path);
    if path_exists(&lower_path) {
        if let Some(detail) = build_meta_detail(upper_path, &lower_path) {
            record_change(
                by_path,
                FsChange {
                    path: logical_path(overlay, rel_path),
                    kind: FsChangeKind::Modified,
                    detail: Some(detail),
                },
            );
        }
    } else {
        record_change(
            by_path,
            FsChange {
                path: logical_path(overlay, rel_path),
                kind: FsChangeKind::Created,
                detail: None,
            },
        );
    }

    Ok(())
}

fn collect_entry_change(
    overlay: &OverlayMountRecord,
    rel_path: &Path,
    upper_path: &Path,
    by_path: &mut BTreeMap<String, FsChange>,
    upper_files: &mut BTreeSet<PathBuf>,
) -> Result<(), SandboxError> {
    if let Some(target_rel) = whiteout_target_from_name(rel_path) {
        record_deleted_path(overlay, &target_rel, by_path)?;
        return Ok(());
    }

    if is_whiteout_inode(upper_path)? {
        record_deleted_path(overlay, rel_path, by_path)?;
        return Ok(());
    }

    upper_files.insert(rel_path.to_path_buf());
    let lower_path = overlay.lowerdir.join(rel_path);
    if path_exists(&lower_path) {
        record_change(
            by_path,
            FsChange {
                path: logical_path(overlay, rel_path),
                kind: FsChangeKind::Modified,
                detail: build_meta_detail(upper_path, &lower_path),
            },
        );
    } else {
        record_change(
            by_path,
            FsChange {
                path: logical_path(overlay, rel_path),
                kind: FsChangeKind::Created,
                detail: None,
            },
        );
    }

    Ok(())
}

fn collect_opaque_dir_deletions(
    overlay: &OverlayMountRecord,
    rel_dir: &Path,
    upper_files: &BTreeSet<PathBuf>,
    by_path: &mut BTreeMap<String, FsChange>,
) -> Result<(), SandboxError> {
    let lower_dir = overlay.lowerdir.join(rel_dir);
    if !path_exists(&lower_dir) {
        return Ok(());
    }

    for rel_path in walk_lower_files(&overlay.lowerdir, &lower_dir)? {
        if upper_files.contains(&rel_path) {
            continue;
        }
        record_change(
            by_path,
            FsChange {
                path: logical_path(overlay, &rel_path),
                kind: FsChangeKind::Deleted,
                detail: None,
            },
        );
    }

    Ok(())
}

fn record_deleted_path(
    overlay: &OverlayMountRecord,
    rel_path: &Path,
    by_path: &mut BTreeMap<String, FsChange>,
) -> Result<(), SandboxError> {
    record_change(
        by_path,
        FsChange {
            path: logical_path(overlay, rel_path),
            kind: FsChangeKind::Deleted,
            detail: None,
        },
    );

    let lower_path = overlay.lowerdir.join(rel_path);
    if lower_path.is_dir() {
        for child_rel in walk_lower_files(&overlay.lowerdir, &lower_path)? {
            record_change(
                by_path,
                FsChange {
                    path: logical_path(overlay, &child_rel),
                    kind: FsChangeKind::Deleted,
                    detail: None,
                },
            );
        }
    }

    Ok(())
}

fn walk_lower_files(root: &Path, start: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    let mut files = Vec::new();
    walk_lower_files_inner(root, start, &mut files)?;
    Ok(files)
}

fn walk_lower_files_inner(
    root: &Path,
    dir: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), SandboxError> {
    for entry in read_dir_sorted(dir)? {
        let file_type = entry
            .file_type()
            .map_err(|error| collect_error("read_type", &entry.path(), error))?;
        let path = entry.path();
        if file_type.is_dir() {
            walk_lower_files_inner(root, &path, files)?;
            continue;
        }

        let rel_path = path.strip_prefix(root).map_err(|_| {
            SandboxError::with_details(
                SandboxReason::SandboxException,
                format!("lower_path_outside_root: {}", path.display()),
            )
        })?;
        files.push(rel_path.to_path_buf());
    }
    Ok(())
}

fn read_dir_sorted(path: &Path) -> Result<Vec<fs::DirEntry>, SandboxError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| collect_error("read_dir", path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| collect_error("read_dir_entry", path, error))?;
    entries.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    Ok(entries)
}

fn build_meta_detail(upper_path: &Path, lower_path: &Path) -> Option<SandboxChangeDetail> {
    let upper = fs::symlink_metadata(upper_path).ok()?;
    let lower = fs::symlink_metadata(lower_path).ok()?;

    let mut detail = SandboxChangeDetail::new();
    let upper_mode = upper.mode() & 0o7777;
    let lower_mode = lower.mode() & 0o7777;
    if upper_mode != lower_mode {
        detail.insert(
            "mode".to_string(),
            format!("{:o}->{:o}", lower_mode, upper_mode),
        );
    }
    if upper.uid() != lower.uid() {
        detail.insert(
            "uid".to_string(),
            format!("{}->{}", lower.uid(), upper.uid()),
        );
    }
    if upper.gid() != lower.gid() {
        detail.insert(
            "gid".to_string(),
            format!("{}->{}", lower.gid(), upper.gid()),
        );
    }

    if detail.is_empty() {
        None
    } else {
        Some(detail)
    }
}

fn is_whiteout_inode(path: &Path) -> Result<bool, SandboxError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| collect_error("lstat", path, error))?;
    Ok(metadata.file_type().is_char_device() && metadata.rdev() == 0)
}

fn is_opaque_dir(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let name = CString::new("trusted.overlay.opaque").expect("static xattr name");
    let mut buf = [0_u8; 8];
    let read = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    read > 0 && buf[0] == b'y'
}

fn whiteout_target_from_name(rel_path: &Path) -> Option<PathBuf> {
    let file_name = rel_path.file_name()?.to_str()?;
    let target_name = file_name.strip_prefix(".wh.")?;
    let parent = rel_path.parent().unwrap_or_else(|| Path::new(""));
    Some(parent.join(target_name))
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn should_report_change(plan: &OverlayPlan, change: &FsChange) -> bool {
    if plan.strategy != crate::sandbox::runtime::overlay::OverlayStrategy::SingleRepo {
        return true;
    }

    if change.kind != FsChangeKind::Created {
        return true;
    }

    let scaffold_paths: Vec<_> = SINGLE_REPO_SCAFFOLD_ROOTS
        .iter()
        .map(|root| normalize_absolute_path(&plan.repo_root.join(root)))
        .collect();
    !scaffold_paths
        .iter()
        .any(|path| change.path == *path || change.path.starts_with(&format!("{path}/")))
}

fn logical_path(overlay: &OverlayMountRecord, rel_path: &Path) -> String {
    normalize_absolute_path(&overlay.lowerdir.join(rel_path))
}

fn normalize_absolute_path(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                let _ = parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => {}
        }
    }

    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn record_change(by_path: &mut BTreeMap<String, FsChange>, change: FsChange) {
    use std::collections::btree_map::Entry;

    match by_path.entry(change.path.clone()) {
        Entry::Vacant(entry) => {
            entry.insert(change);
        }
        Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            if change_precedence(change.kind) > change_precedence(existing.kind) {
                let merged_detail = merge_detail(existing.detail.take(), change.detail);
                *existing = FsChange {
                    path: change.path,
                    kind: change.kind,
                    detail: merged_detail,
                };
            } else if change.kind == existing.kind {
                existing.detail = merge_detail(existing.detail.take(), change.detail);
            } else if existing.detail.is_none() {
                existing.detail = change.detail;
            }
        }
    }
}

fn merge_detail(
    existing: Option<SandboxChangeDetail>,
    incoming: Option<SandboxChangeDetail>,
) -> Option<SandboxChangeDetail> {
    match (existing, incoming) {
        (None, None) => None,
        (Some(detail), None) | (None, Some(detail)) => Some(detail),
        (Some(mut existing), Some(incoming)) => {
            existing.extend(incoming);
            Some(existing)
        }
    }
}

fn change_precedence(kind: FsChangeKind) -> u8 {
    match kind {
        FsChangeKind::Deleted => 3,
        FsChangeKind::Modified => 2,
        FsChangeKind::Created => 1,
        FsChangeKind::Chmod | FsChangeKind::Chown | FsChangeKind::Unknown => 0,
    }
}

fn collect_error(action: &str, path: &Path, error: std::io::Error) -> SandboxError {
    SandboxError::with_details(
        SandboxReason::SandboxException,
        format!("{} {}: {}", action, path.display(), error),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{collect_changes, collect_opaque_dir_deletions, logical_path};
    use crate::sandbox::runtime::overlay::{OverlayMountRecord, OverlayPlan, OverlayStrategy};
    use crate::sandbox::types::{FsChangeKind, SandboxLimits};

    fn single_overlay_plan(lowerdir: &Path, upperdir: &Path) -> OverlayPlan {
        OverlayPlan {
            repo_root: lowerdir.to_path_buf(),
            cwd: lowerdir.to_path_buf(),
            sandbox_cwd: PathBuf::from("/"),
            temp_root: upperdir.parent().unwrap().to_path_buf(),
            merged_root: upperdir.parent().unwrap().join("merged"),
            strategy: OverlayStrategy::SingleRepo,
            mounts: Vec::new(),
            overlays: vec![OverlayMountRecord {
                lowerdir: lowerdir.to_path_buf(),
                upperdir: upperdir.to_path_buf(),
                workdir: upperdir.parent().unwrap().join("work"),
                target: upperdir.parent().unwrap().join("merged"),
            }],
        }
    }

    fn suffix_paths(changes: &[crate::sandbox::types::FsChange], root: &Path) -> Vec<String> {
        changes
            .iter()
            .map(|change| {
                Path::new(&change.path)
                    .strip_prefix(root)
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn collect_changes_classifies_created_modified_deleted_and_sorts() {
        let temp = tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(&lower).unwrap();
        fs::create_dir_all(&upper).unwrap();

        fs::write(lower.join("modified.txt"), "old").unwrap();
        fs::write(lower.join("deleted.txt"), "gone").unwrap();
        fs::write(upper.join("modified.txt"), "new").unwrap();
        fs::write(upper.join("created.txt"), "fresh").unwrap();
        fs::write(upper.join(".wh.deleted.txt"), "").unwrap();

        let collected = collect_changes(
            &single_overlay_plan(&lower, &upper),
            SandboxLimits::default(),
        )
        .unwrap();

        assert!(!collected.truncated);
        assert_eq!(
            suffix_paths(&collected.changes, &lower),
            vec!["created.txt", "deleted.txt", "modified.txt"]
        );
        assert_eq!(collected.changes[0].kind, FsChangeKind::Created);
        assert_eq!(collected.changes[1].kind, FsChangeKind::Deleted);
        assert_eq!(collected.changes[2].kind, FsChangeKind::Modified);
    }

    #[test]
    fn collect_changes_includes_metadata_detail_for_existing_entries() {
        let temp = tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(&lower).unwrap();
        fs::create_dir_all(&upper).unwrap();

        let lower_file = lower.join("script.sh");
        let upper_file = upper.join("script.sh");
        fs::write(&lower_file, "echo old\n").unwrap();
        fs::write(&upper_file, "echo new\n").unwrap();
        fs::set_permissions(&lower_file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&upper_file, fs::Permissions::from_mode(0o755)).unwrap();

        let collected = collect_changes(
            &single_overlay_plan(&lower, &upper),
            SandboxLimits::default(),
        )
        .unwrap();

        assert_eq!(collected.changes.len(), 1);
        assert_eq!(collected.changes[0].kind, FsChangeKind::Modified);
        assert_eq!(
            collected.changes[0].detail.as_ref().unwrap().get("mode"),
            Some(&"644->755".to_string())
        );
    }

    #[test]
    fn collect_changes_truncates_after_stable_sort() {
        let temp = tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(&lower).unwrap();
        fs::create_dir_all(&upper).unwrap();

        fs::write(upper.join("z.txt"), "z").unwrap();
        fs::write(upper.join("a.txt"), "a").unwrap();
        fs::write(upper.join("m.txt"), "m").unwrap();

        let mut limits = SandboxLimits::default();
        limits.changes_max = 2;
        let collected = collect_changes(&single_overlay_plan(&lower, &upper), limits).unwrap();

        assert!(collected.truncated);
        assert_eq!(
            suffix_paths(&collected.changes, &lower),
            vec!["a.txt", "m.txt"]
        );
    }

    #[test]
    fn collect_changes_ignores_single_repo_scaffold_dirs() {
        let temp = tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(&lower).unwrap();
        fs::create_dir_all(&upper).unwrap();

        for dir in ["bin", "dev", "etc", "lib", "lib64", "proc", "usr"] {
            fs::create_dir_all(upper.join(dir)).unwrap();
        }
        fs::write(upper.join("user.txt"), "payload").unwrap();

        let collected = collect_changes(
            &single_overlay_plan(&lower, &upper),
            SandboxLimits::default(),
        )
        .unwrap();

        assert_eq!(suffix_paths(&collected.changes, &lower), vec!["user.txt"]);
        assert_eq!(collected.changes[0].kind, FsChangeKind::Created);
    }

    #[test]
    fn collect_changes_ignores_single_repo_scaffold_descendants() {
        let temp = tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(upper.join("usr/share")).unwrap();
        fs::write(upper.join("usr/share/bootstrap.txt"), "bootstrap").unwrap();
        fs::write(upper.join("work.txt"), "payload").unwrap();

        let collected = collect_changes(
            &single_overlay_plan(&lower, &upper),
            SandboxLimits::default(),
        )
        .unwrap();

        assert_eq!(suffix_paths(&collected.changes, &lower), vec!["work.txt"]);
    }

    #[test]
    fn opaque_dir_deletions_skip_files_recreated_in_upper() {
        let temp = tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(lower.join("opaque/sub")).unwrap();
        fs::create_dir_all(upper.join("opaque/sub")).unwrap();
        fs::write(lower.join("opaque/keep.txt"), "keep").unwrap();
        fs::write(lower.join("opaque/drop.txt"), "drop").unwrap();
        fs::write(lower.join("opaque/sub/nested.txt"), "nested").unwrap();

        let overlay = OverlayMountRecord {
            lowerdir: lower.clone(),
            upperdir: upper.clone(),
            workdir: temp.path().join("work"),
            target: temp.path().join("merged"),
        };
        let mut by_path = BTreeMap::new();
        let upper_files = BTreeSet::from([PathBuf::from("opaque/keep.txt")]);

        collect_opaque_dir_deletions(&overlay, Path::new("opaque"), &upper_files, &mut by_path)
            .unwrap();

        let deleted: Vec<_> = by_path.into_values().collect();
        assert_eq!(
            suffix_paths(&deleted, &lower),
            vec!["opaque/drop.txt", "opaque/sub/nested.txt"]
        );
        assert_eq!(
            logical_path(&overlay, Path::new("opaque/drop.txt")),
            format!("{}/opaque/drop.txt", lower.display())
        );
    }
}
