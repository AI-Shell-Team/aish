use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use aish_i18n;
use aish_llm::{Tool, ToolResult};

/// Cached translated description.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.grep.description"))
}

/// Directories excluded by default (shared with GlobTool).
const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    ".bzr",
    ".jj",
    ".sl",
    "node_modules",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    "venv",
    "target",
    "build",
    "dist",
];

const DEFAULT_MAX_RESULTS: usize = 200;
const MAX_LINE_LENGTH: usize = 500;

/// Tool for searching file contents by regex pattern.
pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        get_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "root": {
                    "type": "string",
                    "description": "Optional search root directory. Defaults to the current working directory."
                },
                "include": {
                    "type": "string",
                    "description": "Optional glob filter for file names, e.g. *.py or *.rs"
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let pattern_str = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) if !p.trim().is_empty() => p,
            _ => return ToolResult::error(aish_i18n::t("tools.grep.missing_pattern")),
        };

        let re = match regex::Regex::new(pattern_str) {
            Ok(re) => re,
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.grep.invalid_regex",
                    &args_map,
                ));
            }
        };

        let root = normalize_root(args.get("root").and_then(|r| r.as_str()));
        if !root.exists() || !root.is_dir() {
            return ToolResult::error(format!(
                "Error: root directory not found: {}",
                root.display()
            ));
        }

        let include_glob = args
            .get("include")
            .and_then(|g| g.as_str())
            .filter(|g| !g.trim().is_empty());

        let mut matches: Vec<String> = Vec::new();
        let files = walk_files(&root);

        for file_path in files {
            if matches.len() >= DEFAULT_MAX_RESULTS {
                break;
            }

            // Apply include filter
            if let Some(glob_pattern) = include_glob {
                let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !glob_match(glob_pattern, file_name) {
                    continue;
                }
            }

            // Skip binary / unreadable files
            let Ok(file) = File::open(&file_path) else {
                continue;
            };
            // Skip large files (>1MB)
            if file.metadata().map(|m| m.len()).unwrap_or(0) > 1_048_576 {
                continue;
            }

            let reader = BufReader::new(file);
            let rel_path = file_path
                .strip_prefix(&root)
                .unwrap_or(&file_path)
                .display()
                .to_string();

            for (line_no, line_result) in reader.lines().enumerate() {
                if matches.len() >= DEFAULT_MAX_RESULTS {
                    break;
                }
                let Ok(line) = line_result else {
                    continue;
                };
                if re.is_match(&line) {
                    let truncated = if line.len() > MAX_LINE_LENGTH {
                        format!("{}...", &line[..MAX_LINE_LENGTH])
                    } else {
                        line
                    };
                    matches.push(format!("{}:{}: {}", rel_path, line_no + 1, truncated));
                }
            }
        }

        if matches.is_empty() {
            return ToolResult::success("No matches found.");
        }

        let truncated = matches.len() >= DEFAULT_MAX_RESULTS;
        let mut output = matches.join("\n");
        if truncated {
            output.push_str("\n(results truncated at 200)");
        }

        ToolResult::success(output)
    }
}

/// Simple glob matching for the include filter (supports * wildcard only).
fn glob_match(pattern: &str, name: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

/// Walk directory tree, collecting file paths (excluding default dirs).
fn walk_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    walk_dir_recursive(root, &mut result);
    result.sort();
    result
}

fn walk_dir_recursive(dir: &std::path::Path, result: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !is_excluded_dir_name(&path) {
                    walk_dir_recursive(&path, result);
                }
            } else if path.is_file() {
                result.push(path);
            }
        }
    }
}

fn is_excluded_dir_name(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| DEFAULT_EXCLUDE_DIRS.contains(&s))
        .unwrap_or(false)
}

fn normalize_root(root: Option<&str>) -> PathBuf {
    match root {
        Some(r) if !r.trim().is_empty() => {
            let expanded = shellexpand::tilde(r).to_string();
            PathBuf::from(expanded)
        }
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grep_tool_missing_pattern() {
        let tool = GrepTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
    }

    #[test]
    fn test_grep_tool_invalid_regex() {
        let tool = GrepTool::new();
        let result = tool.execute(serde_json::json!({"pattern": "[invalid"}));
        assert!(!result.ok);
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(!glob_match("*.rs", "main.py"));
        assert!(glob_match("test_*", "test_foo"));
        assert!(!glob_match("test_*", "prod_foo"));
    }

    #[test]
    fn test_is_excluded_dir_name() {
        assert!(is_excluded_dir_name(PathBuf::from(".git").as_path()));
        assert!(is_excluded_dir_name(
            PathBuf::from("node_modules").as_path()
        ));
        assert!(!is_excluded_dir_name(PathBuf::from("src").as_path()));
    }
}
