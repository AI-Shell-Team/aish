use std::process::Command;

use aish_i18n;
use aish_llm::{Tool, ToolResult};

/// Cached translated description.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.python.description"))
}

/// Tool for executing Python code.
pub struct PythonTool;

impl Default for PythonTool {
    fn default() -> Self {
        Self::new()
    }
}

impl PythonTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for PythonTool {
    fn name(&self) -> &str {
        "python_exec"
    }

    fn description(&self) -> &str {
        get_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "The Python code to execute."
                }
            },
            "required": ["code"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let code = match args.get("code").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return ToolResult::error(aish_i18n::t("tools.python.missing_code")),
        };

        // Build a wrapper script that captures stdout and handles errors
        let wrapper = format!(
            "import sys, os\n\
             os.chdir({:?})\n\
             try:\n\
             {}\
             except Exception as e:\n\
             print(f'Error: {{e}}', file=sys.stderr)\n\
             sys.exit(1)",
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/".to_string()),
            indent_each_line(code, "    ")
        );

        let result = Command::new("python3")
            .arg("-c")
            .arg(&wrapper)
            .env("PYTHONIOENCODING", "utf-8")
            .output();

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let mut result_text = String::new();
                if !stdout.is_empty() {
                    result_text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result_text.is_empty() {
                        result_text.push('\n');
                    }
                    result_text.push_str(&format!("[stderr]\n{}", stderr));
                }

                if exit_code == 0 && result_text.is_empty() {
                    ToolResult::success(aish_i18n::t("tools.python.no_output"))
                } else if exit_code == 0 {
                    ToolResult::success(result_text)
                } else {
                    ToolResult {
                        ok: false,
                        output: result_text,
                        meta: Some(serde_json::json!({"exit_code": exit_code})),
                    }
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ToolResult::error(aish_i18n::t("tools.python.not_installed"))
                } else {
                    let mut args_map = std::collections::HashMap::new();
                    args_map.insert("error".to_string(), e.to_string());
                    ToolResult::error(aish_i18n::t_with_args(
                        "tools.python.execute_failed",
                        &args_map,
                    ))
                }
            }
        }
    }
}

/// Indent each line of code for embedding inside a try block.
fn indent_each_line(code: &str, indent: &str) -> String {
    code.lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{}{}", indent, line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
