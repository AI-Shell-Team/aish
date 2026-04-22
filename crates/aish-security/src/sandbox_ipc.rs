//! Python-compatible IPC client for sandbox daemon communication.
//!
//! This module provides a synchronous IPC client that communicates with the
//! sandbox daemon over Unix sockets using newline-delimited JSON protocol.
//!
//! Protocol:
//! - Client → Daemon: {"id":"<uuid>","command":"...","cwd":"...","repo_root":"...","client_pid":N,"timeout_s":T}\n
//! - Daemon → Client: {"id":"<uuid>","ok":true,"result":{"exit_code":0,"stdout":"...","stderr":"...","changes":[...]}}\n

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aish_core::{AishError, Result};
use tracing::debug;

use crate::types::{IpcRequest, IpcResponse, SandboxResult, SandboxSecurityResult};

/// Default socket path for the sandbox daemon.
pub const DEFAULT_SOCKET_PATH: &str = "/run/aish/sandbox.sock";

/// Maximum response size (8MB) to prevent denial-of-service.
const MAX_RESPONSE_SIZE: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// SandboxIpcClient
// ---------------------------------------------------------------------------

/// IPC client for communicating with the sandbox daemon.
///
/// This client uses synchronous I/O (std::os::unix::net::UnixStream) to match
/// Python's socket behavior and the daemon's newline-delimited JSON protocol.
pub struct SandboxIpcClient {
    socket_path: PathBuf,
    timeout_s: f64,
}

impl SandboxIpcClient {
    /// Create a new IPC client with the given socket path and timeout.
    ///
    /// # Arguments
    /// * `socket_path` - Path to the Unix socket of the sandbox daemon
    /// * `timeout_s` - Timeout in seconds for IPC communication
    pub fn new(socket_path: &Path, timeout_s: f64) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
            timeout_s,
        }
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Send a simulate request to the daemon and return the result.
    ///
    /// # Arguments
    /// * `command` - Command string to execute in the sandbox
    /// * `cwd` - Current working directory for the command
    /// * `repo_root` - Repository root directory for sandbox isolation
    ///
    /// # Returns
    /// `SandboxResult` containing exit code, stdout, stderr, and file changes
    ///
    /// # Errors
    /// Returns an error if:
    /// - Socket timeout occurs
    /// - Socket file not found or connection refused
    /// - Protocol error (empty response, id mismatch, invalid JSON)
    /// - Daemon returns an error response
    pub fn simulate(&self, command: &str, cwd: &Path, repo_root: &Path) -> Result<SandboxResult> {
        // Step 1: Generate UUID request_id
        let request_id = uuid::Uuid::new_v4().to_string();

        // Step 2: Build IpcRequest
        let request = IpcRequest {
            id: request_id.clone(),
            command: command.to_string(),
            cwd: cwd
                .to_str()
                .ok_or_else(|| AishError::Security("Invalid cwd path".to_string()))?
                .to_string(),
            repo_root: repo_root
                .to_str()
                .ok_or_else(|| AishError::Security("Invalid repo_root path".to_string()))?
                .to_string(),
            client_pid: Some(std::process::id()),
            timeout_s: Some(self.timeout_s),
        };

        // Step 3: Serialize to JSON + "\n"
        let json_payload = serde_json::to_string(&request)
            .map_err(|e| AishError::Security(format!("Failed to serialize request: {}", e)))?;
        let payload = format!("{}\n", json_payload);

        // Step 4: Connect Unix socket
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                AishError::Security(format!("sandbox_ipc_unavailable: {}", e))
            }
            _ => AishError::Security(format!("Failed to connect to socket: {}", e)),
        })?;

        // Step 5: Set socket timeout (add 5s buffer for daemon processing)
        let timeout_dur = Duration::from_secs_f64(self.timeout_s + 5.0);
        stream
            .set_read_timeout(Some(timeout_dur))
            .map_err(|e| AishError::Security(format!("Failed to set read timeout: {}", e)))?;
        stream
            .set_write_timeout(Some(timeout_dur))
            .map_err(|e| AishError::Security(format!("Failed to set write timeout: {}", e)))?;

        debug!(
            "Sending sandbox IPC request: id={}, command={}",
            request_id, command
        );

        // Step 6: Send payload
        stream
            .write_all(payload.as_bytes())
            .map_err(|e| AishError::Security(format!("Failed to send request: {}", e)))?;
        stream
            .flush()
            .map_err(|e| AishError::Security(format!("Failed to flush request: {}", e)))?;

        // Step 7: Read until "\n" (limit 8MB via take() to prevent OOM)
        let mut reader = BufReader::new(&stream).take(MAX_RESPONSE_SIZE as u64);
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    AishError::Security("sandbox_ipc_timeout".to_string())
                }
                _ => AishError::Security(format!("Failed to read response: {}", e)),
            })?;

        if response_line.is_empty() {
            return Err(AishError::Security(
                "sandbox_ipc_protocol_error: empty_response".to_string(),
            ));
        }

        debug!("Received sandbox IPC response: id={}", request_id);

        // Step 8: Parse JSON → IpcResponse
        let response: IpcResponse = serde_json::from_str(&response_line).map_err(|e| {
            AishError::Security(format!("sandbox_ipc_protocol_error: invalid_json: {}", e))
        })?;

        // Step 9: Verify response.id == request_id
        if response.id != request_id {
            return Err(AishError::Security(format!(
                "sandbox_ipc_protocol_error: id_mismatch (expected={}, got={})",
                request_id, response.id
            )));
        }

        // Step 10: If !response.ok → Err
        if !response.ok {
            let reason = response.reason.unwrap_or_else(|| "unknown".to_string());
            let error = response.error.unwrap_or_else(|| "no details".to_string());
            return Err(AishError::Security(format!("{}: {}", reason, error)));
        }

        // Step 11: Map response.result → SandboxResult
        let result = response.result.ok_or_else(|| {
            AishError::Security("sandbox_ipc_protocol_error: missing_result".to_string())
        })?;

        // Convert IpcResult to SandboxResult
        let sandbox_result = SandboxResult {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
            changes: result.changes,
            stdout_truncated: result.stdout_truncated,
            stderr_truncated: result.stderr_truncated,
            changes_truncated: result.changes_truncated,
        };

        Ok(sandbox_result)
    }
}

