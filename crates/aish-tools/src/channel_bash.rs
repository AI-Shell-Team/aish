//! Channel-based bash tool for SSH sessions.
//!
//! When the LLM calls bash_exec in an SSH session, this tool sends the command
//! through a channel to the forwarding loop. The forwarding loop executes it on
//! the remote host and returns the output through a response channel.

use aish_llm::{Tool, ToolResult};
use aish_pty::AiEvent;

static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.bash.description"))
}

pub struct ChannelBashTool {
    event_sender: std::sync::mpsc::Sender<AiEvent>,
}

impl ChannelBashTool {
    pub fn new(event_sender: std::sync::mpsc::Sender<AiEvent>) -> Self {
        Self { event_sender }
    }
}

impl Tool for ChannelBashTool {
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
                    "description": aish_i18n::t("tools.bash.param.command")
                },
                "timeout": {
                    "type": "integer",
                    "description": aish_i18n::t("tools.bash.param.timeout"),
                    "default": 120
                }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolResult::error(aish_i18n::t("tools.bash.missing_command")),
        };
        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

        let (output_tx, output_rx) = std::sync::mpsc::channel::<String>();

        if self
            .event_sender
            .send(AiEvent::BashExec {
                command: command.clone(),
                output_sender: output_tx,
            })
            .is_err()
        {
            return ToolResult::error("Channel closed");
        }

        match output_rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
            Ok(output) => ToolResult::success(output),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => ToolResult::error(
                aish_i18n::t("tools.bash.execute_failed").replace("{error}", "timeout"),
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                ToolResult::error("Channel closed")
            }
        }
    }
}
