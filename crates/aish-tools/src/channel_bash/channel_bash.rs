//! Channel-based bash tool for SSH sessions.
//!
//! When the LLM calls bash_exec in an SSH session, this tool sends the command
//! through a channel to the forwarding loop. The forwarding loop executes it on
//! the remote host and returns the output through a response channel.
//!
//! Long output is offloaded to a file on the **remote** host so the LLM can
//! access it via subsequent bash commands. Only a preview is sent inline.
//!
//! The wait is sliced into short [`WAIT_SLICE`] windows so the session
//! cancellation token is checked while waiting (issue #547): a user Ctrl+C
//! aborts the wait immediately instead of blocking for up to the full
//! timeout, and the LLM session stops cleanly on the short-circuit result.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aish_llm::{CancellationToken, Tool, ToolResult};
use aish_pty::{truncate_utf8_safe, AiEvent, BashExecResult};

use super::prompt;

/// How often the wait wakes to check the cancel token. Short enough that a
/// Ctrl+C feels immediate; long enough that the syscall overhead is nil.
const WAIT_SLICE: Duration = Duration::from_millis(200);

pub struct ChannelBashTool {
    event_sender: std::sync::mpsc::Sender<AiEvent>,
    cancellation_token: Option<Arc<CancellationToken>>,
}

impl ChannelBashTool {
    pub fn new(event_sender: std::sync::mpsc::Sender<AiEvent>) -> Self {
        Self {
            event_sender,
            cancellation_token: None,
        }
    }

    /// Bind the LLM session's cancellation token so Ctrl+C interrupts the
    /// wait for the remote result (issue #547).
    pub fn with_cancellation_token(mut self, token: Arc<CancellationToken>) -> Self {
        self.cancellation_token = Some(token);
        self
    }
}

impl Tool for ChannelBashTool {
    fn name(&self) -> &str {
        "bash"
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
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolResult::error(aish_i18n::t("tools.bash.missing_command")),
        };
        // Use a long default timeout to account for user confirmation
        // delay and slow remote command execution over SSH.
        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(1800);

        // Validate the timeout before dispatching: the schema has no upper
        // bound and as_u64() accepts any non-negative JSON integer, so an
        // absurd value would make Instant + Duration panic. checked_add
        // rejects it up front (issue #551 review).
        let deadline = Instant::now().checked_add(Duration::from_secs(timeout_secs));
        let Some(deadline) = deadline else {
            return ToolResult::error(aish_i18n::t("tools.bash.invalid_timeout"));
        };

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

        // Wait in slices, checking the cancel token between windows. A plain
        // recv_timeout blocks the whole timeout without observing Ctrl+C.
        let result = loop {
            if self
                .cancellation_token
                .as_ref()
                .is_some_and(|t| t.is_cancelled())
            {
                return user_cancelled_result();
            }
            if Instant::now() >= deadline {
                return ToolResult::error(
                    aish_i18n::t("tools.bash.execute_failed").replace("{error}", "timeout"),
                );
            }
            match output_rx.recv_timeout(WAIT_SLICE) {
                Ok(r) => break r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return ToolResult::error("Channel closed");
                }
            }
        };

        // If output was offloaded to a local file, return preview with the path.
        if let Some(ref raw_offload_path) = result.offload_path {
            // Prefer .clean (ANSI-stripped, valid UTF-8) over .raw for
            // read_file compatibility.
            let offload_path = {
                let p = std::path::PathBuf::from(raw_offload_path);
                if p.extension().is_some_and(|ext| ext == "raw") {
                    let clean = p.with_extension("clean");
                    if clean.exists() {
                        clean.to_str().unwrap_or(raw_offload_path).to_string()
                    } else {
                        raw_offload_path.clone()
                    }
                } else {
                    raw_offload_path.clone()
                }
            };
            let preview = if result.output.len() > 1024 {
                let (truncated, _) = truncate_utf8_safe(result.output.as_bytes(), 1024);
                String::from_utf8_lossy(&truncated).to_string()
            } else {
                result.output.clone()
            };
            let offload_payload = serde_json::json!({
                "status": "offloaded",
                "stdout_path": offload_path,
                "hint": "Use read_file tool to read the offload path for full output (file is on the LOCAL machine)"
            });
            let output_text =
                crate::registry::format_tagged_result(&preview, "", 0, Some(&offload_payload));
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

/// Short-circuit result on user cancel: stops the tool loop and is shown as
/// `shell.interrupted` by the shell (same convention as the local BashTool).
fn user_cancelled_result() -> ToolResult {
    ToolResult {
        ok: false,
        output: aish_i18n::t("shell.interrupted"),
        meta: Some(serde_json::json!({
            "dispatch_status": "short_circuit",
            "reason": "user_cancelled",
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #547 regression: a Ctrl+C while waiting for the forwarding
    /// loop must abort the wait immediately with a short-circuit result
    /// instead of blocking until the tool timeout.
    #[test]
    fn cancel_interrupts_wait_without_remote_result() {
        let (tx, rx) = std::sync::mpsc::channel::<AiEvent>();
        // Simulate the forwarding loop: receive the BashExec event, then
        // stall forever — the remote command never completes.
        let (_done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            if let Ok(AiEvent::BashExec { output_sender, .. }) = rx.recv() {
                let _ = output_sender;
                let _ = done_rx.recv();
            }
        });

        let token = Arc::new(CancellationToken::new());
        let canceller = {
            let token = Arc::clone(&token);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                token.cancel();
            })
        };

        let tool = ChannelBashTool::new(tx).with_cancellation_token(Arc::clone(&token));
        let started = Instant::now();
        let result = tool.execute(serde_json::json!({
            "command": "sleep 600",
            "timeout": 60
        }));
        canceller.join().unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "wait ignored cancellation: {:?}",
            started.elapsed()
        );
        assert!(!result.ok);
        assert_eq!(
            result.meta.and_then(|m| m.get("reason").cloned()),
            Some(serde_json::json!("user_cancelled"))
        );
    }

    /// Issue #551 review: the schema has no upper bound on `timeout`, and
    /// `Instant + Duration` panics when the result is not representable.
    /// An absurd value must fail validation instead of panicking.
    #[test]
    fn absurd_timeout_errors_instead_of_panicking() {
        let (tx, _rx) = std::sync::mpsc::channel::<AiEvent>();
        let tool = ChannelBashTool::new(tx);
        let result = tool.execute(serde_json::json!({
            "command": "echo hi",
            "timeout": 18446744073709551615u64
        }));
        assert!(!result.ok);
        assert!(result.output.contains("timeout"), "out: {}", result.output);
    }
}
