use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::debug;

// ---------------------------------------------------------------------------
// StateChanges
// ---------------------------------------------------------------------------

/// Detected changes after a command execution.
#[derive(Debug, Clone, Default)]
pub struct StateChanges {
    pub cwd: Option<String>,
    pub new_vars: HashMap<String, String>,
    pub changed_vars: HashMap<String, (String, String)>, // (old, new)
    pub unset_vars: Vec<String>,
}

impl StateChanges {
    pub fn is_empty(&self) -> bool {
        self.cwd.is_none()
            && self.new_vars.is_empty()
            && self.changed_vars.is_empty()
            && self.unset_vars.is_empty()
    }
}

// ---------------------------------------------------------------------------
// State file helpers
// ---------------------------------------------------------------------------

/// Create a temporary file used for capturing shell state after command
/// execution.  The file path is deterministic under `/tmp/aish-state/`.
pub fn create_state_file() -> PathBuf {
    let dir = std::env::temp_dir().join("aish-state");
    let _ = fs::create_dir_all(&dir);
    let id = uuid::Uuid::new_v4();
    dir.join(format!("state-{id}"))
}

/// Build the state payload string that we expect to find in the state file.
/// Format: `CWD:<path>` on the first line, then `KEY=VALUE` for each tracked
/// env var.
pub fn get_current_state(env_vars: &HashMap<String, String>) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut lines = vec![format!("CWD:{cwd}")];
    for (k, v) in env_vars {
        lines.push(format!("{k}={v}"));
    }
    lines.join("\n")
}

/// Wrap a user command so that the shell dumps its final working directory
/// into `state_file`.  Both success and failure paths are covered.
///
/// The wrapper also dumps exported variables that are commonly tracked.
pub fn wrap_command_with_state_capture(cmd: &str, state_file: &Path) -> String {
    let path = state_file.display();
    format!(
        "{{ {cmd}; }} && \
         printf 'CWD:%%s\\n' \"$(pwd)\" > {path} 2>/dev/null || \
         printf 'CWD:%%s\\n' \"$(pwd)\" > {path} 2>/dev/null"
    )
}

/// Parse the state file produced by the wrapper.
///
/// Returns a `HashMap` where key `"CWD"` holds the working directory and
/// all other entries hold `KEY=VALUE` pairs.
pub fn parse_state_file(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            debug!(path = %path.display(), error = %e, "failed to read state file");
            return map;
        }
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(cwd) = line.strip_prefix("CWD:") {
            map.insert("CWD".to_string(), cwd.to_string());
        } else if let Some(eq) = line.find('=') {
            let key = &line[..eq];
            let value = &line[eq + 1..];
            map.insert(key.to_string(), value.to_string());
        }
    }

    // Clean up the temp file.
    let _ = fs::remove_file(path);

    map
}

/// Compare two state snapshots and compute the diff.
pub fn detect_changes(
    old: &HashMap<String, String>,
    new: &HashMap<String, String>,
) -> StateChanges {
    let mut changes = StateChanges::default();

    // CWD change.
    if old.get("CWD") != new.get("CWD") {
        changes.cwd = new.get("CWD").cloned();
    }

    // Look for new or changed variables (skip the pseudo-key "CWD").
    for (k, v) in new {
        if k == "CWD" {
            continue;
        }
        match old.get(k) {
            None => {
                changes.new_vars.insert(k.clone(), v.clone());
            }
            Some(old_v) if old_v != v => {
                changes
                    .changed_vars
                    .insert(k.clone(), (old_v.clone(), v.clone()));
            }
            _ => {}
        }
    }

    // Look for unset variables.
    for k in old.keys() {
        if k == "CWD" {
            continue;
        }
        if !new.contains_key(k) {
            changes.unset_vars.push(k.clone());
        }
    }

    changes
}

/// Apply detected changes via a callback.
///
/// The callback receives `(key, Some(value))` for set/export operations and
/// `(key, None)` for unset operations.
pub fn apply_changes(changes: &StateChanges, mut env_manager: impl FnMut(&str, Option<&str>)) {
    if let Some(ref cwd) = changes.cwd {
        env_manager("CWD", Some(cwd));
    }
    for (k, v) in &changes.new_vars {
        env_manager(k, Some(v));
    }
    for (k, (_, new_v)) in &changes.changed_vars {
        env_manager(k, Some(new_v));
    }
    for k in &changes.unset_vars {
        env_manager(k, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_changes_basic() {
        let mut old = HashMap::new();
        old.insert("CWD".to_string(), "/home/user".to_string());
        old.insert("PATH".to_string(), "/usr/bin".to_string());
        old.insert("FOO".to_string(), "bar".to_string());

        let mut new = HashMap::new();
        new.insert("CWD".to_string(), "/tmp".to_string());
        new.insert("PATH".to_string(), "/usr/bin".to_string());
        new.insert("BAZ".to_string(), "qux".to_string());

        let changes = detect_changes(&old, &new);

        assert_eq!(changes.cwd.as_deref(), Some("/tmp"));
        assert!(changes.changed_vars.is_empty());
        assert_eq!(changes.new_vars.get("BAZ").map(|s| s.as_str()), Some("qux"));
        assert_eq!(changes.unset_vars, vec!["FOO"]);
    }

    #[test]
    fn test_parse_state_file() {
        let dir = std::env::temp_dir().join("aish-state-test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test-state");
        fs::write(&path, "CWD:/tmp\nFOO=bar\nBAZ=123\n").unwrap();

        let map = parse_state_file(&path);
        assert_eq!(map.get("CWD").map(|s| s.as_str()), Some("/tmp"));
        assert_eq!(map.get("FOO").map(|s| s.as_str()), Some("bar"));
        assert_eq!(map.get("BAZ").map(|s| s.as_str()), Some("123"));

        // File should be cleaned up.
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
