use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;
use crate::fs::{atomic_write, SharedSnapshotStore, SnapshotOp, SnapshotTag};
use std::path::Path;

/// Upper bound on file size for the full-file edit path. Larger files are
/// rejected; `edit_file` with start_line/end_line can still edit them by
/// confining the replacement to the requested line range.
const MAX_FILE_SIZE: u64 = 256 * 1024; // 256 KiB

/// Upper bound on the prior content stored for /undo rollback. A full-file
/// edit whose content exceeds this is still applied but not snapshotted.
const SNAPSHOT_MAX_BYTES: usize = 32 * 1024; // 32 KiB

/// Edit file tool (string replacement).
pub struct EditFileTool {
    store: Option<SharedSnapshotStore>,
}

impl Default for EditFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EditFileTool {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: SharedSnapshotStore) -> Self {
        Self { store: Some(store) }
    }
}

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolResult::error(aish_i18n::t("tools.fs.edit_file.missing_path")),
        };
        let old = match args.get("old_string").and_then(|o| o.as_str()) {
            Some(o) => o,
            None => {
                return ToolResult::error(aish_i18n::t("tools.fs.edit_file.missing_old_string"))
            }
        };
        let new = match args.get("new_string").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => {
                return ToolResult::error(aish_i18n::t("tools.fs.edit_file.missing_new_string"))
            }
        };
        let replace_all = args
            .get("replace_all")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);

        let tag = args.get("tag").and_then(|t| t.as_str());
        let start_line = args.get("start_line").and_then(|v| v.as_u64());
        let end_line = args.get("end_line").and_then(|v| v.as_u64());

        // Route to the line-range edit path when start_line is provided.
        if let Some(start) = start_line {
            let end = end_line.unwrap_or(start);
            return self.edit_line_range(path, old, new, start, end, replace_all, tag);
        }

        // --- Full-file edit path (original behavior, raised limit) ---

        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.edit_read_failed",
                    &args_map,
                ));
            }
        };
        if metadata.len() > MAX_FILE_SIZE {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            args_map.insert("size".to_string(), metadata.len().to_string());
            args_map.insert("limit".to_string(), MAX_FILE_SIZE.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.edit_file.file_too_large",
                &args_map,
            ));
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.edit_read_failed",
                    &args_map,
                ));
            }
        };

        let count = content.matches(old).count();
        if count == 0 {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.edit_file.old_string_not_found",
                &args_map,
            ));
        }

        let new_content = if replace_all {
            content.replace(old, new)
        } else {
            if count > 1 {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("count".to_string(), count.to_string());
                args_map.insert("path".to_string(), path.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.old_string_ambiguous",
                    &args_map,
                ));
            }
            content.replacen(old, new, 1)
        };

        // Server-side drift enforcement: if this file was observed before
        // (read_file/edit_file/write_file), the on-disk content must still
        // match the remembered tag — a mismatch means it drifted since the
        // model last saw it, so reject and force a re-read. Files never
        // observed have no baseline and pass through.
        if let Some(store) = &self.store {
            let fresh = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_fresh(Path::new(path), &content);
            if !fresh {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.stale_tag",
                    &args_map,
                ));
            }
        }
        // Validate an explicit tag's format when supplied. The drift check
        // above already covers staleness regardless of the tag value.
        if let Some(tag) = tag {
            if tag.parse::<SnapshotTag>().is_err() {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("tag".to_string(), tag.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.invalid_tag",
                    &args_map,
                ));
            }
        }

        self.write_and_record(path, content, new_content)
    }
}

