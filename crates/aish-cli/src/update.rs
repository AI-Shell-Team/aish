//! Self-update via GitHub releases.
//!
//! Supports platform-aware download, progress display, mirror fallback,
//! archive extraction with install.sh execution, and automatic cleanup.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aish_core::AishError;

const GITHUB_API_LATEST: &str = "https://api.github.com/repos/AI-Shell-Team/aish/releases/latest";
const GITHUB_API_LIST: &str = "https://api.github.com/repos/AI-Shell-Team/aish/releases";
const GITHUB_RELEASES_BASE: &str = "https://github.com/AI-Shell-Team/aish/releases/download";
const FALLBACK_MIRROR: &str = "https://www.aishell.ai/repo";
const CONNECTION_TIMEOUT_SECS: u64 = 10;
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

#[derive(Debug)]
pub struct UpdateInfo {
    pub tag_name: String,
    pub current_version: String,
    pub latest_version: String,
    pub html_url: String,
    #[allow(dead_code)]
    pub release_notes: String,
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Compare two version strings by numeric parts.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse_parts = |v: &str| -> Vec<u64> {
        v.strip_prefix('v')
            .unwrap_or(v)
            .split('.')
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let a_parts = parse_parts(a);
    let b_parts = parse_parts(b);
    for i in 0..a_parts.len().max(b_parts.len()) {
        let a_val = a_parts.get(i).unwrap_or(&0);
        let b_val = b_parts.get(i).unwrap_or(&0);
        match a_val.cmp(b_val) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

fn detect_platform() -> Result<(&'static str, &'static str), AishError> {
    let plat = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        other => {
            return Err(AishError::Config(format!(
                "Unsupported platform: {}",
                other
            )))
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => {
            return Err(AishError::Config(format!(
                "Unsupported architecture: {}",
                other
            )))
        }
    };
    Ok((plat, arch))
}

// ---------------------------------------------------------------------------
// Update check
// ---------------------------------------------------------------------------

fn build_http_client(timeout_secs: u64) -> Result<reqwest::blocking::Client, AishError> {
    reqwest::blocking::Client::builder()
        .user_agent("aish-update-checker")
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| AishError::Config(format!("HTTP client error: {e}")))
}

pub fn check_for_updates(
    current_version: &str,
    include_pre_release: bool,
) -> Result<Option<UpdateInfo>, AishError> {
    let client = build_http_client(CONNECTION_TIMEOUT_SECS)?;
    let url = if include_pre_release {
        GITHUB_API_LIST
    } else {
        GITHUB_API_LATEST
    };

    let resp = client
        .get(url)
        .send()
        .map_err(|e| AishError::Config(format!("Failed to check for updates: {e}")))?;

    if !resp.status().is_success() {
        return Err(AishError::Config(format!(
            "GitHub API returned status {}",
            resp.status()
        )));
    }

    let release = if include_pre_release {
        let releases: Vec<serde_json::Value> = resp
            .json()
            .map_err(|e| AishError::Config(format!("Failed to parse releases: {e}")))?;
        match releases.into_iter().next() {
            Some(r) => r,
            None => return Ok(None),
        }
    } else {
        resp.json()
            .map_err(|e| AishError::Config(format!("Failed to parse release: {e}")))?
    };

    extract_update_info(&release, current_version)
}

fn extract_update_info(
    release: &serde_json::Value,
    current_version: &str,
) -> Result<Option<UpdateInfo>, AishError> {
    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let latest = tag.strip_prefix('v').unwrap_or(tag);
    let current = current_version.strip_prefix('v').unwrap_or(current_version);

    if compare_versions(latest, current) == std::cmp::Ordering::Greater {
        return Ok(Some(UpdateInfo {
            tag_name: tag.to_string(),
            current_version: current.to_string(),
            latest_version: latest.to_string(),
            html_url: release
                .get("html_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            release_notes: release
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Download with progress
// ---------------------------------------------------------------------------

fn download_with_progress(url: &str, dest: &Path, label: &str) -> Result<(), AishError> {
    let client = build_http_client(DOWNLOAD_TIMEOUT_SECS)?;

    let resp = client
        .get(url)
        .send()
        .map_err(|e| AishError::Config(format!("Download failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(AishError::Config(format!(
            "Download returned status {}",
            resp.status()
        )));
    }

    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut file = std::fs::File::create(dest)
        .map_err(|e| AishError::Config(format!("Failed to create file: {e}")))?;

    let mut downloaded: u64 = 0;
    let mut buf = [0u8; 8192];
    let mut resp = resp;

    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| AishError::Config(format!("Download read error: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| AishError::Config(format!("Write error: {e}")))?;
        downloaded += n as u64;

        if total > 0 {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            print!(
                "\r\x1b[2K\x1b[1;36m{}: {:.1}/{:.1} MB ({}%)\x1b[0m",
                label,
                downloaded as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0,
                pct
            );
        } else {
            print!(
                "\r\x1b[2K\x1b[1;36m{}: {:.1} MB\x1b[0m",
                label,
                downloaded as f64 / 1_048_576.0
            );
        }
        std::io::stdout().flush().ok();
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// SHA256
// ---------------------------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String, AishError> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    let mut file =
        std::fs::File::open(path).map_err(|e| AishError::Config(format!("Open error: {e}")))?;
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AishError::Config(format!("Read error: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Download release (GitHub → mirror fallback)
// ---------------------------------------------------------------------------

fn download_release(tag_name: &str) -> Result<PathBuf, AishError> {
    let (plat, arch) = detect_platform()?;
    let version_str = tag_name.strip_prefix('v').unwrap_or(tag_name);
    let filename = format!("aish-{}-{}-{}.tar.gz", version_str, plat, arch);

    let temp_dir = std::env::temp_dir().join("aish_update");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| AishError::Config(format!("Failed to create temp dir: {e}")))?;

    let dest_path = temp_dir.join(&filename);

    // Try GitHub first
    let github_url = format!("{}/{}/{}", GITHUB_RELEASES_BASE, tag_name, filename);
    println!("\x1b[1;36mDownloading from GitHub...\x1b[0m");
    if download_with_progress(&github_url, &dest_path, &filename).is_ok() {
        println!("\x1b[32mDownloaded: {}\x1b[0m", dest_path.display());
        return Ok(dest_path);
    }

    // Fallback to mirror
    println!("\x1b[33mGitHub download failed, trying mirror...\x1b[0m");
    let mirror_url = format!("{}/{}/{}", FALLBACK_MIRROR, tag_name, filename);
    download_with_progress(&mirror_url, &dest_path, &format!("{} (mirror)", filename))?;
    println!("\x1b[32mDownloaded: {}\x1b[0m", dest_path.display());
    Ok(dest_path)
}

// ---------------------------------------------------------------------------
// Archive extraction & install
// ---------------------------------------------------------------------------

fn find_install_sh(dir: &Path) -> Result<PathBuf, AishError> {
    fn search(dir: &Path) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = search(&path) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|n| n == "install.sh") {
                return Some(path);
            }
        }
        None
    }
    search(dir).ok_or_else(|| AishError::Config("install.sh not found in archive".into()))
}

fn install_release(archive_path: &Path) -> Result<(), AishError> {
    let extract_dir = std::env::temp_dir().join("aish_update").join("extract");

    // Clean previous extraction
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| AishError::Config(format!("Failed to create extract dir: {e}")))?;

    // Extract via system tar
    println!("\x1b[1;36mExtracting archive...\x1b[0m");
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(&extract_dir)
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run tar: {e}")))?;

    if !output.status.success() {
        return Err(AishError::Config(format!(
            "Extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Locate install.sh
    let install_script = find_install_sh(&extract_dir)?;

    // Show SHA256 for verification
    let hash = sha256_file(&install_script)?;
    println!("\x1b[2minstall.sh SHA256: {}\x1b[0m", hash);

    // Run with sudo
    println!("\x1b[1;36mRunning install script...\x1b[0m");
    let result = std::process::Command::new("sudo")
        .arg(&install_script)
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run install script: {e}")))?;

    if !result.status.success() {
        return Err(AishError::Config(format!(
            "Installation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }

    println!("\x1b[32mInstallation successful!\x1b[0m");
    Ok(())
}

/// Remove temporary download and extraction files.
fn cleanup() {
    let temp_dir = std::env::temp_dir().join("aish_update");
    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run_update(check_only: bool, pre_release: bool) {
    let current = env!("CARGO_PKG_VERSION").to_string();

    println!("\x1b[1;36mChecking for updates...\x1b[0m");

    match check_for_updates(&current, pre_release) {
        Ok(Some(info)) => {
            println!(
                "\x1b[1;33mUpdate available: {} → {}\x1b[0m",
                info.current_version, info.latest_version
            );
            println!("\x1b[2m{}\x1b[0m", info.html_url);

            if check_only {
                return;
            }

            print!("\n\x1b[33mDownload and install? [y/N] \x1b[0m");
            std::io::stdout().flush().unwrap();
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).unwrap();
            if answer.trim().to_lowercase() != "y" {
                println!("Update cancelled.");
                return;
            }

            match download_release(&info.tag_name) {
                Ok(archive_path) => {
                    if let Err(e) = install_release(&archive_path) {
                        eprintln!("\x1b[31mInstallation error: {}\x1b[0m", e);
                    }
                }
                Err(e) => {
                    eprintln!("\x1b[31mDownload failed: {}\x1b[0m", e);
                }
            }

            cleanup();
        }
        Ok(None) => {
            println!(
                "\x1b[32mAlready on the latest version ({}).\x1b[0m",
                current
            );
        }
        Err(e) => {
            eprintln!("\x1b[31mUpdate check failed: {}\x1b[0m", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions_equal() {
        assert_eq!(
            compare_versions("0.2.0", "0.2.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn test_compare_versions_major() {
        assert_eq!(
            compare_versions("1.0.0", "0.9.9"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_compare_versions_minor() {
        assert_eq!(
            compare_versions("0.3.0", "0.2.9"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_compare_versions_patch() {
        assert_eq!(
            compare_versions("0.2.1", "0.2.0"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_compare_versions_with_v_prefix() {
        assert_eq!(
            compare_versions("v0.2.0", "0.2.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn test_detect_platform() {
        // Should succeed on any supported platform
        let result = detect_platform();
        assert!(result.is_ok());
        let (plat, arch) = result.unwrap();
        assert!(plat == "linux" || plat == "darwin");
        assert!(arch == "amd64" || arch == "arm64");
    }

    #[test]
    fn test_sha256_file() {
        let dir = std::env::temp_dir().join("aish_test_sha256");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();
        let hash = sha256_file(&path).unwrap();
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_install_sh() {
        let dir = std::env::temp_dir().join("aish_test_find");
        let sub = dir.join("aish-0.3.0");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("install.sh"), "#!/bin/bash\necho ok").unwrap();
        let result = find_install_sh(&dir);
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("install.sh"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_install_sh_not_found() {
        let dir = std::env::temp_dir().join("aish_test_find_empty");
        std::fs::create_dir_all(&dir).unwrap();
        let result = find_install_sh(&dir);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
