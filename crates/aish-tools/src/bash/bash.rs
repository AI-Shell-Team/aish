use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aish_i18n;
use aish_llm::{
    CancellationToken, PreflightResult, PreflightSecurityContext, SecurityPanelMode, Tool,
    ToolContext, ToolResult,
};
use aish_pty::{BashOffloadSettings, BashOutputOffload, CancelToken, PtyExecutor};
use aish_security::{
    load_policy, secret::SecretVault, SecurityDecision, SecurityManager, SecurityPolicy,
    SecurityRequest,
};

use super::prompt;
use super::read_only::{self, preflight_enforce, ReadOnlyVerdict};

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

pub fn acquire_interactive_input_guard() -> InteractiveInputGuard {
    InteractiveInputGuard::acquire()
}

#[must_use]
pub struct InteractiveInputGuard;

impl InteractiveInputGuard {
    fn acquire() -> Self {
        let previous = INTERACTIVE_INPUT_COUNT.fetch_add(1, Ordering::SeqCst);
        if previous == 0 {
            // Background stdin readers poll with 100ms timeouts. Wait a bit
            // longer on the first acquisition so they can observe the guard
            // before the foreground prompt starts querying the terminal.
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
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

/// Shared vault slot for secret placeholder restoration.
pub type VaultSlot = Arc<Mutex<Option<Arc<Mutex<SecretVault>>>>>;

/// Live security policy shared with the shell's SecurityManager (`/setting`).
/// When set, bash preflight uses this snapshot instead of reloading from disk.
pub type PolicySlot = Arc<Mutex<Option<SecurityPolicy>>>;

/// Tool for executing bash commands via PTY.
pub struct BashTool {
    /// Shared cancellation token from the AI handler.
    /// When the user presses Ctrl+C during AI processing, this token is set,
    /// and a bridge thread propagates the cancellation to the tool's CancelToken.
    cancellation_token: Option<Arc<CancellationToken>>,
    /// Shared slot for PersistentPty — set after PTY creation.
    pty_slot: PtySlot,
    /// Shared vault for restoring $SECRET_* placeholders before command execution.
    secret_vault: VaultSlot,
    /// Live policy from the shell; falls back to `load_policy` when empty.
    policy_slot: PolicySlot,
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
    security_preflight_with_policy(command, cwd, load_policy(config_path), sandbox_socket_path)
}

fn security_preflight_with_policy(
    command: &str,
    cwd: Option<&Path>,
    policy: SecurityPolicy,
    sandbox_socket_path: Option<&Path>,
) -> PreflightResult {
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

fn bash_preflight(
    tool: &BashTool,
    args: &serde_json::Value,
    enforce_read_only_bash: bool,
) -> PreflightResult {
    let Some(command) = args.get("command").and_then(|c| c.as_str()) else {
        return PreflightResult::Allow;
    };

    // Restore secret placeholders so security checks run on the actual command.
    let (command, _) = tool.restore_secrets(command);

    if let Some(result) = preflight_enforce(&command, enforce_read_only_bash) {
        return result;
    }

    let cwd = std::env::current_dir().ok();
    if let Some(policy) = tool.live_policy() {
        security_preflight_with_policy(&command, cwd.as_deref(), policy, None)
    } else {
        // No live slot (standalone/tests) — load from disk SSOT.
        security_preflight(&command, cwd.as_deref(), None)
    }
}

fn format_security_message(decision: &SecurityDecision) -> String {
    let primary = primary_security_message(decision);
    annotate_security_message(primary, decision)
}

fn primary_security_message(decision: &SecurityDecision) -> String {
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

    if let Some(rule) = decision.analysis.matched_rule.as_ref() {
        if let Some(name) = rule
            .name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return name.to_string();
        }
        if let Some(id) = rule.id.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            return id.to_string();
        }
    }

    let mut args = std::collections::HashMap::new();
    args.insert("level".to_string(), decision.level.to_string());
    aish_i18n::t_with_args("tools.bash.security_handling_required", &args)
}

fn annotate_security_message(primary: String, decision: &SecurityDecision) -> String {
    let mut extras = Vec::new();
    if let Some(id) = decision
        .analysis
        .matched_rule
        .as_ref()
        .and_then(|rule| rule.id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if !primary.contains(id) {
            extras.push(id.to_string());
        }
    }
    if !decision.analysis.matched_paths.is_empty() {
        let preview = decision
            .analysis
            .matched_paths
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if !preview.is_empty() && !primary.contains(&preview) {
            extras.push(format!("paths: {preview}"));
        }
    }
    if extras.is_empty() {
        primary
    } else {
        format!("{primary} ({})", extras.join("; "))
    }
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
            secret_vault: Arc::new(Mutex::new(None)),
            policy_slot: Arc::new(Mutex::new(None)),
        }
    }

