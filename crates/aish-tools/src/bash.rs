use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aish_i18n;
use aish_llm::{
    CancellationToken, PreflightResult, PreflightSecurityContext, SecurityPanelMode, Tool,
    ToolResult,
};
use aish_pty::{BashOffloadSettings, BashOutputOffload, CancelToken, PtyExecutor};
use aish_security::{load_policy, SecurityDecision, SecurityManager, SecurityRequest};

/// Large keep_bytes for the silent PTY executor to capture full command output.
/// The BashOutputOffload will handle threshold-based truncation and disk offload.
const CAPTURE_KEEP_BYTES: usize = 10 * 1024 * 1024; // 10MB

/// Commands that need a real terminal for interactive use.
const INTERACTIVE_COMMANDS: &[&str] = &[
    "vim", "vi", "nano", "emacs", "ssh", "telnet", "mosh", "htop", "top", "btop", "iotop", "less",
    "more", "most", "man", "screen", "tmux", "mc", "ranger",
];

/// Commands that establish a remote session where Ctrl-C should be forwarded
/// as a character rather than converted to SIGINT.
const SESSION_COMMANDS: &[&str] = &["ssh", "telnet", "mosh", "nc", "netcat", "ftp", "sftp"];

/// Check if a command likely needs interactive stdin (e.g. sudo password prompt,
/// ssh password, or a full-screen TUI program).
/// False positives are acceptable because output is still captured for the LLM.
fn needs_interactive(command: &str) -> bool {
    let lower = command.to_lowercase();

    // sudo / su always need interactive stdin for password prompts.
    if lower.contains("sudo") || lower.contains(" su ") || lower.starts_with("su ") {
        return true;
    }

    // Check first word against known interactive commands.
    let first = command.split_whitespace().next().unwrap_or("");
    let basename = first.rsplit('/').next().unwrap_or(first);

    if INTERACTIVE_COMMANDS.contains(&basename) || SESSION_COMMANDS.contains(&basename) {
        return true;
    }

    false
}

pub fn command_needs_interactive(command: &str) -> bool {
    needs_interactive(command)
}

/// Shared slot for injecting a PersistentPty reference after tool creation.
/// None = fall back to PtyExecutor (one-shot PTY).
pub type PtySlot = Arc<Mutex<Option<Arc<Mutex<aish_pty::PersistentPty>>>>>;

static INTERACTIVE_INPUT_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn interactive_input_active() -> bool {
    INTERACTIVE_INPUT_COUNT.load(Ordering::SeqCst) > 0
}

struct InteractiveInputGuard;

