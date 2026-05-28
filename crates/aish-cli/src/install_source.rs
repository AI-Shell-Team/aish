use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const ARCHIVE_BIN_DIR: &str = "/usr/local/bin";
pub(crate) const AISH_BINARY_NAME: &str = "aish";
pub(crate) const AISH_UNINSTALL_BINARY_NAME: &str = "aish-uninstall";
pub(crate) const CARGO_PACKAGE_NAME: &str = "aish-cli";
pub(crate) const PIP_DISTRIBUTION_NAME: &str = "aish-cli";
pub(crate) const SYSTEM_PACKAGE_NAME: &str = "aish";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallMethod {
    Archive,
    Cargo,
    Pip,
    System,
    Unknown,
}

impl std::fmt::Display for InstallMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive => write!(f, "archive"),
            Self::Cargo => write!(f, "cargo"),
            Self::Pip => write!(f, "pip"),
            Self::System => write!(f, "system"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PipContext {
    VirtualEnv(PathBuf),
    UserLocal,
    Global,
}

pub(crate) fn is_command_available(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn detect_installation_method() -> InstallMethod {
    let current_exe = std::env::current_exe().ok();

    if is_cargo_exe(current_exe.as_deref()) {
        return InstallMethod::Cargo;
    }

    if is_archive_exe(current_exe.as_deref()) {
        return InstallMethod::Archive;
    }

    if current_exe
        .as_deref()
        .is_some_and(|path| path == Path::new("/usr/bin").join(AISH_BINARY_NAME))
    {
        return InstallMethod::System;
    }

    if detect_pip_context().is_some() || pip_distribution_installed() {
        return InstallMethod::Pip;
    }

    if system_package_installed() {
        return InstallMethod::System;
    }

    InstallMethod::Unknown
}

pub(crate) fn detect_pip_context() -> Option<PipContext> {
    let current_exe = std::env::current_exe().ok()?;
    detect_pip_context_for_path(&current_exe)
}

pub(crate) fn build_pip_command(context: Option<&PipContext>) -> Command {
    match context {
        Some(PipContext::VirtualEnv(root)) => {
            let python = root.join("bin").join("python");
            let python3 = root.join("bin").join("python3");
            let executable = if python.exists() { python } else { python3 };
            let mut command = Command::new(executable);
            command.arg("-m").arg("pip");
            command
        }
        _ => Command::new("pip"),
    }
}

fn archive_binary_path() -> PathBuf {
    Path::new(ARCHIVE_BIN_DIR).join(AISH_BINARY_NAME)
}

fn archive_uninstall_path() -> PathBuf {
    Path::new(ARCHIVE_BIN_DIR).join(AISH_UNINSTALL_BINARY_NAME)
}

fn is_archive_exe(current_exe: Option<&Path>) -> bool {
    let archive_binary = archive_binary_path();
    let archive_uninstall = archive_uninstall_path();

    current_exe.is_some_and(|path| path == archive_binary)
        && archive_uninstall.exists()
        && is_elf_binary(&archive_binary)
}

fn is_cargo_exe(current_exe: Option<&Path>) -> bool {
    current_exe
        .and_then(Path::to_str)
        .is_some_and(|path| path.contains("/.cargo/bin/"))
}

fn pip_distribution_installed() -> bool {
    if !is_command_available("pip") {
        return false;
    }

    build_pip_command(None)
        .args(["show", PIP_DISTRIBUTION_NAME])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn system_package_installed() -> bool {
    if is_command_available("dpkg")
        && Command::new("dpkg")
            .args(["-s", SYSTEM_PACKAGE_NAME])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    {
        return true;
    }

    is_command_available("rpm")
        && Command::new("rpm")
            .args(["-q", SYSTEM_PACKAGE_NAME])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}

fn detect_pip_context_for_path(path: &Path) -> Option<PipContext> {
    if path.file_name()? != AISH_BINARY_NAME {
        return None;
    }

    if let Some(home) = dirs::home_dir() {
        if path.starts_with(home.join(".local").join("bin")) {
            return Some(PipContext::UserLocal);
        }
    }

    let parent = path.parent()?;
    if parent.file_name()? == "bin" {
        let root = parent.parent()?;
        if root.join("pyvenv.cfg").exists() {
            return Some(PipContext::VirtualEnv(root.to_path_buf()));
        }

        if !path.starts_with("/usr/bin")
            && !path.starts_with(ARCHIVE_BIN_DIR)
            && !path.to_string_lossy().contains("/.cargo/bin/")
        {
            return Some(PipContext::Global);
        }
    }

    if path.starts_with("/usr/local/bin") && !archive_uninstall_path().exists() {
        return Some(PipContext::Global);
    }

    None
}

fn is_elf_binary(path: &Path) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => bytes.len() >= 4 && bytes[..4] == [0x7f, b'E', b'L', b'F'],
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_user_local_pip_context() {
        let home = dirs::home_dir().expect("home directory should exist in tests");
        let owned_path = home.join(".local").join("bin").join("aish");
        let path = owned_path.as_path();
        assert_eq!(
            detect_pip_context_for_path(path),
            Some(PipContext::UserLocal)
        );
    }

    #[test]
    fn detects_virtualenv_pip_context() {
        let root = std::env::temp_dir().join("aish_install_source_venv");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(root.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();

        let detected = detect_pip_context_for_path(&bin.join("aish"));
        assert_eq!(detected, Some(PipContext::VirtualEnv(root.clone())));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_non_aish_binary() {
        let path = Path::new("/tmp/bin/not-aish");
        assert_eq!(detect_pip_context_for_path(path), None);
    }
}