    /// Whether a command is read-only in the literal shell sense.
    pub fn is_read_only(command: &str) -> bool {
        read_only::is_read_only(command)
    }

    /// Classify a command for read-only semantics.
    pub fn classify_read_only(command: &str) -> ReadOnlyVerdict {
        read_only::classify(command)
    }

    /// Set the shared cancellation token from the AI handler.
    /// This allows Ctrl+C during AI processing to cancel in-progress tool execution.
    pub fn set_cancellation_token(&mut self, token: Arc<CancellationToken>) {
        self.cancellation_token = Some(token);
    }

    /// Build a sub-session bash that shares PTY/vault slots but uses `token` for cancel.
    pub fn for_sub_session_token(&self, token: Arc<CancellationToken>) -> Self {
        let mut bash = BashTool::new();
        bash.set_cancellation_token(token);
        bash.set_pty_slot(Arc::clone(&self.pty_slot));
        bash.set_secret_vault(Arc::clone(&self.secret_vault));
        bash.set_policy_slot(Arc::clone(&self.policy_slot));
        bash
    }

    /// Test helper: shared cancel token currently wired into this tool, if any.
    #[cfg(test)]
    pub fn cancellation_token_for_test(&self) -> Option<Arc<CancellationToken>> {
        self.cancellation_token.clone()
    }

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

    /// After PTY/executor returns, map cancel-token state to a short-circuit
    /// result. Raw-mode Ctrl+C sets `user_interrupt` on the PTY token only;
    /// propagate that to the session token so tool loops abort.
    fn outcome_if_cancelled(&self, cancel_token: &CancelToken) -> Option<ToolResult> {
        if cancel_token.is_user_interrupt() {
            if let Some(ref ct) = self.cancellation_token {
                ct.cancel();
            }
            return Some(Self::user_cancelled_result());
        }
        if cancel_token.is_cancelled()
            && self
                .cancellation_token
                .as_ref()
                .is_some_and(|t| t.is_cancelled())
        {
            return Some(Self::user_cancelled_result());
        }
        None
    }

    /// Set the shared PersistentPty slot for Ctrl+Z/bg/fg support.
    pub fn set_pty_slot(&mut self, slot: PtySlot) {
        self.pty_slot = slot;
    }

    /// Set the shared secret vault for placeholder restoration.
    pub fn set_secret_vault(&mut self, vault: VaultSlot) {
        self.secret_vault = vault;
    }

    /// Share the shell's live security policy with bash preflight.
    pub fn set_policy_slot(&mut self, slot: PolicySlot) {
        self.policy_slot = slot;
    }

    /// Publish an updated live policy (used after `/setting` Security edits).
    pub fn publish_live_policy(slot: &PolicySlot, policy: SecurityPolicy) {
        *slot.lock().unwrap() = Some(policy);
    }

    fn live_policy(&self) -> Option<SecurityPolicy> {
        self.policy_slot.lock().unwrap().clone()
    }

    /// Restore `$SECRET_*` placeholders in the command to their actual values.
    /// Returns `(restored_command, restore_count)`.
    fn restore_secrets(&self, command: &str) -> (String, usize) {
        let vault_arc = {
            let guard = self.secret_vault.lock().unwrap();
            guard.clone()
        };
        if let Some(vault) = vault_arc {
            let vault = vault.lock().unwrap();
            vault.restore(command)
        } else {
            (command.to_string(), 0)
        }
    }

