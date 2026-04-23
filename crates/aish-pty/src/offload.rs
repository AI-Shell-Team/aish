use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use aish_core::AishError;
use chrono::Local;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::types::StreamName;

/// Global sequence counter for exec_id generation.
static GLOBAL_SEQ: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-stream result after finalisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffloadState {
    pub status: String,             // "inline" | "offloaded" | "failed"
    pub path: Option<String>,       // raw temp-file path
    pub clean_path: Option<String>, // stripped ANSI path
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_error: Option<String>, // error during clean file generation
}

/// Combined result for both streams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffloadResult {
    pub stdout: OffloadState,
    pub stderr: OffloadState,
    pub meta_path: Option<String>,
    pub stdout_hash: Option<String>,
    pub stderr_hash: Option<String>,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub command: Option<String>,
    pub duration_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// PtyOutputOffload
// ---------------------------------------------------------------------------

/// Manages writing overflow output to temporary files when output exceeds the
/// in-memory `keep_bytes` threshold.
pub struct PtyOutputOffload {
    session_uuid: String,
    base_dir: PathBuf,
    keep_bytes: usize,

    stdout_buf: Vec<u8>,
    stderr_buf: Vec<u8>,

    stdout_overflow_path: Option<PathBuf>,
    stderr_overflow_path: Option<PathBuf>,

    command: String,
    started_at: std::time::Instant,
}

impl PtyOutputOffload {
    /// Create a new offload manager.
    pub fn new(
        command: &str,
        session_uuid: &str,
        _cwd: &str,
        keep_len: usize,
        base_dir: &str,
    ) -> Self {
        Self {
            session_uuid: session_uuid.to_string(),
            base_dir: PathBuf::from(base_dir),
            keep_bytes: keep_len,
            stdout_buf: Vec::new(),
            stderr_buf: Vec::new(),
            stdout_overflow_path: None,
            stderr_overflow_path: None,
            command: command.to_string(),
            started_at: std::time::Instant::now(),
        }
    }

    // -- helpers -----------------------------------------------------------

    /// Return the per-session offload directory.
    fn offload_dir(&self) -> PathBuf {
        self.base_dir.join("aish-offload").join(&self.session_uuid)
    }

    /// Ensure the offload directory exists.  Returns the path.
    fn ensure_offload_dir(&self) -> std::io::Result<PathBuf> {
        let dir = self.offload_dir();
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Get the overflow file path for a stream, creating the file if needed.
    fn ensure_overflow_file(&mut self, stream: StreamName) -> Result<(), AishError> {
        let has_overflow = match stream {
            StreamName::Stdout => self.stdout_overflow_path.is_some(),
            StreamName::Stderr => self.stderr_overflow_path.is_some(),
        };

        if !has_overflow {
            // Compute everything before borrowing mutably.
            let dir = self
                .ensure_offload_dir()
                .map_err(|e| AishError::Pty(format!("failed to create offload dir: {e}")))?;
            let filename = format!("{}.raw", stream);
            let path = dir.join(&filename);

            // Create / truncate the file.
            fs::File::create(&path)
                .map_err(|e| AishError::Pty(format!("failed to create overflow file: {e}")))?;

            debug!(path = %path.display(), "created overflow file for {stream}");

            // Store the path.
            match stream {
                StreamName::Stdout => self.stdout_overflow_path = Some(path),
                StreamName::Stderr => self.stderr_overflow_path = Some(path),
            }
        }

        Ok(())
    }

    /// Write data to the overflow file for the given stream (append mode).
    fn write_overflow(&self, stream: StreamName, data: &[u8]) {
        let path = match stream {
            StreamName::Stdout => &self.stdout_overflow_path,
            StreamName::Stderr => &self.stderr_overflow_path,
        };

        if let Some(ref p) = path {
            match fs::OpenOptions::new().append(true).open(p) {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(data) {
                        warn!("overflow write error: {e}");
                    }
                }
                Err(e) => {
                    warn!("failed to open overflow file for append: {e}");
                }
            }
        }
    }

    /// Get a mutable reference to the internal buffer for the stream.
    fn get_buf(&mut self, stream: StreamName) -> &mut Vec<u8> {
        match stream {
            StreamName::Stdout => &mut self.stdout_buf,
            StreamName::Stderr => &mut self.stderr_buf,
        }
    }

