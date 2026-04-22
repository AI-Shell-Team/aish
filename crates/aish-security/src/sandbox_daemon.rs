//! Privileged sandbox daemon with systemd socket activation.
//!
//! The daemon runs as root, accepts sandbox execution requests over a Unix
//! socket, spawns worker processes via `unshare --mount --propagation private`,
//! and returns results.
//!
//! Features:
//! - systemd socket activation support
//! - Per-user logging
//! - Peer credential verification
//! - Worker subprocess isolation

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::strip_sudo::strip_sudo_prefix;
use crate::types::{FsChange, IpcRequest, IpcResponse, IpcResult};
use aish_core::Result;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_SOCKET_PATH: &str = "/run/aish/sandbox.sock";
const IPC_STDIO_MAX_BYTES: usize = 2 * 1024 * 1024; // 2MB
const IPC_CHANGES_MAX: usize = 10_000;
const MAX_REQUEST_BYTES: usize = 1024 * 1024; // 1MB
const MAX_FILE_HANDLERS: usize = 32;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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
            socket_path: PathBuf::from(DEFAULT_SOCKET_PATH),
            backlog: 32,
            max_request_bytes: MAX_REQUEST_BYTES,
        }
    }
}

/// Sandbox daemon that listens for commands and executes them in worker processes.
pub struct SandboxDaemon {
    config: DaemonConfig,
    stop_requested: Arc<std::sync::atomic::AtomicBool>,
    loggers: Arc<Mutex<HashMap<u32, File>>>,
}

impl SandboxDaemon {
    /// Create a new sandbox daemon with the given configuration.
    pub fn new(config: DaemonConfig) -> Self {
        Self {
            config,
            stop_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            loggers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run the daemon server loop.
    ///
    /// This method blocks and handles incoming connections. Call `stop()` from
    /// another thread to gracefully shut down the server.
    pub fn serve_forever(&self) -> Result<()> {
        // Check for systemd socket activation
        let listener = if let Some(sock) = get_systemd_listen_socket() {
            info!("Using systemd socket activation (fd 3)");
            sock
        } else {
            // Create socket directory
            if let Some(parent) = self.config.socket_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    aish_core::AishError::Security(format!(
                        "Failed to create socket directory: {}",
                        e
                    ))
                })?;
            }

            // Remove existing socket file
            let _ = std::fs::remove_file(&self.config.socket_path);

            // Create and bind the Unix socket listener
            let listener = UnixListener::bind(&self.config.socket_path).map_err(|e| {
                aish_core::AishError::Security(format!(
                    "Failed to bind socket at {}: {}",
                    self.config.socket_path.display(),
                    e
                ))
            })?;

            // Set socket permissions: restrict to owner/group only
            let perms = PermissionsExt::from_mode(0o660);
            let _ = std::fs::set_permissions(&self.config.socket_path, perms);

            info!(
                "Sandbox daemon listening on {}",
                self.config.socket_path.display()
            );

            listener
        };

        listener.set_nonblocking(true).map_err(|e| {
            aish_core::AishError::Security(format!("Failed to set non-blocking: {}", e))
        })?;

