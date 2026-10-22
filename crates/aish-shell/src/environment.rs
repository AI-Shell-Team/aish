use std::collections::HashMap;
use std::process::Command;
use tracing::{debug, warn};

/// Bootstrap environment from bashrc / bash_profile.
///
/// Sources `~/.bash_profile` and `~/.bashrc` via a single short-lived bash
/// subprocess, parses the null-delimited env output, and merges *new*
/// variables into the current process environment (existing values are never
/// overwritten).
///
/// Both rc files are sourced in one `bash -lc` invocation — the per-process
/// startup cost (nvm, conda, pyenv, …) is paid once instead of once per file.
pub fn load_bash_env() -> HashMap<String, String> {
    let mut merged: HashMap<String, String> = HashMap::new();

    // Collect existing rc files in order: bash_profile first, then bashrc.
    let rc_files: Vec<String> = [".bash_profile", ".bashrc"]
        .iter()
        .filter_map(|rc_file| {
            let path = dirs::home_dir()
                .map(|h| h.join(rc_file))
                .unwrap_or_default();
            if path.exists() {
                Some(path.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();

    if rc_files.is_empty() {
        return merged;
    }

    // Source every existing rc file in one bash invocation, then dump env.
    // Each source is separated by `;` (not `&&`) so a failing rc file does
    // not skip the rest — matches the old per-file behaviour where a broken
    // .bash_profile never prevented .bashrc from being sourced.
    let sources: Vec<String> = rc_files
        .iter()
        .map(|p| format!("source '{p}' 2>/dev/null"))
        .collect();
    let script = format!("{}; env -0", sources.join("; "));

    match Command::new("/bin/bash").arg("-lc").arg(&script).output() {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            for entry in raw.split('\0') {
                if let Some((key, value)) = entry.split_once('=') {
                    let key = key.trim();
                    if key.is_empty() {
                        continue;
                    }
                    // Only insert keys not already present in the current
                    // process environment (preserves parent PATH, etc.).
                    if std::env::var(key).is_err() {
                        std::env::set_var(key, value);
                        merged.insert(key.to_string(), value.to_string());
                    }
                }
            }
            debug!(
                target: "aish_env",
                "loaded {} new vars from {} rc file(s)",
                merged.len(),
                rc_files.len()
            );
        }
        Ok(output) => {
            debug!(
                target: "aish_env",
                "bash sourcing {} exited with {:?}",
                rc_files.join(", "),
                output.status.code()
            );
        }
        Err(e) => {
            warn!(
                target: "aish_env",
                "failed to source {}: {}",
                rc_files.join(", "),
                e
            );
        }
    }

    merged
}

/// Set sensible terminal defaults if they are missing.
pub fn ensure_terminal_defaults() {
    if std::env::var("TERM").is_err() {
        std::env::set_var("TERM", "xterm-256color");
    }
    if std::env::var("CLICOLOR").is_err() {
        std::env::set_var("CLICOLOR", "1");
    }
}
