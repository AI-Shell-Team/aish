use std::path::Path;

use aish_i18n;
use aish_llm::{Tool, ToolResult};

/// Cached translated descriptions.
static READ_DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static WRITE_DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static EDIT_DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_read_description() -> &'static str {
    READ_DESCRIPTION.get_or_init(|| aish_i18n::t("tools.fs.read_file.description"))
}

fn get_write_description() -> &'static str {
    WRITE_DESCRIPTION.get_or_init(|| aish_i18n::t("tools.fs.write_file.description"))
}

fn get_edit_description() -> &'static str {
    EDIT_DESCRIPTION.get_or_init(|| aish_i18n::t("tools.fs.edit_file.description"))
}

/// Read file content tool.
pub struct ReadFileTool;

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        get_read_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read" },
                "offset": { "type": "integer", "description": "Line offset to start reading from (0-based)" },
                "limit": { "type": "integer", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolResult::error(aish_i18n::t("tools.fs.read_file.missing_path")),
        };

        // Read raw bytes first for size check
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

        // Enforce 32KB size limit
        const SIZE_LIMIT: usize = 32 * 1024;
        if raw_bytes.len() > SIZE_LIMIT {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("path".to_string(), path.to_string());
            args_map.insert("size".to_string(), raw_bytes.len().to_string());
            args_map.insert("limit".to_string(), SIZE_LIMIT.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.fs.read_file.file_too_large",
                &args_map,
            ));
        }

        // Convert to UTF-8
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

        // Handle empty file
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

        // Format output with line numbers (1-based, offset-aware)
        let selected: Vec<String> = if let Some(limit) = limit {
            lines
                .iter()
                .skip(offset)
                .take(limit)
                .enumerate()
                .map(|(i, line)| format!("{:>6}\t{}", offset + i + 1, line))
                .collect()
        } else {
            lines
                .iter()
                .skip(offset)
                .enumerate()
                .map(|(i, line)| format!("{:>6}\t{}", offset + i + 1, line))
                .collect()
        };

        ToolResult::success(selected.join("\n"))
    }
}

/// Write file tool (creates or overwrites).
pub struct WriteFileTool;

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        get_write_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
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
        // Create parent dirs if needed
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
        match std::fs::write(path, content) {
            Ok(()) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("bytes".to_string(), content.len().to_string());
                args_map.insert("path".to_string(), path.to_string());
                ToolResult::success(aish_i18n::t_with_args(
                    "tools.fs.write_file.write_success",
                    &args_map,
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

/// Edit file tool (string replacement).
pub struct EditFileTool;

impl Default for EditFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EditFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        get_edit_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "old_string": { "type": "string", "description": "The text to replace" },
                "new_string": { "type": "string", "description": "The replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default: false)" }
            },
            "required": ["path", "old_string", "new_string"]
        })
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

        if !content.contains(old) {
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
            // Check uniqueness
            let count = content.matches(old).count();
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

        match std::fs::write(path, new_content) {
            Ok(()) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("path".to_string(), path.to_string());
                ToolResult::success(aish_i18n::t_with_args(
                    "tools.fs.edit_file.edit_success",
                    &args_map,
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
        // Offset is 0-based, so offset=2 starts at line3 (3rd line)
        // Line numbers are 1-based and offset-aware: 3, 4
        assert_eq!(result.output, "     3\tline3\n     4\tline4");
    }

    #[test]
    fn test_read_file_size_limit() {
        // Initialize i18n for testing
        aish_i18n::set_locale("en-US");

        let dir = temp_dir();
        let file_path = dir.path().join("big.txt");
        // Create a file larger than 32KB (33 * 1024 = 33792 bytes)
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
        // Initialize i18n for testing
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
        // Verify file was NOT modified
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "foo bar foo baz");
    }

    #[test]
    fn test_write_file_creates_parent_dirs() {
        let dir = temp_dir();
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
}
