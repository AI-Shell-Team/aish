//! Sandbox worker process for isolated command execution.
//!
//! The worker is invoked by the daemon via:
//!   unshare --mount --propagation private -- <aish-bin> --sandbox-worker
//!
//! Protocol:
//! - Read one JSON object from stdin
//! - Execute sandbox command
//! - Write one JSON object to stdout

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use aish_core::Result;

use crate::sandbox::SandboxExecutor;
use crate::types::SandboxConfig;

/// Worker input request from daemon.
#[derive(Debug, Deserialize)]
struct WorkerRequest {
    command: String,
    cwd: String,
    repo_root: String,
    sim_uid: u32,
    sim_gid: u32,
    timeout_s: f64,
}

/// Worker success output.
#[derive(Debug, Serialize)]
struct WorkerOutput {
    ok: bool,
    result: WorkerResult,
}

/// Worker result payload.
#[derive(Debug, Serialize)]
struct WorkerResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    changes: Vec<FsChangeJson>,
}

/// Filesystem change in JSON format.
#[derive(Debug, Serialize)]
struct FsChangeJson {
    path: String,
    kind: String,
}

/// Worker error output.
#[derive(Debug, Serialize)]
struct WorkerError {
    ok: bool,
    reason: String,
    error: String,
}

/// Run the worker event loop.
///
/// Reads stdin JSON, executes sandbox, writes stdout JSON.
/// Returns 0 always — errors communicated via JSON output.
pub fn run_worker() -> i32 {
    // Read entire stdin
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        let error = WorkerError {
            ok: false,
            reason: "stdin_read_failed".to_string(),
            error: format!("Failed to read stdin: {}", e),
        };
        println!("{}", serde_json::to_string(&error).unwrap_or_default());
        return 0;
    }

    // Parse JSON request
    let request: WorkerRequest = match serde_json::from_str(&input) {
        Ok(req) => req,
        Err(e) => {
            let error = WorkerError {
                ok: false,
                reason: "invalid_json".to_string(),
                error: format!("Failed to parse JSON: {}", e),
            };
            println!("{}", serde_json::to_string(&error).unwrap_or_default());
            return 0;
        }
    };

    // Execute sandbox
    let result = execute_sandbox_worker(&request);

    // Output result as JSON
    let output = match result {
        Ok(res) => serde_json::to_string(&WorkerOutput {
            ok: true,
            result: res,
        }),
        Err(e) => {
            let error = WorkerError {
                ok: false,
                reason: "sandbox_execution_failed".to_string(),
                error: e.to_string(),
            };
            serde_json::to_string(&error)
        }
    };

    match output {
        Ok(json) => println!("{}", json),
        Err(e) => {
            let fallback = WorkerError {
                ok: false,
                reason: "json_serialize_failed".to_string(),
                error: format!("Failed to serialize output: {}", e),
            };
            println!("{}", serde_json::to_string(&fallback).unwrap_or_default());
        }
    }

    // Ensure stdout is flushed before exiting so daemon can read the response.
    let _ = std::io::stdout().flush();

    0
}

/// Execute sandbox command from worker request.
fn execute_sandbox_worker(request: &WorkerRequest) -> Result<WorkerResult> {
    let cwd = Path::new(&request.cwd);
    let repo_root = Path::new(&request.repo_root);

    // Create sandbox config
    let config = SandboxConfig {
        repo_root: PathBuf::from(repo_root),
        ..Default::default()
    };

    // Create executor
    let executor = SandboxExecutor::new(config);

    // Execute command
    let timeout_s = if request.timeout_s > 0.0 {
        Some(request.timeout_s)
    } else {
        None
    };

    let sandbox_result = executor.simulate(
        &request.command,
        cwd,
        Some(request.sim_uid),
        Some(request.sim_gid),
        timeout_s,
    )?;

    // Convert FsChange to JSON format
    let changes: Vec<FsChangeJson> = sandbox_result
        .changes
        .into_iter()
        .map(|c| FsChangeJson {
            path: c.path,
            kind: c.kind,
        })
        .collect();

    Ok(WorkerResult {
        exit_code: sandbox_result.exit_code,
        stdout: sandbox_result.stdout,
        stderr: sandbox_result.stderr,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_request_deserialize() {
        let json = r#"{
            "command": "rm -rf /etc/important",
            "cwd": "/home/user/project",
            "repo_root": "/home/user/project",
            "sim_uid": 1000,
            "sim_gid": 1000,
            "timeout_s": 10.0
        }"#;

        let req: WorkerRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.command, "rm -rf /etc/important");
        assert_eq!(req.cwd, "/home/user/project");
        assert_eq!(req.sim_uid, 1000);
        assert_eq!(req.timeout_s, 10.0);
    }

    #[test]
    fn test_fs_change_json_serialize() {
        let change = FsChangeJson {
            path: "/etc/important".to_string(),
            kind: "deleted".to_string(),
        };

        let json = serde_json::to_string(&change).unwrap();
        assert!(json.contains("\"path\":\"/etc/important\""));
        assert!(json.contains("\"kind\":\"deleted\""));
    }

    #[test]
    fn test_worker_output_serialize() {
        let output = WorkerOutput {
            ok: true,
            result: WorkerResult {
                exit_code: 0,
                stdout: "success".to_string(),
                stderr: String::new(),
                changes: vec![FsChangeJson {
                    path: "/tmp/file.txt".to_string(),
                    kind: "created".to_string(),
                }],
            },
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"exit_code\":0"));
        assert!(json.contains("\"created\""));
    }

    #[test]
    fn test_worker_error_serialize() {
        let error = WorkerError {
            ok: false,
            reason: "test_reason".to_string(),
            error: "test error details".to_string(),
        };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("test_reason"));
        assert!(json.contains("test error details"));
    }
}