        // Accept connections in a loop
        while !self
            .stop_requested
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            // Accept with timeout to check stop flag
            match listener.accept() {
                Ok((stream, _addr)) => {
                    // Handle connection in a thread
                    let stop_clone = self.stop_requested.clone();
                    let loggers_clone = self.loggers.clone();
                    let max_req = self.config.max_request_bytes;
                    thread::spawn(move || {
                        if let Err(e) =
                            handle_connection(stream, stop_clone, loggers_clone, max_req)
                        {
                            warn!("Connection handler error: {}", e);
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No pending connection, sleep briefly and check stop flag
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
// Systemd socket activation
// ---------------------------------------------------------------------------

/// Get systemd socket activation listener if available.
///
/// Checks LISTEN_FDS and LISTEN_PID environment variables.
/// If present and PID matches, returns fd 3 as UnixListener.
fn get_systemd_listen_socket() -> Option<UnixListener> {
    let listen_fds: i32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    let listen_pid: i32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;

    if listen_fds < 1 || listen_pid != std::process::id() as i32 {
        return None;
    }

    // fd 3 is the first passed fd
    unsafe { Some(UnixListener::from_raw_fd(3)) }
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

/// Handle a single client connection.
/// Read a single newline-terminated line from a reader, with a byte limit.
fn read_line_bounded<R: BufRead>(reader: &mut R, max_bytes: usize) -> aish_core::Result<String> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while buf.len() < max_bytes {
        match reader.read(&mut byte) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(e) => {
                return Err(aish_core::AishError::Security(format!(
                    "Failed to read request: {}",
                    e
                )));
            }
        }
    }
    if buf.len() >= max_bytes {
        return Err(aish_core::AishError::Security(
            "request_too_large".to_string(),
        ));
    }
    String::from_utf8(buf)
        .map_err(|e| aish_core::AishError::Security(format!("Invalid UTF-8 in request: {}", e)))
}

fn handle_connection(
    mut stream: UnixStream,
    _stop_requested: Arc<std::sync::atomic::AtomicBool>,
    loggers: Arc<Mutex<HashMap<u32, File>>>,
    max_request_bytes: usize,
) -> Result<()> {
    // Get peer credentials
    let (peer_pid, peer_uid, peer_gid) = get_peer_credentials(&stream)?;

    debug!(
        "Connection from peer: pid={}, uid={}, gid={}",
        peer_pid, peer_uid, peer_gid
    );

    // Only allow connections from the same uid or root
    let daemon_uid = unsafe { libc::geteuid() };
    if peer_uid != 0 && peer_uid != daemon_uid {
        return Err(aish_core::AishError::Security(format!(
            "Unauthorized peer uid={} (daemon uid={})",
            peer_uid, daemon_uid
        )));
    }

    // Read newline-terminated JSON request with size limit
    let mut reader = BufReader::new(&stream);
    let max_bytes = max_request_bytes;
    let line = read_line_bounded(&mut reader, max_bytes)?;

    if line.is_empty() {
        return Err(aish_core::AishError::Security("Empty request".to_string()));
    }

    if line.is_empty() {
        return Err(aish_core::AishError::Security("Empty request".to_string()));
    }

    // Parse request
    let request: IpcRequest = serde_json::from_str(&line)
        .map_err(|e| aish_core::AishError::Security(format!("Failed to parse request: {}", e)))?;

    // Validate required fields
    if request.id.is_empty() {
        send_error(&mut stream, &request.id, "missing_id")?;
        return Err(aish_core::AishError::Security("missing_id".to_string()));
    }
    if request.command.is_empty() || request.cwd.is_empty() || request.repo_root.is_empty() {
        send_error(&mut stream, &request.id, "missing_fields")?;
        return Err(aish_core::AishError::Security("missing_fields".to_string()));
    }

    // Clamp timeout to [1.0, 300.0]
    let timeout_s = request.timeout_s.unwrap_or(30.0);
    let timeout_s = timeout_s.clamp(1.0, 300.0);

    // Strip sudo prefix
    let (stripped_cmd, sudo_detected, sudo_ok) = strip_sudo_prefix(&request.command);

    // Determine run_as user
    let run_as = if sudo_detected {
        if !sudo_ok {
            send_error(&mut stream, &request.id, "missing_command")?;
            return Err(aish_core::AishError::Security(
                "missing_command".to_string(),
            ));
        }
        None // Run as root (daemon)
    } else {
        Some((peer_uid, peer_gid))
    };

    // Log the request
    log_request(
        &loggers,
        peer_uid,
        peer_pid,
        &request.id,
        &request.command,
        &request.cwd,
        &request.repo_root,
        run_as,
    )?;

    // Execute in worker process
    let result = execute_in_worker(
        &request.id,
        &stripped_cmd,
        &request.cwd,
        &request.repo_root,
        run_as,
        timeout_s,
    );

    // Build response
    let response = match result {
        Ok(worker_result) => IpcResponse {
            id: request.id.clone(),
            ok: true,
            reason: None,
            error: None,
            result: Some(worker_result),
        },
        Err(e) => IpcResponse {
            id: request.id.clone(),
            ok: false,
            reason: Some("worker_failed".to_string()),
            error: Some(e.to_string()),
            result: None,
        },
    };

    // Send response
    send_response(&mut stream, &response)?;

    // Log the result
    log_result(&loggers, peer_uid, &response)?;

    Ok(())
}

/// Get peer credentials (pid, uid, gid) from a Unix socket.
#[cfg(target_os = "linux")]
fn get_peer_credentials(stream: &UnixStream) -> Result<(i32, u32, u32)> {
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
        return Err(aish_core::AishError::Security(
            "Failed to get peer credentials".to_string(),
        ));
    }

    Ok((creds.pid, creds.uid, creds.gid))
}

#[cfg(not(target_os = "linux"))]
fn get_peer_credentials(_stream: &UnixStream) -> Result<(i32, u32, u32), aish_core::AishError> {
    // Peer credentials are Linux-specific
    Ok((-1, 0, 0))
}

/// Execute command in a worker subprocess.
fn execute_in_worker(
    _id: &str,
    command: &str,
    cwd: &str,
    repo_root: &str,
    run_as: Option<(u32, u32)>,
    timeout_s: f64,
) -> Result<IpcResult> {
    // Get current executable path (aish binary)
    let aish_bin = std::env::current_exe().map_err(|e| {
        aish_core::AishError::Security(format!("Failed to get current executable: {}", e))
    })?;

    // Build worker payload
    let (sim_uid, sim_gid) = run_as.unwrap_or((0, 0));
    let payload = WorkerPayload {
        command: command.to_string(),
        cwd: cwd.to_string(),
        repo_root: repo_root.to_string(),
        sim_uid,
        sim_gid,
        timeout_s,
    };

    let payload_json = serde_json::to_string(&payload).map_err(|e| {
        aish_core::AishError::Security(format!("Failed to serialize worker payload: {}", e))
    })?;

    // Build unshare command
    let mut cmd = Command::new("unshare");
    cmd.arg("--mount")
        .arg("--propagation")
        .arg("private")
        .arg("--")
        .arg(&aish_bin)
        .arg("--sandbox-worker");

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    debug!(
        "Spawning worker: unshare --mount --propagation private -- {} --sandbox-worker",
        aish_bin.display()
    );

    let mut child = cmd
        .spawn()
        .map_err(|e| aish_core::AishError::Security(format!("Failed to spawn worker: {}", e)))?;

    // Write payload to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(payload_json.as_bytes()).map_err(|e| {
            aish_core::AishError::Security(format!("Failed to write to worker stdin: {}", e))
        })?;
    }

    // Read stdout with timeout
    let worker_timeout = Duration::from_secs_f64(timeout_s + 10.0);
    let output = read_with_timeout(&mut child, worker_timeout)?;

    // Parse worker response
    let worker_response: WorkerResponse = serde_json::from_str(&output).map_err(|e| {
        aish_core::AishError::Security(format!("Failed to parse worker response: {}", e))
    })?;

    if !worker_response.ok {
        return Err(aish_core::AishError::Security(format!(
            "Worker failed: {:?} - {:?}",
            worker_response.reason, worker_response.error
        )));
    }

    // Truncate if needed
    let result_ref = worker_response
        .result
        .as_ref()
        .ok_or_else(|| aish_core::AishError::Security("Worker result is missing".to_string()))?;

    let stdout_truncated = result_ref.stdout.len() > IPC_STDIO_MAX_BYTES;
    let stderr_truncated = result_ref.stderr.len() > IPC_STDIO_MAX_BYTES;
    let changes_truncated = result_ref.changes.len() > IPC_CHANGES_MAX;

    let mut stdout = result_ref.stdout.clone();
    let mut stderr = result_ref.stderr.clone();
    let mut changes = result_ref.changes.clone();

    if stdout_truncated {
        stdout.truncate(IPC_STDIO_MAX_BYTES);
    }
    if stderr_truncated {
        stderr.truncate(IPC_STDIO_MAX_BYTES);
    }
    if changes_truncated {
        changes.truncate(IPC_CHANGES_MAX);
    }

    Ok(IpcResult {
        exit_code: result_ref.exit_code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        changes_truncated,
        changes,
    })
}

/// Read worker output with timeout.
fn read_with_timeout(child: &mut std::process::Child, timeout: Duration) -> Result<String> {
    // Drain stdout and stderr concurrently to prevent pipe deadlock.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout {
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr {
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
        }
        buf
    });

    // Wait for child with timeout
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout_data = stdout_thread.join().unwrap_or_default();
                let _stderr_data = stderr_thread.join().unwrap_or_default();

                if !status.success() {
                    return Err(aish_core::AishError::Security(format!(
                        "Worker exited with status: {:?}",
                        status
                    )));
                }

                return String::from_utf8(stdout_data).map_err(|e| {
                    aish_core::AishError::Security(format!("Worker output not valid UTF-8: {}", e))
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(aish_core::AishError::Security("Worker timeout".to_string()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(aish_core::AishError::Security(format!(
                    "Failed to wait for worker: {}",
                    e
                )));
            }
        }
    }
}

