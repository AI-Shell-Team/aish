//! Sandbox daemon for isolated command execution.
//!
//! Runs as a long-lived daemon process listening on a Unix socket.
//! Accepts command execution requests and runs them in isolated bubblewrap
//! environments, returning results via JSON protocol.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use tracing::{debug, info, warn};

use super::sandbox::{detect_fs_changes, FsChange, SandboxConfig, SandboxResult};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_BACKLOG: i32 = 32;
const MAX_REQUEST_BYTES: usize = 1024 * 1024; // 1MB
const DEFAULT_TIMEOUT_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Request sent by client to the sandbox daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub repo_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_pid: Option<u32>,
}

/// Response sent by sandbox daemon to client.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<DaemonResult>,
}

/// Result payload in successful responses.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub changes_truncated: bool,
    pub changes: Vec<FileChange>,
}

/// File change in daemon format (simplified from FsChange).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: String,
}

/// Configuration for the sandbox daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub backlog: i32,
    pub max_request_bytes: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/run/user/1000/aish-sandbox.sock"),
            backlog: DEFAULT_BACKLOG,
            max_request_bytes: MAX_REQUEST_BYTES,
        }
    }
}

// ---------------------------------------------------------------------------
// Sandbox daemon
// ---------------------------------------------------------------------------

