use std::collections::BTreeSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::runtime::executor::SandboxRunGuard;
use crate::sandbox::runtime::mount::{MountExecutor, MountSpec};

const PSEUDO_FS_ROOTS: [&str; 3] = ["/proc", "/sys", "/dev"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayStrategy {
    SingleRepo,
    WholeRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverlayMountRecord {
    pub(crate) lowerdir: PathBuf,
    pub(crate) upperdir: PathBuf,
    pub(crate) workdir: PathBuf,
    pub(crate) target: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverlayPlan {
    pub(crate) repo_root: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) sandbox_cwd: PathBuf,
    pub(crate) temp_root: PathBuf,
    pub(crate) merged_root: PathBuf,
    pub(crate) strategy: OverlayStrategy,
    pub(crate) mounts: Vec<MountSpec>,
    pub(crate) overlays: Vec<OverlayMountRecord>,
}

pub(crate) struct OverlayPlanBuilder {
    repo_root: PathBuf,
    cwd: PathBuf,
    temp_root: PathBuf,
    host_submounts: Vec<PathBuf>,
    root_overlay_targets: Option<Vec<PathBuf>>,
}

impl OverlayPlanBuilder {
    pub(crate) fn new(
        repo_root: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        temp_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            cwd: cwd.into(),
            temp_root: temp_root.into(),
            host_submounts: Vec::new(),
            root_overlay_targets: None,
        }
    }

    pub(crate) fn with_host_submounts(mut self, host_submounts: Vec<PathBuf>) -> Self {
        self.host_submounts = host_submounts;
        self
    }

    pub(crate) fn with_root_overlay_targets(mut self, root_overlay_targets: Vec<PathBuf>) -> Self {
        self.root_overlay_targets = Some(root_overlay_targets);
        self
    }

    pub(crate) fn build(self) -> Result<OverlayPlan, SandboxError> {
        if !self.repo_root.is_absolute() || !self.cwd.is_absolute() {
            return Err(SandboxError::with_details(
                SandboxReason::CwdOutsideRepoRoot,
                "repo_root and cwd must be absolute",
            ));
        }

        let rel_cwd = self.cwd.strip_prefix(&self.repo_root).map_err(|_| {
            SandboxError::with_details(
                SandboxReason::CwdOutsideRepoRoot,
                format!(
                    "cwd={}, repo_root={}",
                    self.cwd.display(),
                    self.repo_root.display()
                ),
            )
        })?;
        let sandbox_cwd = PathBuf::from("/").join(rel_cwd);
        let merged_root = self.temp_root.join("merged");

        if self.repo_root == Path::new("/") {
            self.build_whole_root(merged_root, sandbox_cwd)
        } else {
            self.build_single_repo(merged_root, sandbox_cwd)
        }
    }

    fn build_single_repo(
        self,
        merged_root: PathBuf,
        sandbox_cwd: PathBuf,
    ) -> Result<OverlayPlan, SandboxError> {
        let mut mounts = Vec::new();
        let mut overlays = Vec::new();

        let main_overlay = OverlayMountRecord {
            lowerdir: self.repo_root.clone(),
            upperdir: self.temp_root.join("upper"),
            workdir: self.temp_root.join("work"),
            target: merged_root.clone(),
        };
        push_overlay(&mut mounts, &mut overlays, main_overlay);

        for submount in filter_host_submounts(&self.host_submounts, &self.repo_root, None) {
            let Ok(relative) = submount.strip_prefix(&self.repo_root) else {
                continue;
            };
            let relative = relative.to_path_buf();
            let encoded = encode_mount_path(&submount);
            let overlay = OverlayMountRecord {
                lowerdir: submount,
                upperdir: self.temp_root.join("upper_submounts").join(&encoded),
                workdir: self.temp_root.join("work_submounts").join(&encoded),
                target: merged_root.join(relative),
            };
            push_overlay(&mut mounts, &mut overlays, overlay);
        }

        Ok(OverlayPlan {
            repo_root: self.repo_root,
            cwd: self.cwd,
            sandbox_cwd,
            temp_root: self.temp_root,
            merged_root,
            strategy: OverlayStrategy::SingleRepo,
            mounts,
            overlays,
        })
    }

    fn build_whole_root(
        self,
        merged_root: PathBuf,
        sandbox_cwd: PathBuf,
    ) -> Result<OverlayPlan, SandboxError> {
        let root_targets = self
            .root_overlay_targets
            .unwrap_or_else(list_system_root_overlay_targets);
        let root_targets = filter_root_overlay_targets(root_targets);
        let mut top_level_targets = BTreeSet::new();
        let mut mounts = vec![MountSpec::bind("/", &merged_root, false)];
        let mut overlays = Vec::new();

        for lowerdir in root_targets {
            let Ok(relative) = lowerdir.strip_prefix("/") else {
                continue;
            };
            let relative = relative.to_path_buf();
            if relative.components().count() != 1 {
                continue;
            }
            top_level_targets.insert(lowerdir.clone());

            let encoded = encode_mount_path(&lowerdir);
            let overlay = OverlayMountRecord {
                lowerdir,
                upperdir: self.temp_root.join("upper_rootdirs").join(&encoded),
                workdir: self.temp_root.join("work_rootdirs").join(&encoded),
                target: merged_root.join(relative),
            };
            push_overlay(&mut mounts, &mut overlays, overlay);
        }

        for submount in filter_host_submounts(
            &self.host_submounts,
            &self.repo_root,
            Some(&top_level_targets),
        ) {
            let Ok(relative) = submount.strip_prefix("/") else {
                continue;
            };
            let relative = relative.to_path_buf();
            let encoded = encode_mount_path(&submount);
            let overlay = OverlayMountRecord {
                lowerdir: submount,
                upperdir: self.temp_root.join("upper_submounts").join(&encoded),
                workdir: self.temp_root.join("work_submounts").join(&encoded),
                target: merged_root.join(relative),
            };
            push_overlay(&mut mounts, &mut overlays, overlay);
        }

        mounts.push(MountSpec::remount_readonly(&merged_root));

        Ok(OverlayPlan {
            repo_root: self.repo_root,
            cwd: self.cwd,
            sandbox_cwd,
            temp_root: self.temp_root,
            merged_root,
            strategy: OverlayStrategy::WholeRoot,
            mounts,
            overlays,
        })
    }
}

pub(crate) fn setup_overlay_plan(
    plan: &OverlayPlan,
    mount_executor: &MountExecutor,
    run_guard: &mut SandboxRunGuard,
) -> Result<(), SandboxError> {
    fs::create_dir_all(&plan.temp_root)
        .map_err(|error| overlay_perm_error(&plan.temp_root, error))?;
    fs::create_dir_all(&plan.merged_root)
        .map_err(|error| overlay_perm_error(&plan.merged_root, error))?;
    run_guard.push_temp_dir(plan.temp_root.clone());

    for mount in &plan.mounts {
        prepare_mount_dirs(mount)?;
        mount_executor.apply(mount, run_guard.mounts_mut())?;
    }

    Ok(())
}

pub(crate) fn read_host_mount_points_under(repo_root: &Path) -> Vec<PathBuf> {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    parse_mountinfo_points_under(repo_root, &mountinfo)
}

pub(crate) fn parse_mountinfo_points_under(repo_root: &Path, mountinfo: &str) -> Vec<PathBuf> {
    let mut mount_points = BTreeSet::new();
    for line in mountinfo.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mountinfo_path(parts[4]));
        if mount_point == repo_root {
            continue;
        }
        if mount_point.strip_prefix(repo_root).is_ok() && !is_pseudo_fs_path(&mount_point) {
            mount_points.insert(mount_point);
        }
    }

    let mut mount_points: Vec<_> = mount_points.into_iter().collect();
    mount_points.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    mount_points
}

