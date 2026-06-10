//! Terminal session recorder using asciinema v2 format.
//!
//! Produces `.cast` files that can be replayed with `asciinema play`
//! or any asciinema-compatible player.

use std::fs::{self, File};
use std::io::{self, LineWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Local;
use serde_json::json;

#[cfg(test)]
use serde_json::Value;

/// Thread-safe shared recorder handle.
///
/// All `ShellRenderer` instances (shared or temporary) can hold a clone of this
/// Arc and write events through the same Mutex-protected Recorder. This ensures
/// that recording captures output from every code path, not just the main renderer.
pub type SharedRecorder = Arc<Mutex<Option<Recorder>>>;

/// Create a new SharedRecorder (initially inactive, contains None).
pub fn new_shared_recorder() -> SharedRecorder {
    Arc::new(Mutex::new(None))
}

/// Record an output event if a shared recorder is active.
pub fn shared_record_output(recorder: &SharedRecorder, data: &str) {
    if let Ok(mut guard) = recorder.lock() {
        if let Some(ref mut rec) = *guard {
            rec.record_output(data);
        }
    }
}

/// Record an input event if a shared recorder is active.
pub fn shared_record_input(recorder: &SharedRecorder, data: &str) {
    if let Ok(mut guard) = recorder.lock() {
        if let Some(ref mut rec) = *guard {
            rec.record_input(data);
        }
    }
}

/// Records terminal input/output events in asciinema v2 format.
///
/// # Asciinema v2 format
///
/// Line 1 (header): JSON object with `version`, `width`, `height`, `timestamp`, `env`.
/// Line 2+ (events): JSON array `[timestamp, type, payload]` where:
/// - timestamp: float seconds since recording started
/// - type: `"o"` for output, `"i"` for input
/// - payload: raw text, JSON-string escaped
pub struct Recorder {
    start_time: Instant,
    writer: LineWriter<File>,
    file_path: PathBuf,
}

impl Recorder {
    /// Create a new recorder that writes asciinema v2 events to `file_path`.
    ///
    /// Creates parent directories if they do not exist, opens the file, and
    /// writes the v2 header line.
    pub fn new(file_path: PathBuf, term_size: (u16, u16)) -> io::Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = File::create(&file_path)?;
        let mut writer = LineWriter::new(file);

        let unix_ts = Local::now().timestamp();
        let header = json!({
            "version": 2,
            "width": term_size.0,
            "height": term_size.1,
            "timestamp": unix_ts,
            "env": {
                "SHELL": "/bin/bash",
                "TERM": "xterm-256color",
            }
        });
        writeln!(writer, "{header}")?;

        Ok(Self {
            start_time: Instant::now(),
            writer,
            file_path,
        })
    }

    /// Record an output event (newlines normalized to \r\n, ANSI colors preserved).
    pub fn record_output(&mut self, data: &str) {
        let normalized = normalize_newlines(data);
        self.write_event("o", &normalized);
    }

    /// Record an input event, echoing non-empty text as output for replay.
    /// Empty input (bare Enter / Ctrl+C) is not echoed — the prompt
    /// re-display already handles line positioning via `\r\x1b[2K`,
    /// avoiding spurious blank lines in GIF output.
    pub fn record_input(&mut self, data: &str) {
        self.write_event("i", data);
        if !data.trim().is_empty() {
            let echoed = normalize_newlines(data);
            self.write_event("o", &echoed);
        }
    }

    /// Return the file path this recorder writes to.
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// Return the time elapsed since this recorder was created.
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Return the default recordings directory: `~/.local/share/aish/recordings/`
    pub fn recordings_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aish")
            .join("recordings")
    }

    /// Generate a timestamped file path under the recordings directory.
    ///
    /// Format: `~/.local/share/aish/recordings/2024-01-15_14-30-00.cast`
    pub fn generate_file_path() -> PathBuf {
        let filename = format!("{}.cast", Local::now().format("%Y-%m-%d_%H-%M-%S"));
        Self::recordings_dir().join(filename)
    }

    /// Flush buffered events to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    fn write_event(&mut self, event_type: &str, data: &str) {
        let ts = self.start_time.elapsed().as_secs_f64();
        let event = json!([ts, event_type, data]);
        // Best-effort write — recording should not crash the shell on I/O errors,
        // but we log failures so users know if recording is corrupted.
        if let Err(e) = writeln!(self.writer, "{event}") {
            eprintln!("Recording write failed: {}", e);
        }
    }
}

