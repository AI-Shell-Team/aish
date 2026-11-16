//! Session recording (#586 hardening): append-only terminal capture.
//!
//! The daemon sits between the PTY and every client, so recording output is
//! a single hook at the PTY-read site: every byte that leaves the shell goes
//! to `<session>.cast` in the sessions directory before (or in addition to)
//! being broadcast. Format: asciinema v2-compatible JSON lines — one
//! `{"t": <seconds-since-start>, "event": "o", "data": <string>}` per write.
//! Lossless for UTF-8; invalid byte sequences are lossy-replaced at read time
//! (the raw bytes are kept in the line's `raw` field when they are not valid
//! UTF-8).
//!
//! Recording is always on at a bounded cost: one append per PTY read, no
//! buffering, no fsync (crash may lose the last writes, acceptable for a
//! forensic trail). Files are pruned to the most recent 20 sessions.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

/// Max bytes per recording before rotation drops the oldest segment.
/// Bounds disk usage for a single chatty session (output is also bounded by
/// file count via `prune_recordings`).
pub const MAX_RECORDING_BYTES: u64 = 64 * 1024 * 1024;

pub struct SessionRecorder {
    session_id: String,
    file: Option<std::fs::File>,
    started: Instant,
    written: u64,
}

impl SessionRecorder {
    /// Start recording for a session. Returns a recorder even if the file
    /// cannot be opened — recording failures never break the session.
    pub fn start(session_id: &str) -> Self {
        let written = Self::cast_size(session_id);
        let file = Self::open_append(session_id);
        Self {
            session_id: session_id.to_string(),
            file,
            started: Instant::now(),
            written,
        }
    }

    pub fn cast_path(session_id: &str) -> Option<PathBuf> {
        crate::pty_session_dir()
            .ok()
            .map(|d| d.join(format!("{session_id}.cast")))
    }

    fn open_append(session_id: &str) -> Option<std::fs::File> {
        let path = Self::cast_path(session_id)?;
        OpenOptions::new().create(true).append(true).open(path).ok()
    }

    /// Record an output chunk. `data` is raw PTY bytes. When the recording
    /// exceeds MAX_RECORDING_BYTES it is rotated: the existing file is
    /// renamed to `<id>.cast.1` (replacing an older rotation) and a fresh
    /// file starts. Replay reads the current segment; older segments remain
    /// on disk until the count-based prune collects them.
    pub fn record_output(&mut self, data: &[u8]) {
        if self.written + data.len() as u64 > MAX_RECORDING_BYTES {
            self.rotate();
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let t = self.started.elapsed().as_secs_f64();
        let line = match std::str::from_utf8(data) {
            Ok(text) => serde_json::json!({"t": t, "event": "o", "data": text}),
            Err(_) => {
                // Keep raw bytes for lossless forensics; the replay path
                // prefers `raw_b64` over `data`.
                use base64::Engine as _;
                serde_json::json!({
                    "t": t,
                    "event": "o",
                    "data": String::from_utf8_lossy(data),
                    "raw_b64": base64::engine::general_purpose::STANDARD.encode(data),
                })
            }
        };
        let _ = writeln!(file, "{line}");
        self.written += data.len() as u64;
    }

    /// Rotate the current recording: rename to `.cast.1` and start fresh.
    fn rotate(&mut self) {
        if let Some(path) = Self::cast_path(&self.session_id) {
            let rotated = path.with_extension("cast.1");
            let _ = std::fs::rename(&path, &rotated);
            self.file = Self::open_append(&self.session_id);
            self.written = 0;
        }
    }

    fn cast_size(session_id: &str) -> u64 {
        Self::cast_path(session_id)
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Re-encode a base64 string to bytes (replay helper).
pub fn decode_raw_b64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// List recordings, newest first, with sizes.
pub fn list_recordings() -> Vec<(String, u64)> {
    let Ok(dir) = crate::pty_session_dir() else {
        return Vec::new();
    };
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let sid = name.strip_suffix(".cast")?.to_string();
            let size = e.metadata().ok()?.len();
            Some((sid, size))
        })
        .collect();
    out.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    out
}

/// Prune recordings: keep the `keep` most recently modified. Rotation
/// segments (`.cast.1`) are removed together with their current `.cast`
/// file, and orphaned segments (no matching `.cast`) are collected too.
pub fn prune_recordings(keep: usize) {
    let Ok(dir) = crate::pty_session_dir() else {
        return;
    };
    let mut entries: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            if e.file_name().to_string_lossy().ends_with(".cast") {
                Some((e.path(), e.metadata().ok()?.modified().ok()?))
            } else {
                None
            }
        })
        .collect();
    if entries.len() > keep {
        entries.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
        for (path, _) in entries.into_iter().skip(keep) {
            // Remove the rotated segment alongside its current file so a
            // pruned session leaves nothing behind.
            let _ = std::fs::remove_file(path.with_extension("cast.1"));
            let _ = std::fs::remove_file(path);
        }
    }
    // Orphaned rotation segments: current .cast already gone (e.g. pruned
    // by an older build, or removed manually).
    let mut orphans: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".cast.1") {
                let current = dir.join(format!("{}.cast", name.trim_end_matches(".cast.1")));
                if !current.exists() {
                    return Some(e.path());
                }
            }
            None
        })
        .collect();
    for path in orphans.drain(..) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_list_roundtrip() {
        let sid = "rec-test-0000";
        let mut rec = SessionRecorder::start(sid);
        rec.record_output(b"hello ");
        rec.record_output(&[0xff, 0xfe]); // invalid UTF-8 → raw_b64 path
        rec.record_output(b"world\n");
        let list = list_recordings();
        assert!(list.iter().any(|(s, _)| s == sid));
        let path = SessionRecorder::cast_path(sid).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let mid: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert!(mid.get("raw_b64").is_some());
        let raw = decode_raw_b64(mid["raw_b64"].as_str().unwrap()).unwrap();
        assert_eq!(raw, vec![0xff, 0xfe]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn monotonic_timestamps() {
        let sid = "rec-test-0001";
        let mut rec = SessionRecorder::start(sid);
        rec.record_output(b"a");
        rec.record_output(b"b");
        let path = SessionRecorder::cast_path(sid).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let ts: Vec<f64> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("t").and_then(|t| t.as_f64()))
            .collect();
        assert_eq!(ts.len(), 2);
        assert!(ts[1] >= ts[0]);
        std::fs::remove_file(path).ok();
    }
}
