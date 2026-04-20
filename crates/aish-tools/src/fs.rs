use std::path::Path;

use aish_llm::{Tool, ToolResult};

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
        "Read the content of a file"
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
            None => return ToolResult::error("Missing 'path' parameter"),
        };

        // Read raw bytes first for size check
        let raw_bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Failed to read {}: {}", path, e)),
        };

        // Enforce 32KB size limit
        const SIZE_LIMIT: usize = 32 * 1024;
        if raw_bytes.len() > SIZE_LIMIT {
            return ToolResult::error(format!(
                "File {} is {} bytes, exceeding the {} byte (32KB) limit",
                path,
                raw_bytes.len(),
                SIZE_LIMIT
            ));
        }

        // Convert to UTF-8
        let content = match String::from_utf8(raw_bytes) {
            Ok(s) => s,
            Err(e) => {
                return ToolResult::error(format!("Failed to decode {} as UTF-8: {}", path, e))
            }
        };

        let lines: Vec<&str> = content.lines().collect();

        // Handle empty file
        if lines.is_empty() {
            return ToolResult::success("(empty file)".to_string());
        }

        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize);

        if offset >= lines.len() {
            return ToolResult::error(format!(
                "Offset {} exceeds file length ({})",
                offset,
                lines.len()
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
        "Write content to a file (creates or overwrites)"
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
            None => return ToolResult::error("Missing 'path' parameter"),
        };
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Missing 'content' parameter"),
        };
        // Create parent dirs if needed
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::error(format!("Failed to create parent dirs: {}", e));
                }
            }
        }
        match std::fs::write(path, content) {
            Ok(()) => ToolResult::success(format!("Wrote {} bytes to {}", content.len(), path)),
            Err(e) => ToolResult::error(format!("Failed to write {}: {}", path, e)),
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
        "Edit a file by replacing a specific string with a new string"
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
            None => return ToolResult::error("Missing 'path' parameter"),
        };
        let old = match args.get("old_string").and_then(|o| o.as_str()) {
            Some(o) => o,
            None => return ToolResult::error("Missing 'old_string' parameter"),
        };
        let new = match args.get("new_string").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("Missing 'new_string' parameter"),
        };
        let replace_all = args
            .get("replace_all")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read {}: {}", path, e)),
        };

        if !content.contains(old) {
            return ToolResult::error(format!("'old_string' not found in {}", path));
        }

        let new_content = if replace_all {
            content.replace(old, new)
        } else {
            // Check uniqueness
            let count = content.matches(old).count();
            if count > 1 {
                return ToolResult::error(format!(
                    "'old_string' appears {} times in {} - use replace_all=true or provide more context",
                    count, path
                ));
            }
            content.replacen(old, new, 1)
        };

        match std::fs::write(path, new_content) {
            Ok(()) => ToolResult::success(format!("Edited {}", path)),
            Err(e) => ToolResult::error(format!("Failed to write {}: {}", path, e)),
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
            result.output.contains("exceeding the"),
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
            result.output.contains("appears 2 times"),
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
