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

/// Parse a sentinel-separated env dump, applying the first definition of
/// each key and skipping keys already present in the process environment.
///
/// Segments arrive in rc-file order; within one process a variable exported
/// by an earlier rc file keeps its value even when a later rc file
/// re-exports it — the precedence the old per-file merge had.
fn parse_env_dump(raw: &str, merged: &mut HashMap<String, String>) {
    for entry in raw.split('\0') {
        if entry == SEGMENT_SENTINEL {
            continue;
        }
        if let Some((key, value)) = entry.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            // Only insert keys not already present in the current process
            // environment or in `merged` (preserves parent PATH and the
            // first rc file's exports).
            if std::env::var(key).is_err() && !merged.contains_key(key) {
                std::env::set_var(key, value);
                merged.insert(key.to_string(), value.to_string());
            }
        }
    }
}

/// Bootstrap environment from bashrc / bash_profile.
///
/// Sources `~/.bash_profile` and `~/.bashrc` via a single short-lived bash
/// subprocess, parses the null-delimited env output, and merges *new*
/// variables into the current process environment (existing values are never
/// overwritten).
///
/// Both rc files are handled in one `bash -lc` invocation — the per-process
/// startup cost (nvm, conda, pyenv, …) is paid once instead of once per file.
/// The login shell has already sourced `~/.bash_profile` at startup, so the
/// script only sources `~/.bashrc` itself; each file's state is captured by
/// a sentinel-separated `env -0` dump, and the first definition of a key
/// wins, matching the old per-file merge. Completed dumps are parsed even
/// when the shell later exits nonzero (e.g. `exit` inside `.bashrc`), so a
/// broken later file does not discard what earlier files exported.
pub fn load_bash_env() -> HashMap<String, String> {
    let mut merged: HashMap<String, String> = HashMap::new();

    let home = dirs::home_dir().unwrap_or_default();
    let profile_path = home.join(".bash_profile");
    let rc_path = home.join(".bashrc");

    if !profile_path.exists() && !rc_path.exists() {
        return merged;
    }

    // One `bash -lc` invocation. `-l` makes bash read `.bash_profile` (if
    // present) exactly once at shell startup; the first dump below captures
    // that state without sourcing it a second time. Then `.bashrc` is
    // sourced — mirroring an interactive login shell's load order — and a
    // second dump captures the rc additions. Each `source` failure must not
    // skip the rest, so commands are separated by `;` rather than `&&`.
    let script = format!(
        "printf '\\0{SEGMENT_SENTINEL}\\0'; env -0; \
         if [ -f '{rc}' ]; then source '{rc}' 2>/dev/null; fi; \
         printf '\\0{SEGMENT_SENTINEL}\\0'; env -0",
        rc = rc_path.to_string_lossy()
    );

    match Command::new("/bin/bash").arg("-lc").arg(&script).output() {
        Ok(output) => {
            let raw = String::from_utf8_lossy(&output.stdout);
            // Completed dumps are parsed even on a nonzero exit: a later rc
            // file that runs `exit` must not discard what earlier segments
            // already printed. The old per-file invocations kept a failed
            // file's predecessors the same way.
            if !raw.is_empty() {
                parse_env_dump(&raw, &mut merged);
                debug!(
                    target: "aish_env",
                    "loaded {} new vars from bash rc files (exit status: {:?})",
                    merged.len(),
                    output.status.code()
                );
            }
            if !output.status.success() {
                debug!(
                    target: "aish_env",
                    "bash sourcing rc files exited with {:?}",
                    output.status.code()
                );
            }
        }
        Err(e) => {
            warn!(target: "aish_env", "failed to source bash rc files: {}", e);
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
