use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;
use crate::fs::SharedSnapshotStore;
use std::path::Path;

/// Upper bound on total file bytes we are willing to read into memory and
/// return inline. Files larger than this are read by range (offset/limit)
/// or truncated with a notice so the model knows to fetch the remainder.
const MAX_READ_BYTES: usize = 256 * 1024;

/// Default number of lines returned when no explicit limit is given and the
/// file exceeds MAX_READ_BYTES. Keeps a single read within the byte budget
/// while still returning a useful window of content.
const DEFAULT_TRUNCATE_LINES: usize = 500;

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

        let file_size = metadata.len() as usize;
        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize);

        // Large file path: stream only the requested line range instead of
        // loading the whole file into memory. When no range is given, return
        // a bounded head window and tell the model how to read the rest.
        if file_size > MAX_READ_BYTES {
            return self.read_large_file(path, file_size, offset, limit);
        }

        // Normal path: file fits in the byte budget, read it all.
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

impl ReadFileTool {
    /// Read a large file by streaming only the requested line range, without
    /// loading the entire file into memory. Uses `BufReader` + line iteration
    /// so only the window the model asked for is materialized.
    fn read_large_file(
        &self,
        path: &str,
        file_size: usize,
        offset: usize,
        limit: Option<usize>,
    ) -> ToolResult {
        use std::io::BufRead;

        let file = match std::fs::File::open(path) {
            Ok(f) => f,
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

        let reader = std::io::BufReader::new(file);
        let effective_limit = limit.unwrap_or(DEFAULT_TRUNCATE_LINES);

        // Skip `offset` lines, then collect up to `effective_limit` lines.
        let mut collected: Vec<String> = Vec::with_capacity(effective_limit);
        let mut line_no = 0usize;
        let mut total_lines = 0usize;

        for line_result in reader.lines() {
            total_lines += 1;
            if line_no < offset {
                line_no += 1;
                continue;
            }
            if collected.len() >= effective_limit {
                // Keep counting lines so we know the total for the notice.
                continue;
            }
            match line_result {
                Ok(line) => collected.push(format!("{:>6}\t{}", line_no + 1, line)),
                Err(e) => {
                    let mut args_map = std::collections::HashMap::new();
                    args_map.insert("path".to_string(), path.to_string());
                    args_map.insert("error".to_string(), e.to_string());
                    return ToolResult::error(aish_i18n::t_with_args(
                        "tools.fs.read_file.decode_failed",
                        &args_map,
                    ));
                }
            }
            line_no += 1;
        }

        if collected.is_empty() {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("offset".to_string(), offset.to_string());
            args_map.insert("length".to_string(), total_lines.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.read_file.offset_exceeds_length",
                &args_map,
            ));
        }

        let body = collected.join("\n");

        // Build a truncation notice when we didn't return the whole file.
        let truncated = (offset > 0) || (collected.len() < total_lines);
        let header = if truncated {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            args_map.insert("size".to_string(), file_size.to_string());
            args_map.insert("lines".to_string(), total_lines.to_string());
            args_map.insert("shown".to_string(), collected.len().to_string());
            args_map.insert("offset".to_string(), offset.to_string());
            aish_i18n::t_with_args("tools.fs.read_file.large_file_truncated", &args_map)
        } else {
            String::new()
        };

        // Large-file reads are partial windows; a tag computed on the
        // window would never match the full disk content, making every
        // subsequent edit_file's is_fresh check fail. Skip tagging (like
        // omp which omits the header for files > 4 MiB) — the model must
        // re-read before editing.
        let tag_suffix = if self.store.is_some() {
            // Do not record_read for partial content — it would create a
            // false tag baseline. Just emit a plain header without a tag.
            format!("[{}]\n", path)
        } else {
            String::new()
        };
        if truncated {
            ToolResult::success(format!("{}{}{}", tag_suffix, header, body))
        } else {
            ToolResult::success(format!("{}{}", tag_suffix, body))
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
    fn test_read_file_large_file_truncated_with_notice() {
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("big.txt");
        // Exceed MAX_READ_BYTES (256 KiB) to trigger the large-file path.
        // 30000 lines × ~20 bytes each ≈ 600 KB.
        let big_content: String = (0..30000)
            .map(|i| format!("line_{i:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file_path, &big_content).unwrap();
        let tool = ReadFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap()
        }));

        assert!(
            result.ok,
            "large file should not be rejected: {}",
            result.output
        );
        // Default truncation: first 500 lines + a notice.
        assert!(
            result.output.contains("line_00000"),
            "should contain the first line"
        );
        assert!(
            result.output.contains("line_00499"),
            "should contain line 500 (0-based offset 499)"
        );
        assert!(
            !result.output.contains("line_00500"),
            "should not contain line 501 — truncated"
        );
    }

    #[test]
    fn test_read_file_large_file_with_offset_and_limit() {
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("big.txt");
        let big_content: String = (0..30000)
            .map(|i| format!("line_{i:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file_path, &big_content).unwrap();

        let tool = ReadFileTool::new();
        let result = tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "offset": 1000,
            "limit": 3
        }));

        assert!(result.ok);
        assert!(result.output.contains("line_01000"));
        assert!(result.output.contains("line_01002"));
        assert!(!result.output.contains("line_01003"));
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