/// Sandbox daemon that listens for commands and executes them in isolation.
pub struct SandboxDaemon {
    config: DaemonConfig,
    stop_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SandboxDaemon {
    pub fn new(config: DaemonConfig) -> Self {
        Self {
            config,
            stop_requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Run the daemon server loop.
    ///
    /// This method blocks and handles incoming connections. Call `stop()` from
    /// another thread to gracefully shut down the server.
    pub fn serve(&self) -> Result<(), String> {
        // Ensure socket directory exists.
        if let Some(parent) = self.config.socket_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create socket directory: {}", e))?;
        }

        // Remove existing socket file if present.
        let _ = std::fs::remove_file(&self.config.socket_path);

        // Create and bind the Unix socket listener.
        let listener = UnixListener::bind(&self.config.socket_path).map_err(|e| {
            format!(
                "failed to bind socket at {}: {}",
                self.config.socket_path.display(),
                e
            )
        })?;

        // Set socket permissions to 0666.
        let perms = PermissionsExt::from_mode(0o666);
        let _ = std::fs::set_permissions(&self.config.socket_path, perms);

        info!(
            "Sandbox daemon listening on {}",
            self.config.socket_path.display()
        );

        listener
            .set_nonblocking(true)
            .map_err(|e| format!("failed to set non-blocking: {}", e))?;

        // Accept connections in a loop.
        while !self
            .stop_requested
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            // Accept with timeout to check stop flag.
            match listener.accept() {
                Ok((stream, _addr)) => {
                    // Handle connection in a thread.
                    let stop_clone = self.stop_requested.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_connection(stream, stop_clone) {
                            warn!("Connection handler error: {}", e);
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No pending connection, sleep briefly and check stop flag.
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => {
                    warn!("Accept error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        info!("Sandbox daemon shutting down");
        Ok(())
    }

    /// Signal the daemon to stop gracefully.
    pub fn stop(&self) {
        self.stop_requested
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

/// Handle a single client connection.
fn handle_connection(
    mut stream: UnixStream,
    _stop_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    // Get peer credentials.
    let (peer_pid, peer_uid, peer_gid) = get_peer_credentials(&stream)?;

    debug!(
        "Connection from peer: pid={}, uid={}, gid={}",
        peer_pid, peer_uid, peer_gid
    );

    // Read newline-terminated JSON request.
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("failed to read request: {}", e))?;

    if line.is_empty() {
        return Err("empty request".to_string());
    }

    // Parse request.
    let request: DaemonRequest =
        serde_json::from_str(&line).map_err(|e| format!("failed to parse request: {}", e))?;

    // Validate required fields.
    if request.id.is_empty() {
        send_error(&mut stream, &request.id, "missing_id")?;
        return Err("missing_id".to_string());
    }
    if request.command.is_empty() || request.cwd.is_empty() || request.repo_root.is_empty() {
        send_error(&mut stream, &request.id, "missing_fields")?;
        return Err("missing_fields".to_string());
    }

    info!(
        "Executing request {}: cwd={}, command={}",
        request.id, request.cwd, request.command
    );

    // Execute command in sandbox.
    let result = execute_in_sandbox(&request, peer_uid, peer_gid);

    // Send response.
    let response = DaemonResponse {
        id: request.id.clone(),
        ok: result.is_ok(),
        reason: if result.is_ok() {
            None
        } else {
            result.as_ref().err().map(|e| e.to_string())
        },
        error: None,
        result: result.as_ref().ok().map(|r| convert_result(&request, r)),
    };

    send_response(&mut stream, &response)?;
    Ok(())
}

/// Get peer credentials (pid, uid, gid) from a Unix socket.
#[cfg(target_os = "linux")]
fn get_peer_credentials(stream: &UnixStream) -> Result<(i32, i32, i32), String> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();

    let mut creds: libc::ucred = unsafe { std::mem::zeroed() };
    let mut creds_size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut creds as *mut _ as *mut libc::c_void,
            &mut creds_size,
        )
    };

    if ret != 0 {
        return Err("failed to get peer credentials".to_string());
    }

    Ok((creds.pid, creds.uid as i32, creds.gid as i32))
}

#[cfg(not(target_os = "linux"))]
fn get_peer_credentials(_stream: &UnixStream) -> Result<(i32, i32, i32), String> {
    // Peer credentials are Linux-specific.
    Ok((-1, -1, -1))
}

/// Execute a command inside the sandbox.
fn execute_in_sandbox(
    request: &DaemonRequest,
    uid: i32,
    gid: i32,
) -> Result<SandboxResult, String> {
    // Validate absolute paths.
    let cwd = Path::new(&request.cwd);
    let repo_root = Path::new(&request.repo_root);
    if !cwd.is_absolute() || !repo_root.is_absolute() {
        return Err("repo_root and cwd must be absolute paths".to_string());
    }

    // Build sandbox config.
    let config = SandboxConfig {
        timeout_secs: request
            .timeout_s
            .map(|s| s as u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
        ..Default::default()
    };

    // Execute using direct bwrap call.
    // In production, this would spawn a worker subprocess like Python does.
    let result = execute_with_bwrap(request, &config, uid, gid)?;
    Ok(result)
}

/// Execute command directly with bwrap.
fn execute_with_bwrap(
    request: &DaemonRequest,
    _config: &SandboxConfig,
    _uid: i32,
    _gid: i32,
) -> Result<SandboxResult, String> {
    // Create a temporary overlay directory.
    let overlay_dir = std::env::temp_dir().join(format!("aish-sandbox-{}", uuid::Uuid::new_v4()));
    let upper_dir = overlay_dir.join("upper");
    std::fs::create_dir_all(&upper_dir).map_err(|e| format!("failed to create overlay: {}", e))?;

    // Build bwrap command.
    let mut cmd = Command::new("bwrap");
    cmd.arg("--ro-bind")
        .arg("/")
        .arg("/")
        .arg("--bind")
        .arg(&upper_dir)
        .arg("/")
        .arg("--dev")
        .arg("/dev")
        .arg("--proc")
        .arg("/proc")
        .arg("--unshare-net")
        .arg("--die-with-parent");

    // Set working directory.
    let _ = cmd.current_dir(&request.cwd);
    // If current_dir fails, the command execution will fail with appropriate error.

    // Execute command via bash.
    cmd.arg("--")
        .arg("/bin/bash")
        .arg("-c")
        .arg(&request.command);

    debug!("Executing: bwrap ... -- /bin/bash -c {:?}", request.command);

    let output = cmd
        .output()
        .map_err(|e| format!("bwrap execution failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(1);

    // Detect file changes.
    let fs_changes = detect_fs_changes(&upper_dir);

    // Cleanup overlay.
    let _ = std::fs::remove_dir_all(&overlay_dir);

    Ok(SandboxResult {
        exit_code,
        stdout,
        stderr,
        fs_changes,
    })
}

/// Convert SandboxResult to DaemonResult format.
fn convert_result(_request: &DaemonRequest, result: &SandboxResult) -> DaemonResult {
    let changes: Vec<FileChange> = result
        .fs_changes
        .iter()
        .map(|c| match c {
            FsChange::Created(p) => FileChange {
                path: p.display().to_string(),
                kind: "created".to_string(),
            },
            FsChange::Modified(p) => FileChange {
                path: p.display().to_string(),
                kind: "modified".to_string(),
            },
            FsChange::Deleted(p) => FileChange {
                path: p.display().to_string(),
                kind: "deleted".to_string(),
            },
        })
        .collect();

    DaemonResult {
        exit_code: result.exit_code,
        stdout: result.stdout.clone(),
        stderr: result.stderr.clone(),
        stdout_truncated: false,
        stderr_truncated: false,
        changes_truncated: false,
        changes,
    }
}

/// Send a response to the client.
fn send_response(stream: &mut UnixStream, response: &DaemonResponse) -> Result<(), String> {
    let json = serde_json::to_string(response)
        .map_err(|e| format!("failed to serialize response: {}", e))?;
    let line = format!("{}\n", json);
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("failed to send response: {}", e))?;
    stream
        .flush()
        .map_err(|e| format!("failed to flush response: {}", e))?;
    Ok(())
}

/// Send an error response.
fn send_error(stream: &mut UnixStream, id: &str, reason: &str) -> Result<(), String> {
    let response = DaemonResponse {
        id: id.to_string(),
        ok: false,
        reason: Some(reason.to_string()),
        error: None,
        result: None,
    };
    send_response(stream, &response)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert_eq!(config.backlog, 32);
        assert_eq!(config.max_request_bytes, 1024 * 1024);
    }

    #[test]
    fn test_request_serialization() {
        let req = DaemonRequest {
            id: "test-123".to_string(),
            command: "ls -la".to_string(),
            cwd: "/home/user".to_string(),
            repo_root: "/home/user/project".to_string(),
            timeout_s: Some(30.0),
            client_pid: Some(12345),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("ls -la"));
        assert!(json.contains("test-123"));

        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-123");
        assert_eq!(parsed.command, "ls -la");
    }

    #[test]
    fn test_response_serialization() {
        let resp = DaemonResponse {
            id: "test-123".to_string(),
            ok: true,
            reason: None,
            error: None,
            result: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":true"));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
    }

    #[test]
    fn test_error_response() {
        let resp = DaemonResponse {
            id: "abc".to_string(),
            ok: false,
            reason: Some("test_error".to_string()),
            error: None,
            result: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("test_error"));
    }

    #[test]
    fn test_file_change_serialization() {
        let change = FileChange {
            path: "/tmp/test.txt".to_string(),
            kind: "created".to_string(),
        };

        let json = serde_json::to_string(&change).unwrap();
        let parsed: FileChange = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, "/tmp/test.txt");
        assert_eq!(parsed.kind, "created");
    }

    #[test]
    fn test_convert_result() {
        let request = DaemonRequest {
            id: "test".to_string(),
            command: "echo hi".to_string(),
            cwd: "/tmp".to_string(),
            repo_root: "/tmp".to_string(),
            timeout_s: None,
            client_pid: None,
        };

        let result = SandboxResult {
            exit_code: 0,
            stdout: "hi\n".to_string(),
            stderr: String::new(),
            fs_changes: vec![FsChange::Modified(PathBuf::from("/tmp/out.txt"))],
        };

        let daemon_result = convert_result(&request, &result);
        assert_eq!(daemon_result.exit_code, 0);
        assert_eq!(daemon_result.stdout, "hi\n");
        assert_eq!(daemon_result.changes.len(), 1);
        assert_eq!(daemon_result.changes[0].kind, "modified");
    }
}
