//! Task persistence for fleet diagnostics (#588): each `diag run` is
//! recorded with an ID, per-host status and results, so it can be listed,
//! inspected and retried later.
//!
//! Storage: `~/.local/share/aish/diag-tasks/<id>.json` — one file per task,
//! matching the pty-sessions storage pattern (no SQLite migration needed).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Per-host outcome, mirroring the issue's status vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HostOutcome {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "unreachable")]
    Unreachable,
    #[serde(rename = "denied")]
    Denied,
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "evidence-insufficient")]
    EvidenceInsufficient,
}

impl HostOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            HostOutcome::Success => "success",
            HostOutcome::Unreachable => "unreachable",
            HostOutcome::Denied => "denied",
            HostOutcome::Timeout => "timeout",
            HostOutcome::EvidenceInsufficient => "evidence-insufficient",
        }
    }

    /// Whether retrying this host makes sense (success is already done).
    pub fn needs_retry(&self) -> bool {
        !matches!(self, HostOutcome::Success)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagTask {
    /// Short task id, e.g. "dgn-7f3a2c".
    pub id: String,
    pub hosts: Vec<String>,
    pub checks: Vec<String>,
    pub started_at: String,
    /// Per-host outcome (absent host = pending).
    pub outcomes: std::collections::BTreeMap<String, HostOutcome>,
    /// Per-host check results (raw JSON from the remote agent).
    pub results: std::collections::BTreeMap<String, serde_json::Value>,
}

fn tasks_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir().ok_or("cannot resolve data dir")?;
    let dir = base.join("aish").join("diag-tasks");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    Ok(dir)
}

/// Generate a short task id: dgn-<6 hex>.
pub fn new_task_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("dgn-{:06x}", (nanos & 0xffffff) as u32)
}

pub fn save(task: &DiagTask) -> Result<(), String> {
    let dir = tasks_dir()?;
    let path = dir.join(format!("{}.json", task.id));
    let json = serde_json::to_string_pretty(task).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot install {}: {e}", path.display()))
}

pub fn load(id: &str) -> Result<DiagTask, String> {
    let path = tasks_dir()?.join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!("no diag task '{id}'.\nTip: run 'aish remote diag list' to see saved tasks")
    })?;
    serde_json::from_str(&text).map_err(|e| format!("corrupt task file {id}: {e}"))
}

pub fn list() -> Result<Vec<DiagTask>, String> {
    let dir = tasks_dir()?;
    let mut tasks = Vec::new();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|_| unreachable!("dir created"));
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Ok(task) = serde_json::from_str::<DiagTask>(&text) {
                    tasks.push(task);
                }
            }
        }
    }
    // Newest first.
    tasks.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(tasks)
}

/// Hosts in a saved task that need retrying (outcome != success).
pub fn retry_hosts(task: &DiagTask) -> Vec<String> {
    task.hosts
        .iter()
        .filter(|h| {
            task.outcomes
                .get(*h)
                .map(|o| o.needs_retry())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}
