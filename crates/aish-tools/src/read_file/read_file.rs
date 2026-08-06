use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;
use crate::fs::SharedSnapshotStore;
use std::path::Path;

const SIZE_LIMIT: usize = 32 * 1024;

/// Read file content tool.
pub struct ReadFileTool {
    store: Option<SharedSnapshotStore>,
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: SharedSnapshotStore) -> Self {
        Self { store: Some(store) }
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
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
            None => return ToolResult::error(aish_i18n::t("tools.fs.read_file.missing_path")),
        };

        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.read_file.read_failed",
                    &args_map,
                ));
            }
        };

        if metadata.len() > SIZE_LIMIT as u64 {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            args_map.insert("size".to_string(), metadata.len().to_string());
            args_map.insert("limit".to_string(), SIZE_LIMIT.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.read_file.file_too_large",
                &args_map,
            ));
        }

        let raw_bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.read_file.read_failed",
                    &args_map,
                ));
            }
        };

        let content = match String::from_utf8(raw_bytes) {
            Ok(s) => s,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.read_file.decode_failed",
                    &args_map,
                ));
            }
        };

        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return ToolResult::success(aish_i18n::t("tools.fs.read_file.empty_file"));
        }

        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize);

        if offset >= lines.len() {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("offset".to_string(), offset.to_string());
            args_map.insert("length".to_string(), lines.len().to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.read_file.offset_exceeds_length",
                &args_map,
            ));
        }

        let selected: Vec<String> = lines
            .iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", offset + i + 1, line))
            .collect();

        let body = selected.join("\n");

        // Stamp a snapshot tag so a later edit_file can detect stale content.
        if let Some(store) = &self.store {
            let tag = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record_read(Path::new(path), &content);
            ToolResult::success(format!("[{}#{}]\n{}", path, tag, body))
        } else {
            ToolResult::success(body)
        }
    }
}

/// Path-restricted wrapper around [`ReadFileTool`] for SSH sessions.
pub struct SshReadFileTool {
    inner: ReadFileTool,
    offload_root: std::path::PathBuf,
}

impl SshReadFileTool {
    pub fn new() -> Self {
        let offload_root = std::env::temp_dir().join("aish-offload");
        std::fs::create_dir_all(&offload_root).expect("failed to create aish offload directory");
        let canonical_root = std::fs::canonicalize(&offload_root)
            .expect("failed to canonicalize aish offload directory");
        Self {
            inner: ReadFileTool::new(),
            offload_root: canonical_root,
        }
    }
}

impl Tool for SshReadFileTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    fn prompt(&self) -> &str {
        self.inner.prompt()
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolResult::error(aish_i18n::t("tools.fs.read_file.missing_path")),
        };
        let canonical = match std::fs::canonicalize(path) {
            Ok(c) => c,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.fs.read_file.read_failed",
                    &args_map,
                ));
            }
        };
        if !canonical.starts_with(&self.offload_root) {
            return ToolResult::error(aish_i18n::t("tools.fs.read_file.access_denied"));
        }
        let mut safe_args = args;
        if let Some(obj) = safe_args.as_object_mut() {
            obj.insert(
                "path".to_string(),
                serde_json::Value::String(canonical.to_string_lossy().into_owned()),
            );
        }
        self.inner.execute(safe_args)
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
    fn test_read_file_with_line_numbers() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello\nworld\nfoo").unwrap();

        let tool = ReadFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap()
        }));

        assert!(result.ok);
        assert_eq!(result.output, "     1\thello\n     2\tworld\n     3\tfoo");
    }

    #[test]
    fn test_read_file_with_offset() {
        let dir = temp_dir();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "line1\nline2\nline3\nline4\nline5").unwrap();

        let tool = ReadFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "offset": 2,
            "limit": 2
        }));

        assert!(result.ok);
        assert_eq!(result.output, "     3\tline3\n     4\tline4");
    }

    #[test]
    fn test_read_file_size_limit() {
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("big.txt");
        let big_content = "x".repeat(33 * 1024);
        fs::write(&file_path, &big_content).unwrap();

        let tool = ReadFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap()
        }));

        assert!(!result.ok);
        assert!(
            result.output.contains("limit") || result.output.contains("bytes"),
            "Expected size limit error, got: {}",
            result.output
        );
    }

    #[test]
    fn test_ssh_read_file_rejects_paths_outside_offload_root() {
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("outside.txt");
        fs::write(&file_path, "secret").unwrap();

        let tool = SshReadFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap()
        }));

        assert!(!result.ok);
        assert!(
            result.output.contains("Access denied"),
            "Expected access denied error, got: {}",
            result.output
        );
    }
}