/// Normalize standalone `\n` to `\r\n` so virtual terminals (agg, asciinema)
/// correctly return the cursor to column 0 on each line break.
/// Preserves standalone `\r` used for cursor positioning in ANSI sequences.
fn normalize_newlines(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + s.len() / 8);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    result.push_str("\r\n");
                    chars.next();
                } else {
                    result.push('\r');
                }
            }
            '\n' => {
                result.push_str("\r\n");
            }
            _ => {
                result.push(c);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use tempfile::TempDir;

    /// Helper: create a Recorder writing into a temp directory.
    fn make_recorder(dir: &TempDir) -> Recorder {
        let path = dir.path().join("test.cast");
        Recorder::new(path, (80, 24)).expect("failed to create recorder")
    }

    /// Helper: read all lines from the recorder's output file.
    fn read_lines(recorder: &Recorder) -> Vec<String> {
        let content = fs::read_to_string(recorder.file_path()).expect("read file");
        content.lines().map(|l| l.to_string()).collect()
    }

    // 1. Header is valid JSON with expected fields
    #[test]
    fn test_recorder_creates_file_with_valid_header() {
        let dir = TempDir::new().unwrap();
        let rec = make_recorder(&dir);
        let lines = read_lines(&rec);

        assert!(lines.len() >= 1, "should have at least a header line");

        let header: Value = serde_json::from_str(&lines[0]).expect("header should be valid JSON");
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 80);
        assert_eq!(header["height"], 24);
        assert!(header["timestamp"].is_number());
        assert_eq!(header["env"]["SHELL"], "/bin/bash");
        assert_eq!(header["env"]["TERM"], "xterm-256color");
    }

    // 2. Output events are written correctly
    #[test]
    fn test_recorder_records_output_events() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cast");
        {
            let mut rec = Recorder::new(path.clone(), (80, 24)).unwrap();
            rec.record_output("Hello");
            rec.record_output("World");
        } // drop flushes

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 events

        let evt1: Vec<Value> = serde_json::from_str(lines[1]).unwrap();
        assert!(evt1[0].is_number());
        assert_eq!(evt1[1], "o");
        assert_eq!(evt1[2].as_str().unwrap(), "Hello");

        let evt2: Vec<Value> = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(evt2[1], "o");
        assert_eq!(evt2[2].as_str().unwrap(), "World");
    }

    // 3. Input events are written with echo (input + output pair)
    #[test]
    fn test_recorder_records_input_events() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cast");
        {
            let mut rec = Recorder::new(path.clone(), (80, 24)).unwrap();
            rec.record_input("ls\n");
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3); // header + input event + echo output event

        let evt_i: Vec<Value> = serde_json::from_str(lines[1]).unwrap();
        assert!(evt_i[0].is_number());
        assert_eq!(evt_i[1], "i");
        assert_eq!(evt_i[2].as_str().unwrap(), "ls\n");

        let evt_o: Vec<Value> = serde_json::from_str(lines[2]).unwrap();
        assert!(evt_o[0].is_number());
        assert_eq!(evt_o[1], "o");
        assert_eq!(evt_o[2].as_str().unwrap(), "ls\r\n");
    }

    // 4. Timestamps increase across events
    #[test]
    fn test_recorder_timestamps_increase() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cast");
        {
            let mut rec = Recorder::new(path.clone(), (80, 24)).unwrap();
            rec.record_output("first");
            thread::sleep(Duration::from_millis(50));
            rec.record_output("second");
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        let evt1: Vec<Value> = serde_json::from_str(lines[1]).unwrap();
        let evt2: Vec<Value> = serde_json::from_str(lines[2]).unwrap();

        let t1 = evt1[0].as_f64().unwrap();
        let t2 = evt2[0].as_f64().unwrap();
        assert!(
            t2 > t1,
            "second event timestamp {t2} should be > first {t1}"
        );
    }

    // 5. Special characters are properly JSON-escaped, \n normalized to \r\n
    #[test]
    fn test_recorder_escapes_special_characters() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cast");
        {
            let mut rec = Recorder::new(path.clone(), (80, 24)).unwrap();
            rec.record_output("line1\nline2\ttab\"quote");
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        let evt: Vec<Value> = serde_json::from_str(lines[1]).unwrap();
        let payload = evt[2].as_str().unwrap();
        assert_eq!(payload, "line1\r\nline2\ttab\"quote");
    }

    // 6. Parent directories are created automatically
    #[test]
    fn test_recorder_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c").join("test.cast");
        {
            let _rec = Recorder::new(nested.clone(), (80, 24)).unwrap();
        }
        assert!(nested.exists(), "file should exist at nested path");
    }

    // 7. generate_file_path contains "recordings" and ends with ".cast"
    #[test]
    fn test_generate_file_path() {
        let path = Recorder::generate_file_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("recordings"),
            "path should contain 'recordings': {path_str}"
        );
        assert!(
            path_str.ends_with(".cast"),
            "path should end with '.cast': {path_str}"
        );
    }

    // 8. A recorder that is immediately dropped produces only the header
    #[test]
    fn test_empty_recording() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cast");
        {
            let _rec = Recorder::new(path.clone(), (80, 24)).unwrap();
        } // drop immediately

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "empty recording should have only the header"
        );

        let header: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["version"], 2);
    }

    // 9. ANSI colors preserved, \n normalized to \r\n
    #[test]
    fn test_recorder_preserves_ansi_and_normalizes_newlines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cast");
        {
            let mut rec = Recorder::new(path.clone(), (80, 24)).unwrap();
            rec.record_output("\x1b[31mred\x1b[0m plain \x1b[1;32mbold green\x1b[0m");
            rec.record_output("\x1b[2mdim\nline2\x1b[0m");
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        let evt1: Vec<Value> = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            evt1[2].as_str().unwrap(),
            "\x1b[31mred\x1b[0m plain \x1b[1;32mbold green\x1b[0m"
        );

        let evt2: Vec<Value> = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(evt2[2].as_str().unwrap(), "\x1b[2mdim\r\nline2\x1b[0m");
    }

    // 10. normalize_newlines converts \n to \r\n, preserves standalone \r
    #[test]
    fn test_normalize_newlines() {
        use super::normalize_newlines;

        assert_eq!(normalize_newlines("line1\nline2\n"), "line1\r\nline2\r\n");
        assert_eq!(
            normalize_newlines("line1\r\nline2\r\n"),
            "line1\r\nline2\r\n"
        );
        assert_eq!(normalize_newlines("no newline"), "no newline");
        assert_eq!(normalize_newlines(""), "");
        // Standalone \r is preserved (used in ANSI cursor sequences)
        assert_eq!(normalize_newlines("\r\x1b[2Kprompt"), "\r\x1b[2Kprompt");
    }
}