fn push_overlay(
    mounts: &mut Vec<MountSpec>,
    overlays: &mut Vec<OverlayMountRecord>,
    overlay: OverlayMountRecord,
) {
    mounts.push(MountSpec::overlay(
        overlay.lowerdir.clone(),
        overlay.upperdir.clone(),
        overlay.workdir.clone(),
        overlay.target.clone(),
    ));
    overlays.push(overlay);
}

fn prepare_mount_dirs(mount: &MountSpec) -> Result<(), SandboxError> {
    match mount {
        MountSpec::Overlay {
            lowerdir,
            upperdir,
            workdir,
            target,
            ..
        } => {
            fs::create_dir_all(upperdir).map_err(|error| overlay_perm_error(upperdir, error))?;
            fs::create_dir_all(workdir).map_err(|error| overlay_perm_error(workdir, error))?;
            fs::create_dir_all(target).map_err(|error| overlay_perm_error(target, error))?;
            sync_dir_metadata(lowerdir, upperdir)?;
            sync_dir_metadata(lowerdir, target)?;
        }
        MountSpec::Bind { target, .. } => {
            fs::create_dir_all(target).map_err(|error| overlay_perm_error(target, error))?;
        }
        MountSpec::RemountReadonly { .. } => {}
    }
    Ok(())
}

