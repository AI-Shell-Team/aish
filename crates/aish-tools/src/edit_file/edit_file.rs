use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;
use crate::fs::{SharedSnapshotStore, SnapshotOp, SnapshotTag};
use std::path::Path;

const SIZE_LIMIT: u64 = 32 * 1024;

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
        if metadata.len() > SIZE_LIMIT {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            args_map.insert("size".to_string(), metadata.len().to_string());
            args_map.insert("limit".to_string(), SIZE_LIMIT.to_string());
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
        if let Some(tag_str) = args.get("tag").and_then(|t| t.as_str()) {
            if tag_str.parse::<SnapshotTag>().is_err() {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("tag".to_string(), tag_str.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.edit_file.invalid_tag",
                    &args_map,
                ));
            }
        }

        match std::fs::write(path, &new_content) {
            Ok(()) => {
                // Record the mutation for rollback and mint a fresh tag.
                let tag_suffix = if let Some(store) = &self.store {
                    store
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .record_mutation(
                            Path::new(path),
                            Some(content.into_bytes()),
                            &new_content,
                            SnapshotOp::Edit,
                        );
                    let new_tag = SnapshotTag::from_content(&new_content);
                    format!("\n[{}#{}]", path, new_tag)
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
    use aish_llm::Tool;
    use std::fs;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn test_edit_file_replace_all() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "qux",
            "replace_all": true
        }));

        assert!(result.ok);
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "qux bar qux baz qux");
    }

    #[test]
    fn test_edit_file_uniqueness_check() {
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "foo bar foo baz").unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "qux"
        }));

        assert!(!result.ok);
        assert!(
            result.output.contains("times") || result.output.contains("ambiguous"),
            "Expected uniqueness error, got: {}",
            result.output
        );
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "foo bar foo baz");
    }

    #[test]
    fn test_edit_file_size_limit() {
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("big.txt");
        fs::write(&file_path, "x".repeat(33 * 1024)).unwrap();

        let tool = EditFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "x",
            "new_string": "y"
        }));

        assert!(!result.ok);
        assert!(
            result.output.contains("limit") || result.output.contains("bytes"),
            "Expected size limit error, got: {}",
            result.output
        );
    }

    #[test]
    fn edit_file_rejects_stale_drift_without_tag() {
        // #3: server-side enforcement — even with no `tag` arg, an edit on a
        // file that changed since read_file must be rejected.
        use std::sync::{Arc, Mutex};

        use crate::fs::{SharedSnapshotStore, SnapshotStore};

        aish_i18n::set_locale("en-US");
        let dir = temp_dir();
        let f = dir.path().join("a.txt");
        fs::write(&f, "v1").unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        // Simulate read_file observing "v1", then the file drifts.
        store.lock().unwrap().record_read(&f, "v1");
        fs::write(&f, "v2").unwrap();

        let tool = EditFileTool::with_store(store);
        let res = tool.execute(serde_json::json!({
            "path": f.to_str().unwrap(),
            "old_string": "v2",
            "new_string": "v3",
        }));
        assert!(!res.ok, "stale edit must be rejected even without a tag");
        assert!(
            res.output.contains("changed") || res.output.contains("re-read"),
            "expected stale message, got: {}",
            res.output
        );
        assert_eq!(fs::read_to_string(&f).unwrap(), "v2");
    }

    #[test]
    fn edit_file_allows_when_fresh_and_records_mutation() {
        use std::sync::{Arc, Mutex};

        use crate::fs::{SharedSnapshotStore, SnapshotStore};

        let dir = temp_dir();
        let f = dir.path().join("a.txt");
        fs::write(&f, "hello").unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        store.lock().unwrap().record_read(&f, "hello");

        let tool = EditFileTool::with_store(store.clone());
        let res = tool.execute(serde_json::json!({
            "path": f.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "world",
        }));
        assert!(res.ok, "fresh edit must succeed");
        assert_eq!(fs::read_to_string(&f).unwrap(), "world");
        assert_eq!(store.lock().unwrap().history_len(), 1);
    }

    #[test]
    fn edit_file_rejects_malformed_tag() {
        use std::sync::{Arc, Mutex};

        use crate::fs::{SharedSnapshotStore, SnapshotStore};

        aish_i18n::set_locale("en-US");
        let dir = temp_dir();
        let f = dir.path().join("a.txt");
        fs::write(&f, "hello").unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        store.lock().unwrap().record_read(&f, "hello");

        let tool = EditFileTool::with_store(store);
        let res = tool.execute(serde_json::json!({
            "path": f.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "world",
            "tag": "ZZZZ",
        }));
        assert!(!res.ok, "malformed tag must be rejected");
        assert!(
            res.output.contains("Invalid") || res.output.contains("invalid"),
            "expected invalid-tag message, got: {}",
            res.output
        );
    }
}
