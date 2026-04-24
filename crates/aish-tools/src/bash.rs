use std::sync::Arc;
use std::time::Duration;

use aish_i18n;
use aish_llm::{Tool, ToolResult};
use aish_pty::{BashOffloadSettings, BashOutputOffload, CancelToken, PtyExecutor};

/// Large keep_bytes for the silent PTY executor to capture full command output.
/// The BashOutputOffload will handle threshold-based truncation and disk offload.
const CAPTURE_KEEP_BYTES: usize = 10 * 1024 * 1024; // 10MB

/// Check if a command likely needs interactive stdin (e.g. sudo password prompt).
/// False positives are acceptable because output is still captured for the LLM.
fn needs_interactive(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.contains("sudo") || lower.contains(" su ") || lower.starts_with("su ")
}

/// Tool for executing bash commands via PTY.
pub struct BashTool {}

/// Cached translated description.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.bash.description"))
}

fn timeout_secs(args: &serde_json::Value) -> Result<Option<u64>, ToolResult> {
    match args.get("timeout") {
        None => Ok(None),
        Some(timeout) => match timeout.as_i64() {
            Some(seconds) if seconds > 0 => Ok(Some(seconds as u64)),
            _ => Err(ToolResult::error(aish_i18n::t(
                "tools.bash.invalid_timeout",
            ))),
        },
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BashTool {
    pub fn new() -> Self {
        Self {}
    }
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        get_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Timeout in seconds. If omitted, the command runs until completion or cancellation."
                }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let command = match args.get("command").and_then(|c| c.as_str()) {
            Some(cmd) => cmd,
            None => return ToolResult::error(aish_i18n::t("tools.bash.missing_command")),
        };
        let timeout_secs = match timeout_secs(&args) {
            Ok(value) => value,
            Err(error) => return error,
        };

        let executor = if needs_interactive(command) {
            PtyExecutor::new(CAPTURE_KEEP_BYTES)
        } else {
            PtyExecutor::new_silent(CAPTURE_KEEP_BYTES)
        };
        let cancel_token = Arc::new(CancelToken::new());

        if let Some(timeout_secs) = timeout_secs {
            let timeout_token = Arc::clone(&cancel_token);
            let timeout_duration = Duration::from_secs(timeout_secs);
            std::thread::spawn(move || {
                std::thread::sleep(timeout_duration);
                timeout_token.cancel();
            });
        }

        let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();

        match executor.execute_blocking(command, env_vars, &cancel_token) {
            Ok(result) => {
                // Use BashOutputOffload for threshold-based offload, matching Python's
                // render_bash_output(). This handles:
                // - Checking if output > threshold_bytes (1KB)
                // - Writing full output to disk (stdout.txt, stderr.txt, result.json)
                // - Returning HEAD preview (1KB) + offload payload with paths and hint
                let session_uuid = uuid::Uuid::new_v4().to_string();
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let settings = BashOffloadSettings::default();
                let offloader = BashOutputOffload::new(&session_uuid, &cwd, settings);
                let offload_result =
                    offloader.render(&result.stdout, &result.stderr, command, result.exit_code);

                let output = crate::registry::format_tagged_result(
                    &offload_result.stdout_text,
                    &offload_result.stderr_text,
                    result.exit_code,
                    offload_result.offload_payload.as_ref(),
                );

                ToolResult {
                    ok: true,
                    output,
                    meta: offload_result
                        .offload_payload
                        .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null)),
                }
            }
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("error".to_string(), e.to_string());
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.bash.execute_failed",
                    &args_map,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_interactive_sudo() {
        assert!(needs_interactive("sudo apt update"));
        assert!(needs_interactive("echo 3 | sudo tee /file"));
        assert!(needs_interactive("sudo -u root ls"));
        assert!(needs_interactive("w;sudo ip a"));
        assert!(needs_interactive("sync&&sudo tee /file"));
    }

    #[test]
    fn test_needs_interactive_su() {
        assert!(needs_interactive("su -"));
        assert!(needs_interactive("su -c 'whoami'"));
        assert!(needs_interactive("cmd && su -"));
        assert!(needs_interactive("cmd || su -"));
        assert!(needs_interactive("cmd | su -"));
    }

    #[test]
    fn test_needs_interactive_no() {
        assert!(!needs_interactive("ls -la"));
        assert!(!needs_interactive("echo hello"));
    }

    #[test]
    #[ignore] // PTY output not reliable in CI environments
    fn test_bash_tool_echo() {
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({
            "command": "echo hello world"
        }));
        assert!(result.ok, "echo should succeed");
        assert!(
            result.output.contains("hello world"),
            "output should contain 'hello world', got: {}",
            result.output
        );
    }

    #[test]
    fn test_bash_tool_exit_code() {
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({
            "command": "exit 42"
        }));
        assert!(
            result.ok,
            "tool execution should succeed regardless of exit code"
        );
        assert!(
            result.output.contains("<return_code>\n42\n</return_code>"),
            "output should mention return code 42, got: {}",
            result.output
        );
    }

    #[test]
    fn test_bash_tool_timeout() {
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({
            "command": "sleep 60",
            "timeout": 1
        }));
        assert!(
            result.ok,
            "tool execution should succeed even after timeout kill"
        );
    }

    #[test]
    fn test_bash_tool_parameters_do_not_advertise_default_timeout() {
        let tool = BashTool::new();
        let params = tool.parameters();
        let timeout = &params["properties"]["timeout"];

        assert!(timeout.get("default").is_none());
        assert_eq!(
            timeout["description"].as_str(),
            Some("Timeout in seconds. If omitted, the command runs until completion or cancellation.")
        );
    }

    #[test]
    fn test_timeout_secs_is_optional() {
        assert_eq!(
            timeout_secs(&serde_json::json!({ "command": "echo hi" })).unwrap(),
            None
        );
        assert_eq!(
            timeout_secs(&serde_json::json!({ "command": "echo hi", "timeout": 3 })).unwrap(),
            Some(3)
        );
    }

    #[test]
    fn test_timeout_secs_rejects_invalid_values() {
        assert!(timeout_secs(&serde_json::json!({ "command": "echo hi", "timeout": 0 })).is_err());
        assert!(timeout_secs(&serde_json::json!({ "command": "echo hi", "timeout": -1 })).is_err());
        assert!(
            timeout_secs(&serde_json::json!({ "command": "echo hi", "timeout": "5" })).is_err()
        );
    }
}
