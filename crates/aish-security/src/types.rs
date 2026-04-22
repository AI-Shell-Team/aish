use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use aish_core::RiskLevel;

// ---- Policy types (keep existing) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    pub pattern: String,
    pub risk: RiskLevel,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub operations: Option<Vec<String>>,
    #[serde(default)]
    pub command_list: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub confirm_message: Option<String>,
    #[serde(default)]
    pub suggestion: Option<String>,
}

impl Default for PolicyRule {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            risk: RiskLevel::Low,
            description: None,
            operations: None,
            command_list: None,
            exclude: None,
            rule_id: None,
            name: None,
            reason: None,
            confirm_message: None,
            suggestion: None,
        }
    }
}

// ---- Sandbox types (unified, Python-IPC compatible) ----

/// Single filesystem change record. IPC-compatible with Python.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsChange {
    pub path: String,
    /// "created" | "modified" | "deleted"
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<HashMap<String, String>>,
}

/// Sandbox execution result. IPC-compatible with Python.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub changes: Vec<FsChange>,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
    #[serde(default)]
    pub changes_truncated: bool,
}

/// Sandbox execution result for main-process callers.
#[derive(Debug, Clone)]
pub struct SandboxSecurityResult {
    pub command: String,
    pub cwd: PathBuf,
    pub sandbox: SandboxResult,
}

/// AI risk assessment result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRiskAssessment {
    pub level: RiskLevel,
    pub reasons: Vec<String>,
    pub changes: Vec<FsChange>,
}

// ---- IPC types (newline-delimited JSON, Python-compatible) ----

/// IPC request sent to sandbox daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub repo_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<f64>,
}

/// IPC response from sandbox daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<IpcResult>,
}

/// Result payload inside IPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
    #[serde(default)]
    pub changes_truncated: bool,
    pub changes: Vec<FsChange>,
}

/// Sandbox executor configuration.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub repo_root: PathBuf,
    pub enable_overlay: bool,
    pub readonly_binds: Option<Vec<(PathBuf, PathBuf)>>,
    pub readwrite_binds: Option<Vec<(PathBuf, PathBuf)>>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            enable_overlay: true,
            readonly_binds: None,
            readwrite_binds: None,
        }
    }
}
