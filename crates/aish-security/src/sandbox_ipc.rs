//! IPC client for communicating with bubblewrap sandbox daemon over Unix sockets.
//!
//! The sandbox daemon runs bubblewrap with isolation and monitors file system changes.
//! This module provides a JSON-based protocol for sending commands and receiving results.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::debug;

/// Request sent to the bubblewrap sandbox daemon via Unix socket.
#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxRequest {
    pub command: String,
    pub timeout: u64,
    pub readonly: bool,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Response received from the sandbox daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub changes: Vec<FileChange>,
    pub blocked: bool,
}

/// File system change reported by the sandbox daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub operation: String,
}

/// IPC client for communicating with bubblewrap sandbox daemon.
pub struct SandboxIpc {
    socket_path: PathBuf,
    timeout: std::time::Duration,
}

impl SandboxIpc {
    /// Create a new sandbox IPC client with the given socket path.
    ///
    /// # Arguments
    /// * `socket_path` - Path to the Unix socket of the sandbox daemon
    pub fn new<P: AsRef<Path>>(socket_path: P) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Set the timeout for IPC communication.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Check if the sandbox IPC is available (socket exists and is accessible).
    ///
    /// This is a synchronous check for quick availability testing.
    pub fn is_available(&self) -> bool {
        self.socket_path.exists()
    }

    /// Execute a command through the sandbox daemon.
    ///
    /// # Arguments
    /// * `command` - Command string to execute in the sandbox
    /// * `readonly` - Whether to run in read-only mode (no filesystem writes)
    ///
    /// # Returns
    /// `SandboxResponse` containing exit code, stdout, stderr, and file changes
    ///
    /// # Errors
    /// Returns an error if the socket is unavailable, connection fails, or
    /// communication times out.
    pub async fn execute(&self, command: &str, readonly: bool) -> Result<SandboxResponse, String> {
        // Prepare request
        let request = SandboxRequest {
            command: command.to_string(),
            timeout: self.timeout.as_secs(),
            readonly,
            env: HashMap::new(),
        };

        // Serialize to JSON
        let json_payload = serde_json::to_string(&request)
            .map_err(|e| format!("failed to serialize request: {}", e))?;

        // Connect to socket with timeout
        let socket_path = self.socket_path.clone();
        let conn = tokio::time::timeout(self.timeout, UnixStream::connect(&socket_path))
            .await
            .map_err(|_| {
                format!(
                    "timeout connecting to sandbox socket after {:?}",
                    self.timeout
                )
            })?
            .map_err(|e| format!("failed to connect to sandbox socket: {}", e))?;

        debug!("connected to sandbox daemon at {}", socket_path.display());

        // Send length-prefixed JSON (4 bytes big-endian length + payload)
        let len = json_payload.len() as u32;
        let mut buf = Vec::with_capacity(4 + json_payload.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(json_payload.as_bytes());

        let mut conn = conn;
        conn.write_all(&buf)
            .await
            .map_err(|e| format!("failed to send request: {}", e))?;

        debug!("sent sandbox request: {} bytes", buf.len());

        // Read response length
        let mut len_bytes = [0u8; 4];
        conn.read_exact(&mut len_bytes)
            .await
            .map_err(|e| format!("failed to read response length: {}", e))?;

        let response_len = u32::from_be_bytes(len_bytes) as usize;

        // Sanity check: limit response size to 10MB
        if response_len > 10 * 1024 * 1024 {
            return Err(format!(
                "response too large: {} bytes (max 10MB)",
                response_len
            ));
        }

        // Read response payload
        let mut response_buf = vec![0u8; response_len];
        conn.read_exact(&mut response_buf)
            .await
            .map_err(|e| format!("failed to read response payload: {}", e))?;

        // Deserialize response
        let response: SandboxResponse = serde_json::from_slice(&response_buf)
            .map_err(|e| format!("failed to deserialize response: {}", e))?;

        debug!(
            "received sandbox response: exit_code={}, blocked={}, changes={}",
            response.exit_code,
            response.blocked,
            response.changes.len()
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let request = SandboxRequest {
            command: "ls -la".to_string(),
            timeout: 30,
            readonly: true,
            env: HashMap::new(),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("ls -la"));
        assert!(json.contains("readonly"));
    }

    #[test]
    fn test_response_deserialization() {
        let json = r#"{
            "exit_code": 0,
            "stdout": "file.txt\n",
            "stderr": "",
            "changes": [
                {"path": "/tmp/test.txt", "operation": "create"}
            ],
            "blocked": false
        }"#;

        let response: SandboxResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.exit_code, 0);
        assert_eq!(response.stdout, "file.txt\n");
        assert_eq!(response.changes.len(), 1);
        assert_eq!(response.changes[0].path, "/tmp/test.txt");
        assert_eq!(response.changes[0].operation, "create");
        assert!(!response.blocked);
    }

    #[test]
    fn test_file_change_serialization() {
        let change = FileChange {
            path: "/etc/passwd".to_string(),
            operation: "write".to_string(),
        };

        let json = serde_json::to_string(&change).unwrap();
        assert!(json.contains("/etc/passwd"));
        assert!(json.contains("write"));

        let parsed: FileChange = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, "/etc/passwd");
        assert_eq!(parsed.operation, "write");
    }

    #[test]
    fn test_ipc_unavailable_when_no_socket() {
        let ipc = SandboxIpc::new("/nonexistent/path/to/socket");
        assert!(!ipc.is_available());
    }

    #[test]
    fn test_timeout_configuration() {
        let ipc =
            SandboxIpc::new("/tmp/test.sock").with_timeout(std::time::Duration::from_secs(60));
        assert_eq!(ipc.timeout.as_secs(), 60);
    }
}