impl EditFileTool {
    /// Line-range edit: confine the `old_string` replacement to lines
    /// `start..=end` (1-based, inclusive) and splice the result back into
    /// the full file content. The window is located by byte offsets in the
    /// original text, so line endings (LF or CRLF) and the final trailing
    /// newline are preserved exactly, and the full prior content stays
    /// available for both drift detection and /undo.
    fn edit_line_range(
        &self,
        path: &str,
        old: &str,
        new: &str,
        start: u64,
        end: u64,
        replace_all: bool,
        tag: Option<&str>,
    ) -> ToolResult {
        if start == 0 || end < start {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("start".to_string(), start.to_string());
            args_map.insert("end".to_string(), end.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.edit_file.invalid_line_range",
                &args_map,
            ));
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.edit_read_failed",
                    &args_map,
                ));
            }
        };

        // Byte offsets of every '\n' terminator. Line k (1-based) spans
        // [start_k, end_k) where start_k = previous terminator + 1 and
        // end_k = position of its own terminator (or EOF for the last line
        // when the file does not end with '\n').
        let nl: Vec<usize> = content.match_indices('\n').map(|(i, _)| i).collect();
        let total_lines = if content.is_empty() {
            0
        } else if content.ends_with('\n') {
            nl.len()
        } else {
            nl.len() + 1
        };
        if start as usize > total_lines {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("start".to_string(), start.to_string());
            args_map.insert("lines".to_string(), total_lines.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.edit_file.line_range_out_of_bounds",
                &args_map,
            ));
        }

        let start_idx = start as usize;
        let end_idx = (end as usize).min(total_lines);
        let window_start = if start_idx == 1 {
            0
        } else {
            nl[start_idx - 2] + 1
        };
        let window_end = if end_idx >= total_lines {
            // Last line: exclude its terminator when the file ends with
            // '\n' so the final newline is never touched by the edit.
            if content.ends_with('\n') {
                nl[total_lines - 1]
            } else {
                content.len()
            }
        } else {
            nl[end_idx - 1]
        };

        // Apply the replacement within the window only.
        let window = &content[window_start..window_end];
        let count = window.matches(old).count();
        if count == 0 {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.edit_file.old_string_not_found",
                &args_map,
            ));
        }
        if count > 1 && !replace_all {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("count".to_string(), count.to_string());
            args_map.insert("path".to_string(), path.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.edit_file.old_string_ambiguous",
                &args_map,
            ));
        }
        let new_window = if replace_all {
            window.replace(old, new)
        } else {
            window.replacen(old, new, 1)
        };
        let new_content = format!(
            "{}{}{}",
            &content[..window_start],
            new_window,
            &content[window_end..]
        );

        // Drift enforcement (same as the full-file path): if this file was
        // observed before, the on-disk content must still match the
        // remembered tag; a mismatch means it drifted since the model last
        // saw it, so reject and force a re-read.
        if let Some(store) = &self.store {
            let fresh = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_fresh(Path::new(path), &content);
            if !fresh {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.stale_tag",
                    &args_map,
                ));
            }
        }
        // Validate an explicit tag's format when supplied. The drift check
        // above already covers staleness regardless of the tag value.
        if let Some(tag) = tag {
            if tag.parse::<SnapshotTag>().is_err() {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("tag".to_string(), tag.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.invalid_tag",
                    &args_map,
                ));
            }
        }

        self.write_and_record(path, content, new_content)
    }

    /// Write `new_content` to `path` atomically and record the mutation for
    /// rollback. `prior_content` is the full pre-edit content; it is stored
    /// for /undo when within the snapshot budget.
    fn write_and_record(
        &self,
        path: &str,
        prior_content: String,
        new_content: String,
    ) -> ToolResult {
        match atomic_write(Path::new(path), new_content.as_bytes()) {
            Ok(()) => {
                let tag_suffix = if let Some(store) = &self.store {
                    let skip = prior_content.len() > SNAPSHOT_MAX_BYTES;
                    let mut g = store.lock().unwrap_or_else(|e| e.into_inner());
                    if skip {
                        // Prior too large for rollback memory; refresh tag only.
                        g.record_read(Path::new(path), &new_content);
                    } else {
                        g.record_mutation(
                            Path::new(path),
                            Some(prior_content.into_bytes()),
                            &new_content,
                            SnapshotOp::Edit,
                        );
                    }
                    let new_tag = SnapshotTag::from_content(&new_content);
                    if skip {
                        format!(
                            "\n[{}#{}] (not undoable: prior content too large)",
                            path, new_tag
                        )
                    } else {
                        format!("\n[{}#{}]", path, new_tag)
                    }
                } else {
                    String::new()
                };
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                ToolResult::success(format!(
                    "{}{}",
                    aish_i18n::t_with_args("tools.fs.edit_file.edit_success", &args_map),
                    tag_suffix
                ))
            }
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.edit_write_failed",
                    &args_map,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{SharedSnapshotStore, SnapshotStore};
    use aish_llm::Tool;
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn test_edit_file_basic() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello\nworld\nfoo").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "world",
            "new_string": "earth"
        }));

        assert!(result.ok);
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "hello\nearth\nfoo");
    }

    #[test]
    fn test_edit_file_replace_all() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "foo\nbar\nfoo").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "baz",
            "replace_all": true
        }));

        assert!(result.ok);
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "baz\nbar\nbaz");
    }

    #[test]
    fn test_edit_file_not_found() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "nope",
            "new_string": "yes"
        }));

        assert!(!result.ok);
    }

    #[test]
    fn test_edit_file_ambiguous() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "foo\nfoo\nfoo").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "bar"
        }));

        assert!(!result.ok);
    }

    #[test]
    fn test_edit_file_line_range_basic() {
        let dir = temp_dir();
        let file_path = dir.path().join("big.txt");
        // 10 lines; edit lines 3-5 without loading the full file.
        let content: String = (1..=10)
            .map(|i| format!("line_{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file_path, &content).unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "line_04",
            "new_string": "CHANGED",
            "start_line": 3,
            "end_line": 5
        }));

        assert!(
            result.ok,
            "edit_line_range should succeed: {}",
            result.output
        );
        let result_content = fs::read_to_string(&file_path).unwrap();
        assert!(result_content.contains("CHANGED"));
        assert!(result_content.contains("line_01"));
        assert!(result_content.contains("line_10"));
        assert!(!result_content.contains("line_04"));
    }

    #[test]
    fn test_edit_file_line_range_large_file() {
        let dir = temp_dir();
        let file_path = dir.path().join("huge.txt");
        // File larger than MAX_FILE_SIZE (256 KiB) — full-file edit would be
        // rejected, but line-range edit should work because it only reads the
        // targeted lines.
        let content: String = (0..20000)
            .map(|i| format!("data_line_{i:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file_path, &content).unwrap();
        assert!(
            file_path.metadata().unwrap().len() > MAX_FILE_SIZE,
            "file must exceed 256 KiB"
        );

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "data_line_05000",
            "new_string": "EDITED_LINE",
            "start_line": 5000,
            "end_line": 5001
        }));

        assert!(
            result.ok,
            "line-range edit on large file: {}",
            result.output
        );
        let result_content = fs::read_to_string(&file_path).unwrap();
        assert!(result_content.contains("EDITED_LINE"));
        assert!(
            result_content.contains("data_line_19999"),
            "last line intact"
        );
    }

    #[test]
    fn test_edit_file_line_range_out_of_bounds() {
        aish_i18n::set_locale("en-US");
        let dir = temp_dir();
        let file_path = dir.path().join("small.txt");
        fs::write(&file_path, "only\nthree\nlines").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "only",
            "new_string": "first",
            "start_line": 10,
            "end_line": 20
        }));

        assert!(!result.ok);
        assert!(result.output.contains("out of bounds") || result.output.contains("exceed"));
    }

    #[test]
    fn test_edit_file_line_range_not_found() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "alpha\nbeta\ngamma\ndelta").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "nonexistent",
            "new_string": "found",
            "start_line": 1,
            "end_line": 2
        }));

        assert!(!result.ok);
    }

    #[test]
    fn test_edit_file_invalid_line_range() {
        aish_i18n::set_locale("en-US");
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello\nworld").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "bye",
            "start_line": 5,
            "end_line": 2
        }));

        assert!(!result.ok);
    }

    #[test]
    fn test_edit_file_line_range_preserves_trailing_newline() {
        // Regression: a file ending with '\n' must keep its final newline
        // after a line-range edit.
        let dir = temp_dir();
        let file_path = dir.path().join("f.txt");
        fs::write(&file_path, "a\nb\nc\n").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "b",
            "new_string": "B",
            "start_line": 2,
            "end_line": 2
        }));

        assert!(result.ok, "edit failed: {}", result.output);
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "a\nB\nc\n");
    }

    #[test]
    fn test_edit_file_line_range_preserves_crlf() {
        // Regression: line-range edits must not convert CRLF line endings
        // to LF.
        let dir = temp_dir();
        let file_path = dir.path().join("f.txt");
        fs::write(&file_path, "a\r\nb\r\nc\r\n").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "b",
            "new_string": "B",
            "start_line": 2,
            "end_line": 2
        }));

        assert!(result.ok, "edit failed: {}", result.output);
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "a\r\nB\r\nc\r\n");
    }

    #[test]
    fn test_edit_file_line_range_multiline_match() {
        // old_string spanning a line boundary inside the window.
        let dir = temp_dir();
        let file_path = dir.path().join("f.txt");
        fs::write(&file_path, "one\ntwo\nthree\nfour\n").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "two\nthree",
            "new_string": "TWO\nTHREE",
            "start_line": 2,
            "end_line": 3
        }));

        assert!(result.ok, "edit failed: {}", result.output);
        assert_eq!(
            fs::read_to_string(&file_path).unwrap(),
            "one\nTWO\nTHREE\nfour\n"
        );
    }

    #[test]
    fn test_edit_file_line_range_rejects_drifted_file() {
        // The line-range path enforces the same drift check as the
        // full-file path: content observed earlier must not have changed
        // on disk since the baseline read.
        aish_i18n::set_locale("en-US");
        let dir = temp_dir();
        let file_path = dir.path().join("f.txt");
        fs::write(&file_path, "alpha\nbeta\ngamma\n").unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        store.lock().unwrap().record_read(
            Path::new(file_path.to_str().unwrap()),
            "alpha\nbeta\ngamma\n",
        );

        // External drift after the baseline read.
        fs::write(&file_path, "alpha\nDRIFTED\ngamma\n").unwrap();

        let tool = EditFileTool::with_store(store);
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "DRIFTED",
            "new_string": "EDITED",
            "start_line": 2,
            "end_line": 2
        }));

        assert!(
            !result.ok,
            "drifted file must be rejected: {}",
            result.output
        );
        assert!(
            result.output.contains("changed since"),
            "expected stale-tag message, got: {}",
            result.output
        );
    }

    #[test]
    fn test_edit_file_line_range_is_undoable() {
        // A line-range edit records the full prior content for /undo, not
        // just the edited window.
        let dir = temp_dir();
        let file_path = dir.path().join("f.txt");
        let prior = "alpha\nbeta\ngamma\n";
        fs::write(&file_path, prior).unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        store
            .lock()
            .unwrap()
            .record_read(Path::new(file_path.to_str().unwrap()), prior);

        let tool = EditFileTool::with_store(store.clone());
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "beta",
            "new_string": "BETA",
            "start_line": 2,
            "end_line": 2
        }));

        assert!(result.ok, "edit failed: {}", result.output);
        assert!(
            !result.output.contains("not undoable"),
            "line-range edit of a small file must be undoable, got: {}",
            result.output
        );

        let undo = store
            .lock()
            .unwrap()
            .peek_undo_last_for(Path::new(file_path.to_str().unwrap()))
            .expect("undo entry must exist");
        undo.apply_to_disk(false).expect("undo must apply");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), prior);
    }
}