/// Send a response to the client.
fn send_response(stream: &mut UnixStream, response: &IpcResponse) -> Result<()> {
    let json = serde_json::to_string(response).map_err(|e| {
        aish_core::AishError::Security(format!("Failed to serialize response: {}", e))
    })?;
    let line = format!("{}\n", json);
    stream
        .write_all(line.as_bytes())
        .map_err(|e| aish_core::AishError::Security(format!("Failed to send response: {}", e)))?;
    stream
        .flush()
        .map_err(|e| aish_core::AishError::Security(format!("Failed to flush response: {}", e)))?;
    Ok(())
}

/// Send an error response.
fn send_error(stream: &mut UnixStream, id: &str, reason: &str) -> Result<()> {
    let response = IpcResponse {
        id: id.to_string(),
        ok: false,
        reason: Some(reason.to_string()),
        error: None,
        result: None,
    };
    send_response(stream, &response)
}

// ---------------------------------------------------------------------------
// Per-user logging
// ---------------------------------------------------------------------------

/// Log a sandbox request.
fn log_request(
    loggers: &Arc<Mutex<HashMap<u32, File>>>,
    uid: u32,
    pid: i32,
    id: &str,
    command: &str,
    cwd: &str,
    repo_root: &str,
    run_as: Option<(u32, u32)>,
) -> Result<()> {
    let log_msg = format!(
        "sandboxd(uid={}) [pid={}] Request: id={}, command={:?}, cwd={:?}, repo_root={:?}, run_as={:?}\n",
        uid, pid, id, command, cwd, repo_root, run_as
    );

    write_log(loggers, uid, &log_msg)
}

