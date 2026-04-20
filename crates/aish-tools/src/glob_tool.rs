use std::path::PathBuf;

use aish_llm::{Tool, ToolResult};

/// Directories excluded by default (VCS and common large generated trees).
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

/// Tool for enumerating files by glob pattern within a directory.
pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Enumerate files by glob pattern within a directory. Automatically excludes VCS directories (.git, .svn, …) and common generated trees (node_modules, __pycache__, .venv, target)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern such as **/*.py or src/**/*.md"
                },
                "root": {
                    "type": "string",
                    "description": "Optional search root directory. Defaults to the current working directory."
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => return ToolResult::error("Error: pattern is required"),
        };

        let root = normalize_root(args.get("root").and_then(|r| r.as_str()));
        if !root.exists() || !root.is_dir() {
            return ToolResult::error(format!(
                "Error: root directory not found: {}",
                root.display()
            ));
        }

        // Build the full glob pattern from root + pattern
        let full_pattern = if pattern.starts_with('/') {
            pattern.clone()
        } else {
            format!("{}/{}", root.display(), pattern)
        };

        let mut matches: Vec<PathBuf> = Vec::new();
        match glob::glob(&full_pattern) {
            Ok(paths) => {
                for entry in paths.flatten() {
                    // Skip excluded directories
                    if is_in_excluded_dir(&entry) {
                        continue;
                    }
                    matches.push(entry);
                }
            }
            Err(e) => {
                return ToolResult::error(format!("Error: invalid glob pattern: {}", e));
            }
        }

        matches.sort();
        matches.dedup();

        if matches.is_empty() {
            return ToolResult::success("No files found.");
        }

        let truncated = matches.len() > DEFAULT_MAX_RESULTS;
        let display: Vec<String> = matches
            .into_iter()
            .take(DEFAULT_MAX_RESULTS)
            .map(|p| p.display().to_string())
            .collect();

        let mut output = display.join("\n");
        if truncated {
            output.push_str("\n(results truncated at 200)");
        }

        ToolResult::success(output)
    }
}

/// Check if a path contains any excluded directory component.
fn is_in_excluded_dir(path: &std::path::Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| DEFAULT_EXCLUDE_DIRS.contains(&s))
            .unwrap_or(false)
    })
}

/// Resolve the root directory from the optional argument.
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
    fn test_is_in_excluded_dir() {
        assert!(is_in_excluded_dir(
            PathBuf::from("/home/user/project/.git/config").as_path()
        ));
        assert!(is_in_excluded_dir(
            PathBuf::from("/home/user/project/node_modules/foo/bar.js").as_path()
        ));
        assert!(!is_in_excluded_dir(
            PathBuf::from("/home/user/project/src/main.rs").as_path()
        ));
    }

    #[test]
    fn test_normalize_root_default() {
        let root = normalize_root(None);
        assert!(root.is_absolute());
    }

    #[test]
    fn test_normalize_root_explicit() {
        let root = normalize_root(Some("/tmp"));
        assert_eq!(root, PathBuf::from("/tmp"));
    }

    #[test]
    fn test_glob_tool_missing_pattern() {
        let tool = GlobTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
    }
}