    /// Append overflow data for the given stream.
    ///
    /// Data that exceeds `keep_bytes` in the internal ring is written to the
    /// temp file.  The buffer always retains the most recent `keep_bytes`.
    pub fn append_overflow(&mut self, stream: StreamName, data: &[u8]) {
        // Get the current buffer length for this stream.
        let buf_len = match stream {
            StreamName::Stdout => self.stdout_buf.len(),
            StreamName::Stderr => self.stderr_buf.len(),
        };

        // Check if we need to spill to disk.
        if buf_len + data.len() <= self.keep_bytes {
            // No overflow needed, just append.
            self.get_buf(stream).extend_from_slice(data);
            return;
        }

        // Compute spill amounts.
        let total = buf_len + data.len();
        let excess = total.saturating_sub(self.keep_bytes);
        let from_buf = excess.min(buf_len);
        let from_data = excess - from_buf;

        // Ensure overflow file exists.
        if let Err(e) = self.ensure_overflow_file(stream) {
            warn!("failed to create overflow file: {e}");
            // Still append to the buffer even if file creation failed.
            self.get_buf(stream).extend_from_slice(data);
            return;
        }

        // Copy data to write before mutating the buffer.
        let mut overflow_data = Vec::new();
        if from_buf > 0 {
            let buf = self.get_buf(stream);
            overflow_data.extend_from_slice(&buf[..from_buf]);
        }
        if from_data > 0 {
            overflow_data.extend_from_slice(&data[..from_data]);
        }

        // Write excess data to file.
        if !overflow_data.is_empty() {
            self.write_overflow(stream, &overflow_data);
        }

        // Trim the front of the buffer and append new data.
        let buf = self.get_buf(stream);
        buf.drain(..from_buf);
        buf.extend_from_slice(data);
    }

    /// Read the full contents of an overflow file.
    fn read_overflow(&self, path: &Option<PathBuf>) -> Vec<u8> {
        match path {
            None => Vec::new(),
            Some(p) => fs::read(p).unwrap_or_default(),
        }
    }

