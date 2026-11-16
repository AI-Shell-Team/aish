//! Remote host aliases: a local config mapping short names to ssh targets
//! and remote aish binary paths, so subcommands accept `--host db-0`
//! instead of repeating `root@10.10.17.130`.
//!
//! Storage: `<config dir>/aish/remote-hosts.json` (JSON array). The file is
//! created on first `host add` and never touched by single-machine flows.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteHostEntry {
    /// Short, user-chosen alias (also the lookup key).
    pub name: String,
    /// ssh target form (user@host).
    pub target: String,
    /// Optional remote aish binary path (overrides probing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_bin: Option<String>,
    /// Optional per-host defaults for diag (comma-separated check list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_checks: Option<String>,
    /// Optional group tags (e.g. ["prod", "web"]). A `--host group:prod`
    /// selector expands to every host carrying that tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Optional ssh password (used with sshpass). Empty = passwordless ssh
    /// (keys or agent). The whole file is 0600; storing passwords here is a
    /// deliberate tradeoff documented in the architecture guide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

fn hosts_file() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or("cannot resolve config dir")?;
    let dir = base.join("aish");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir.join("remote-hosts.json"))
}

/// Write `content` to `path` atomically with owner-only permissions: the
/// temp file is created 0600 *before* content lands in it, so the secret is
/// never observable under the ambient umask, and rename publishes it
/// atomically. Used for password/identity/trusted-operator state files.
pub fn write_secret_file(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("tmp0600");
    // OpenOptions sets the mode at create time — no window where the file
    // exists under the ambient umask.
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot install {}: {e}", path.display())
    })
}

/// Password from the environment wins over stored entries (never logged).
pub fn env_password() -> Option<String> {
    std::env::var("AISH_REMOTE_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
}

fn load_all() -> Result<Vec<RemoteHostEntry>, String> {
    let path = hosts_file()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("corrupt {}: {e}", path.display()))
}

fn save_all(entries: &[RemoteHostEntry]) -> Result<(), String> {
    let path = hosts_file()?;
    let json = serde_json::to_string_pretty(entries).unwrap_or_default();
    // The file may hold ssh passwords: atomic 0600 publish via tmp file.
    write_secret_file(&path, &json)
}

/// Resolve a `--host` value: an exact alias name expands to its entry;
/// anything containing `@` or `.` is treated as a literal ssh target.
/// Returns (ssh_target, remote_bin_override, password).
pub fn resolve(host: &str) -> Result<(String, Option<String>, Option<String>), String> {
    // Fast path: looks like a literal ssh target (user@host, IP, host:port).
    // The password can still come from the environment.
    if host.contains('@') || host.parse::<std::net::IpAddr>().is_ok() || host.contains(':') {
        return Ok((host.to_string(), None, env_password()));
    }
    if let Some(entry) = lookup(host)? {
        return Ok((
            entry.target,
            entry.remote_bin,
            entry.password.or_else(env_password),
        ));
    }
    // A dotted name that is not a configured alias is still accepted as a
    // literal hostname (e.g. 'prod.corp.lan').
    if host.contains('.') {
        return Ok((host.to_string(), None, env_password()));
    }
    Err(format!(
        "unknown remote host alias '{host}'.\nTip: add it with 'aish remote host add {host} user@real-host', or pass a full ssh target like user@host"
    ))
}

fn lookup(name: &str) -> Result<Option<RemoteHostEntry>, String> {
    Ok(load_all()?.into_iter().find(|e| e.name == name))
}

pub fn list() -> Result<Vec<RemoteHostEntry>, String> {
    load_all()
}

pub fn add(
    name: &str,
    target: &str,
    remote_bin: Option<&str>,
    groups: &[String],
    password: Option<&str>,
) -> Result<(), String> {
    let mut entries = load_all()?;
    if entries.iter().any(|e| e.name == name) {
        return Err(format!(
            "alias '{name}' already exists. Use 'aish remote host remove {name}' first"
        ));
    }
    entries.push(RemoteHostEntry {
        name: name.to_string(),
        target: target.to_string(),
        remote_bin: remote_bin.map(String::from),
        default_checks: None,
        groups: groups.to_vec(),
        password: password.map(String::from),
    });
    save_all(&entries)
}

pub fn remove(name: &str) -> Result<bool, String> {
    let mut entries = load_all()?;
    let before = entries.len();
    entries.retain(|e| e.name != name);
    let removed = entries.len() != before;
    if removed {
        save_all(&entries)?;
    }
    Ok(removed)
}

/// Is `host` a group selector (`group:<tag>`)? Returns the tag.
pub fn group_tag(host: &str) -> Option<&str> {
    host.strip_prefix("group:")
}

/// Expand a group selector to the matching host entries (alias + remote_bin).
/// Unknown tag → error listing the known tags.
pub fn expand_group(tag: &str) -> Result<Vec<(String, Option<String>, Option<String>)>, String> {
    let entries = load_all()?;
    let matched: Vec<&RemoteHostEntry> = entries
        .iter()
        .filter(|e| e.groups.iter().any(|g| g == tag))
        .collect();
    if matched.is_empty() {
        let known: Vec<&String> = entries.iter().flat_map(|e| e.groups.iter()).collect();
        return Err(if known.is_empty() {
            format!("no hosts carry group '{tag}' (no groups configured at all)")
        } else {
            format!(
                "no hosts carry group '{tag}'. Known groups: {}",
                known
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    Ok(matched
        .into_iter()
        .map(|e| {
            (
                e.target.clone(),
                e.remote_bin.clone(),
                e.password.clone().or_else(env_password),
            )
        })
        .collect())
}
