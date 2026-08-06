use std::path::Path;

use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;
use crate::fs::{SharedSnapshotStore, SnapshotOp, SnapshotTag};

const MAX_WRITE_BYTES: usize = 32 * 1024;

/// Write file tool (creates or overwrites).
pub struct WriteFileTool {
    store: Option<SharedSnapshotStore>,
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: SharedSnapshotStore) -> Self {
        Self { store: Some(store) }
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
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
            None => return ToolResult::error(aish_i18n::t("tools.fs.write_file.missing_path")),
        };
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return ToolResult::error(aish_i18n::t("tools.fs.write_file.missing_content")),
        };

        if content.len() > MAX_WRITE_BYTES {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            args_map.insert("size".to_string(), content.len().to_string());
            args_map.insert("limit".to_string(), MAX_WRITE_BYTES.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.write_file.content_too_large",
                &args_map,
            ));
        }

        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    let mut args_map = std::collections::HashMap::new();
                    args_map.insert("error".to_string(), e.to_string());
                    return ToolResult::error(aish_i18n::t_with_args(
                        "tools.fs.write_file.create_dirs_failed",
                        &args_map,
                    ));
                }
            }
        }
        // Capture prior content for rollback before overwriting. Use raw
        // bytes (not read_to_string) so binary files are snapshotted too.
        // `None` means the file does not exist yet (undo deletes). A read
        // failure on an existing-but-unreadable file MUST NOT record a bogus
        // empty prior — that would make /undo truncate the file to zero
        // bytes. Instead `skip_rollback` refreshes only the anchor tag (no
        // history entry), so the write stays undoable-safe.
        let (prior, skip_rollback): (Option<Vec<u8>>, bool) = match self.store.as_ref() {
            Some(_) => {
                if !Path::new(path).exists() {
                    (None, false)
                } else {
                    match std::fs::read(path) {
                        Ok(bytes) => {
                            // Cap rollback memory: a prior larger than the
                            // write budget isn't tracked (mirrors the
                            // SIZE_LIMIT gate in read_file/edit_file). The
                            // overwrite still happens; it just isn't
                            // undoable — same boundary those tools enforce.
                            if bytes.len() > MAX_WRITE_BYTES {
                                (None, true)
                            } else {
                                (Some(bytes), false)
                            }
                        }
                        Err(_) => (None, true),
                    }
                }
            }
            None => (None, false),
        };
        match std::fs::write(path, content) {
            Ok(()) => {
                let tag_suffix = if let Some(store) = &self.store {
                    let mut g = store.lock().unwrap_or_else(|e| e.into_inner());
                    if skip_rollback {
                        // No safe prior to restore: refresh the anchor tag
                        // only, without pushing a rollback entry.
                        g.record_read(Path::new(path), content);
                    } else {
                        g.record_mutation(Path::new(path), prior, content, SnapshotOp::Write);
                    }
                    let new_tag = SnapshotTag::from_content(content);
                    // Surface that this write is not undoable (prior was
                    // unreadable or oversized) so the caller knows /undo
                    // and /rollback cannot bring the old content back.
                    if skip_rollback {
                        format!(
                            "\n[{}#{}] (not undoable: prior content unavailable)",
                            path, new_tag
                        )
                    } else {
                        format!("\n[{}#{}]", path, new_tag)
                    }
                } else {
                    String::new()
                };
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("bytes".to_string(), content.len().to_string());
                args_map.insert("path".to_string(), path.to_string());
                ToolResult::success(format!(
                    "{}{}",
                    aish_i18n::t_with_args("tools.fs.write_file.write_success", &args_map),
                    tag_suffix
                ))
            }
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.write_file.write_failed",
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

    #[test]
    fn test_write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let file_path = dir.path().join("nested").join("deep").join("test.txt");

        let tool = WriteFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "hello world"
        }));

        assert!(result.ok);
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_write_file_size_limit() {
        aish_i18n::set_locale("en-US");

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let file_path = dir.path().join("big.txt");

        let tool = WriteFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "x".repeat(33 * 1024)
        }));

        assert!(!result.ok);
        assert!(
            result.output.contains("limit") || result.output.contains("bytes"),
            "Expected size limit error, got: {}",
            result.output
        );
        assert!(!file_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_file_unreadable_prior_skips_rollback() {
        // Regression (#1): an existing-but-unreadable file must NOT record
        // an empty prior — otherwise /undo would restore zero bytes and
        // truncate the file. The write still succeeds; the store just skips
        // the rollback entry while refreshing the anchor tag.
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Mutex};

        use crate::fs::{SharedSnapshotStore, SnapshotStore};

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let f = dir.path().join("write_only.txt");
        fs::write(&f, "secret-prior").unwrap();
        // write-only (0o222): writable, NOT readable
        fs::set_permissions(&f, fs::Permissions::from_mode(0o222)).unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        let tool = WriteFileTool::with_store(store.clone());
        let res = tool.execute(serde_json::json!({
            "path": f.to_str().unwrap(),
            "content": "new-content",
        }));
        assert!(res.ok, "write must succeed on a write-only file");

        let g = store.lock().unwrap();
        assert_eq!(
            g.history_len(),
            0,
            "unreadable prior must not enter the rollback chain"
        );
        assert!(
            g.current_tag(std::path::Path::new(&f)).is_some(),
            "anchor tag should still be refreshed"
        );
    }
    #[test]
    fn write_file_oversized_prior_skips_rollback() {
        // A prior larger than MAX_WRITE_BYTES must not enter history (memory
        // cap + consistency with read_file/edit_file SIZE_LIMIT). The
        // overwrite still succeeds; it just isn't undoable.
        use std::sync::{Arc, Mutex};

        use crate::fs::{SharedSnapshotStore, SnapshotStore};

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let f = dir.path().join("big.log");
        fs::write(&f, "x".repeat(MAX_WRITE_BYTES + 1)).unwrap();

        let store: SharedSnapshotStore = Arc::new(Mutex::new(SnapshotStore::new()));
        let tool = WriteFileTool::with_store(store.clone());
        let res = tool.execute(serde_json::json!({
            "path": f.to_str().unwrap(),
            "content": "small",
        }));
        assert!(res.ok, "overwrite of an oversized prior must still succeed");

        let g = store.lock().unwrap();
        assert_eq!(
            g.history_len(),
            0,
            "oversized prior must not enter the rollback chain"
        );
        assert!(
            g.current_tag(std::path::Path::new(&f)).is_some(),
            "anchor tag should still be refreshed"
        );
        drop(g);
        assert_eq!(fs::read_to_string(&f).unwrap(), "small");
    }
}
