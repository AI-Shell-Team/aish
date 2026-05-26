use std::collections::HashMap;
use std::process::Command;
use tracing::{debug, warn};

/// Bootstrap environment from bashrc / bash_profile.
///
/// Sources `~/.bash_profile` and `~/.bashrc` via a short-lived bash subprocess,
/// parses the null-delimited env output, and merges *new* variables into the
/// current process environment (existing values are never overwritten).
pub fn load_bash_env() -> HashMap<String, String> {
    let mut merged: HashMap<String, String> = HashMap::new();

    for rc_file in &[".bash_profile", ".bashrc"] {
        let path = dirs::home_dir()
            .map(|h| h.join(rc_file))
            .unwrap_or_default();

        if !path.exists() {
            continue;
        }

        let path_str = path.to_string_lossy().to_string();
        // bash -lc ensures login-shell profile is loaded.
        // `env -0` prints null-delimited KEY=VALUE pairs.
        let script = format!("source '{}' 2>/dev/null && env -0", path_str);

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
                debug!(target: "aish_env", "loaded {} new vars from {}", merged.len(), rc_file);
            }
            Ok(output) => {
                debug!(
                    target: "aish_env",
                    "bash sourcing {} exited with {:?}",
                    rc_file,
                    output.status.code()
                );
            }
            Err(e) => {
                warn!(target: "aish_env", "failed to source {}: {}", rc_file, e);
            }
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