/// Log a sandbox result.
fn log_result(
    loggers: &Arc<Mutex<HashMap<u32, File>>>,
    uid: u32,
    response: &IpcResponse,
) -> Result<()> {
    let log_msg = if response.ok {
        if let Some(result) = &response.result {
            format!(
                "sandboxd(uid={}) Result: exit_code={}, stdout_len={}, stderr_len={}, changes={}, truncated={},{},{}\n",
                uid,
                result.exit_code,
                result.stdout.len(),
                result.stderr.len(),
                result.changes.len(),
                result.stdout_truncated,
                result.stderr_truncated,
                result.changes_truncated
            )
        } else {
            format!("sandboxd(uid={}) Result: ok=true (no result)\n", uid)
        }
    } else {
        format!(
            "sandboxd(uid={}) Result: ok=false, reason={:?}, error={:?}\n",
            uid, response.reason, response.error
        )
    };

    write_log(loggers, uid, &log_msg)
}

/// Write a log message to a user's log file.
fn write_log(loggers: &Arc<Mutex<HashMap<u32, File>>>, uid: u32, msg: &str) -> Result<()> {
    let mut loggers_guard = loggers
        .lock()
        .map_err(|e| aish_core::AishError::Security(format!("Failed to lock loggers: {}", e)))?;

    // Evict oldest if too many handlers
    if loggers_guard.len() >= MAX_FILE_HANDLERS && !loggers_guard.contains_key(&uid) {
        // Remove first entry
        let key = loggers_guard.keys().next().copied();
        if let Some(k) = key {
            loggers_guard.remove(&k);
        }
    }

    // Get or create logger for this user
    if let std::collections::hash_map::Entry::Vacant(e) = loggers_guard.entry(uid) {
        let log_path = get_user_log_path(uid)?;
        let file = File::options()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| {
                aish_core::AishError::Security(format!("Failed to open log file: {}", e))
            })?;
        e.insert(file);
    }

    // Write log message
    if let Some(file) = loggers_guard.get_mut(&uid) {
        file.write_all(msg.as_bytes())
            .map_err(|e| aish_core::AishError::Security(format!("Failed to write log: {}", e)))?;
        file.flush()
            .map_err(|e| aish_core::AishError::Security(format!("Failed to flush log: {}", e)))?;
    }

    Ok(())
}

