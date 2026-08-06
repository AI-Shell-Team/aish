use std::path::Path;

use aish_llm::{Tool, ToolResult};

use crate::fs::{ApplyOutcome, SharedSnapshotStore, UndoResult};

use super::prompt;

/// Undo the most recent file mutation (edit_file / write_file) recorded by the
/// shared snapshot store. Restores prior content, or deletes the file if it was
/// created by the undone mutation.
pub struct UndoEditTool {
    store: SharedSnapshotStore,
}

impl UndoEditTool {
    pub fn new(store: SharedSnapshotStore) -> Self {
        Self { store }
    }
}

impl Tool for UndoEditTool {
    fn name(&self) -> &str {
        "undo_edit"
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
        let path = args.get("path").and_then(|p| p.as_str());
        // Peek WITHOUT consuming — a failed disk restore must not lose the
        // snapshot, so we commit only after the IO succeeds.
        let peeked = {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            match path {
                Some(p) => store.peek_undo_last_for(Path::new(p)),
                None => store.peek_undo_last(),
            }
        };
        let result = match peeked {
            Some(r) => r,
            None => return ToolResult::error(aish_i18n::t("tools.fs.undo_edit.nothing_to_undo")),
        };
        let applied = apply_undo(&result);
        if applied.ok {
            // Disk restore succeeded — now safe to consume the history entry.
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            match path {
                Some(p) => store.commit_undo_last_for(Path::new(p), result.snapshot_id),
                None => store.commit_undo_last(result.snapshot_id),
            };
        }
        // On failure the history is retained so the caller can retry.
        applied
    }
}

/// Apply an [`UndoResult`] to disk. Writes prior content back, or removes the
/// file when the undone mutation had created it.
fn apply_undo(result: &UndoResult) -> ToolResult {
    let mut args = std::collections::HashMap::new();
    args.insert("path".to_string(), result.path.display().to_string());
    match result.apply_to_disk(false) {
        Ok(ApplyOutcome::Restored) => {
            ToolResult::success(aish_i18n::t_with_args("tools.fs.undo_edit.restored", &args))
        }
        Ok(ApplyOutcome::Removed) => {
            ToolResult::success(aish_i18n::t_with_args("tools.fs.undo_edit.removed", &args))
        }
        Err(e) => {
            args.insert("error".to_string(), e.to_string());
            let key = if result.content.is_some() {
                "tools.fs.undo_edit.restore_failed"
            } else {
                "tools.fs.undo_edit.remove_failed"
            };
            ToolResult::error(aish_i18n::t_with_args(key, &args))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::SnapshotStore;
    use std::fs;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn undo_restores_overwritten_content() {
        let dir = temp_dir();
        let f = dir.path().join("a.txt");
        fs::write(&f, "original").unwrap();

        let store: SharedSnapshotStore =
            std::sync::Arc::new(std::sync::Mutex::new(SnapshotStore::new()));
        // Simulate a write_file mutation that overwrites.
        {
            let mut g = store.lock().unwrap();
            g.record_mutation(
                &f,
                Some("original".into()),
                "changed",
                crate::fs::SnapshotOp::Write,
            );
        }
        fs::write(&f, "changed").unwrap();

        let tool = UndoEditTool::new(store.clone());
        let res = tool.execute(serde_json::json!({}));
        assert!(res.ok, "undo should succeed");
        assert_eq!(fs::read_to_string(&f).unwrap(), "original");
    }

    #[test]
    fn undo_deletes_created_file() {
        let dir = temp_dir();
        let f = dir.path().join("new.txt");
        fs::write(&f, "created").unwrap();

        let store: SharedSnapshotStore =
            std::sync::Arc::new(std::sync::Mutex::new(SnapshotStore::new()));
        {
            let mut g = store.lock().unwrap();
            // prior = None => file was created
            g.record_mutation(&f, None, "created", crate::fs::SnapshotOp::Write);
        }

        let tool = UndoEditTool::new(store.clone());
        let res = tool.execute(serde_json::json!({}));
        assert!(res.ok);
        assert!(!f.exists(), "created file should be deleted on undo");
    }

    #[test]
    fn undo_nothing_returns_error() {
        let store: SharedSnapshotStore =
            std::sync::Arc::new(std::sync::Mutex::new(SnapshotStore::new()));
        let tool = UndoEditTool::new(store);
        let res = tool.execute(serde_json::json!({}));
        assert!(!res.ok);
    }
}
