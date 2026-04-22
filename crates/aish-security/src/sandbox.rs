//! Bubblewrap-based sandbox executor for isolated command execution.
//!
//! This module provides the core sandbox execution logic using overlayfs
//! and bubblewrap (bwrap). It creates an isolated environment where commands
//! can run safely, and detects filesystem changes by scanning the overlay
//! upper directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use aish_core::{AishError, Result};
use tracing::{debug, info, warn};

use crate::overlay::{
    bind_mount, collect_fs_changes, list_system_root_overlay_targets,
    prepare_overlay_dirs_for_user, read_host_mount_points_under, remount_bind_readonly,
    safe_umount, sync_overlay_upper_root_metadata, FsChange as OverlayFsChange,
};
use crate::types::{FsChange, SandboxConfig, SandboxResult};

/// Sandbox executor using bubblewrap and overlayfs.
pub struct SandboxExecutor {
    config: SandboxConfig,
}

impl SandboxExecutor {
    /// Create a new SandboxExecutor with the given configuration.
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    /// Check whether bwrap is available on the system.
    pub fn is_available() -> bool {
        Command::new("which")
            .arg("bwrap")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Execute a command in the sandbox overlay.
    ///
    /// # Arguments
    ///
    /// * `command` - Command string to execute
    /// * `cwd` - Current working directory
    /// * `run_as_uid` - Optional user ID to run as
    /// * `run_as_gid` - Optional group ID to run as
    /// * `timeout_s` - Optional timeout in seconds
    pub fn simulate(
        &self,
        command: &str,
        cwd: &Path,
        run_as_uid: Option<u32>,
        run_as_gid: Option<u32>,
        timeout_s: Option<f64>,
    ) -> Result<SandboxResult> {
        self.run_in_overlay_sandbox(
            command,
            &self.config.repo_root,
            cwd,
            run_as_uid,
            run_as_gid,
            timeout_s,
        )
    }

    /// Execute a command in an overlayfs-based sandbox.
    ///
    /// This is the core sandbox execution logic. It creates an overlayfs
    /// environment, runs commands via bubblewrap, and detects filesystem
    /// changes by scanning the overlay upper directory.
    fn run_in_overlay_sandbox(
        &self,
        command: &str,
        repo_root: &Path,
        cwd: &Path,
        run_as_uid: Option<u32>,
        run_as_gid: Option<u32>,
        timeout_s: Option<f64>,
    ) -> Result<SandboxResult> {
        let repo_root = repo_root
            .canonicalize()
            .map_err(|e| AishError::Security(format!("Failed to resolve repo_root: {}", e)))?;
        let cwd = cwd
            .canonicalize()
            .map_err(|e| AishError::Security(format!("Failed to resolve cwd: {}", e)))?;

        // Validate that cwd is under repo_root
        if !cwd.starts_with(&repo_root) {
            return Err(AishError::Security(format!(
                "cwd_outside_repo_root: cwd={}, repo_root={}",
                cwd.display(),
                repo_root.display()
            )));
        }

        // Determine temp directory parent
        let tmp_parent = if repo_root == Path::new("/") {
            let shm = Path::new("/dev/shm");
            if shm.is_dir() {
                // Check if we have write access
                if let Ok(metadata) = fs::metadata(shm) {
                    // Try to check permissions
                    #[allow(clippy::unnecessary_literal_unwrap)]
                    let readonly = metadata.permissions().readonly();
                    if !readonly {
                        Some(shm.to_path_buf())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Create temporary directory for sandbox
        let tmpdir = if let Some(parent) = tmp_parent {
            tempfile::Builder::new()
                .prefix("aish-sandbox-")
                .tempdir_in(parent)
                .map_err(|e| AishError::Security(format!("Failed to create tempdir: {}", e)))?
        } else {
            tempfile::Builder::new()
                .prefix("aish-sandbox-")
                .tempdir()
                .map_err(|e| AishError::Security(format!("Failed to create tempdir: {}", e)))?
        };

        let workdir = tmpdir.path().join("work");
        let merged = tmpdir.path().join("merged");
        fs::create_dir_all(&workdir)
            .map_err(|e| AishError::Security(format!("Failed to create workdir: {}", e)))?;
        fs::create_dir_all(&merged)
            .map_err(|e| AishError::Security(format!("Failed to create merged dir: {}", e)))?;

        // Track overlays for cleanup: (lower, upper)
        let mut overlays: Vec<(PathBuf, PathBuf)> = Vec::new();

        // Setup overlay filesystem
        if repo_root == Path::new("/") {
            // Base root: readonly bind mount
            bind_mount(Path::new("/"), &merged)?;

            // Remount as readonly
            remount_bind_readonly(&merged)?;

            // Overlay each top-level directory
            let overlay_targets = list_system_root_overlay_targets();
            let upper_base = tmpdir.path().join("upper_rootdirs");
            let work_base = tmpdir.path().join("work_rootdirs");
            fs::create_dir_all(&upper_base)
                .map_err(|e| AishError::Security(format!("Failed to create upper_base: {}", e)))?;
            fs::create_dir_all(&work_base)
                .map_err(|e| AishError::Security(format!("Failed to create work_base: {}", e)))?;

            for lower in overlay_targets {
                let rel = lower.strip_prefix(Path::new("/")).unwrap_or(lower.as_ref());
                let target = merged.join(rel);

                // Encode path for directory name
                let encoded = lower
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .filter(|s| !s.is_empty() && *s != "/")
                    .collect::<Vec<_>>()
                    .join("_");
                let encoded = if encoded.is_empty() {
                    "root".to_string()
                } else {
                    encoded
                };

                let upperdir = upper_base.join(&encoded);
                let ovl_workdir = work_base.join(&encoded);
                fs::create_dir_all(&upperdir).map_err(|e| {
                    AishError::Security(format!("Failed to create upperdir: {}", e))
                })?;
                fs::create_dir_all(&ovl_workdir).map_err(|e| {
                    AishError::Security(format!("Failed to create ovl_workdir: {}", e))
                })?;

                // Prepare overlay directories
                if let (Some(uid), Some(gid)) = (run_as_uid, run_as_gid) {
                    prepare_overlay_dirs_for_user(&upperdir, &ovl_workdir, uid, gid)?;
                } else {
                    sync_overlay_upper_root_metadata(&lower, &upperdir)?;
                }

                // Create target directory if it doesn't exist
                fs::create_dir_all(&target)
                    .map_err(|e| AishError::Security(format!("Failed to create target: {}", e)))?;

                // Mount overlay
                mount_overlay(&lower, &upperdir, &ovl_workdir, &target)?;
                overlays.push((lower, upperdir));
            }
        } else {
            // Normal case: single overlay for repo_root
            let upperdir = tmpdir.path().join("upper");
            let upper_workdir = tmpdir.path().join("work");
            fs::create_dir_all(&upperdir)
                .map_err(|e| AishError::Security(format!("Failed to create upperdir: {}", e)))?;
            fs::create_dir_all(&upper_workdir).map_err(|e| {
                AishError::Security(format!("Failed to create upper_workdir: {}", e))
            })?;

            // Prepare overlay directories
            if let (Some(uid), Some(gid)) = (run_as_uid, run_as_gid) {
                prepare_overlay_dirs_for_user(&upperdir, &upper_workdir, uid, gid)?;
            }

            // Mount overlay
            mount_overlay(&repo_root, &upperdir, &upper_workdir, &merged)?;
            overlays.push((repo_root.clone(), upperdir));

            // Handle submounts from host
            let submounts = read_host_mount_points_under(&repo_root);
            for submount in submounts {
                // Skip submounts under tmpdir (they're part of the sandbox)
                if submount.starts_with(tmpdir.path()) {
                    continue;
                }

                // Create overlay for submount
                let rel = submount
                    .strip_prefix(&repo_root)
                    .unwrap_or(submount.as_ref());
                let target = merged.join(rel);

                // Encode path
                let encoded = submount
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .filter(|s| !s.is_empty() && *s != "/")
                    .collect::<Vec<_>>()
                    .join("_");
                let encoded = if encoded.is_empty() {
                    "root".to_string()
                } else {
                    encoded
                };

                let upperdir = tmpdir.path().join("upper").join(&encoded);
                let ovl_workdir = tmpdir.path().join("work").join(&encoded);
                fs::create_dir_all(&upperdir).map_err(|e| {
                    AishError::Security(format!("Failed to create submount upperdir: {}", e))
                })?;
                fs::create_dir_all(&ovl_workdir).map_err(|e| {
                    AishError::Security(format!("Failed to create submount workdir: {}", e))
                })?;

                // Prepare overlay directories
                if let (Some(uid), Some(gid)) = (run_as_uid, run_as_gid) {
                    prepare_overlay_dirs_for_user(&upperdir, &ovl_workdir, uid, gid)?;
                } else {
                    sync_overlay_upper_root_metadata(&submount, &upperdir)?;
                }

                // Create target directory if it doesn't exist
                fs::create_dir_all(&target).map_err(|e| {
                    AishError::Security(format!("Failed to create submount target: {}", e))
                })?;

                // Mount overlay
                mount_overlay(&submount, &upperdir, &ovl_workdir, &target)?;
                overlays.push((submount, upperdir));
            }
        }

        // Execute in sandbox and cleanup
        let result = self.execute_in_sandbox(
            &repo_root,
            tmpdir.path(),
            &workdir,
            &merged,
            &cwd,
            command,
            run_as_uid,
            run_as_gid,
            timeout_s,
            &overlays,
        );

        // Cleanup: unmount all overlays deep-to-shallow, then merged.
        {
            let mut overlay_targets_sorted: Vec<PathBuf> = overlays
                .iter()
                .map(|(lower, _)| {
                    if repo_root == Path::new("/") {
                        merged.join(lower.strip_prefix(Path::new("/")).unwrap_or(lower.as_ref()))
                    } else {
                        // For non-/ repo_root, the overlay mount is at merged itself
                        // plus any submounts inside merged
                        merged.join(lower.strip_prefix(&repo_root).unwrap_or(lower.as_ref()))
                    }
                })
                .collect();
            // Sort by depth (deep to shallow) for proper unmount order
            overlay_targets_sorted.sort_by(|a, b| {
                let a_depth: usize = a.components().count();
                let b_depth: usize = b.components().count();
                b_depth
                    .cmp(&a_depth)
                    .then_with(|| a.to_string_lossy().cmp(&b.to_string_lossy()))
            });

            for target in overlay_targets_sorted {
                if let Err(e) = safe_umount(&target) {
                    warn!("Failed to unmount {}: {}", target.display(), e);
                }
            }
        }

        // Unmount merged directory
        if let Err(e) = safe_umount(&merged) {
            warn!("Failed to unmount {}: {}", merged.display(), e);
        }

        // tmpdir will be cleaned up when it goes out of scope

        result
    }

    /// Execute command in bubblewrap sandbox.
    fn execute_in_sandbox(
        &self,
        lower_root: &Path,
        _upperdir: &Path,
        _workdir: &Path,
        merged: &Path,
        cwd: &Path,
        command: &str,
        run_as_uid: Option<u32>,
        run_as_gid: Option<u32>,
        timeout_s: Option<f64>,
        overlays: &[(PathBuf, PathBuf)],
    ) -> Result<SandboxResult> {
        // Calculate sandbox working directory
        let rel_cwd = cwd
            .strip_prefix(lower_root)
            .map_err(|e| AishError::Security(format!("Failed to calculate relative cwd: {}", e)))?;
        let sandbox_cwd = Path::new("/").join(rel_cwd);

        // Build bwrap command
        let mut bwrap_cmd = Command::new("bwrap");
        bwrap_cmd.arg("--bind").arg(merged).arg("/");
        bwrap_cmd.arg("--dev").arg("/dev");
        bwrap_cmd.arg("--proc").arg("/proc");

        // Add readonly binds
        let readonly_binds = self.config.readonly_binds.as_ref();
        if let Some(binds) = readonly_binds {
            for (host_path, sandbox_path) in binds {
                bwrap_cmd.arg("--ro-bind").arg(host_path).arg(sandbox_path);
            }
        } else if lower_root != Path::new("/") {
            // Default readonly binds for normal repo_root
            let default_binds: &[(&str, &str)] = &[
                ("/usr", "/usr"),
                ("/bin", "/bin"),
                ("/lib", "/lib"),
                ("/lib64", "/lib64"),
            ];
            for (host_path, sandbox_path) in default_binds {
                bwrap_cmd.arg("--ro-bind").arg(host_path).arg(sandbox_path);
            }
        }

        // Add readwrite binds
        if let Some(readwrite_binds) = &self.config.readwrite_binds {
            for (host_path, sandbox_path) in readwrite_binds {
                bwrap_cmd.arg("--bind").arg(host_path).arg(sandbox_path);
            }
        }

        // Set working directory
        bwrap_cmd.arg("--chdir").arg(&sandbox_cwd);

        // Build command to execute
        if run_as_uid.is_some() || run_as_gid.is_some() {
            if run_as_uid.is_none() || run_as_gid.is_none() {
                return Err(AishError::Security(
                    "run_as_uid/run_as_gid must both be set".to_string(),
                ));
            }

            // Use setpriv for uid/gid switching
            let uid_arg = format!("--reuid={}", run_as_uid.unwrap());
            let gid_arg = format!("--regid={}", run_as_gid.unwrap());
            bwrap_cmd.args([
                "setpriv",
                &uid_arg,
                &gid_arg,
                "--clear-groups",
                "--inh-caps=-all",
                "bash",
                "-lc",
                command,
            ]);
        } else {
            bwrap_cmd.args(["bash", "-lc", command]);
        }

        // Capture output
        bwrap_cmd.stdout(Stdio::piped());
        bwrap_cmd.stderr(Stdio::piped());

        info!("Executing in sandbox: {}", command);

        let mut child = bwrap_cmd
            .spawn()
            .map_err(|e| AishError::Security(format!("Failed to spawn bwrap: {}", e)))?;

        // Poll with timeout: use try_wait() to avoid blocking past the deadline.
        let timeout_dur = timeout_s.map(Duration::from_secs_f64);
        let start = std::time::Instant::now();
        let timed_out = loop {
            match child.try_wait() {
                Ok(Some(_status)) => break false,
                Ok(None) => {
                    if let Some(dur) = timeout_dur {
                        if start.elapsed() >= dur {
                            let _ = child.kill();
                            let _ = child.wait();
                            break true;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(AishError::Security(format!("Failed to poll bwrap: {}", e)));
                }
            }
        };

        if timed_out {
            return Ok(SandboxResult {
                exit_code: 137, // SIGKILL
                stdout: String::new(),
                stderr: format!("sandbox_timeout after {:.1}s", timeout_s.unwrap_or(0.0)),
                changes: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                changes_truncated: false,
            });
        }

        // Read stdout/stderr via wait_with_output (process has exited, so no deadlock risk)
        let output = child
            .wait_with_output()
            .map_err(|e| AishError::Security(format!("Failed to read bwrap output: {}", e)))?;

        let exit_code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Collect filesystem changes
        let mut fs_changes = Vec::new();
        for (lower, upper) in overlays {
            let overlay_changes = collect_fs_changes(&lower, &upper, lower_root);
            for change in overlay_changes {
                fs_changes.push(convert_overlay_fs_change(change)?);
            }
        }

        Ok(SandboxResult {
            exit_code,
            stdout,
            stderr,
            changes: fs_changes,
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        })
    }

    /// Fallback: execute without sandbox when bwrap is not available.
    pub fn execute_unsandboxed(command: &str) -> Result<SandboxResult> {
        info!("Executing without sandbox: {}", command);

        let output = Command::new("/bin/bash")
            .arg("-lc")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| AishError::Security(format!("Failed to execute command: {}", e)))?;

        let exit_code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok(SandboxResult {
            exit_code,
            stdout,
            stderr,
            changes: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        })
    }
}

/// Mount an overlay filesystem.
///
/// # Arguments
///
/// * `lower` - Lower (read-only) directory
/// * `upper` - Upper (write) directory
/// * `work` - Work directory
/// * `merged` - Mount point
fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    debug!(
        "Mounting overlay: lower={}, upper={}, work={}, merged={}",
        lower.display(),
        upper.display(),
        work.display(),
        merged.display()
    );

    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );

    let status = Command::new("mount")
        .arg("-t")
        .arg("overlay")
        .arg("overlay")
        .arg("-o")
        .arg(&options)
        .arg(merged)
        .status()
        .map_err(|e| AishError::Security(format!("Failed to execute mount: {}", e)))?;

    if !status.success() {
        return Err(AishError::Security(format!(
            "Mount command failed with status: {:?}",
            status
        )));
    }

    Ok(())
}

/// Convert overlay FsChange to types FsChange.
fn convert_overlay_fs_change(change: OverlayFsChange) -> Result<FsChange> {
    let (path, kind) = match change {
        OverlayFsChange::Created(p) => (p, "created".to_string()),
        OverlayFsChange::Modified(p) => (p, "modified".to_string()),
        OverlayFsChange::Deleted(p) => (p, "deleted".to_string()),
        OverlayFsChange::MetadataChanged(p) => (p, "metadata_changed".to_string()),
    };

    Ok(FsChange {
        path: path
            .to_str()
            .ok_or_else(|| AishError::Security("Invalid path in FsChange".to_string()))?
            .to_string(),
        kind,
        detail: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let config = SandboxConfig::default();
        assert_eq!(config.repo_root, PathBuf::from("."));
        assert!(config.enable_overlay);
        assert!(config.readonly_binds.is_none());
        assert!(config.readwrite_binds.is_none());
    }

    #[test]
    fn test_execute_unsandboxed() {
        let result = SandboxExecutor::execute_unsandboxed("echo hello").unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(result.changes.is_empty());
    }

    #[test]
    fn test_convert_overlay_fs_change() {
        let change = OverlayFsChange::Created(PathBuf::from("/test/file.txt"));
        let converted = convert_overlay_fs_change(change).unwrap();
        assert_eq!(converted.path, "/test/file.txt");
        assert_eq!(converted.kind, "created");
    }
}
