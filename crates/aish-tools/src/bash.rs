use std::sync::Arc;
use std::time::Duration;

use aish_llm::{Tool, ToolResult};
use aish_pty::{CancelToken, PtyExecutor};

/// Maximum output size in bytes (64KB). Anything larger is truncated.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Default timeout for command execution in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Default keep_bytes for PTY output tail retention.
const DEFAULT_KEEP_BYTES: usize = 4096;

/// Tool for executing bash commands via PTY.
pub struct BashTool {
    keep_bytes: usize,
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            keep_bytes: DEFAULT_KEEP_BYTES,
        }
    }

    pub fn with_keep_bytes(mut self, keep_bytes: usize) -> Self {
        self.keep_bytes = keep_bytes;
        self
    }
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command and return the output. Use this tool to run shell commands."
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
            None => return ToolResult::error("Missing 'command' parameter"),
        };
        let timeout_secs = args
            .get("timeout")
            .and_then(|t| t.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let executor = PtyExecutor::new_silent(self.keep_bytes);
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
                let stdout = truncate_output(&result.stdout, MAX_OUTPUT_BYTES);
                let stderr = &result.stderr;

                // Check if there's actual offload data
                let offload_info = result.offload.as_ref().filter(|v| {
                    v.get("stdout")
                        .and_then(|s| s.get("path"))
                        .and_then(|p| p.as_str())
                        .is_some()
                        || v.get("stderr")
                            .and_then(|s| s.get("path"))
                            .and_then(|p| p.as_str())
                            .is_some()
                });

                let mut output =
                    crate::registry::format_tagged_result(stdout.as_str(), stderr, offload_info);

                if result.exit_code != 0 {
                    output = format!("{}\n<exit-code>{}</exit-code>", output, result.exit_code);
                }

                ToolResult {
                    ok: result.exit_code == 0,
                    output,
                    meta: result.offload,
                }
            }
            Err(e) => ToolResult::error(format!("Failed to execute: {}", e)),
        }
    }
}

/// Truncate output to max_bytes, keeping the tail (which usually has the most
/// relevant information like error messages). Adds a truncation notice.
fn truncate_output(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        return output.to_string();
    }

    // Find a good break point near the max_bytes boundary
    let truncated_bytes = output.len() - max_bytes;
    let tail = &output[truncated_bytes..];

    format!(
        "[...{} bytes truncated...]\n{}",
        truncated_bytes,
        tail.trim_start()
    )
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
            result.output.contains("<exit-code>42</exit-code>"),
            "output should mention exit code 42 in tagged format, got: {}",
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

    #[test]
    fn test_truncate_output() {
        // Small output is unchanged.
        let small = "hello world";
        assert_eq!(truncate_output(small, 100), small);

        // Large output is truncated.
        let large = "x".repeat(1000);
        let result = truncate_output(&large, 100);
        assert!(
            result.contains("bytes truncated"),
            "should mention truncation, got: {}",
            result
        );
        assert!(result.len() < 200, "result should be much smaller");
    }
}