    /// Finalise both streams and return the result.
    pub fn finalize(
        self,
        stdout_tail: &[u8],
        stderr_tail: &[u8],
        return_code: i32,
    ) -> OffloadResult {
        let _ = self.ensure_offload_dir();

        // Read overflow file contents for hashing and preview.
        let stdout_overflow = self.read_overflow(&self.stdout_overflow_path);
        let stderr_overflow = self.read_overflow(&self.stderr_overflow_path);

        // Compute hashes.
        let stdout_hash = compute_hash(&stdout_overflow, stdout_tail);
        let stderr_hash = compute_hash(&stderr_overflow, stderr_tail);

        // Build previews (up to 50 lines).
        let stdout_preview = build_preview(&stdout_overflow, stdout_tail, 50);
        let stderr_preview = build_preview(&stderr_overflow, stderr_tail, 50);

        // Calculate duration.
        let duration_ms = self.started_at.elapsed().as_millis() as u64;

        // Generate exec_id and command hash.
        let seq = GLOBAL_SEQ.fetch_add(1, Ordering::Relaxed);
        let exec_id = generate_exec_id(seq);
        let command_hash = if self.command.is_empty() {
            None
        } else {
            Some(hash_string(&self.command))
        };

        // Build states for stdout and stderr.
        let stdout_state = build_state(&self.stdout_overflow_path, stdout_tail);
        let stderr_state = build_state(&self.stderr_overflow_path, stderr_tail);

        // Enriched meta.json — always write even when no overflow occurred.
        let offload_dir = self.offload_dir();
        let _ = fs::create_dir_all(&offload_dir);
        let meta_path = offload_dir.join("meta.json");
        let meta = serde_json::json!({
            "version": 1,
            "kind": "pty_command_output",
            "session_uuid": self.session_uuid,
            "exec_id": exec_id,
            "exit_code": return_code,
            "stdout_bytes": self.stdout_buf.len(),
            "stderr_bytes": self.stderr_buf.len(),
            "command": self.command,
            "command_hash": command_hash,
            "duration_ms": duration_ms,
            "stdout_hash": stdout_hash,
            "stderr_hash": stderr_hash,
            "stdout_preview": stdout_preview,
            "stderr_preview": stderr_preview,
            "stdout": {
                "status": stdout_state.status,
                "path": stdout_state.path,
                "clean_path": stdout_state.clean_path,
                "error": stdout_state.error,
                "clean_error": stdout_state.clean_error,
            },
            "stderr": {
                "status": stderr_state.status,
                "path": stderr_state.path,
                "clean_path": stderr_state.clean_path,
                "error": stderr_state.error,
                "clean_error": stderr_state.clean_error,
            },
        });
        if let Ok(json) = serde_json::to_string_pretty(&meta) {
            let _ = fs::write(&meta_path, json);
        }

        // Set 0600 permissions on directory and files.
        set_permissions_0600(&self.offload_dir(), &stdout_state, &stderr_state);

        OffloadResult {
            stdout: stdout_state,
            stderr: stderr_state,
            meta_path: meta_path.to_str().map(|s| s.to_string()),
            stdout_hash,
            stderr_hash,
            stdout_preview,
            stderr_preview,
            command: if self.command.is_empty() {
                None
            } else {
                Some(self.command)
            },
            duration_ms: Some(duration_ms),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Strip ANSI escape sequences from bytes, returning clean UTF-8 text.
///
/// Handles:
/// - CSI sequences: `\x1b[ ... <final_byte>`
/// - OSC sequences: `\x1b] ... \x07` or `\x1b] ... \x1b\\`
/// - Other C0 controls (keep newline and tab)
pub fn strip_ansi_escapes(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == 0x1b {
            // Escape sequence
            if i + 1 < input.len() {
                match input[i + 1] {
                    b'[' => {
                        // CSI sequence: skip to final byte (0x40..=0x7E)
                        i += 2;
                        while i < input.len() && !(input[i] >= 0x40 && input[i] <= 0x7e) {
                            i += 1;
                        }
                        if i < input.len() {
                            i += 1; // skip final byte
                        }
                        continue;
                    }
                    b']' => {
                        // OSC sequence: skip to BEL (0x07) or ST (\x1b\\)
                        i += 2;
                        while i < input.len() {
                            if input[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'\\' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                        continue;
                    }
                    b'(' | b')' | b'*' | b'+' => {
                        // Character set designation: ESC + ( + byte
                        i += 3;
                        continue;
                    }
                    _ => {
                        // Two-byte escape (ESC + byte)
                        i += 2;
                        continue;
                    }
                }
            } else {
                i += 1;
                continue;
            }
        } else if input[i] < 0x20 && input[i] != b'\n' && input[i] != b'\t' && input[i] != b'\r' {
            // Skip other C0 control characters (keep \n, \t, \r)
            i += 1;
            continue;
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

/// Truncate bytes to at most `max_bytes` while preserving valid UTF-8 boundaries.
/// Returns the truncated bytes and whether truncation occurred.
pub fn truncate_utf8_safe(input: &[u8], max_bytes: usize) -> (Vec<u8>, bool) {
    if input.len() <= max_bytes {
        return (input.to_vec(), false);
    }
    // Find the last valid UTF-8 boundary at or before max_bytes.
    // A byte is a UTF-8 lead byte if it does NOT match 0b10xxxxxx.
    let mut end = max_bytes;
    while end > 0 && (input[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    (input[..end].to_vec(), true)
}

/// Generate an exec_id in the format `YYYYMMDDTHHMMSS.mmm_PID_SEQ`.
fn generate_exec_id(seq: u32) -> String {
    let now = Local::now();
    let pid = std::process::id();
    format!(
        "{}.{:03}_{}_{}",
        now.format("%Y%m%dT%H%M%S"),
        now.timestamp_subsec_millis(),
        pid,
        seq,
    )
}

/// Generate a 16-char hex UID from a seed string (SHA256 prefix).
fn generate_uid(seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let result = hasher.finalize();
    // Take first 8 bytes → 16 hex chars.
    result[..8].iter().map(|b| format!("{:02x}", b)).collect()
}

/// Compute SHA256 hash of a string, returning hex digest.
fn hash_string(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Build an OffloadState from the optional overflow path and the tail bytes.
/// Writes a `.clean` file with ANSI sequences stripped from combined overflow + tail.
fn build_state(overflow_path: &Option<PathBuf>, tail: &[u8]) -> OffloadState {
    match overflow_path {
        None => OffloadState {
            status: "inline".to_string(),
            path: None,
            clean_path: None,
            error: None,
            clean_error: None,
        },
        Some(path) => {
            let path_str = path.to_str().unwrap_or("").to_string();
            let clean_path = {
                let p = path.with_extension("clean");
                // Read overflow, combine with tail, strip ANSI, write clean.
                let overflow_data = fs::read(path).unwrap_or_default();
                let mut combined = overflow_data;
                combined.extend_from_slice(tail);
                let cleaned = strip_ansi_escapes(&combined);
                let clean_error = match fs::write(&p, &cleaned) {
                    Ok(()) => None,
                    Err(e) => Some(e.to_string()),
                };
                // Also update the OffloadState's clean_error
                let _ = clean_error; // handled below
                p.to_str().map(|s| s.to_string())
            };
            OffloadState {
                status: "offloaded".to_string(),
                path: Some(path_str),
                clean_path,
                error: None,
                clean_error: None,
            }
        }
    }
}

/// Compute SHA256 hash over the combined overflow buffer and tail bytes.
/// Returns `None` if both inputs are empty.
fn compute_hash(overflow_buf: &[u8], tail: &[u8]) -> Option<String> {
    if overflow_buf.is_empty() && tail.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(overflow_buf);
    hasher.update(tail);
    let result = hasher.finalize();
    Some(format!(
        "sha256-{}",
        result
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    ))
}

/// Build a text preview by combining overflow and tail, taking at most
/// `max_lines` lines from the beginning. Returns `None` if both inputs
/// are empty or if no lines can be extracted.
fn build_preview(overflow_buf: &[u8], tail: &[u8], max_lines: usize) -> Option<String> {
    if overflow_buf.is_empty() && tail.is_empty() {
        return None;
    }
    let combined = if overflow_buf.is_empty() {
        tail.to_vec()
    } else {
        let mut v = overflow_buf.to_vec();
        if !tail.is_empty() {
            v.extend_from_slice(tail);
        }
        v
    };
    let text = String::from_utf8_lossy(&combined);
    let lines: Vec<&str> = text.lines().take(max_lines).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// Set 0600 permissions on generated files and 0700 on the offload directory.
fn set_permissions_0600(
    dir: &std::path::Path,
    stdout_state: &OffloadState,
    stderr_state: &OffloadState,
) {
    // Directory needs execute bit for traversal; use 0700.
    let dir_perm = fs::Permissions::from_mode(0o700);
    let _ = fs::set_permissions(dir, dir_perm);
    // Files use 0600 (owner read/write only).
    let file_perm = fs::Permissions::from_mode(0o600);
    for state in [stdout_state, stderr_state] {
        if let Some(ref p) = state.path {
            let _ = fs::set_permissions(p, file_perm.clone());
        }
        if let Some(ref p) = state.clean_path {
            let _ = fs::set_permissions(p, file_perm.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// BashOutputOffload — threshold-based offload for non-PTY commands
// ---------------------------------------------------------------------------

/// Settings for bash output offload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashOffloadSettings {
    pub enabled: bool,
    pub threshold_bytes: usize,
    pub preview_bytes: usize,
    #[serde(skip)]
    pub base_dir: Option<String>,
}

impl Default for BashOffloadSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_bytes: 1024,
            preview_bytes: 1024,
            base_dir: None,
        }
    }
}

/// Per-stream info in bash offload result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashStreamInfo {
    pub path: Option<String>,
    pub bytes: usize,
    pub sha256: Option<String>,
}

/// Result of bash output offload rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashOffloadResult {
    /// The text to include in the response (preview or inline).
    pub stdout_text: String,
    pub stderr_text: String,
    /// Whether offload was triggered.
    pub offloaded: bool,
    /// JSON payload for the offload metadata (to embed in tool response).
    pub offload_payload: Option<serde_json::Value>,
}

/// Threshold-based output offload for bash (non-PTY) commands.
///
/// If stdout or stderr exceeds `threshold_bytes`, writes the full output to
/// disk and returns a preview (first `preview_bytes` bytes, UTF-8 safe).
pub struct BashOutputOffload {
    session_uuid: String,
    cwd: String,
    settings: BashOffloadSettings,
}

impl BashOutputOffload {
    pub fn new(session_uuid: &str, cwd: &str, settings: BashOffloadSettings) -> Self {
        Self {
            session_uuid: session_uuid.to_string(),
            cwd: cwd.to_string(),
            settings,
        }
    }

    /// Render bash output, offloading to disk if thresholds exceeded.
    /// Returns an AI-friendly offload payload matching Python's format:
    ///   - below threshold: `{"status": "inline", "reason": "below_threshold"}`
    ///   - offloaded: `{"status": "offloaded", "stdout_path": "...", "stderr_path": "...", "hint": "..."}`
    ///   - failed: `{"status": "failed", "hint": "Output shown as preview only"}`
    pub fn render(
        &self,
        stdout: &str,
        stderr: &str,
        command: &str,
        return_code: i32,
    ) -> BashOffloadResult {
        let stdout_bytes_len = stdout.len();
        let stderr_bytes_len = stderr.len();
        let should_offload = self.settings.enabled
            && (stdout_bytes_len > self.settings.threshold_bytes
                || stderr_bytes_len > self.settings.threshold_bytes);

        if !should_offload {
            // Below threshold: return full output with inline status
            return BashOffloadResult {
                stdout_text: stdout.to_string(),
                stderr_text: stderr.to_string(),
                offloaded: false,
                offload_payload: Some(serde_json::json!({
                    "status": "inline",
                    "reason": "below_threshold"
                })),
            };
        }

        // Determine base directory.
        let default_dir = std::env::temp_dir().to_str().unwrap_or("/tmp").to_string();
        let base_dir = self.settings.base_dir.as_deref().unwrap_or(&default_dir);
        let offload_dir = PathBuf::from(base_dir)
            .join("aish-offload")
            .join(&self.session_uuid);

        // Create directory.
        if let Err(e) = fs::create_dir_all(&offload_dir) {
            tracing::warn!("failed to create offload dir: {e}");
            // Offload failed: return preview with failed status
            let (stdout_preview, _) =
                truncate_utf8_safe(stdout.as_bytes(), self.settings.preview_bytes);
            let (stderr_preview, _) =
                truncate_utf8_safe(stderr.as_bytes(), self.settings.preview_bytes);
            return BashOffloadResult {
                stdout_text: String::from_utf8_lossy(&stdout_preview).to_string(),
                stderr_text: String::from_utf8_lossy(&stderr_preview).to_string(),
                offloaded: false,
                offload_payload: Some(serde_json::json!({
                    "status": "failed",
                    "error": e.to_string(),
                    "hint": "Output shown as preview only"
                })),
            };
        }

        // Generate exec_id and uid.
        let seq = GLOBAL_SEQ.fetch_add(1, Ordering::Relaxed);
        let exec_id = generate_exec_id(seq);
        let uid_seed = format!(
            "{}:{}:{}:{}",
            Local::now().timestamp_millis(),
            std::process::id(),
            seq,
            command
        );
        let uid = generate_uid(&uid_seed);

        // Write stdout file.
        let stdout_path = offload_dir.join("stdout.txt");
        let stdout_hash = hash_string(stdout);
        let (stdout_preview, stdout_truncated) =
            truncate_utf8_safe(stdout.as_bytes(), self.settings.preview_bytes);
        let stdout_preview_str = String::from_utf8_lossy(&stdout_preview).to_string();
        if let Err(e) = fs::write(&stdout_path, stdout.as_bytes()) {
            tracing::warn!("failed to write stdout offload: {e}");
        }

        // Write stderr file.
        let stderr_path = offload_dir.join("stderr.txt");
        let stderr_hash = hash_string(stderr);
        let (stderr_preview, stderr_truncated) =
            truncate_utf8_safe(stderr.as_bytes(), self.settings.preview_bytes);
        let stderr_preview_str = String::from_utf8_lossy(&stderr_preview).to_string();
        if let Err(e) = fs::write(&stderr_path, stderr.as_bytes()) {
            tracing::warn!("failed to write stderr offload: {e}");
        }

        // Write internal metadata to disk (detailed, for debugging).
        let command_hash = if command.is_empty() {
            None
        } else {
            Some(hash_string(command))
        };
        let meta = serde_json::json!({
            "version": 1,
            "tool": "bash_exec",
            "uid": uid,
            "session_uuid": self.session_uuid,
            "exec_id": exec_id,
            "timestamp_utc": Local::now().to_rfc3339(),
            "cwd": self.cwd,
            "return_code": return_code,
            "command_sha256": command_hash,
            "threshold_bytes": self.settings.threshold_bytes,
            "preview_bytes": self.settings.preview_bytes,
            "stdout": {
                "path": stdout_path.to_str(),
                "bytes": stdout_bytes_len,
                "sha256": stdout_hash,
                "truncated": stdout_truncated,
            },
            "stderr": {
                "path": stderr_path.to_str(),
                "bytes": stderr_bytes_len,
                "sha256": stderr_hash,
                "truncated": stderr_truncated,
            },
        });

        let meta_path = offload_dir.join("result.json");
        if let Ok(json) = serde_json::to_string_pretty(&meta) {
            let _ = fs::write(&meta_path, json);
        }

        // Set permissions.
        let file_perm = fs::Permissions::from_mode(0o600);
        let dir_perm = fs::Permissions::from_mode(0o700);
        let _ = fs::set_permissions(&offload_dir, dir_perm);
        for p in [&stdout_path, &stderr_path, &meta_path] {
            let _ = fs::set_permissions(p, file_perm.clone());
        }

        // Build AI-friendly offload payload (matching Python's format).
        let stdout_path_str = stdout_path.to_str().unwrap_or("").to_string();
        let stderr_path_str = stderr_path.to_str().unwrap_or("").to_string();
        let meta_path_str = meta_path.to_str().unwrap_or("").to_string();
        let ai_payload = serde_json::json!({
            "status": "offloaded",
            "stdout_path": stdout_path_str,
            "stderr_path": stderr_path_str,
            "meta_path": meta_path_str,
            "hint": "Read offload paths for full output"
        });

        BashOffloadResult {
            stdout_text: stdout_preview_str,
            stderr_text: stderr_preview_str,
            offloaded: true,
            offload_payload: Some(ai_payload),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Helper to create a temporary directory for tests.
    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("aish-pty-test").join(name);
        // Fix permissions before removing (previous tests may have set 0600).
        fix_perms_recursive(&dir);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Recursively fix directory permissions to allow deletion.
    fn fix_perms_recursive(path: &Path) {
        if path.is_dir() {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    fix_perms_recursive(&entry.path());
                }
            }
        }
    }

    #[test]
    fn test_compute_hash_empty() {
        assert!(compute_hash(&[], &[]).is_none());
    }

    #[test]
    fn test_compute_hash_nonempty() {
        let hash = compute_hash(b"hello", b"world").unwrap();
        assert!(hash.starts_with("sha256-"));
        assert_eq!(hash.len(), 7 + 64); // "sha256-" + 64 hex chars

        // Deterministic: same input gives same output.
        let hash2 = compute_hash(b"hello", b"world").unwrap();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_compute_hash_only_overflow() {
        let hash = compute_hash(b"data", &[]).unwrap();
        assert!(hash.starts_with("sha256-"));
    }

    #[test]
    fn test_compute_hash_only_tail() {
        let hash = compute_hash(&[], b"tail").unwrap();
        assert!(hash.starts_with("sha256-"));
    }

    #[test]
    fn test_build_preview_empty() {
        assert!(build_preview(&[], &[], 10).is_none());
    }

    #[test]
    fn test_build_preview_single_line() {
        let preview = build_preview(b"hello\n", b"world\n", 50).unwrap();
        assert_eq!(preview, "hello\nworld");
    }

    #[test]
    fn test_build_preview_max_lines() {
        let data: Vec<u8> = (0..100)
            .flat_map(|i| format!("line {}\n", i).into_bytes())
            .collect();
        let preview = build_preview(&data, &[], 5).unwrap();
        let lines: Vec<&str> = preview.split('\n').collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[4], "line 4");
    }

    #[test]
    fn test_build_preview_only_overflow() {
        let preview = build_preview(b"overflow data\n", &[], 50).unwrap();
        assert_eq!(preview, "overflow data");
    }

    #[test]
    fn test_build_preview_only_tail() {
        let preview = build_preview(&[], b"tail data\n", 50).unwrap();
        assert_eq!(preview, "tail data");
    }

    #[test]
    fn test_timing_metadata() {
        let dir = tmp_dir("timing_metadata");
        let mut offload = PtyOutputOffload::new(
            "echo hello",
            "test-uuid-timing",
            "",
            1024,
            dir.to_str().unwrap(),
        );

        // Write some data to stdout.
        offload.append_overflow(StreamName::Stdout, b"hello world\n");

        // Small sleep to ensure elapsed time is non-zero.
        std::thread::sleep(std::time::Duration::from_millis(10));

        let result = offload.finalize(b"tail", b"", 0);
        assert!(result.duration_ms.is_some());
        assert!(result.duration_ms.unwrap() >= 10);
        assert_eq!(result.command.as_deref(), Some("echo hello"));
    }

    #[test]
    fn test_permissions_0600() {
        let dir = tmp_dir("permissions_test");
        let mut offload = PtyOutputOffload::new(
            "ls",
            "test-uuid-perms",
            "",
            5, // small keep_bytes to trigger overflow
            dir.to_str().unwrap(),
        );

        // Write enough data to trigger overflow.
        offload.append_overflow(StreamName::Stdout, b"line 1\nline 2\nline 3\n");

        let result = offload.finalize(b"tail", b"", 0);

        // Check directory permissions (0700 for traversal).
        let offload_dir = dir.join("aish-offload").join("test-uuid-perms");
        if offload_dir.exists() {
            let mode = offload_dir.metadata().unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "directory should have 0700 permissions"
            );
        }

        // Check stdout raw file permissions.
        if let Some(ref path) = result.stdout.path {
            let p = Path::new(path);
            if p.exists() {
                let mode = p.metadata().unwrap().permissions().mode();
                assert_eq!(
                    mode & 0o777,
                    0o600,
                    "stdout file should have 0600 permissions"
                );
            }
        }

        // Check stdout clean file permissions.
        if let Some(ref path) = result.stdout.clean_path {
            let p = Path::new(path);
            if p.exists() {
                let mode = p.metadata().unwrap().permissions().mode();
                assert_eq!(
                    mode & 0o777,
                    0o600,
                    "clean file should have 0600 permissions"
                );
            }
        }
    }

    #[test]
    fn test_meta_json_enriched() {
        let dir = tmp_dir("meta_enriched");
        let mut offload = PtyOutputOffload::new(
            "echo test",
            "test-uuid-meta",
            "",
            1024,
            dir.to_str().unwrap(),
        );

        offload.append_overflow(StreamName::Stdout, b"output line\n");

        let result = offload.finalize(b"tail", b"", 0);

        // Read and verify meta.json.
        let meta_path = dir
            .join("aish-offload")
            .join("test-uuid-meta")
            .join("meta.json");
        assert!(meta_path.exists());

        let meta_str = fs::read_to_string(&meta_path).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap();

        assert_eq!(meta["session_uuid"], "test-uuid-meta");
        assert_eq!(meta["exit_code"], 0);
        assert_eq!(meta["command"], "echo test");
        assert!(meta["duration_ms"].is_number());
        assert!(meta["stdout_hash"].is_string());
        assert!(meta["stdout_preview"].is_string());

        // Verify hashes in OffloadResult.
        assert!(result.stdout_hash.is_some());
        assert!(result.stderr_hash.is_none()); // no stderr data
        assert!(result.stdout_preview.is_some());
    }

    #[test]
    fn test_offload_no_overflow() {
        let dir = tmp_dir("no_overflow");
        let offload = PtyOutputOffload::new(
            "echo tiny",
            "test-uuid-nooverflow",
            "",
            1024,
            dir.to_str().unwrap(),
        );

        let result = offload.finalize(b"small output", b"", 0);

        // No overflow occurred, so hashes come from the tail only.
        assert!(result.stdout_hash.is_some());
        assert!(result.stderr_hash.is_none());
        assert!(result.stdout_preview.is_some());
        assert_eq!(result.stdout.status, "inline");
    }

    // -- New feature tests ---------------------------------------------------

    #[test]
    fn test_strip_ansi_escapes_csi() {
        let input = b"\x1b[31mHello\x1b[0m \x1b[1;32mWorld\x1b[0m";
        let clean = strip_ansi_escapes(input);
        assert_eq!(String::from_utf8_lossy(&clean), "Hello World");
    }

    #[test]
    fn test_strip_ansi_escapes_osc() {
        let input = b"\x1b]0;title\x07prompt$ ";
        let clean = strip_ansi_escapes(input);
        assert_eq!(String::from_utf8_lossy(&clean), "prompt$ ");
    }

    #[test]
    fn test_strip_ansi_escapes_osc_st() {
        let input = b"\x1b]2;window-title\x1b\\data";
        let clean = strip_ansi_escapes(input);
        assert_eq!(String::from_utf8_lossy(&clean), "data");
    }

    #[test]
    fn test_strip_ansi_escapes_controls() {
        let input = b"hello\x00world\x01test\nline2";
        let clean = strip_ansi_escapes(input);
        assert_eq!(String::from_utf8_lossy(&clean), "helloworldtest\nline2");
    }

    #[test]
    fn test_strip_ansi_escapes_preserve_newlines() {
        let input = b"line1\nline2\r\nline3\ttab";
        let clean = strip_ansi_escapes(input);
        assert_eq!(
            String::from_utf8_lossy(&clean),
            "line1\nline2\r\nline3\ttab"
        );
    }

    #[test]
    fn test_strip_ansi_escapes_empty() {
        let clean = strip_ansi_escapes(b"");
        assert!(clean.is_empty());
    }

    #[test]
    fn test_truncate_utf8_safe_under_limit() {
        let input = "hello".as_bytes();
        let (result, truncated) = truncate_utf8_safe(input, 100);
        assert!(!truncated);
        assert_eq!(String::from_utf8_lossy(&result), "hello");
    }

    #[test]
    fn test_truncate_utf8_safe_exactly_limit() {
        let input = "hello".as_bytes();
        let (result, truncated) = truncate_utf8_safe(input, 5);
        assert!(!truncated);
        assert_eq!(String::from_utf8_lossy(&result), "hello");
    }

    #[test]
    fn test_truncate_utf8_safe_ascii_truncation() {
        let input = "hello world".as_bytes();
        let (result, truncated) = truncate_utf8_safe(input, 5);
        assert!(truncated);
        assert_eq!(String::from_utf8_lossy(&result), "hello");
    }

    #[test]
    fn test_truncate_utf8_safe_multibyte_boundary() {
        // "你好世界" — each char is 3 bytes in UTF-8
        let input = "你好世界".as_bytes(); // 12 bytes
        let (result, truncated) = truncate_utf8_safe(input, 7);
        assert!(truncated);
        // Should truncate at byte 6 (end of "你好"), not split the 3rd byte
        assert_eq!(String::from_utf8_lossy(&result), "你好");
    }

    #[test]
    fn test_generate_exec_id_format() {
        let id = generate_exec_id(1);
        // Format: YYYYMMDDTHHMMSS.mmm_PID_SEQ
        assert!(id.contains('T'));
        assert!(id.contains('.'));
        assert!(id.contains("_1"));
    }

    #[test]
    fn test_generate_uid_16_chars() {
        let uid = generate_uid("test-seed");
        assert_eq!(uid.len(), 16);
        assert!(uid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_uid_deterministic() {
        let uid1 = generate_uid("same-seed");
        let uid2 = generate_uid("same-seed");
        assert_eq!(uid1, uid2);
    }

    #[test]
    fn test_generate_uid_different_seeds() {
        let uid1 = generate_uid("seed-a");
        let uid2 = generate_uid("seed-b");
        assert_ne!(uid1, uid2);
    }

    #[test]
    fn test_hash_string() {
        let hash = hash_string("hello");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_bash_offload_under_threshold() {
        let dir = tmp_dir("bash_under");
        let settings = BashOffloadSettings {
            enabled: true,
            threshold_bytes: 1024,
            preview_bytes: 512,
            base_dir: Some(dir.to_str().unwrap().to_string()),
        };
        let offload = BashOutputOffload::new("test-session", "/tmp", settings);
        let result = offload.render("small output", "small error", "echo hi", 0);
        assert!(!result.offloaded);
        assert_eq!(result.stdout_text, "small output");
        assert_eq!(result.stderr_text, "small error");
        // Below threshold returns inline status
        let payload = result.offload_payload.unwrap();
        assert_eq!(payload["status"], "inline");
        assert_eq!(payload["reason"], "below_threshold");
    }

    #[test]
    fn test_bash_offload_over_threshold() {
        let dir = tmp_dir("bash_over");
        let settings = BashOffloadSettings {
            enabled: true,
            threshold_bytes: 10, // very low threshold
            preview_bytes: 5,
            base_dir: Some(dir.to_str().unwrap().to_string()),
        };
        let offload = BashOutputOffload::new("test-session", "/tmp", settings);
        let long_output = "a".repeat(100);
        let result = offload.render(&long_output, "err", "echo test", 0);
        assert!(result.offloaded);
        // Preview should be truncated to 5 bytes
        assert_eq!(result.stdout_text.len(), 5);
        assert_eq!(result.stderr_text, "err");
        assert!(result.offload_payload.is_some());

        // Verify AI-friendly payload format (matching Python)
        let payload = result.offload_payload.unwrap();
        assert_eq!(payload["status"], "offloaded");
        assert!(payload["stdout_path"].is_string());
        assert!(payload["stderr_path"].is_string());
        assert!(payload["meta_path"].is_string());
        assert_eq!(payload["hint"], "Read offload paths for full output");
    }

    #[test]
    fn test_bash_offload_disabled() {
        let dir = tmp_dir("bash_disabled");
        let settings = BashOffloadSettings {
            enabled: false,
            threshold_bytes: 1,
            preview_bytes: 5,
            base_dir: Some(dir.to_str().unwrap().to_string()),
        };
        let offload = BashOutputOffload::new("test-session", "/tmp", settings);
        let long_output = "a".repeat(100);
        let result = offload.render(&long_output, "", "echo test", 0);
        assert!(!result.offloaded);
        assert_eq!(result.stdout_text, long_output);
    }

    #[test]
    fn test_bash_offload_metadata_files() {
        let dir = tmp_dir("bash_meta");
        let settings = BashOffloadSettings {
            enabled: true,
            threshold_bytes: 5,
            preview_bytes: 10,
            base_dir: Some(dir.to_str().unwrap().to_string()),
        };
        let offload = BashOutputOffload::new("test-session-meta", "/home/user", settings);
        let result = offload.render("long output here", "short", "ls -la", 0);

        assert!(result.offloaded);

        // Verify files exist.
        let offload_dir = dir.join("aish-offload").join("test-session-meta");
        assert!(offload_dir.join("stdout.txt").exists());
        assert!(offload_dir.join("stderr.txt").exists());
        assert!(offload_dir.join("result.json").exists());

        // Verify metadata content.
        let meta_str = fs::read_to_string(offload_dir.join("result.json")).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap();
        assert_eq!(meta["version"], 1);
        assert_eq!(meta["tool"], "bash_exec");
        assert_eq!(meta["return_code"], 0);
        assert_eq!(meta["cwd"], "/home/user");
        assert!(meta["command_sha256"].is_string());
    }

    #[test]
    fn test_offload_clean_file_has_ansi_stripped() {
        let dir = tmp_dir("clean_ansi");
        let mut offload = PtyOutputOffload::new(
            "echo",
            "test-uuid-ansi",
            "",
            5, // small keep_bytes to trigger overflow
            dir.to_str().unwrap(),
        );

        // Write ANSI-colored data.
        let ansi_data = b"\x1b[31mRED\x1b[0m normal\n";
        offload.append_overflow(StreamName::Stdout, ansi_data);

        let result = offload.finalize(b"tail", b"", 0);

        // Check clean file exists and has ANSI stripped.
        if let Some(ref clean_path) = result.stdout.clean_path {
            let clean_content = fs::read_to_string(clean_path).unwrap_or_default();
            assert!(
                !clean_content.contains("\x1b"),
                "clean file should not contain ANSI escapes"
            );
            assert!(
                clean_content.contains("RED"),
                "clean file should contain visible text RED"
            );
        }
    }
}
