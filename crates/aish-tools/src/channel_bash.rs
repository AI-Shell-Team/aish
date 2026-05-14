//! Channel-based bash tool for SSH sessions.
//!
//! When the LLM calls bash_exec in an SSH session, this tool sends the command
//! through a channel to the forwarding loop. The forwarding loop executes it on
//! the remote host and returns the output through a response channel.
//!
//! Long output is offloaded to a file on the **remote** host so the LLM can
//! access it via subsequent bash commands. Only a preview is sent inline.

use aish_llm::{Tool, ToolResult};
use aish_pty::{truncate_utf8_safe, AiEvent, BashExecResult};

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
        // Use a long default timeout to account for user confirmation
        // delay and slow remote command execution over SSH.
        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(1800);

        let (output_tx, output_rx) = std::sync::mpsc::channel::<BashExecResult>();

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

        let result = match output_rx.recv_timeout(std::time::Duration::from_secs(timeout_secs))
        {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return ToolResult::error(
                    aish_i18n::t("tools.bash.execute_failed").replace("{error}", "timeout"),
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return ToolResult::error("Channel closed");
            }
        };

        // If the forwarding loop wrote the output to a remote file,
        // return a preview with the remote path instead of doing local
        // offload (the LLM's bash commands run on the remote host).
        if let Some(ref remote_path) = result.remote_offload_path {
            let preview = if result.output.len() > 1024 {
                let (truncated, _) = truncate_utf8_safe(
                    result.output.as_bytes(),
                    1024,
                );
                String::from_utf8_lossy(&truncated).to_string()
            } else {
                result.output.clone()
            };
            let offload_payload = serde_json::json!({
                "status": "offloaded",
                "stdout_path": remote_path,
                "hint": "Read offload paths for full output"
            });
            let output_text = crate::registry::format_tagged_result(
                &preview,
                "",
                0,
                Some(&offload_payload),
            );
            return ToolResult {
                ok: true,
                output: output_text,
                meta: Some(offload_payload),
            };
        }

        // No remote offload — apply local BashOutputOffload for preview.
        let session_uuid = uuid::Uuid::new_v4().to_string();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let settings = aish_pty::BashOffloadSettings::default();
        let offloader = aish_pty::BashOutputOffload::new(&session_uuid, &cwd, settings);
        let offload_result = offloader.render(&result.output, "", &command, 0);

        let output_text = crate::registry::format_tagged_result(
            &offload_result.stdout_text,
            &offload_result.stderr_text,
            0,
            offload_result.offload_payload.as_ref(),
        );

        ToolResult {
            ok: true,
            output: output_text,
            meta: offload_result
                .offload_payload
                .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null)),
        }
    }
}