// ---------------------------------------------------------------------------
// SandboxSecurityIpc
// ---------------------------------------------------------------------------

/// Security manager integration for sandbox IPC.
///
/// This wrapper provides a convenient interface for the security manager
/// to use sandbox IPC for command simulation.
pub struct SandboxSecurityIpc {
    repo_root: PathBuf,
    enabled: bool,
    client: SandboxIpcClient,
}

impl SandboxSecurityIpc {
    /// Create a new SandboxSecurityIpc instance.
    ///
    /// # Arguments
    /// * `repo_root` - Repository root directory for sandbox isolation
    /// * `enabled` - Whether sandbox IPC is enabled
    /// * `socket_path` - Path to the Unix socket of the sandbox daemon
    /// * `timeout_s` - Timeout in seconds for IPC communication
    pub fn new(repo_root: &Path, enabled: bool, socket_path: &Path, timeout_s: f64) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
            enabled,
            client: SandboxIpcClient::new(socket_path, timeout_s),
        }
    }

    /// Check if sandbox IPC is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Set the enabled state of sandbox IPC.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Run a command in the sandbox and return the result.
    ///
    /// Returns `None` if sandbox IPC is disabled.
    /// Returns `Some(SandboxSecurityResult)` if simulation succeeded.
    /// Returns `None` if simulation failed (graceful degradation).
    pub fn run(&self, command: &str, cwd: Option<&Path>) -> Option<SandboxSecurityResult> {
        if !self.enabled {
            return None;
        }

        let work_cwd = cwd.unwrap_or(&self.repo_root);

        // Call simulate
        let sandbox_result = match self.client.simulate(command, work_cwd, &self.repo_root) {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("Sandbox IPC simulation failed: {}", e);
                return None; // Graceful degradation
            }
        };

        Some(SandboxSecurityResult {
            command: command.to_string(),
            cwd: work_cwd.to_path_buf(),
            sandbox: sandbox_result,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FsChange;

    #[test]
    fn test_sandbox_ipc_client_new() {
        let client = SandboxIpcClient::new(Path::new("/tmp/test.sock"), 30.0);
        assert_eq!(client.socket_path, PathBuf::from("/tmp/test.sock"));
        assert_eq!(client.timeout_s, 30.0);
    }

    #[test]
    fn test_sandbox_security_ipc_new() {
        let ipc = SandboxSecurityIpc::new(
            Path::new("/home/user/project"),
            true,
            Path::new("/tmp/test.sock"),
            30.0,
        );
        assert!(ipc.enabled());
        assert_eq!(ipc.repo_root, PathBuf::from("/home/user/project"));
    }

    #[test]
    fn test_sandbox_security_ipc_enabled_flag() {
        let mut ipc = SandboxSecurityIpc::new(
            Path::new("/home/user/project"),
            true,
            Path::new("/tmp/test.sock"),
            30.0,
        );
        assert!(ipc.enabled());

        ipc.set_enabled(false);
        assert!(!ipc.enabled());

        ipc.set_enabled(true);
        assert!(ipc.enabled());
    }

    #[test]
    fn test_ipc_request_serialize() {
        let request = IpcRequest {
            id: "test-id".to_string(),
            command: "ls -la".to_string(),
            cwd: "/home/user".to_string(),
            repo_root: "/home/user/project".to_string(),
            client_pid: Some(12345),
            timeout_s: Some(30.0),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"id\":\"test-id\""));
        assert!(json.contains("\"command\":\"ls -la\""));
        assert!(json.contains("\"client_pid\":12345"));
    }

    #[test]
    fn test_ipc_response_deserialize_ok() {
        let json = r#"{
            "id": "test-id",
            "ok": true,
            "result": {
                "exit_code": 0,
                "stdout": "file.txt\n",
                "stderr": "",
                "stdout_truncated": false,
                "stderr_truncated": false,
                "changes_truncated": false,
                "changes": [
                    {"path": "/tmp/file.txt", "kind": "created"}
                ]
            }
        }"#;

        let response: IpcResponse = serde_json::from_str(json).unwrap();
        assert!(response.ok);
        assert_eq!(response.id, "test-id");
        assert!(response.result.is_some());
    }

    #[test]
    fn test_ipc_response_deserialize_error() {
        let json = r#"{
            "id": "test-id",
            "ok": false,
            "reason": "test_reason",
            "error": "test error details"
        }"#;

        let response: IpcResponse = serde_json::from_str(json).unwrap();
        assert!(!response.ok);
        assert_eq!(response.reason, Some("test_reason".to_string()));
        assert_eq!(response.error, Some("test error details".to_string()));
    }

    #[test]
    fn test_fs_change_serialize() {
        let change = FsChange {
            path: "/etc/passwd".to_string(),
            kind: "modified".to_string(),
            detail: None,
        };

        let json = serde_json::to_string(&change).unwrap();
        assert!(json.contains("\"path\":\"/etc/passwd\""));
        assert!(json.contains("\"kind\":\"modified\""));
    }
}
