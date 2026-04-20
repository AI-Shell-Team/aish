//! Self-update via GitHub releases.

use std::io::{self, Write};

use aish_core::AishError;

const GITHUB_REPO: &str = "xzx-idc/aish";

#[derive(Debug)]
#[allow(dead_code)]
pub struct UpdateInfo {
    #[allow(dead_code)]
    pub tag_name: String,
    pub current_version: String,
    pub latest_version: String,
    pub html_url: String,
}

pub fn check_for_updates(
    current_version: &str,
    include_pre_release: bool,
) -> Result<Option<UpdateInfo>, AishError> {
    let url = format!("https://api.github.com/repos/{}/releases", GITHUB_REPO);

    let client = reqwest::blocking::Client::builder()
        .user_agent("aish-update-checker")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AishError::Config(format!("HTTP client error: {e}")))?;

    let resp = client
        .get(&url)
        .send()
        .map_err(|e| AishError::Config(format!("Failed to check for updates: {e}")))?;

    if !resp.status().is_success() {
        return Err(AishError::Config(format!(
            "GitHub API returned status {}",
            resp.status()
        )));
    }

    let releases: Vec<serde_json::Value> = resp
        .json()
        .map_err(|e| AishError::Config(format!("Failed to parse releases: {e}")))?;

    let current = normalize_version(current_version);

    for release in releases {
        let is_prerelease = release
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_prerelease && !include_pre_release {
            continue;
        }

        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let latest = normalize_version(tag);
        let html_url = release
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if latest != current {
            return Ok(Some(UpdateInfo {
                tag_name: tag.to_string(),
                current_version: current.to_string(),
                latest_version: tag.to_string(),
                html_url,
            }));
        }

        break;
    }

    Ok(None)
}

fn normalize_version(v: &str) -> String {
    v.strip_prefix('v').unwrap_or(v).to_string()
}

pub fn run_update(check_only: bool, pre_release: bool) {
    let current = env!("CARGO_PKG_VERSION").to_string();

    println!("\x1b[1;36mChecking for updates...\x1b[0m");

    match check_for_updates(&current, pre_release) {
        Ok(Some(info)) => {
            println!(
                "\x1b[1;33mUpdate available: {} → {}\x1b[0m",
                info.current_version, info.latest_version
            );
            println!("\x1b[2mCurrent: {}\x1b[0m", info.current_version);
            println!("\x1b[2mLatest:  {}\x1b[0m", info.latest_version);
            println!("\x1b[2m{}\x1b[0m", info.html_url);

            if check_only {
                return;
            }

            print!("\n\x1b[33mDownload and install? [y/N] \x1b[0m");
            io::stdout().flush().unwrap();
            let mut answer = String::new();
            io::stdin().read_line(&mut answer).unwrap();
            if answer.trim().to_lowercase() != "y" {
                println!("Update cancelled.");
                return;
            }

            println!("\x1b[33mAutomatic update is not yet implemented in the Rust version.\x1b[0m");
            println!("Download from: {}", info.html_url);
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
