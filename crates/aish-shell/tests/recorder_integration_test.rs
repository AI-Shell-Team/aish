// Integration tests for the asciinema v2 recording lifecycle.
//
// These tests verify the complete Recorder workflow: start -> write events ->
// stop -> verify the .cast file structure is valid for asciinema-compatible
// players.

use std::fs;
use std::io::{BufRead, BufReader};

use aish_shell::recorder::Recorder;

/// Test the full Recorder lifecycle: create -> record output -> record input ->
/// flush -> drop -> verify the resulting .cast file.
#[test]
fn test_full_recording_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lifecycle.cast");

    // Create recorder
    let mut recorder = Recorder::new(path.clone(), (80, 24)).unwrap();

    // Record some output
    recorder.record_output("$ ls\r\n");
    recorder.record_output("file1.txt  file2.txt\r\n");

    // Record user input (also creates echo output event)
    recorder.record_input("echo hello\r\n");

    // Record more output
    recorder.record_output("hello\r\n");

    // Flush and drop (simulates stop)
    recorder.flush().unwrap();
    drop(recorder);

    // Verify the .cast file
    let file = fs::File::open(&path).unwrap();
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map(|l| l.unwrap()).collect();

    // Should have: 1 header + 5 events (3 output + 1 input + 1 echo output) = 6 lines
    assert_eq!(lines.len(), 6);

    // Verify header
    let header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(header["version"], 2);
    assert_eq!(header["width"], 80);
    assert_eq!(header["height"], 24);

    // Verify event 1: output with "ls"
    let e1: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert!(e1.is_array());
    assert_eq!(e1[1], "o");
    assert!(e1[2].as_str().unwrap().contains("ls"));

    // Verify event 2: output with "file1"
    let e2: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    assert!(e2.is_array());
    assert_eq!(e2[1], "o");
    assert!(e2[2].as_str().unwrap().contains("file1"));

    // Verify event 3: input with "echo hello"
    let e3: serde_json::Value = serde_json::from_str(&lines[3]).unwrap();
    assert!(e3.is_array());
    assert_eq!(e3[1], "i");
    assert!(e3[2].as_str().unwrap().contains("echo hello"));

    // Verify event 4: echo output from record_input
    let e4: serde_json::Value = serde_json::from_str(&lines[4]).unwrap();
    assert!(e4.is_array());
    assert_eq!(e4[1], "o");
    assert!(e4[2].as_str().unwrap().contains("echo hello"));

    // Verify event 5: output with "hello"
    let e5: serde_json::Value = serde_json::from_str(&lines[5]).unwrap();
    assert!(e5.is_array());
    assert_eq!(e5[1], "o");
    assert!(e5[2].as_str().unwrap().contains("hello"));
}

/// Verify that the generated .cast file has a valid structure that
/// asciinema-compatible players would accept.
#[test]
fn test_cast_file_playable_by_asciinema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("playable.cast");

    let mut recorder = Recorder::new(path.clone(), (120, 30)).unwrap();
    recorder.record_output("Hello\r\n");
    recorder.record_output("World\r\n");
    recorder.flush().unwrap();
    drop(recorder);

    let content = fs::read_to_string(&path).unwrap();

    // Each line should be valid JSON
    for line in content.lines() {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("Invalid JSON in .cast file: {:?}\nLine: {:?}", e, line));
        assert!(parsed.is_object() || parsed.is_array());
    }

    // First line should be the header object
    let header: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    assert_eq!(header["version"], 2);
    assert!(header.get("width").is_some());
    assert!(header.get("height").is_some());
    assert!(header.get("timestamp").is_some());
    assert!(header.get("env").is_some());

    // Event lines should be arrays of [timestamp, type, payload]
    for line in content.lines().skip(1) {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(event.is_array(), "Event should be a JSON array");
        assert_eq!(
            event.as_array().unwrap().len(),
            3,
            "Event should have 3 elements"
        );
        assert!(event[0].is_number(), "Timestamp should be a number");
        assert!(event[1].is_string(), "Event type should be a string");
        assert!(event[2].is_string(), "Payload should be a string");
        let event_type = event[1].as_str().unwrap();
        assert!(
            event_type == "o" || event_type == "i",
            "Event type should be 'o' or 'i', got: {:?}",
            event_type
        );
    }
}

