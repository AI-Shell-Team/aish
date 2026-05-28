//! Uninstall aish binary, supporting multiple installation methods.
//!
//! Detects how aish was installed (archive, cargo, pip, system package)
//! and runs the appropriate uninstall procedure. With `--purge`, also
//! removes user config/data/cache and system-level config files.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use aish_core::AishError;
use aish_i18n::{t, t_with_args};

use crate::install_source::{
    build_pip_command, detect_installation_method, detect_pip_context, InstallMethod,
    AISH_BINARY_NAME, ARCHIVE_BIN_DIR, CARGO_PACKAGE_NAME, PIP_DISTRIBUTION_NAME,
    SYSTEM_PACKAGE_NAME,
};

// Paths used by the archive/script installer (install.sh)
const ARCHIVE_BINARY_NAMES: &[&str] = &[AISH_BINARY_NAME, "aish-uninstall"];
const ARCHIVE_SHARE_DIR: &str = "/usr/local/share/aish";
const SYSTEM_CONFIG_DIR: &str = "/etc/aish";
const SYSTEMD_UNIT_DIR: &str = "/etc/systemd/system";

fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Uninstall by method
// ---------------------------------------------------------------------------

fn run_sudo(args: &[&str]) -> Result<(), AishError> {
    let output = std::process::Command::new("sudo")
        .args(args)
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run sudo: {e}")))?;
    if !output.status.success() {
        return Err(AishError::Config(format!(
            "Command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn uninstall_archive(purge: bool) -> Result<(), AishError> {
    // Prefer bundled uninstall script
    let uninstall_script = PathBuf::from(ARCHIVE_BIN_DIR).join("aish-uninstall");
    if uninstall_script.exists() {
        let mut args = vec![uninstall_script.to_str().unwrap_or("")];
        if purge {
            args.push("--purge-config");
        }
        return run_sudo(&args);
    }

    // Fallback: remove files manually
    let mut success = true;

    stop_and_remove_sandbox_units();

    for name in ARCHIVE_BINARY_NAMES {
        let binary = PathBuf::from(ARCHIVE_BIN_DIR).join(name);
        if binary.exists() {
            if let Err(e) = run_sudo(&["rm", "-f", binary.to_str().unwrap_or("")]) {
                let path_str = binary.display().to_string();
                eprintln!("\x1b[31m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("path".to_string(), path_str);
                    args.insert("error".to_string(), e.to_string());
                    t_with_args("cli.uninstall.file_remove_failed", &args)
                });
                success = false;
            }
        }
    }

    let share_dir = PathBuf::from(ARCHIVE_SHARE_DIR);
    if share_dir.exists() {
        if let Err(e) = run_sudo(&["rm", "-rf", share_dir.to_str().unwrap_or("")]) {
            let path_str = share_dir.display().to_string();
            eprintln!("\x1b[31m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("path".to_string(), path_str);
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.uninstall.file_remove_failed", &args)
            });
            success = false;
        }
    }

    if purge {
        purge_system_config();
    }

    if success {
        Ok(())
    } else {
        Err(AishError::Config(t("cli.uninstall.some_files_failed")))
    }
}

fn stop_and_remove_sandbox_units() {
    if which_exists("systemctl") && Path::new("/run/systemd/system").exists() {
        let _ = run_sudo(&["systemctl", "disable", "--now", "aish-sandbox.socket"]);
        let _ = run_sudo(&["systemctl", "stop", "--no-block", "aish-sandbox.service"]);
        let _ = run_sudo(&["systemctl", "reset-failed", "aish-sandbox.service"]);
    }

    let service = Path::new(SYSTEMD_UNIT_DIR).join("aish-sandbox.service");
    let socket = Path::new(SYSTEMD_UNIT_DIR).join("aish-sandbox.socket");
    let _ = run_sudo(&["rm", "-f", service.to_str().unwrap_or("")]);
    let _ = run_sudo(&["rm", "-f", socket.to_str().unwrap_or("")]);

    if which_exists("systemctl") && Path::new("/run/systemd/system").exists() {
        let _ = run_sudo(&["systemctl", "daemon-reload"]);
    }
}

fn uninstall_cargo() -> Result<(), AishError> {
    let output = std::process::Command::new("cargo")
        .args(["uninstall", CARGO_PACKAGE_NAME])
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run cargo: {e}")))?;
    if !output.status.success() {
        return Err(AishError::Config(format!(
            "cargo uninstall failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn uninstall_pip() -> Result<(), AishError> {
    let pip_context = detect_pip_context();
    let mut command = build_pip_command(pip_context.as_ref());
    command.args(["uninstall", "-y", PIP_DISTRIBUTION_NAME]);

    let output = command
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run pip: {e}")))?;
    if output.status.success() {
        return Ok(());
    }

    // Retry with --break-system-packages for externally-managed environments
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("externally-managed-environment") {
        let mut retry_command = build_pip_command(pip_context.as_ref());
        retry_command.args([
            "uninstall",
            "-y",
            "--break-system-packages",
            PIP_DISTRIBUTION_NAME,
        ]);
        let retry = retry_command
            .output()
            .map_err(|e| AishError::Config(format!("Failed to run pip: {e}")))?;
        if retry.status.success() {
            return Ok(());
        }
        return Err(AishError::Config(format!(
            "pip uninstall failed: {}",
            String::from_utf8_lossy(&retry.stderr).trim()
        )));
    }

    Err(AishError::Config(format!(
        "pip uninstall failed: {}",
        stderr.trim()
    )))
}

fn uninstall_system(purge: bool) -> Result<(), AishError> {
    if which_exists("dpkg") {
        let output = std::process::Command::new("sudo")
            .args(["apt-get", "remove", "-y", SYSTEM_PACKAGE_NAME])
            .output()
            .map_err(|e| AishError::Config(format!("Failed to run apt-get: {e}")))?;
        if output.status.success() {
            if purge {
                purge_system_config();
            }
            return Ok(());
        }
    }

    if which_exists("dnf") {
        let output = std::process::Command::new("sudo")
            .args(["dnf", "remove", "-y", SYSTEM_PACKAGE_NAME])
            .output()
            .map_err(|e| AishError::Config(format!("Failed to run dnf: {e}")))?;
        if output.status.success() {
            if purge {
                purge_system_config();
            }
            return Ok(());
        }
    }

    Err(AishError::Config(
        "Could not uninstall via system package manager".into(),
    ))
}

fn purge_system_config() {
    let etc_aish = Path::new(SYSTEM_CONFIG_DIR);
    if etc_aish.exists() {
        let _ = run_sudo(&["rm", "-rf", etc_aish.to_str().unwrap_or("")]);
    }
}

// ---------------------------------------------------------------------------
// User data directories (XDG)
// ---------------------------------------------------------------------------

fn get_data_directories() -> Vec<(String, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(d) = dirs::config_dir() {
        dirs.push(("config".into(), d.join("aish")));
    }
    if let Some(d) = dirs::data_dir() {
        dirs.push(("data".into(), d.join("aish")));
    }
    if let Some(d) = dirs::cache_dir() {
        dirs.push(("cache".into(), d.join("aish")));
    }
    dirs
}

/// Validate a path is safe to recursively delete.
///
/// Rejects system-critical directories, requires the leaf to be named
/// "aish", and requires at least 2 path components.
fn is_safe_purge_path(path: &Path) -> bool {
    let critical_prefixes: &[&str] = &[
        "/etc", "/usr", "/boot", "/dev", "/proc", "/sys", "/lib", "/lib64", "/bin", "/sbin", "/opt",
    ];

    let lexical = match path.strip_prefix("~") {
        Ok(rest) => dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| path.to_path_buf()),
        Err(_) => path.to_path_buf(),
    };

    if !lexical.is_absolute() {
        return false;
    }

    // Must end in "aish"
    if lexical.file_name().map(|n| n != "aish").unwrap_or(true) {
        return false;
    }

    // Must not be root or a system-critical directory
    if lexical == std::path::Path::new("/") {
        return false;
    }
    for prefix in critical_prefixes {
        let prefix_path = Path::new(prefix);
        if lexical == *prefix_path || lexical.starts_with(prefix_path) {
            return false;
        }
    }

    // At least 2 path components
    let parts: Vec<_> = lexical
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .collect();
    parts.len() >= 2
}

/// Remove all user config/data/cache directories.
fn purge_data() -> Result<(), AishError> {
    let dirs = get_data_directories();
    let mut success = true;

    for (name, path) in &dirs {
        if !path.exists() {
            continue;
        }
        if !is_safe_purge_path(path) {
            let path_str = path.display().to_string();
            eprintln!("\x1b[31m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("path".to_string(), path_str);
                t_with_args("cli.uninstall.refusing_unsafe_delete", &args)
            });
            success = false;
            continue;
        }
        match std::fs::remove_dir_all(path) {
            Ok(()) => {
                let path_str = path.display().to_string();
                println!("\x1b[32m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("name".to_string(), name.clone());
                    args.insert("path".to_string(), path_str);
                    t_with_args("cli.uninstall.file_removed", &args)
                })
            }
            Err(e) => {
                let path_str = path.display().to_string();
                eprintln!("\x1b[31m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("path".to_string(), path_str);
                    args.insert("error".to_string(), e.to_string());
                    t_with_args("cli.uninstall.file_remove_failed", &args)
                });
                success = false;
            }
        }
    }

    if success {
        Ok(())
    } else {
        Err(AishError::Config(t("cli.uninstall.some_files_failed")))
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run_uninstall(purge: bool, yes: bool) {
    println!("\x1b[1;36m{}\x1b[0m\n", t("cli.uninstall.title"));

    let method = detect_installation_method();
    println!("\x1b[2m{}\x1b[0m", {
        let mut args = std::collections::HashMap::new();
        args.insert("method".to_string(), method.to_string());
        t_with_args("cli.uninstall.installation_method", &args)
    });

    if purge {
        println!("\x1b[1;33m{}\x1b[0m", t("cli.uninstall.purge_warning"));
        for (_, path) in get_data_directories() {
            if path.exists() {
                println!("  \x1b[2m{}\x1b[0m", path.display());
            }
        }
    }

    if !yes {
        print!("\n\x1b[33m{}\x1b[0m", t("cli.uninstall.proceed_uninstall"));
        io::stdout().flush().unwrap();
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).unwrap();
        if answer.trim().to_lowercase() != "y" {
            println!("{}", t("cli.uninstall.cancelled"));
            return;
        }
    }

    println!("{}", t("cli.uninstall.uninstalling"));

    let result = match method {
        InstallMethod::Archive => uninstall_archive(purge),
        InstallMethod::Cargo => uninstall_cargo(),
        InstallMethod::Pip => uninstall_pip(),
        InstallMethod::System => uninstall_system(purge),
        InstallMethod::Unknown => {
            eprintln!("\x1b[33m{}\x1b[0m", t("cli.uninstall.unknown_method"));
            eprintln!("\x1b[2m{}\x1b[0m", t("cli.uninstall.attempting_removal"));
            remove_current_binary()
        }
    };

    if let Err(e) = result {
        eprintln!("\x1b[31m{}\x1b[0m", {
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.uninstall.uninstall_failed", &args)
        });
        return;
    }
    println!("\x1b[32m{}\x1b[0m", t("cli.uninstall.package_removed"));

    if purge {
        match purge_data() {
            Ok(()) => println!("\x1b[32m{}\x1b[0m", t("cli.uninstall.config_purged")),
            Err(e) => eprintln!("\x1b[31m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.uninstall.purge_failed", &args)
            }),
        }
    }

    println!("\x1b[32m{}\x1b[0m", t("cli.uninstall.goodbye"));
}

/// Fallback: remove the currently running binary.
fn remove_current_binary() -> Result<(), AishError> {
    let exe = std::env::current_exe().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.uninstall.cannot_remove_binary", &args)
        })
    })?;
    if exe.exists() {
        std::fs::remove_file(&exe).map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.uninstall.failed_remove_binary", &args)
            })
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_purge_path_valid() {
        let path = PathBuf::from("/home/user/.config/aish");
        assert!(is_safe_purge_path(&path));
    }

    #[test]
    fn test_safe_purge_path_cache() {
        let path = PathBuf::from("/home/user/.cache/aish");
        assert!(is_safe_purge_path(&path));
    }

    #[test]
    fn test_is_safe_purge_path_rejects_root() {
        assert!(!is_safe_purge_path(&PathBuf::from("/")));
    }

    #[test]
    fn test_is_safe_purge_path_rejects_system() {
        assert!(!is_safe_purge_path(&PathBuf::from("/usr/aish")));
        assert!(!is_safe_purge_path(&PathBuf::from("/etc/aish")));
    }

    #[test]
    fn test_is_safe_purge_path_rejects_wrong_name() {
        assert!(!is_safe_purge_path(&PathBuf::from(
            "/home/user/.config/other"
        )));
    }

    #[test]
    fn test_is_safe_purge_path_rejects_short() {
        assert!(!is_safe_purge_path(&PathBuf::from("/aish")));
    }

    #[test]
    fn test_detect_installation_method_runs() {
        // Just verify it doesn't panic
        let _method = detect_installation_method();
    }

    #[test]
    fn test_get_data_directories() {
        let dirs = get_data_directories();
        assert!(!dirs.is_empty());
        for (name, path) in &dirs {
            assert!(path.ends_with("aish"), "{} should end with aish", name);
        }
    }
}
