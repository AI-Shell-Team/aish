use std::sync::Arc;
use std::time::Duration;

use aish_i18n;
use aish_llm::{Tool, ToolResult};
use aish_pty::{BashOffloadSettings, BashOutputOffload, CancelToken, PtyExecutor};

/// Large keep_bytes for the silent PTY executor to capture full command output.
/// The BashOutputOffload will handle threshold-based truncation and disk offload.
const CAPTURE_KEEP_BYTES: usize = 10 * 1024 * 1024; // 10MB

/// Default timeout for command execution in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Tool for executing bash commands via PTY.
pub struct BashTool {}

/// Cached translated description.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.bash.description"))
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
                    "description": "Timeout in seconds (default: 120)",
                    "default": 120
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
        let timeout_secs = args
            .get("timeout")
            .and_then(|t| t.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Use large keep_bytes so the PTY executor captures full output in memory.
        let executor = PtyExecutor::new_silent(CAPTURE_KEEP_BYTES);
        let cancel_token = Arc::new(CancelToken::new());

        // Spawn a thread that cancels after timeout.
        let timeout_token = Arc::clone(&cancel_token);
        let timeout_duration = Duration::from_secs(timeout_secs);
        std::thread::spawn(move || {
            std::thread::sleep(timeout_duration);
            timeout_token.cancel();
        });

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
                    ok: result.exit_code == 0,
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
        assert!(!result.ok, "non-zero exit should report failure");
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
        // After timeout + cancellation, exit code should be non-zero.
        assert!(!result.ok, "timed-out command should report failure");
    }
}