impl InteractiveInputGuard {
    fn acquire() -> Self {
        INTERACTIVE_INPUT_COUNT.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for InteractiveInputGuard {
    fn drop(&mut self) {
        let previous = INTERACTIVE_INPUT_COUNT.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0, "interactive input guard count underflow");
        if previous == 0 {
            INTERACTIVE_INPUT_COUNT.store(0, Ordering::SeqCst);
        }
    }
}

/// Tool for executing bash commands via PTY.
pub struct BashTool {
    /// Shared cancellation token from the AI handler.
    /// When the user presses Ctrl+C during AI processing, this token is set,
    /// and a bridge thread propagates the cancellation to the tool's CancelToken.
    cancellation_token: Option<Arc<CancellationToken>>,
    /// Shared slot for PersistentPty — set after PTY creation.
    pty_slot: PtySlot,
}

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

fn security_preflight(
    command: &str,
    cwd: Option<&Path>,
    config_path: Option<&Path>,
) -> PreflightResult {
    security_preflight_with_socket(command, cwd, config_path, None)
}

fn security_preflight_with_socket(
    command: &str,
    cwd: Option<&Path>,
    config_path: Option<&Path>,
    sandbox_socket_path: Option<&Path>,
) -> PreflightResult {
    let policy = load_policy(config_path);
    let manager = SecurityManager::new(policy.clone());
    let mut request = SecurityRequest::ai_command()
        .with_repo_root("/")
        .with_sandbox_timeout_s(policy.sandbox_timeout_seconds);
    if let Some(cwd) = cwd {
        request = request.with_cwd(cwd.to_path_buf());
    }
    if let Some(socket_path) = sandbox_socket_path {
        request = request.with_sandbox_socket_path(socket_path.to_path_buf());
    }
    let decision = manager.decide_with_request(command, &request);

    if !decision.allow {
        return PreflightResult::Block {
            message: format_security_message(&decision),
            security: Some(build_security_context(
                command,
                &decision,
                SecurityPanelMode::Blocked,
            )),
        };
    }

    if decision.require_confirmation {
        return PreflightResult::Confirm {
            message: format_security_message(&decision),
            security: Some(build_security_context(
                command,
                &decision,
                SecurityPanelMode::Confirm,
            )),
        };
    }

    PreflightResult::Allow
}

fn format_security_message(decision: &SecurityDecision) -> String {
    if let Some(confirm_message) = decision.analysis.confirm_message.as_deref() {
        let trimmed = confirm_message.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let impact = decision.analysis.impact_description.trim();
    if !impact.is_empty() {
        return impact.to_string();
    }

    if let Some(reason) = decision
        .analysis
        .reasons
        .iter()
        .find(|reason| !is_internal_security_reason(reason))
    {
        return reason.clone();
    }

    let mut args = std::collections::HashMap::new();
    args.insert("level".to_string(), decision.level.to_string());
    aish_i18n::t_with_args("tools.bash.security_handling_required", &args)
}

fn build_security_context(
    command: &str,
    decision: &SecurityDecision,
    mode: SecurityPanelMode,
) -> PreflightSecurityContext {
    PreflightSecurityContext {
        tool_name: "bash".to_string(),
        target: Some(command.to_string()),
        message: format_security_message(decision),
        mode,
        decision: Some(decision.clone()),
    }
}

fn is_internal_security_reason(reason: &str) -> bool {
    [
        "sandbox degraded because ",
        "sandbox exit_code:",
        "policy fallback rule matched for command ",
        "matched paths: ",
        "sandbox matched ",
        "sandbox observed no filesystem changes;",
        "sandbox changes matched no policy rule;",
        "unmatched paths: ",
        "sandbox change list was truncated;",
        "sandbox execution returned non-zero exit code ",
    ]
    .iter()
    .any(|prefix| reason.starts_with(prefix))
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            cancellation_token: None,
            pty_slot: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the shared cancellation token from the AI handler.
    /// This allows Ctrl+C during AI processing to cancel in-progress tool execution.
    pub fn set_cancellation_token(&mut self, token: Arc<CancellationToken>) {
        self.cancellation_token = Some(token);
    }

    /// Set the shared PersistentPty slot for Ctrl+Z/bg/fg support.
    pub fn set_pty_slot(&mut self, slot: PtySlot) {
        self.pty_slot = slot;
    }

    /// Execute via PersistentPty — supports Ctrl+Z/bg/fg job control.
    fn execute_via_persistent_pty(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
        pty_arc: Arc<Mutex<aish_pty::PersistentPty>>,
    ) -> ToolResult {
        let interactive = needs_interactive(command);
        let cancel_token = Arc::new(CancelToken::new());

        if let Some(timeout_secs) = timeout_secs {
            let timeout_token = Arc::clone(&cancel_token);
            let timeout_duration = Duration::from_secs(timeout_secs);
            std::thread::spawn(move || {
                std::thread::sleep(timeout_duration);
                timeout_token.cancel();
            });
        }

        // Bridge: AI handler cancellation -> tool cancel token.
        let done = Arc::new(AtomicBool::new(false));
        if let Some(ref ct) = self.cancellation_token {
            let ct = Arc::clone(ct);
            let tool_cancel = Arc::clone(&cancel_token);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    if ct.is_cancelled() {
                        tool_cancel.cancel();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }

        let mut pty = pty_arc.lock().unwrap();
        let command_timeout = Duration::from_secs(timeout_secs.unwrap_or(365 * 24 * 60 * 60));
        let _interactive_guard = interactive.then(InteractiveInputGuard::acquire);
        let result =
            pty.execute_command(command, command_timeout, Some(&cancel_token), interactive);
        done.store(true, Ordering::SeqCst);

        match result {
            Ok((output, exit_code)) => {
                let session_uuid = uuid::Uuid::new_v4().to_string();
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let settings = BashOffloadSettings::default();
                let offloader = BashOutputOffload::new(&session_uuid, &cwd, settings);
                let offload_result = offloader.render(&output, &"", command, exit_code);

                let output_text = crate::registry::format_tagged_result(
                    &offload_result.stdout_text,
                    &offload_result.stderr_text,
                    exit_code,
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

    /// Execute via one-shot PtyExecutor — original behavior.
    fn execute_via_pty_executor(&self, command: &str, timeout_secs: Option<u64>) -> ToolResult {
        let interactive = needs_interactive(command);
        let executor = if interactive {
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

        let done = Arc::new(AtomicBool::new(false));
        if let Some(ref ct) = self.cancellation_token {
            let ct = Arc::clone(ct);
            let tool_cancel = Arc::clone(&cancel_token);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    if ct.is_cancelled() {
                        tool_cancel.cancel();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }

        let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
        let _interactive_guard = interactive.then(InteractiveInputGuard::acquire);
        let result = executor.execute_blocking(command, env_vars, &cancel_token);
        done.store(true, Ordering::SeqCst);

        match result {
            Ok(result) => {
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

    fn preflight(&self, args: &serde_json::Value) -> PreflightResult {
        let Some(command) = args.get("command").and_then(|c| c.as_str()) else {
            return PreflightResult::Allow;
        };

        let cwd = std::env::current_dir().ok();
        security_preflight(command, cwd.as_deref(), None)
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

        // Try PersistentPty path first (supports Ctrl+Z/bg/fg).
        let pty_arc = {
            let guard = self.pty_slot.lock().unwrap();
            guard.clone()
        };
        if let Some(pty_arc) = pty_arc {
            return self.execute_via_persistent_pty(command, timeout_secs, pty_arc);
        }

        // Fallback: one-shot PtyExecutor.
        self.execute_via_pty_executor(command, timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    use tempfile::tempdir;

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
        assert!(!needs_interactive("cat /etc/hosts"));
        assert!(!needs_interactive("grep pattern file.txt"));
    }

    #[test]
    fn test_needs_interactive_ssh() {
        assert!(needs_interactive("ssh user@host"));
        assert!(needs_interactive("ssh -l root 10.10.17.112"));
        assert!(needs_interactive("/usr/bin/ssh user@host"));
        assert!(needs_interactive("telnet example.com 23"));
        assert!(needs_interactive("mosh user@host"));
    }

    #[test]
    fn test_needs_interactive_tui() {
        assert!(needs_interactive("vim file.txt"));
        assert!(needs_interactive("htop"));
        assert!(needs_interactive("top"));
        assert!(needs_interactive("less /var/log/syslog"));
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
        // Tool succeeds even with non-zero exit — LLM reads <return_code> to decide.
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
        // Tool succeeds even when command is killed by timeout.
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
    fn test_interactive_input_guard_counts_overlapping_sessions() {
        INTERACTIVE_INPUT_COUNT.store(0, Ordering::SeqCst);

        let guard_one = InteractiveInputGuard::acquire();
        assert!(interactive_input_active());

        let guard_two = InteractiveInputGuard::acquire();
        assert!(interactive_input_active());

        drop(guard_one);
        assert!(interactive_input_active());

        drop(guard_two);
        assert!(!interactive_input_active());
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

    #[test]
    fn test_bash_tool_preflight_blocks_high_risk_commands() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\n  sandbox_off_action: ALLOW\n  default_risk_level: LOW\nrules:\n  - id: H-001\n    path: [/etc/**]\n    operations: [DELETE]\n    command_list: [rm]\n    risk: HIGH\n    reason: system config is protected\n",
        )
        .unwrap();

        let result = security_preflight("rm -rf /etc", None, Some(policy_path.as_path()));
        match result {
            PreflightResult::Block {
                message,
                security: Some(security),
            } => {
                assert_eq!(message, "system config is protected");
                assert_eq!(security.tool_name, "bash");
                assert_eq!(security.target.as_deref(), Some("rm -rf /etc"));
                assert_eq!(security.mode, SecurityPanelMode::Blocked);
                assert_eq!(security.message, "system config is protected");
                let decision = security.decision.expect("expected security decision");
                assert_eq!(decision.level.to_string(), "HIGH");
                assert_eq!(decision.analysis.matched_paths, vec!["/etc".to_string()]);
            }
            other => panic!("unexpected preflight result: {other:?}"),
        }
    }

    #[test]
    fn test_bash_tool_preflight_confirms_medium_risk_commands() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\n  sandbox_off_action: ALLOW\n  default_risk_level: LOW\nrules:\n  - id: M-001\n    path: [/home/**]\n    operations: [WRITE]\n    command_list: [cp]\n    risk: MEDIUM\n    reason: home path is protected\n",
        )
        .unwrap();

        let result = security_preflight(
            "cp /tmp/a.txt /home/lixin/a.txt",
            None,
            Some(policy_path.as_path()),
        );
        match result {
            PreflightResult::Confirm {
                message,
                security: Some(security),
            } => {
                assert_eq!(message, "home path is protected");
                assert_eq!(security.tool_name, "bash");
                assert_eq!(
                    security.target.as_deref(),
                    Some("cp /tmp/a.txt /home/lixin/a.txt")
                );
                assert_eq!(security.mode, SecurityPanelMode::Confirm);
                let decision = security.decision.expect("expected security decision");
                assert_eq!(decision.level.to_string(), "MEDIUM");
                assert_eq!(
                    decision.analysis.matched_paths,
                    vec!["/home/lixin/a.txt".to_string()]
                );
            }
            other => panic!("unexpected preflight result: {other:?}"),
        }
    }

    #[test]
    fn test_bash_tool_preflight_allows_low_risk_commands() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\n  sandbox_off_action: CONFIRM\n  default_risk_level: LOW\nrules: []\n",
        )
        .unwrap();

        let result = security_preflight("echo hi", None, Some(policy_path.as_path()));

        assert_eq!(result, PreflightResult::Allow);
    }

    #[test]
    fn test_bash_tool_preflight_uses_sandbox_result_when_enabled() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        let socket_path = dir.path().join("sandbox.sock");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: true\n  sandbox_off_action: ALLOW\n  sandbox_timeout_seconds: 13\n  default_risk_level: LOW\nrules:\n  - id: H-001\n    path: [/etc/**]\n    operations: [DELETE]\n    command_list: [rm]\n    risk: HIGH\n    reason: system config is protected\n    description: system config directory\n    suggestion: |\n      edit manually\n      open a ticket\n",
        )
        .unwrap();

        let listener = UnixListener::bind(&socket_path).unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request_buf.extend_from_slice(&chunk[..read]);
                if request_buf.contains(&b'\n') {
                    break;
                }
            }

            let request: serde_json::Value = serde_json::from_slice(&request_buf).unwrap();
            assert_eq!(request["command"], "rm -f /etc/aish/config.yaml");
            assert_eq!(request["repo_root"], "/");
            assert_eq!(request["timeout_s"], 13.0);
            let id = request["id"].as_str().unwrap();
            let response = serde_json::json!({
                "id": id,
                "ok": true,
                "result": {
                    "exit_code": 0,
                    "stdout": "",
                    "stderr": "",
                    "changes": [
                        {"path": "/etc/aish/config.yaml", "kind": "deleted"}
                    ],
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "changes_truncated": false
                }
            });
            writeln!(stream, "{}", response).unwrap();
        });

        let result = security_preflight_with_socket(
            "rm -f /etc/aish/config.yaml",
            Some(dir.path()),
            Some(policy_path.as_path()),
            Some(socket_path.as_path()),
        );

        handle.join().unwrap();
        match result {
            PreflightResult::Block {
                message,
                security: Some(security),
            } => {
                assert_eq!(message, "system config directory");
                assert_eq!(
                    security.target.as_deref(),
                    Some("rm -f /etc/aish/config.yaml")
                );
                assert_eq!(security.mode, SecurityPanelMode::Blocked);
                let decision = security.decision.expect("expected security decision");
                assert_eq!(decision.level.to_string(), "HIGH");
                assert_eq!(
                    decision.analysis.impact_description,
                    "system config directory"
                );
                assert_eq!(
                    decision.analysis.suggested_alternatives,
                    vec!["edit manually".to_string(), "open a ticket".to_string()]
                );
                assert_eq!(
                    decision.analysis.matched_paths,
                    vec!["/etc/aish/config.yaml".to_string()]
                );
            }
            other => panic!("unexpected preflight result: {other:?}"),
        }
    }

    #[test]
    fn test_bash_tool_preflight_surfaces_sandbox_degraded_when_fallback_matches() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        let socket_path = dir.path().join("missing.sock");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: true\n  sandbox_off_action: ALLOW\n  default_risk_level: LOW\nrules:\n  - id: M-001\n    path: [/home/**]\n    operations: [DELETE]\n    command_list: [rm]\n    risk: MEDIUM\n    reason: test-home-rule\n",
        )
        .unwrap();

        let result = security_preflight_with_socket(
            "rm /home/lixin/123",
            Some(dir.path()),
            Some(policy_path.as_path()),
            Some(socket_path.as_path()),
        );
        match result {
            PreflightResult::Confirm {
                message,
                security: Some(security),
            } => {
                assert_eq!(message, "test-home-rule");
                assert_eq!(security.target.as_deref(), Some("rm /home/lixin/123"));
                assert_eq!(security.mode, SecurityPanelMode::Confirm);
                let decision = security.decision.expect("expected security decision");
                assert_eq!(decision.level.to_string(), "MEDIUM");
                assert_eq!(
                    decision.analysis.sandbox.reason.as_deref(),
                    Some("sandbox_ipc_unavailable")
                );
                assert_eq!(
                    decision.analysis.matched_paths,
                    vec!["/home/lixin/123".to_string()]
                );
            }
            other => panic!("unexpected preflight result: {other:?}"),
        }
    }
}
