//! Self-update via CDN-hosted stable releases and GitHub pre-releases.
//!
//! Stable updates read the CDN latest metadata and download versioned release
//! bundles. Pre-release discovery continues to use the GitHub releases API.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aish_core::AishError;
use aish_i18n::{t, t_with_args};

const DEFAULT_DOWNLOAD_BASE_URL: &str = "https://cdn.aishell.ai/download";
const GITHUB_API_LIST: &str = "https://api.github.com/repos/AI-Shell-Team/aish/releases";
const GITHUB_RELEASES_PAGE_BASE: &str = "https://github.com/AI-Shell-Team/aish/releases";
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
        other => {
            return Err(AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("platform".to_string(), other.to_string());
                t_with_args("cli.update.unsupported_platform", &args)
            }))
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        other => {
            return Err(AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("arch".to_string(), other.to_string());
                t_with_args("cli.update.unsupported_arch", &args)
            }))
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
        .map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.http_error", &args)
            })
        })
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn download_base_url() -> String {
    env_var("AISH_DOWNLOAD_BASE_URL")
        .or_else(|| env_var("AISH_REPO_URL"))
        .unwrap_or_else(|| DEFAULT_DOWNLOAD_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn latest_version_url() -> String {
    env_var("AISH_LATEST_URL").unwrap_or_else(|| format!("{}/latest", download_base_url()))
}

fn release_download_url(tag_name: &str, filename: &str) -> String {
    let version_str = tag_name.strip_prefix('v').unwrap_or(tag_name);
    format!(
        "{}/releases/{}/{}",
        download_base_url(),
        version_str,
        filename
    )
}

fn normalize_release_tag(version_value: &str) -> Result<String, AishError> {
    let trimmed = version_value.trim();
    if trimmed.is_empty() {
        return Err(AishError::Config(t("cli.update.latest_metadata_invalid")));
    }

    let normalized = trimmed.trim_start_matches('v');
    let valid = normalized
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));

    if !valid || !normalized.chars().any(|ch| ch.is_ascii_digit()) {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("value".to_string(), trimmed.to_string());
            t_with_args("cli.update.latest_metadata_invalid_value", &args)
        }));
    }

    Ok(format!("v{}", normalized))
}

fn stable_release_info(client: &reqwest::blocking::Client) -> Result<serde_json::Value, AishError> {
    let resp = client.get(latest_version_url()).send().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.check_failed", &args)
        })
    })?;

    if !resp.status().is_success() {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("status".to_string(), resp.status().to_string());
            t_with_args("cli.update.release_metadata_error", &args)
        }));
    }

    let tag_name = normalize_release_tag(&resp.text().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.parse_release_failed", &args)
        })
    })?)?;

    Ok(serde_json::json!({
        "tag_name": tag_name,
        "html_url": format!("{}/tag/{}", GITHUB_RELEASES_PAGE_BASE, tag_name),
        "body": "",
    }))
}

fn pre_release_info(
    client: &reqwest::blocking::Client,
) -> Result<Option<serde_json::Value>, AishError> {
    let resp = client.get(GITHUB_API_LIST).send().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.check_failed", &args)
        })
    })?;

    if !resp.status().is_success() {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("status".to_string(), resp.status().to_string());
            t_with_args("cli.update.github_api_error", &args)
        }));
    }

    let releases: Vec<serde_json::Value> = resp.json().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.parse_releases_failed", &args)
        })
    })?;

    Ok(releases.into_iter().next())
}

pub fn check_for_updates(
    current_version: &str,
    include_pre_release: bool,
) -> Result<Option<UpdateInfo>, AishError> {
    let client = build_http_client(CONNECTION_TIMEOUT_SECS)?;
    let release = if include_pre_release {
        match pre_release_info(&client)? {
            Some(r) => r,
            None => return Ok(None),
        }
    } else {
        stable_release_info(&client)?
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

    let resp = client.get(url).send().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.download_failed", &args)
        })
    })?;

    if !resp.status().is_success() {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("status".to_string(), resp.status().to_string());
            t_with_args("cli.update.download_status_error", &args)
        }));
    }

    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut file = std::fs::File::create(dest).map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.file_create_failed", &args)
        })
    })?;

    let mut downloaded: u64 = 0;
    let mut buf = [0u8; 8192];
    let mut resp = resp;

    loop {
        let n = resp.read(&mut buf).map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.download_read_error", &args)
            })
        })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.write_error", &args)
            })
        })?;
        downloaded += n as u64;

        if total > 0 {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            let downloaded_mb = downloaded as f64 / 1_048_576.0;
            let total_mb = total as f64 / 1_048_576.0;
            print!("\r\x1b[2K\x1b[1;36m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("label".to_string(), label.to_string());
                args.insert("downloaded".to_string(), format!("{:.1}", downloaded_mb));
                args.insert("total".to_string(), format!("{:.1}", total_mb));
                args.insert("pct".to_string(), pct.to_string());
                t_with_args("cli.update.progress_mb", &args)
            });
        } else {
            let downloaded_mb = downloaded as f64 / 1_048_576.0;
            print!("\r\x1b[2K\x1b[1;36m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("label".to_string(), label.to_string());
                args.insert("downloaded".to_string(), format!("{:.1}", downloaded_mb));
                t_with_args("cli.update.progress_mb_no_total", &args)
            });
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
    let mut file = std::fs::File::open(path).map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.open_error", &args)
        })
    })?;
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.read_error", &args)
            })
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn expected_sha256(path: &Path) -> Result<String, AishError> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.read_error", &args)
        })
    })?;

    contents
        .split_whitespace()
        .next()
        .filter(|value| value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(|value| value.to_ascii_lowercase())
        .ok_or_else(|| AishError::Config(t("cli.update.checksum_file_invalid")))
}

