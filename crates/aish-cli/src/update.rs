//! Self-update via CDN release metadata and bundles.
//!
//! Version discovery and downloads use `cdn.aishell.ai` only (Claude Code–style
//! `/latest` text + versioned release paths). No GitHub API dependency.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aish_core::AishError;
use aish_i18n::{t, t_with_args};
use semver::Version;
use serde_json::Value;

use crate::install_channel::{current_install_channel, InstallChannel, PipChannelContext};

const DEFAULT_DOWNLOAD_BASE_URL: &str = "https://cdn.aishell.ai/download";
const DEFAULT_BETA_DOWNLOAD_BASE_URL: &str = "https://cdn.aishell.ai/download/beta";
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

#[derive(Debug)]
struct PipCheckInfo {
    latest_version: String,
    html_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Compare version strings with SemVer prerelease ordering.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse_semver = |v: &str| Version::parse(v.strip_prefix('v').unwrap_or(v));
    if let (Ok(a_version), Ok(b_version)) = (parse_semver(a), parse_semver(b)) {
        return a_version.cmp(&b_version);
    }

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
            return Err(AishError::Config({
                let mut args = std::collections::HashMap::new();
                args.insert("platform".to_string(), other.to_string());
                t_with_args("cli.update.unsupported_platform", &args)
            }))
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
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

fn get_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn is_prerelease_version(version: &str) -> bool {
    Version::parse(version.strip_prefix('v').unwrap_or(version))
        .map(|parsed| !parsed.pre.is_empty())
        .unwrap_or_else(|_| version.strip_prefix('v').unwrap_or(version).contains('-'))
}

fn resolve_download_base_url<F>(include_pre_release: bool, env_get: F) -> String
where
    F: Fn(&str) -> Option<String> + Copy,
{
    let value = if include_pre_release {
        env_get("AISH_BETA_DOWNLOAD_BASE_URL")
            .unwrap_or_else(|| DEFAULT_BETA_DOWNLOAD_BASE_URL.to_string())
    } else {
        env_get("AISH_DOWNLOAD_BASE_URL")
            .or_else(|| env_get("AISH_REPO_URL"))
            .unwrap_or_else(|| DEFAULT_DOWNLOAD_BASE_URL.to_string())
    };
    value.trim_end_matches('/').to_string()
}

fn resolve_latest_version_url<F>(include_pre_release: bool, env_get: F) -> String
where
    F: Fn(&str) -> Option<String> + Copy,
{
    if include_pre_release {
        env_get("AISH_BETA_LATEST_URL")
            .unwrap_or_else(|| format!("{}/latest", resolve_download_base_url(true, env_get)))
    } else {
        env_get("AISH_LATEST_URL")
            .unwrap_or_else(|| format!("{}/latest", resolve_download_base_url(false, env_get)))
    }
}

fn get_latest_version_url(include_pre_release: bool) -> String {
    resolve_latest_version_url(include_pre_release, get_env_var)
}

fn resolve_release_download_url<F>(tag_name: &str, filename: &str, env_get: F) -> String
where
    F: Fn(&str) -> Option<String> + Copy,
{
    let version_str = tag_name.strip_prefix('v').unwrap_or(tag_name);
    let include_pre_release = is_prerelease_version(version_str);
    format!(
        "{}/releases/{}/{}",
        resolve_download_base_url(include_pre_release, env_get),
        version_str,
        filename
    )
}

fn get_release_download_url(tag_name: &str, filename: &str) -> String {
    resolve_release_download_url(tag_name, filename, get_env_var)
}

fn normalize_tag(version_value: &str) -> Result<String, AishError> {
    let cleaned = version_value.trim();
    let normalized = cleaned.strip_prefix('v').unwrap_or(cleaned);
    if normalized.is_empty() {
        return Err(AishError::Config(
            "Invalid latest version metadata: <empty>".to_string(),
        ));
    }

    Version::parse(normalized)
        .map_err(|_| AishError::Config(format!("Invalid latest version metadata: {cleaned}")))?;

    Ok(format!("v{normalized}"))
}

/// Build update info from a CDN `/latest` tag when `latest > current`.
fn build_update_info(
    tag_name: &str,
    current_version: &str,
    include_pre_release: bool,
) -> Option<UpdateInfo> {
    let latest = tag_name.strip_prefix('v').unwrap_or(tag_name);
    let current = current_version.strip_prefix('v').unwrap_or(current_version);

    // Stable channel must not offer a prerelease even if CDN /latest is mis-tagged.
    if (!include_pre_release && is_prerelease_version(latest))
        || compare_versions(latest, current) != std::cmp::Ordering::Greater
    {
        return None;
    }

    let base = resolve_download_base_url(include_pre_release, get_env_var);
    Some(UpdateInfo {
        tag_name: tag_name.to_string(),
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        html_url: format!("{base}/releases/{latest}"),
        release_notes: String::new(),
    })
}

/// Check CDN `/latest` for a newer version. No GitHub API calls.
pub fn check_for_updates(
    current_version: &str,
    include_pre_release: bool,
) -> Result<Option<UpdateInfo>, AishError> {
    let client = build_http_client(CONNECTION_TIMEOUT_SECS)?;
    let url = get_latest_version_url(include_pre_release);

    let resp = client.get(&url).send().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.check_failed", &args)
        })
    })?;

    if !resp.status().is_success() {
        return Err(AishError::Config(format!(
            "Latest version endpoint returned status {}",
            resp.status()
        )));
    }

    let tag_name = normalize_tag(&resp.text().map_err(|e| {
        AishError::Config({
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            t_with_args("cli.update.check_failed", &args)
        })
    })?)?;

    Ok(build_update_info(
        &tag_name,
        current_version,
        include_pre_release,
    ))
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
            args.insert(
                "error".to_string(),
                format!("HTTP {} for {url}", resp.status()),
            );
            t_with_args("cli.update.download_failed", &args)
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
                let mut args = std::collections::HashMap::with_capacity(4);
                args.insert("label".to_string(), label.to_string());
                args.insert("downloaded".to_string(), format!("{:.1}", downloaded_mb));
                args.insert("total".to_string(), format!("{:.1}", total_mb));
                args.insert("pct".to_string(), pct.to_string());
                t_with_args("cli.update.progress_mb", &args)
            });
        } else {
            let downloaded_mb = downloaded as f64 / 1_048_576.0;
            print!("\r\x1b[2K\x1b[1;36m{}\x1b[0m", {
                let mut args = std::collections::HashMap::with_capacity(2);
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

// ---------------------------------------------------------------------------
// Download release (CDN only)
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
    let cdn_url = get_release_download_url(tag_name, &filename);
    println!("\x1b[1;36mDownloading release bundle...\x1b[0m");
    download_with_progress(&cdn_url, &dest_path, &filename)?;
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

fn find_file_named(dir: &Path, name: &str) -> Option<PathBuf> {
    fn search(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = search(&path, name) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
        None
    }
    search(dir, name)
}

fn find_install_sh(dir: &Path) -> Result<PathBuf, AishError> {
    find_file_named(dir, "install.sh")
        .ok_or_else(|| AishError::Config(t("cli.update.install_sh_not_found")))
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

fn build_pip_command(context: &PipChannelContext) -> std::process::Command {
    if let Some(python_executable) = context.python_executable.as_deref() {
        let mut command = std::process::Command::new(python_executable);
        command.args(["-m", "pip"]);
        command
    } else {
        std::process::Command::new("pip")
    }
}

fn parse_pip_check_info(output: &str) -> Result<Option<PipCheckInfo>, AishError> {
    let report: Value = serde_json::from_str(output)
        .map_err(|e| AishError::Config(format!("Failed to parse pip update report: {e}")))?;

    let Some(installs) = report.get("install").and_then(|value| value.as_array()) else {
        return Err(AishError::Config(
            "Invalid pip update report: missing install list".to_string(),
        ));
    };

    let Some(first) = installs.first() else {
        return Ok(None);
    };

    let latest_version = first
        .get("metadata")
        .and_then(|value| value.get("version"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| AishError::Config("Invalid pip update report: missing version".into()))?
        .to_string();
    let html_url = first
        .get("download_info")
        .and_then(|value| value.get("url"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);

    Ok(Some(PipCheckInfo {
        latest_version,
        html_url,
    }))
}

fn check_for_pip_updates(
    context: &PipChannelContext,
    current_version: &str,
    pre_release: bool,
) -> Result<Option<UpdateInfo>, AishError> {
    let mut command = build_pip_command(context);
    command.args(["install", "--upgrade", "--dry-run", "--report", "-", "-qq"]);
    if pre_release {
        command.arg("--pre");
    }
    command.arg(&context.package_name);

    let output = command
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run pip: {e}")))?;
    if !output.status.success() {
        return Err(AishError::Config(format!(
            "pip update check failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let report_stdout = String::from_utf8_lossy(&output.stdout);
    let Some(check_info) = parse_pip_check_info(&report_stdout)? else {
        return Ok(None);
    };

    let current = current_version.strip_prefix('v').unwrap_or(current_version);
    if compare_versions(&check_info.latest_version, current) != std::cmp::Ordering::Greater {
        return Ok(None);
    }

    Ok(Some(UpdateInfo {
        tag_name: format!("v{}", check_info.latest_version),
        current_version: current.to_string(),
        latest_version: check_info.latest_version,
        html_url: check_info.html_url.unwrap_or_default(),
        release_notes: String::new(),
    }))
}

fn run_pip_update(context: &PipChannelContext, pre_release: bool) -> Result<(), AishError> {
    let mut command = build_pip_command(context);
    command.args(["install", "--upgrade"]);
    if pre_release {
        command.arg("--pre");
    }
    command.arg(&context.package_name);

    let output = command
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run pip: {e}")))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("externally-managed-environment") {
        let mut retry = build_pip_command(context);
        retry.args(["install", "--upgrade", "--break-system-packages"]);
        if pre_release {
            retry.arg("--pre");
        }
        retry.arg(&context.package_name);

        let retry_output = retry
            .output()
            .map_err(|e| AishError::Config(format!("Failed to run pip: {e}")))?;
        if retry_output.status.success() {
            return Ok(());
        }
        return Err(AishError::Config(format!(
            "pip update failed: {}",
            String::from_utf8_lossy(&retry_output.stderr).trim()
        )));
    }

    Err(AishError::Config(format!(
        "pip update failed: {}",
        stderr.trim()
    )))
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run_update(check_only: bool, pre_release: bool) {
    let current = env!("CARGO_PKG_VERSION").to_string();

    println!("\x1b[1;36m{}\x1b[0m", t("cli.update.checking"));

    if let Some(InstallChannel::Pip(context)) = current_install_channel() {
        match check_for_pip_updates(&context, &current, pre_release) {
            Ok(Some(info)) => {
                println!("\x1b[1;33m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("current".to_string(), info.current_version.clone());
                    args.insert("latest".to_string(), info.latest_version.clone());
                    t_with_args("cli.update.update_available", &args)
                });
                if !info.html_url.is_empty() {
                    println!("\x1b[2m{}\x1b[0m", info.html_url);
                }

                if check_only {
                    return;
                }

                println!(
                    "\x1b[1;36m{}\x1b[0m",
                    t("cli.update.running_install_script")
                );

                if let Err(error) = run_pip_update(&context, pre_release) {
                    eprintln!("\x1b[31m{}\x1b[0m", {
                        let mut args = std::collections::HashMap::new();
                        args.insert("error".to_string(), error.to_string());
                        t_with_args("cli.update.pip_update_failed", &args)
                    });
                    return;
                }

                println!("\x1b[32m{}\x1b[0m", t("cli.update.pip_update_success"));
            }
            Ok(None) => {
                println!("\x1b[32m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("version".to_string(), current);
                    t_with_args("cli.update.already_latest", &args)
                });
            }
            Err(error) => {
                eprintln!("\x1b[31m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), error.to_string());
                    t_with_args("cli.update.update_check_failed", &args)
                });
            }
        }
        return;
    }

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
                Ok(archive_path) => match install_release(&archive_path) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("\x1b[31m{}\x1b[0m", {
                            let mut args = std::collections::HashMap::new();
                            args.insert("error".to_string(), e.to_string());
                            t_with_args("cli.update.installation_error", &args)
                        });
                    }
                },
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
    fn test_compare_versions_prerelease_newer_than_previous_stable() {
        assert_eq!(
            compare_versions("1.0.0-beta.1", "0.2.0"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_compare_versions_stable_newer_than_prerelease() {
        assert_eq!(
            compare_versions("1.0.0", "1.0.0-beta.1"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_compare_versions_prerelease_identifiers() {
        assert_eq!(
            compare_versions("1.0.0-beta.2", "1.0.0-beta.1"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_build_update_info_beta_to_stable() {
        let info = build_update_info("v0.3.0", "0.3.0-beta.3", false)
            .expect("expected update from beta to stable");

        assert_eq!(info.current_version, "0.3.0-beta.3");
        assert_eq!(info.latest_version, "0.3.0");
        assert_eq!(info.tag_name, "v0.3.0");
        assert_eq!(
            info.html_url,
            "https://cdn.aishell.ai/download/releases/0.3.0"
        );
    }

    #[test]
    fn test_build_update_info_already_latest() {
        assert!(build_update_info("v0.3.8", "0.3.8", false).is_none());
    }

    #[test]
    fn test_build_update_info_rejects_prerelease_for_stable_channel() {
        assert!(build_update_info("v0.3.0-beta.1", "0.2.0", false).is_none());
        assert!(build_update_info("v0.3.0-beta.1", "0.2.0", true).is_some());
    }

    #[test]
    fn test_normalize_tag_from_latest_metadata() {
        assert_eq!(normalize_tag("0.3.0-beta.3\n").unwrap(), "v0.3.0-beta.3");
        assert_eq!(normalize_tag("v0.3.0\n").unwrap(), "v0.3.0");
    }

    #[test]
    fn test_resolve_latest_version_url_defaults() {
        let stable_url = resolve_latest_version_url(false, |_| None);
        let beta_url = resolve_latest_version_url(true, |_| None);

        assert_eq!(stable_url, "https://cdn.aishell.ai/download/latest");
        assert_eq!(beta_url, "https://cdn.aishell.ai/download/beta/latest");
    }

    #[test]
    fn test_resolve_latest_version_url_with_overrides() {
        let stable_url = resolve_latest_version_url(false, |key| match key {
            "AISH_LATEST_URL" => Some("https://cdn.example.com/custom/latest".to_string()),
            _ => None,
        });
        let beta_url = resolve_latest_version_url(true, |key| match key {
            "AISH_BETA_LATEST_URL" => Some("https://cdn.example.com/beta/latest".to_string()),
            _ => None,
        });

        assert_eq!(stable_url, "https://cdn.example.com/custom/latest");
        assert_eq!(beta_url, "https://cdn.example.com/beta/latest");
    }

    #[test]
    fn test_resolve_release_download_url_uses_stable_cdn() {
        let url = resolve_release_download_url("v0.3.0", "aish-0.3.0-linux-amd64.tar.gz", |_| None);

        assert_eq!(
            url,
            "https://cdn.aishell.ai/download/releases/0.3.0/aish-0.3.0-linux-amd64.tar.gz"
        );
    }

    #[test]
    fn test_resolve_release_download_url_uses_beta_cdn() {
        let url = resolve_release_download_url(
            "v0.3.0-beta.3",
            "aish-0.3.0-beta.3-linux-amd64.tar.gz",
            |key| match key {
                "AISH_BETA_DOWNLOAD_BASE_URL" => {
                    Some("https://cdn.example.com/download/beta".to_string())
                }
                _ => None,
            },
        );

        assert_eq!(
            url,
            "https://cdn.example.com/download/beta/releases/0.3.0-beta.3/aish-0.3.0-beta.3-linux-amd64.tar.gz"
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
