use std::path::Path;

use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;

const MAX_WRITE_BYTES: usize = 32 * 1024;

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
}