fn verify_download_checksum(archive_path: &Path, checksum_path: &Path) -> Result<(), AishError> {
    let expected = expected_sha256(checksum_path)?;
    let actual = sha256_file(archive_path)?;

    if actual != expected {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("expected".to_string(), expected);
            args.insert("actual".to_string(), actual);
            t_with_args("cli.update.checksum_mismatch", &args)
        }));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Download release bundle from CDN
// ---------------------------------------------------------------------------

fn download_release(tag_name: &str) -> Result<PathBuf, AishError> {
    let (plat, arch) = detect_platform()?;
    let version_str = tag_name.strip_prefix('v').unwrap_or(tag_name);
    let filename = format!("aish-{}-{}-{}.tar.gz", version_str, plat, arch);

    let temp_dir = std::env::temp_dir().join("aish_update");
    std::fs::create_dir_all(&temp_dir).map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.temp_dir_failed", &args)
        })
    })?;

    let dest_path = temp_dir.join(&filename);
    let checksum_filename = format!("{filename}.sha256");
    let checksum_path = temp_dir.join(&checksum_filename);

    let release_url = release_download_url(tag_name, &filename);
    let checksum_url = release_download_url(tag_name, &checksum_filename);
    println!("\x1b[1;36m{}\x1b[0m", t("cli.update.downloading_release"));
    download_with_progress(&release_url, &dest_path, &filename)?;
    download_with_progress(&checksum_url, &checksum_path, &checksum_filename)?;
    verify_download_checksum(&dest_path, &checksum_path)?;
    let path_str = dest_path.display().to_string();
    println!("\x1b[32m{}\x1b[0m", {
        let mut args = std::collections::HashMap::new();
        args.insert("path".to_string(), path_str);
        t_with_args("cli.update.downloaded", &args)
    });
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
    search(dir).ok_or_else(|| AishError::Config(t("cli.update.install_sh_not_found")))
}

