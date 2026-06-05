use aish_i18n;
use aish_llm::{Tool, ToolResult};

use super::prompt;

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
}