    /// Redact secret values from command output before sending to AI.
    /// Returns `(redacted_text, count)`.
    fn redact_output(&self, text: &str) -> (String, usize) {
        let vault_arc = {
            let guard = self.secret_vault.lock().unwrap();
            guard.clone()
        };
        if let Some(vault) = vault_arc {
            let vault = vault.lock().unwrap();
            vault.redact_output(text)
        } else {
            (text.to_string(), 0)
        }
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

        // Sync cwd even on cancel — e.g. `cd /tmp; sleep 90` then Ctrl+C must
        // leave the process (and later shell sync) on /tmp, not the old cwd.
        if let Ok((_, _, cwd)) = &result {
            if !cwd.is_empty() {
                let _ = std::env::set_current_dir(cwd);
            }
        }

        if let Some(cancelled) = self.outcome_if_cancelled(&cancel_token) {
            return cancelled;
        }

        match result {
            Ok((output, exit_code, _cwd)) => {
                let raw_line_count = output.lines().count();
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

                let mut meta = serde_json::json!({
                    "raw_line_count": raw_line_count,
                });
                if let Some(p) = offload_result.offload_payload {
                    meta.as_object_mut().unwrap().insert(
                        "offload".to_string(),
                        serde_json::to_value(p).unwrap_or(serde_json::Value::Null),
                    );
                }

                ToolResult {
                    ok: true,
                    output: output_text,
                    meta: Some(meta),
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

        let mut env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
        // Never let an interactive pager (less) block programmatic output
        // capture. `cat` streams straight through and exits immediately.
        for var in ["PAGER", "SYSTEMD_PAGER", "GIT_PAGER"] {
            env_vars.insert(var.to_string(), "cat".to_string());
        }
        let _interactive_guard = interactive.then(InteractiveInputGuard::acquire);
        let result = executor.execute_blocking(command, env_vars, &cancel_token);
        done.store(true, Ordering::SeqCst);

        if let Some(cancelled) = self.outcome_if_cancelled(&cancel_token) {
            return cancelled;
        }

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

                let raw_line_count = result.stdout.lines().count() + result.stderr.lines().count();
                let mut meta = serde_json::json!({
                    "raw_line_count": raw_line_count,
                });
                if let Some(p) = offload_result.offload_payload {
                    meta.as_object_mut().unwrap().insert(
                        "offload".to_string(),
                        serde_json::to_value(p).unwrap_or(serde_json::Value::Null),
                    );
                }
                ToolResult {
                    ok: true,
                    output,
                    meta: Some(meta),
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
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn preflight(&self, args: &serde_json::Value) -> PreflightResult {
        bash_preflight(self, args, false)
    }

    fn preflight_with_context(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> PreflightResult {
        bash_preflight(self, args, ctx.policy.enforce_read_only_bash)
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let raw_command = match args.get("command").and_then(|c| c.as_str()) {
            Some(cmd) => cmd,
            None => return ToolResult::error(aish_i18n::t("tools.bash.missing_command")),
        };
        let timeout_secs = match timeout_secs(&args) {
            Ok(value) => value,
            Err(error) => return error,
        };

        // Restore secret placeholders before execution.
        let (command, _restore_count) = self.restore_secrets(raw_command);

        // Try PersistentPty path first (supports Ctrl+Z/bg/fg).
        let pty_arc = {
            let guard = self.pty_slot.lock().unwrap();
            guard.clone()
        };
        let mut result = if let Some(pty_arc) = pty_arc {
            self.execute_via_persistent_pty(&command, timeout_secs, pty_arc)
        } else {
            // Fallback: one-shot PtyExecutor.
            self.execute_via_pty_executor(&command, timeout_secs)
        };

        // Redact secret values from output before returning to AI.
        if result.ok {
            let (redacted, _) = self.redact_output(&result.output);
            result.output = redacted;
        }

        result
    }

    fn for_sub_session(&self, sub: &aish_llm::LlmSession) -> Option<std::sync::Arc<dyn Tool>> {
        // Sub-agents get a dedicated bash instance bound to the child cancel token
        // (parent→child cascade still cancels via forward_cancellation → sub token).
        // Share PTY/vault slots so remote/secret wiring from the parent still works.
        Some(Arc::new(
            self.for_sub_session_token(sub.cancellation_token_arc()),
        ))
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

    fn xdg_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// Restore `XDG_CONFIG_HOME` on drop (including panic/assert unwind).
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: Option<&std::path::Path>) -> Self {
            let prev = std::env::var_os(key);
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn bash_preflight_uses_live_policy_slot_over_disk() {
        // Isolate load_policy(None) to an ALLOW-only user policy, then prove
        // the live slot (HIGH rm-/etc rule) wins for bash_preflight.
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(xdg.join("aish")).unwrap();
        let user_policy = xdg.join("aish").join("security_policy.yaml");
        fs::write(
            &user_policy,
            "global:\n  enable_sandbox: false\n  sandbox_off_action: ALLOW\n  default_risk_level: LOW\nrules: []\n",
        )
        .unwrap();
        let _lock = xdg_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", Some(xdg.as_path()));
        // A system-level policy (/etc/aish/security_policy.yaml) takes
        // precedence over the user-level file, so point the override at a
        // non-existent path to keep this test isolated from the machine.
        let _sys_guard = EnvGuard::set(
            "AISH_SYSTEM_POLICY_PATH",
            Some(dir.path().join("no-system-policy.yaml").as_path()),
        );

        let mut tool = BashTool::new();
        let args = serde_json::json!({"command": "rm -rf /etc"});
        assert_eq!(
            bash_preflight(&tool, &args, false),
            PreflightResult::Allow,
            "disk policy alone should allow"
        );

        let slot: PolicySlot = Arc::new(Mutex::new(None));
        tool.set_policy_slot(Arc::clone(&slot));
        let live_path = dir.path().join("live_policy.yaml");
        fs::write(
            &live_path,
            "global:\n  enable_sandbox: false\n  sandbox_off_action: ALLOW\n  default_risk_level: LOW\nrules:\n  - id: H-001\n    path: [/etc/**]\n    operations: [DELETE]\n    command_list: [rm]\n    risk: HIGH\n    reason: live policy blocks etc\n",
        )
        .unwrap();
        BashTool::publish_live_policy(&slot, load_policy(Some(live_path.as_path())));

        match bash_preflight(&tool, &args, false) {
            PreflightResult::Block { message, .. } => {
                assert!(message.contains("live policy blocks etc"), "{message}");
            }
            other => panic!("expected Block from live policy slot, got {other:?}"),
        }
    }

    #[test]
    fn test_for_sub_session_token_binds_child_not_parent() {
        let parent_token = Arc::new(CancellationToken::new());
        let child_token = Arc::new(CancellationToken::new());
        let mut parent_bash = BashTool::new();
        parent_bash.set_cancellation_token(Arc::clone(&parent_token));

        let child_bash = parent_bash.for_sub_session_token(Arc::clone(&child_token));
        let bound = child_bash
            .cancellation_token_for_test()
            .expect("child bash should have a cancel token");
        assert!(
            Arc::ptr_eq(&bound, &child_token),
            "sub-session bash must watch the child cancel token"
        );
        assert!(
            !Arc::ptr_eq(&bound, &parent_token),
            "sub-session bash must not keep the parent cancel token"
        );
    }

    #[test]
    fn test_user_interrupt_cancels_session_and_short_circuits() {
        let session_token = Arc::new(CancellationToken::new());
        let mut bash = BashTool::new();
        bash.set_cancellation_token(Arc::clone(&session_token));

        let pty_token = CancelToken::new();
        pty_token.cancel_as_user_interrupt();

        let result = bash
            .outcome_if_cancelled(&pty_token)
            .expect("user interrupt must short-circuit");
        assert!(!result.ok);
        assert_eq!(result.output, aish_i18n::t("shell.interrupted"));
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|m| m.get("reason"))
                .and_then(|v| v.as_str()),
            Some("user_cancelled")
        );
        assert!(
            session_token.is_cancelled(),
            "session token must be cancelled so tool loops abort"
        );
    }

    #[test]
    fn test_timeout_cancel_without_session_cancel_does_not_short_circuit() {
        let session_token = Arc::new(CancellationToken::new());
        let mut bash = BashTool::new();
        bash.set_cancellation_token(Arc::clone(&session_token));

        let pty_token = CancelToken::new();
        pty_token.cancel(); // timeout / plain cancel, not Ctrl+C

        assert!(bash.outcome_if_cancelled(&pty_token).is_none());
        assert!(
            !session_token.is_cancelled(),
            "timeout must not cancel the LLM session"
        );
    }

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
                assert_eq!(message, "system config is protected (H-001; paths: /etc)");
                assert_eq!(security.tool_name, "bash");
                assert_eq!(security.target.as_deref(), Some("rm -rf /etc"));
                assert_eq!(security.mode, SecurityPanelMode::Blocked);
                assert_eq!(
                    security.message,
                    "system config is protected (H-001; paths: /etc)"
                );
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
                assert_eq!(
                    message,
                    "home path is protected (M-001; paths: /home/lixin/a.txt)"
                );
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
    fn test_bash_tool_preflight_blocks_non_readonly_when_enforced() {
        let tool = BashTool::new();
        let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
        let ctx = aish_llm::ToolContext::with_policy(
            &session,
            aish_llm::ToolExecutionPolicy {
                enforce_read_only_bash: true,
            },
        );
        let result = tool.preflight_with_context(&serde_json::json!({"command": "rm -rf /"}), &ctx);
        assert!(matches!(result, PreflightResult::Block { .. }));
    }

    #[tokio::test]
    async fn test_execute_tool_by_name_blocks_non_readonly_when_enforced() {
        let mut session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
        session.set_tool_execution_policy(aish_llm::ToolExecutionPolicy {
            enforce_read_only_bash: true,
        });
        session.register_tool(Box::new(BashTool::new()));
        let result = session
            .execute_tool_by_name("bash", serde_json::json!({"command": "rm -rf /"}))
            .await
            .expect("bash tool should be registered");
        assert!(!result.ok);
        assert!(result.output.contains("Blocked by security policy"));
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
                assert_eq!(
                    message,
                    "system config directory (H-001; paths: /etc/aish/config.yaml)"
                );
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
    fn test_bash_tool_preflight_sandbox_hit_uses_rule_reason_without_description() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        let socket_path = dir.path().join("sandbox.sock");
        // Seeded H-001 shape: reason only, no description/confirm_message.
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: true\n  sandbox_off_action: ALLOW\n  sandbox_timeout_seconds: 13\n  default_risk_level: LOW\nrules:\n  - id: H-001\n    name: Protect /etc\n    path: [/etc/**]\n    operations: [DELETE]\n    command_list: [rm]\n    risk: HIGH\n    reason: System config changes can break the host\n",
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
            let id = request["id"].as_str().unwrap();
            let response = serde_json::json!({
                "id": id,
                "ok": true,
                "result": {
                    "exit_code": 0,
                    "stdout": "",
                    "stderr": "",
                    "changes": [
                        {"path": "/etc/aish/123", "kind": "deleted"}
                    ],
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "changes_truncated": false
                }
            });
            writeln!(stream, "{}", response).unwrap();
        });

        let result = security_preflight_with_socket(
            "rm /etc/aish/123",
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
                assert_eq!(
                    message,
                    "System config changes can break the host (H-001; paths: /etc/aish/123)"
                );
                assert_eq!(
                    security.message,
                    "System config changes can break the host (H-001; paths: /etc/aish/123)"
                );
                assert!(
                    !message.contains("security handling"),
                    "must not fall back to generic level message: {message}"
                );
                let decision = security.decision.expect("expected security decision");
                assert_eq!(
                    decision
                        .analysis
                        .matched_rule
                        .as_ref()
                        .and_then(|rule| rule.id.as_deref()),
                    Some("H-001")
                );
            }
            other => panic!("unexpected preflight result: {other:?}"),
        }
    }

    #[test]
    fn test_bash_tool_preflight_sandbox_hit_falls_back_to_rule_identity() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        let socket_path = dir.path().join("sandbox.sock");
        // No reason/description/confirm_message — still must avoid generic level copy.
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: true\n  sandbox_off_action: ALLOW\n  sandbox_timeout_seconds: 13\n  default_risk_level: LOW\nrules:\n  - id: H-001\n    name: Protect /etc\n    path: [/etc/**]\n    operations: [DELETE]\n    command_list: [rm]\n    risk: HIGH\n",
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
            let id = request["id"].as_str().unwrap();
            let response = serde_json::json!({
                "id": id,
                "ok": true,
                "result": {
                    "exit_code": 0,
                    "stdout": "",
                    "stderr": "",
                    "changes": [
                        {"path": "/etc/aish/identity", "kind": "deleted"}
                    ],
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "changes_truncated": false
                }
            });
            writeln!(stream, "{}", response).unwrap();
        });

        let result = security_preflight_with_socket(
            "rm /etc/aish/identity",
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
                assert!(
                    message.contains("Protect /etc") || message.contains("H-001"),
                    "expected rule identity in message, got {message}"
                );
                assert!(
                    message.contains("paths: /etc/aish/identity"),
                    "expected matched paths in message, got {message}"
                );
                assert!(
                    !message.contains("security handling") && !message.contains("安全处理"),
                    "must not fall back to generic level message: {message}"
                );
                assert_eq!(security.mode, SecurityPanelMode::Blocked);
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
                assert_eq!(message, "test-home-rule (M-001; paths: /home/lixin/123)");
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