/// Test that timestamps in the .cast file are non-negative and events are
/// ordered chronologically.
#[test]
fn test_recording_timestamps_are_ordered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ordered.cast");

    let mut recorder = Recorder::new(path.clone(), (100, 40)).unwrap();
    recorder.record_output("first\n");
    recorder.record_output("second\n");
    recorder.record_output("third\n");
    recorder.flush().unwrap();
    drop(recorder);

    let content = fs::read_to_string(&path).unwrap();
    let mut prev_ts: f64 = -1.0;

    for line in content.lines().skip(1) {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        let ts = event[0].as_f64().unwrap();
        assert!(ts >= 0.0, "Timestamp should be non-negative, got: {}", ts);
        assert!(
            ts >= prev_ts,
            "Timestamps should be non-decreasing: {} < {}",
            ts,
            prev_ts
        );
        prev_ts = ts;
    }
}

/// Test that the recorder correctly handles a mix of output and input events
/// in an interleaved pattern, mimicking a real shell session.
#[test]
fn test_interleaved_input_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interleaved.cast");

    let mut recorder = Recorder::new(path.clone(), (80, 24)).unwrap();

    // Simulate a brief interactive session
    recorder.record_output("$ "); // prompt
    recorder.record_input("date\n"); // user types (also creates echo output)
    recorder.record_output("Mon Jan  1 00:00:00 UTC 2024\n"); // output
    recorder.record_output("$ "); // next prompt
    recorder.record_input("echo done\n"); // user types (also creates echo output)
    recorder.record_output("done\n"); // output

    recorder.flush().unwrap();
    drop(recorder);

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // 1 header + 8 events (prompt, input, echo, output, prompt, input, echo, output)
    assert_eq!(lines.len(), 9);

    // Verify event types: o, i, o(echo), o, o, i, o(echo), o
    let expected_types = ["o", "i", "o", "o", "o", "i", "o", "o"];
    for (i, expected) in expected_types.iter().enumerate() {
        let event: serde_json::Value = serde_json::from_str(lines[i + 1]).unwrap();
        assert_eq!(
            event[1], *expected,
            "Event {} should be type {:?}",
            i, expected
        );
    }
}

/// Test that special characters (newlines, tabs, quotes, unicode) are properly
/// preserved in the .cast file payload. Newlines in output are normalized to \r\n.
#[test]
fn test_special_characters_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("special.cast");

    let special_output = "tab\there\nnewline\n\"quotes\"\nunicode: \u{1f600}";
    let special_input = "input with \t tabs and \"quotes\"";

    let mut recorder = Recorder::new(path.clone(), (80, 24)).unwrap();
    recorder.record_output(special_output);
    recorder.record_input(special_input);
    recorder.flush().unwrap();
    drop(recorder);

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // Verify output payload has normalized newlines (\n → \r\n)
    let out_event: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let expected_output = "tab\there\r\nnewline\r\n\"quotes\"\r\nunicode: \u{1f600}";
    assert_eq!(out_event[2].as_str().unwrap(), expected_output);

    // Verify input payload matches exactly (input is not transformed)
    let in_event: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(in_event[2].as_str().unwrap(), special_input);

    // Verify echo output from record_input
    let echo_event: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(echo_event[1], "o");
    assert!(echo_event[2].as_str().unwrap().contains("input with"));
}

/// Test that recorder metadata (file_path, term_size) is accessible.
#[test]
fn test_recorder_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.cast");

    let recorder = Recorder::new(path.clone(), (200, 50)).unwrap();

    assert_eq!(recorder.file_path(), path.as_path());
    // Elapsed should be very small right after creation
    assert!(recorder.elapsed().as_secs() < 1);
}