fn install_release(archive_path: &Path) -> Result<(), AishError> {
    let extract_dir = std::env::temp_dir().join("aish_update").join("extract");

    // Clean previous extraction
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir).map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.extract_dir_failed", &args)
        })
    })?;

    // Extract via system tar
    println!("\x1b[1;36m{}\x1b[0m", t("cli.update.extracting"));
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(&extract_dir)
        .output()
        .map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.tar_failed", &args)
            })
        })?;

    if !output.status.success() {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert(
                "error".to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            );
            t_with_args("cli.update.extraction_failed", &args)
        }));
    }

    // Locate install.sh
    let install_script = find_install_sh(&extract_dir)?;

    // Show SHA256 for verification
    let hash = sha256_file(&install_script)?;
    println!("\x1b[2m{}\x1b[0m", {
        let mut args = std::collections::HashMap::new();
        args.insert("hash".to_string(), hash);
        t_with_args("cli.update.install_sh_hash", &args)
    });

    // Run with sudo
    println!(
        "\x1b[1;36m{}\x1b[0m",
        t("cli.update.running_install_script")
    );
    let result = std::process::Command::new("sudo")
        .arg(&install_script)
        .output()
        .map_err(|e| {
            AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.install_script_failed", &args)
            })
        })?;

    if !result.status.success() {
        return Err(AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert(
                "error".to_string(),
                String::from_utf8_lossy(&result.stderr).to_string(),
            );
            t_with_args("cli.update.installation_failed", &args)
        }));
    }

    println!("\x1b[32m{}\x1b[0m", t("cli.update.installation_successful"));
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

    println!("\x1b[1;36m{}\x1b[0m", t("cli.update.checking"));

    match check_for_updates(&current, pre_release) {
        Ok(Some(info)) => {
            println!("\x1b[1;33m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("current".to_string(), info.current_version.clone());
                args.insert("latest".to_string(), info.latest_version.clone());
                t_with_args("cli.update.update_available", &args)
            });
            println!("\x1b[2m{}\x1b[0m", info.html_url);

            if check_only {
                return;
            }

            print!(
                "\n\x1b[33m{}\x1b[0m",
                t("cli.update.download_install_prompt")
            );
            std::io::stdout().flush().unwrap();
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).unwrap();
            let ans = answer.trim().to_lowercase();
            if ans != "y" && ans != "yes" {
                println!("{}", t("cli.update.update_cancelled"));
                return;
            }

            match download_release(&info.tag_name) {
                Ok(archive_path) => {
                    if let Err(e) = install_release(&archive_path) {
                        eprintln!("\x1b[31m{}\x1b[0m", {
                            let mut args = std::collections::HashMap::new();
                            args.insert("error".to_string(), e.to_string());
                            t_with_args("cli.update.installation_error", &args)
                        });
                    }
                }
                Err(e) => {
                    eprintln!("\x1b[31m{}\x1b[0m", {
                        let mut args = std::collections::HashMap::new();
                        args.insert("error".to_string(), e.to_string());
                        t_with_args("cli.update.download_error", &args)
                    });
                }
            }

            cleanup();
        }
        Ok(None) => {
            println!("\x1b[32m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("version".to_string(), current);
                t_with_args("cli.update.already_latest", &args)
            });
        }
        Err(e) => {
            eprintln!("\x1b[31m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.update.update_check_failed", &args)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

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
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => assert_eq!(detect_platform().unwrap(), ("linux", "amd64")),
            _ => assert!(detect_platform().is_err()),
        }
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
    fn test_download_base_url_defaults_to_cdn() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("AISH_DOWNLOAD_BASE_URL");
            std::env::remove_var("AISH_REPO_URL");
        }
        assert_eq!(download_base_url(), "https://cdn.aishell.ai/download");
    }

    #[test]
    fn test_download_base_url_prefers_explicit_override() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("AISH_REPO_URL", "https://legacy.example.com/download");
            std::env::set_var(
                "AISH_DOWNLOAD_BASE_URL",
                "https://cdn.example.com/download/",
            );
        }
        assert_eq!(download_base_url(), "https://cdn.example.com/download");
        unsafe {
            std::env::remove_var("AISH_DOWNLOAD_BASE_URL");
            std::env::remove_var("AISH_REPO_URL");
        }
    }

    #[test]
    fn test_latest_version_url_uses_default_pattern() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("AISH_DOWNLOAD_BASE_URL");
            std::env::remove_var("AISH_REPO_URL");
            std::env::remove_var("AISH_LATEST_URL");
        }
        assert_eq!(
            latest_version_url(),
            "https://cdn.aishell.ai/download/latest"
        );
    }

    #[test]
    fn test_latest_version_url_prefers_override() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var(
                "AISH_LATEST_URL",
                "https://cdn.example.com/custom/latest.txt",
            );
        }
        assert_eq!(
            latest_version_url(),
            "https://cdn.example.com/custom/latest.txt"
        );
        unsafe {
            std::env::remove_var("AISH_LATEST_URL");
        }
    }

    #[test]
    fn test_release_download_url_uses_versioned_cdn_path() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("AISH_DOWNLOAD_BASE_URL", "https://cdn.example.com/download");
        }
        assert_eq!(
            release_download_url("v0.3.0", "aish-0.3.0-linux-amd64.tar.gz"),
            "https://cdn.example.com/download/releases/0.3.0/aish-0.3.0-linux-amd64.tar.gz"
        );
        unsafe {
            std::env::remove_var("AISH_DOWNLOAD_BASE_URL");
        }
    }

    #[test]
    fn test_normalize_release_tag_accepts_plain_version_text() {
        assert_eq!(normalize_release_tag("0.3.0\n").unwrap(), "v0.3.0");
        assert_eq!(normalize_release_tag("v0.3.0").unwrap(), "v0.3.0");
    }

    #[test]
    fn test_normalize_release_tag_rejects_invalid_metadata() {
        assert!(normalize_release_tag("").is_err());
        assert!(normalize_release_tag("latest").is_err());
        assert!(normalize_release_tag("release-2026-04-23").is_err());
        assert!(normalize_release_tag("0-rc1").is_err());
    }

    #[test]
    fn test_expected_sha256_accepts_sha256sum_output() {
        let dir = std::env::temp_dir().join("aish_test_expected_sha256");
        std::fs::create_dir_all(&dir).unwrap();
        let checksum_path = dir.join("bundle.tar.gz.sha256");
        std::fs::write(
            &checksum_path,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  bundle.tar.gz\n",
        )
        .unwrap();

        assert_eq!(
            expected_sha256(&checksum_path).unwrap(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_download_checksum_rejects_mismatch() {
        let dir = std::env::temp_dir().join("aish_test_verify_download_checksum");
        std::fs::create_dir_all(&dir).unwrap();
        let archive_path = dir.join("bundle.tar.gz");
        let checksum_path = dir.join("bundle.tar.gz.sha256");
        std::fs::write(&archive_path, b"hello world").unwrap();
        std::fs::write(
            &checksum_path,
            "0000000000000000000000000000000000000000000000000000000000000000  bundle.tar.gz\n",
        )
        .unwrap();

        assert!(verify_download_checksum(&archive_path, &checksum_path).is_err());

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
