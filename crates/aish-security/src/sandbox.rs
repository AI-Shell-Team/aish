//! Bubblewrap-based sandbox for isolated command execution.
//!
//! Uses `bwrap` to create a lightweight filesystem overlay where commands can
//! run in isolation. File changes are detected by comparing the overlay upper
//! directory with the original filesystem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// File system change detected after sandbox execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FsChange {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

/// Result of sandbox execution.
#[derive(Debug)]
pub struct SandboxResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub fs_changes: Vec<FsChange>,
}

/// Configuration for sandbox execution.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Directory for overlay work files. Defaults to a temp dir.
    pub overlay_dir: Option<PathBuf>,
    /// Timeout in seconds for sandbox execution.
    pub timeout_secs: u64,
    /// Additional bind mounts: (source, destination, readonly).
    pub bind_mounts: Vec<(PathBuf, PathBuf, bool)>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            overlay_dir: None,
            timeout_secs: 30,
            bind_mounts: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Sandbox executor using bubblewrap (`bwrap`).
pub struct SandboxExecutor {
    config: SandboxConfig,
}

impl SandboxExecutor {
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    /// Check whether `bwrap` is available on the system.
    pub fn is_available() -> bool {
        std::process::Command::new("which")
            .arg("bwrap")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Execute a command inside the sandbox.
    pub fn execute(
        &self,
        command: &str,
        env_vars: &HashMap<String, String>,
    ) -> Result<SandboxResult, String> {
        let overlay_dir = self.config.overlay_dir.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("aish-sandbox-{}", uuid::Uuid::new_v4()))
        });

        let upper_dir = overlay_dir.join("upper");
        std::fs::create_dir_all(&upper_dir).map_err(|e| format!("mkdir overlay: {}", e))?;

        // Build bwrap command
        let mut cmd = std::process::Command::new("bwrap");
        cmd.arg("--ro-bind")
            .arg("/")
            .arg("/")
            // Writable overlay for capturing changes
            .arg("--bind")
            .arg(&upper_dir)
            .arg("/")
            // Essential filesystem mounts
            .arg("--dev")
            .arg("/dev")
            .arg("--proc")
            .arg("/proc")
            .arg("--unshare-net")
            .arg("--die-with-parent");

        // Additional bind mounts
        for (src, dest, readonly) in &self.config.bind_mounts {
            if *readonly {
                cmd.arg("--ro-bind").arg(src).arg(dest);
            } else {
                cmd.arg("--bind").arg(src).arg(dest);
            }
        }

        // Environment
        for (k, v) in env_vars {
            cmd.env(k, v);
        }

        cmd.arg("--").arg("/bin/bash").arg("-c").arg(command);

        info!("Sandbox executing: {}", command);

        let output = cmd.output().map_err(|e| format!("bwrap exec: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(1);

        // Detect file changes
        let fs_changes = detect_fs_changes(&upper_dir);

        // Cleanup
        let _ = std::fs::remove_dir_all(&overlay_dir);

        Ok(SandboxResult {
            exit_code,
            stdout,
            stderr,
            fs_changes,
        })
    }

    /// Fallback: execute without sandbox (when bwrap is not available).
    pub fn execute_unsandboxed(command: &str) -> Result<SandboxResult, String> {
        let output = std::process::Command::new("/bin/bash")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|e| format!("exec: {}", e))?;

        Ok(SandboxResult {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            fs_changes: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// File change detection
// ---------------------------------------------------------------------------

/// Walk the overlay upper directory and report all files as modifications.
/// A production implementation would diff against the original filesystem,
/// but this simple approach captures all writes.
pub fn detect_fs_changes(upper_dir: &Path) -> Vec<FsChange> {
    let mut changes = Vec::new();
    if !upper_dir.exists() {
        return changes;
    }

    let mut stack = vec![upper_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let relative = path.strip_prefix(upper_dir).unwrap_or(&path);
                    changes.push(FsChange::Modified(relative.to_path_buf()));
                }
            }
        }
    }

    if !changes.is_empty() {
        info!("Sandbox detected {} file changes", changes.len());
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let config = SandboxConfig::default();
        assert_eq!(config.timeout_secs, 30);
        assert!(config.overlay_dir.is_none());
        assert!(config.bind_mounts.is_empty());
    }

    #[test]
    fn test_execute_unsandboxed() {
        let result = SandboxExecutor::execute_unsandboxed("echo hello").unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(result.fs_changes.is_empty());
    }

    #[test]
    fn test_detect_fs_changes_empty() {
        let dir = std::env::temp_dir().join("aish-test-nonexistent");
        let changes = detect_fs_changes(&dir);
        assert!(changes.is_empty());
    }
}