fn sync_dir_metadata(source: &Path, target: &Path) -> Result<(), SandboxError> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(overlay_perm_error(source, error)),
    };
    let permissions = fs::Permissions::from_mode(metadata.mode() & 0o7777);
    fs::set_permissions(target, permissions).map_err(|error| overlay_perm_error(target, error))?;

    if unsafe { libc::geteuid() } == 0 {
        let c_target = path_c_string(target)?;
        let rc = unsafe { libc::chown(c_target.as_ptr(), metadata.uid(), metadata.gid()) };
        if rc != 0 {
            return Err(overlay_perm_error(target, std::io::Error::last_os_error()));
        }
    }

    Ok(())
}

fn path_c_string(path: &Path) -> Result<std::ffi::CString, SandboxError> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        SandboxError::with_details(
            SandboxReason::OverlayPermFailed,
            format!("invalid_path: {}", path.display()),
        )
    })
}

fn list_system_root_overlay_targets() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir("/") else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if is_pseudo_fs_path(&path) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            targets.push(path);
        }
    }
    filter_root_overlay_targets(targets)
}

fn filter_root_overlay_targets(targets: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut targets: Vec<_> = targets
        .into_iter()
        .filter(|target| target.is_absolute())
        .filter(|target| target.parent() == Some(Path::new("/")))
        .filter(|target| !is_pseudo_fs_path(target))
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

fn filter_host_submounts(
    host_submounts: &[PathBuf],
    repo_root: &Path,
    skip_exact: Option<&BTreeSet<PathBuf>>,
) -> Vec<PathBuf> {
    let mut mount_points: Vec<_> = host_submounts
        .iter()
        .filter(|mount_point| mount_point.as_path() != repo_root)
        .filter(|mount_point| mount_point.strip_prefix(repo_root).is_ok())
        .filter(|mount_point| !is_pseudo_fs_path(mount_point))
        .filter(|mount_point| path_metadata_accessible(mount_point))
        .filter(|mount_point| {
            skip_exact
                .map(|skip| !skip.contains(mount_point.as_path()))
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    mount_points.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    mount_points.dedup();
    mount_points
}

fn path_metadata_accessible(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn encode_mount_path(path: &Path) -> String {
    let encoded = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(encode_mount_component(value.as_bytes())),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("__");
    if encoded.is_empty() {
        "root".to_string()
    } else {
        encoded
    }
}

fn encode_mount_component(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2 + 6);
    encoded.push('c');
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut encoded, "{:02x}", byte);
    }
    encoded
}

fn unescape_mountinfo_path(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        let mut octal = String::new();
        for _ in 0..3 {
            let Some(next) = chars.peek().copied() else {
                break;
            };
            if matches!(next, '0'..='7') {
                octal.push(next);
                let _ = chars.next();
            } else {
                break;
            }
        }

        if octal.len() == 3 {
            if let Ok(value) = u8::from_str_radix(&octal, 8) {
                out.push(value as char);
            }
        } else {
            out.push('\\');
            out.push_str(&octal);
        }
    }
    out
}

fn is_pseudo_fs_path(path: &Path) -> bool {
    PSEUDO_FS_ROOTS.iter().any(|root| {
        let root = Path::new(root);
        path == root || path.strip_prefix(root).is_ok()
    })
}