/// Get the log file path for a user.
/// Uses a root-owned directory to prevent symlink attacks.
fn get_user_log_path(uid: u32) -> Result<PathBuf> {
    let log_dir = PathBuf::from("/var/log/aish");

    // Ensure root-owned directory exists with restrictive permissions
    if !log_dir.exists() {
        std::fs::create_dir_all(&log_dir).map_err(|e| {
            aish_core::AishError::Security(format!("Failed to create log directory: {}", e))
        })?;
        let perms = PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&log_dir, perms).map_err(|e| {
            aish_core::AishError::Security(format!("Failed to set log dir permissions: {}", e))
        })?;
    }

    let log_path = log_dir.join(format!("sandbox-{}.log", uid));

    Ok(log_path)
}

// ---------------------------------------------------------------------------
// Worker types
// ---------------------------------------------------------------------------

/// Payload sent to worker process.
#[derive(Debug, Serialize)]
struct WorkerPayload {
    command: String,
    cwd: String,
    repo_root: String,
    sim_uid: u32,
    sim_gid: u32,
    timeout_s: f64,
}

/// Response from worker process.
#[derive(Debug, Deserialize)]
struct WorkerResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<WorkerResultPayload>,
}

/// Result payload from worker.
#[derive(Debug, Deserialize)]
struct WorkerResultPayload {
    exit_code: i32,
    stdout: String,
    stderr: String,
    changes: Vec<FsChange>,
}

// ---------------------------------------------------------------------------
// Legacy compatibility types (deprecated)
// ---------------------------------------------------------------------------

#[allow(deprecated)]
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

#[allow(deprecated)]
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

#[allow(deprecated)]
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

#[allow(deprecated)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: String,
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
        assert_eq!(config.socket_path, PathBuf::from(DEFAULT_SOCKET_PATH));
        assert_eq!(config.backlog, 32);
        assert_eq!(config.max_request_bytes, MAX_REQUEST_BYTES);
    }

    #[test]
    fn test_worker_payload_serialize() {
        let payload = WorkerPayload {
            command: "ls -la".to_string(),
            cwd: "/home/user".to_string(),
            repo_root: "/home/user/project".to_string(),
            sim_uid: 1000,
            sim_gid: 1000,
            timeout_s: 30.0,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"command\":\"ls -la\""));
        assert!(json.contains("\"sim_uid\":1000"));
    }

    #[test]
    fn test_worker_response_deserialize() {
        let json = r#"{
            "ok": true,
            "result": {
                "exit_code": 0,
                "stdout": "hello",
                "stderr": "",
                "changes": []
            }
        }"#;

        let resp: WorkerResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert!(resp.result.is_some());
    }

    #[test]
    fn test_worker_error_deserialize() {
        let json = r#"{
            "ok": false,
            "reason": "test_reason",
            "error": "test error"
        }"#;

        let resp: WorkerResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.reason, Some("test_reason".to_string()));
    }
}
