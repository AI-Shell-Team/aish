use std::collections::HashMap;
use std::process::Command;
use tracing::{debug, warn};

/// Marker emitted between rc-file segments of the combined env dump.
///
/// Because all rc files are sourced in one bash process, `env -0` alone
/// cannot tell which file a variable came from. A sentinel printed after
/// each `source` splits the output into per-file segments so the first
/// definition wins — the precedence the old per-file invocations had.
const SEGMENT_SENTINEL: &str = "__AISH_ENV_SEGMENT__";

/// Bootstrap environment from bashrc / bash_profile.
///
/// Sources `~/.bash_profile` and `~/.bashrc` via a single short-lived bash
/// subprocess, parses the null-delimited env output, and merges *new*
/// variables into the current process environment (existing values are never
/// overwritten).
///
/// Both rc files are sourced in one `bash -lc` invocation — the per-process
/// startup cost (nvm, conda, pyenv, …) is paid once instead of once per file.
/// Within that one process, a variable exported by an earlier rc file keeps
/// its value even when a later rc file re-exports it: each file's segment of
/// the dump is applied in order and the first definition of a key wins,
/// matching the old per-file merge.
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

    // Source every existing rc file in one bash invocation, dumping the env
    // after each one. Each source is separated by `;` (not `&&`) so a failing
    // rc file does not skip the rest — matches the old per-file behaviour
    // where a broken .bash_profile never prevented .bashrc from being sourced.
    let segments: Vec<String> = rc_files
        .iter()
        .map(|p| format!("source '{p}' 2>/dev/null; printf '\\0{SEGMENT_SENTINEL}\\0'; env -0"))
        .collect();
    let script = segments.join("; ");

    match Command::new("/bin/bash").arg("-lc").arg(&script).output() {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            // Segments arrive in rc-file order; the first definition of a
            // key wins, so an earlier file's export survives a later
            // re-export (the old per-file precedence).
            for entry in raw.split('\0') {
                if entry == SEGMENT_SENTINEL {
                    continue;
                }
                if let Some((key, value)) = entry.split_once('=') {
                    let key = key.trim();
                    if key.is_empty() {
                        continue;
                    }
                    // Only insert keys not already present in the current
                    // process environment or in `merged` (preserves parent
                    // PATH and the first rc file's exports).
                    if std::env::var(key).is_err() && !merged.contains_key(key) {
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