fn overlay_perm_error(path: &Path, error: std::io::Error) -> SandboxError {
    SandboxError::with_details(
        SandboxReason::OverlayPermFailed,
        format!("{}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::{
        encode_mount_path, parse_mountinfo_points_under, setup_overlay_plan, OverlayPlanBuilder,
        OverlayStrategy,
    };
    use crate::sandbox::error::SandboxReason;
    use crate::sandbox::runtime::executor::SandboxRunGuard;
    use crate::sandbox::runtime::mount::{MountCall, MountExecutor};

    #[test]
    fn mountinfo_parser_unescapes_filters_and_sorts_submounts() {
        let raw = "\
26 23 0:22 / / rw - ext4 /dev/root rw\n\
27 26 0:23 / /repo rw - ext4 /dev/sda rw\n\
28 27 0:24 / /repo/z rw - ext4 /dev/sdb rw\n\
29 27 0:25 / /repo/a\\040space rw - ext4 /dev/sdc rw\n\
30 26 0:26 / /proc rw - proc proc rw\n\
31 26 0:27 / /repo/a\\040space/deep rw - ext4 /dev/sdd rw\n";

        let parsed = parse_mountinfo_points_under(Path::new("/repo"), raw);

        assert_eq!(
            parsed,
            vec![
                PathBuf::from("/repo/a space"),
                PathBuf::from("/repo/z"),
                PathBuf::from("/repo/a space/deep"),
            ]
        );
    }

    #[test]
    fn whole_root_plan_skips_inaccessible_host_submounts() {
        let temp = tempdir().unwrap();
        let private_root = temp.path().join("private");
        let inaccessible = private_root.join("child");
        fs::create_dir_all(&inaccessible).unwrap();
        fs::set_permissions(&private_root, fs::Permissions::from_mode(0o000)).unwrap();

        let plan = OverlayPlanBuilder::new("/", "/tmp", temp.path().join("sandbox"))
            .with_root_overlay_targets(vec![PathBuf::from("/tmp")])
            .with_host_submounts(vec![PathBuf::from("/tmp"), inaccessible.clone()])
            .build()
            .unwrap();

        let targets: Vec<_> = plan
            .overlays
            .iter()
            .map(|overlay| overlay.target.clone())
            .collect();
        assert_eq!(targets, vec![temp.path().join("sandbox/merged/tmp")]);

        fs::set_permissions(&private_root, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn single_repo_plan_uses_main_overlay_and_submount_overlays() {
        let temp = PathBuf::from("/tmp/aish-plan");
        let plan = OverlayPlanBuilder::new("/repo", "/repo/src", &temp)
            .with_host_submounts(vec![
                PathBuf::from("/repo/mnt"),
                PathBuf::from("/repo/mnt/deep"),
                PathBuf::from("/other"),
            ])
            .build()
            .unwrap();

        assert_eq!(plan.strategy, OverlayStrategy::SingleRepo);
        assert_eq!(plan.sandbox_cwd, PathBuf::from("/src"));
        assert_eq!(plan.overlays.len(), 3);
        assert_eq!(plan.overlays[0].lowerdir, PathBuf::from("/repo"));
        assert_eq!(plan.overlays[1].target, temp.join("merged/mnt"));
        assert_eq!(plan.overlays[2].target, temp.join("merged/mnt/deep"));
    }

    #[test]
    fn whole_root_plan_binds_root_then_overlays_before_remounting_readonly() {
        let temp = PathBuf::from("/tmp/aish-root-plan");
        let plan = OverlayPlanBuilder::new("/", "/tmp", &temp)
            .with_root_overlay_targets(vec![
                PathBuf::from("/tmp"),
                PathBuf::from("/usr"),
                PathBuf::from("/proc"),
                PathBuf::from("/tmp"),
            ])
            .with_host_submounts(vec![
                PathBuf::from("/tmp"),
                PathBuf::from("/run/user"),
                PathBuf::from("/dev/pts"),
            ])
            .build()
            .unwrap();

        assert_eq!(plan.strategy, OverlayStrategy::WholeRoot);
        assert_eq!(plan.sandbox_cwd, PathBuf::from("/tmp"));
        assert_eq!(plan.mounts[0].target(), temp.join("merged"));
        assert_eq!(plan.mounts.last().unwrap().target(), temp.join("merged"));
        let targets: Vec<_> = plan
            .overlays
            .iter()
            .map(|overlay| overlay.target.clone())
            .collect();
        assert_eq!(
            targets,
            vec![
                temp.join("merged/tmp"),
                temp.join("merged/usr"),
                temp.join("merged/run/user"),
            ]
        );
    }

    #[test]
    fn encode_mount_path_is_collision_resistant_for_component_boundaries() {
        assert_ne!(
            encode_mount_path(Path::new("/repo/a_b")),
            encode_mount_path(Path::new("/repo/a/b"))
        );
    }

    #[test]
    fn plan_rejects_cwd_outside_repo_root() {
        let error = OverlayPlanBuilder::new("/repo", "/other", "/tmp/aish")
            .build()
            .unwrap_err();

        assert_eq!(error.reason(), SandboxReason::CwdOutsideRepoRoot);
    }

    #[test]
    fn setup_overlay_plan_creates_dirs_and_mounts_every_spec() {
        let temp = tempdir().unwrap();
        let sandbox_root = temp.path().join("sandbox-run");
        let plan = OverlayPlanBuilder::new("/repo", "/repo", &sandbox_root)
            .with_host_submounts(vec![PathBuf::from("/repo/cache")])
            .build()
            .unwrap();
        let calls = Arc::new(Mutex::new(Vec::<MountCall>::new()));
        let unmounts = Arc::new(Mutex::new(Vec::<String>::new()));
        let executor = MountExecutor::with_hooks(
            {
                let calls = calls.clone();
                move |call| {
                    calls.lock().unwrap().push(call.clone());
                    Ok(())
                }
            },
            {
                let unmounts = unmounts.clone();
                move |target, _flags| {
                    unmounts.lock().unwrap().push(target.display().to_string());
                    Ok(())
                }
            },
        );
        let mut guard = SandboxRunGuard::new();

        setup_overlay_plan(&plan, &executor, &mut guard).unwrap();

        assert!(sandbox_root.join("upper").is_dir());
        assert!(sandbox_root
            .join("upper_submounts")
            .join(encode_mount_path(Path::new("/repo/cache")))
            .is_dir());
        assert_eq!(calls.lock().unwrap().len(), plan.mounts.len());
        assert_eq!(guard.mounts_mut().len(), plan.mounts.len());
        guard.close().unwrap();
        assert!(!sandbox_root.exists());
        assert_eq!(unmounts.lock().unwrap().len(), plan.mounts.len());
    }

    #[test]
    fn setup_overlay_plan_preserves_lowerdir_mode_on_overlay_paths() {
        let temp = tempdir().unwrap();
        let sandbox_root = temp.path().join("sandbox-run");
        let plan = OverlayPlanBuilder::new("/", "/tmp", &sandbox_root)
            .with_root_overlay_targets(vec![PathBuf::from("/tmp")])
            .build()
            .unwrap();
        let executor = MountExecutor::with_hooks(|_call| Ok(()), |_target, _flags| Ok(()));
        let mut guard = SandboxRunGuard::new();

        setup_overlay_plan(&plan, &executor, &mut guard).unwrap();

        let host_mode = fs::symlink_metadata("/tmp").unwrap().permissions().mode() & 0o7777;
        let upper_mode = fs::symlink_metadata(
            sandbox_root
                .join("upper_rootdirs")
                .join(encode_mount_path(Path::new("/tmp"))),
        )
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        let target_mode = fs::symlink_metadata(sandbox_root.join("merged/tmp"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(upper_mode, host_mode);
        assert_eq!(target_mode, host_mode);
    }

    #[test]
    fn setup_overlay_plan_keeps_successful_mounts_guarded_after_later_failure() {
        let temp = tempdir().unwrap();
        let sandbox_root = temp.path().join("sandbox-run");
        let plan = OverlayPlanBuilder::new("/repo", "/repo", &sandbox_root)
            .with_host_submounts(vec![PathBuf::from("/repo/cache")])
            .build()
            .unwrap();
        let seen = Arc::new(Mutex::new(0_usize));
        let executor = MountExecutor::with_hooks(
            {
                let seen = seen.clone();
                move |_call| {
                    let mut seen = seen.lock().unwrap();
                    *seen += 1;
                    if *seen == 2 {
                        Err(io::Error::new(io::ErrorKind::PermissionDenied, "no mount"))
                    } else {
                        Ok(())
                    }
                }
            },
            |_target, _flags| Ok(()),
        );
        let mut guard = SandboxRunGuard::new();

        let error = setup_overlay_plan(&plan, &executor, &mut guard).unwrap_err();

        assert_eq!(error.reason(), SandboxReason::OverlayMountFailed);
        assert_eq!(guard.mounts_mut().len(), 1);
        guard.close().unwrap();
    }
}
