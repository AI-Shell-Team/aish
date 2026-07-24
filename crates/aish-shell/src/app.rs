use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aish_config::ConfigModel;
use aish_context::{
    context_window_hard_min_tokens, context_window_warn_below_tokens,
    effective_reserved_output_tokens, resolve_context_window_tokens, ContextBudgetPolicy,
};
use aish_core::{AuditEventType, AuditSink, LlmEvent, LlmEventType, MemoryCategory};
use aish_i18n::{t, t_with_args};
use aish_llm::{
    langfuse::{LangfuseClient, LangfuseConfig},
    CancellationToken, ChatMessage, LlmCallbackResult, LlmSession, StreamContext,
};
use aish_memory::MemoryManager;
use aish_security::{load_policy, SecurityManager};
use aish_session::{SessionContextMessage, SessionRecord, SessionStateSnapshot, SessionStore};
use aish_skills::hotreload::SkillHotReloader;
use aish_skills::SkillManager;
use aish_tools::ToolRegistry;
use std::path::PathBuf;

use crate::ai_handler::{AiHandler, SharedMemoryManager};
use crate::animation::{SharedAnimation, SubAgentThinkingAnimation};
use crate::environment;
use crate::esc_watcher::CrosstermEscWatcher;
use crate::input;
use crate::prompt;
use crate::readline::ShellReadline;
use crate::renderer::ShellRenderer;
use crate::resume_selector::{select_resume_session, ResumeSessionItem};
use crate::theme;
use crate::types::ShellState;

/// Format prompt + command as displayed after Enter (without trailing newline).
fn format_user_submitted_line(prompt: &str, command: &str) -> String {
    format!("{prompt}{command}")
}

// ---------------------------------------------------------------------------
// SIGINT handler for AI operation cancellation
// ---------------------------------------------------------------------------

/// Raw pointer to the current CancellationToken, set before an AI call and
/// cleared afterwards. Only accessed from `ai_sigint_handler`.
static CANCEL_TOKEN_PTR: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// POSIX signal handler for SIGINT during AI operations.
/// Sets the CancellationToken's atomic flag (async-signal-safe).
extern "C" fn ai_sigint_handler(_: std::ffi::c_int) {
    let ptr = CANCEL_TOKEN_PTR.load(Ordering::SeqCst) as *const CancellationToken;
    if !ptr.is_null() {
        unsafe { &*ptr }.cancel_atomic();
    }
}

/// Poll a CancellationToken until it is cancelled. Used inside `tokio::select!`
/// to race against the AI operation — when the token fires the AI future is
/// dropped, which aborts the in-flight HTTP stream.
///
/// # Safety
///
/// The caller must guarantee that `token` points to a live `CancellationToken`
/// that outlives this async task. This holds because the token lives inside
/// `AiHandler` which is owned by `AishShell`, and `poll_cancelled` is only
/// spawned as part of a `tokio::select!` block within `AishShell::run()`.
async fn poll_cancelled(token: *const CancellationToken) {
    while !unsafe { &*token }.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

const RESUME_LIST_LIMIT: usize = 20;

const SSH_CONTEXT_CAP: usize = 40;
const SSH_CONTEXT_CHAR_BUDGET: usize = 200_000;

fn msg_text_len(m: &ChatMessage) -> usize {
    m.content
        .as_ref()
        .and_then(|c| c.to_text())
        .map(|t| t.len())
        .unwrap_or(0)
}

fn truncate_ssh_history(h: &mut Vec<ChatMessage>) {
    let excess = h.len().saturating_sub(SSH_CONTEXT_CAP);
    if excess > 0 {
        h.drain(..excess);
    }
    let lengths: Vec<usize> = h.iter().map(|m| msg_text_len(m)).collect();
    let total_chars: usize = lengths.iter().sum();
    if total_chars > SSH_CONTEXT_CHAR_BUDGET {
        let min_keep = 6;
        let mut to_remove = 0;
        let mut running = total_chars;
        for &len in &lengths {
            if running <= SSH_CONTEXT_CHAR_BUDGET || h.len() - to_remove <= min_keep {
                break;
            }
            running -= len;
            to_remove += 1;
        }
        if to_remove > 0 {
            h.drain(..to_remove);
        }
    }
}

/// Legacy ReAct sub-sessions tagged events with `source=react_agent`.
/// Kept so leftover events (if any) do not break the main thinking spinner.
fn react_agent_llm_event(event: &LlmEvent) -> bool {
    event
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("source"))
        .and_then(|value| value.as_str())
        == Some("react_agent")
}

fn context_compaction_notice(event: &LlmEvent) -> Option<String> {
    let changed = event
        .data
        .get("changed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !changed {
        return None;
    }

    Some(t("shell.compaction.completed"))
}

fn stream_context_from_config(config: &ConfigModel) -> StreamContext {
    StreamContext::new(
        &config.api_base,
        &config.api_key,
        &config.model,
        config.codex_auth_path.as_ref().map(PathBuf::from),
    )
}

// --- /setting panel helpers (see settings_panel module) --------------------

fn setting_label(def: &crate::settings_panel::SettingDef) -> String {
    t(&format!("shell.setting.k.{}.label", def.key.name()))
}

fn setting_desc(def: &crate::settings_panel::SettingDef) -> String {
    t(&format!("shell.setting.k.{}.desc", def.key.name()))
}

/// Mirror the security-relevant fields of `config.yaml` onto `policy`.
///
/// `config.yaml` overrides `security_policy.yaml` for the globals the
/// `/setting` panel exposes (`enable_sandbox`, `default_risk_level`,
/// `sandbox_off_action`, `sandbox_timeout_seconds`, and the InputGuard
/// master switch). Centralized here so the startup path and the live
/// `/setting` update path cannot drift.
fn apply_config_security_overrides(
    policy: &mut aish_security::SecurityPolicy,
    config: &ConfigModel,
) {
    use aish_security::decision::{RiskLevel, SandboxOffAction};
    policy.input_guard.enabled = config.input_guard_enabled;
    policy.enable_sandbox = config.enable_sandbox;
    policy.default_risk_level = match config.default_risk_level.to_lowercase().as_str() {
        "medium" => RiskLevel::Medium,
        "high" => RiskLevel::High,
        _ => RiskLevel::Low,
    };
    policy.sandbox_off_action = match config.sandbox_off_action.to_lowercase().as_str() {
        "confirm" => SandboxOffAction::Confirm,
        "block" => SandboxOffAction::Block,
        _ => SandboxOffAction::Allow,
    };
    policy.sandbox_timeout_seconds = config.sandbox_timeout_seconds;
}

/// Human-readable current value for the setting-list row.
fn setting_display_current(cfg: &ConfigModel, def: &crate::settings_panel::SettingDef) -> String {
    use crate::settings_panel::{current_raw, SettingKind};
    let raw = current_raw(cfg, def.key);
    match def.kind {
        SettingKind::Bool => {
            if raw == "true" {
                t("shell.setting.on")
            } else {
                t("shell.setting.off")
            }
        }
        SettingKind::Secret => {
            if raw.is_empty() {
                t("shell.setting.not_set")
            } else {
                mask_secret(&raw)
            }
        }
        _ => {
            if raw.is_empty() {
                t("shell.setting.not_set")
            } else {
                raw
            }
        }
    }
}

/// Mask a secret, keeping the first 2 and last 2 chars; fully masked when short.
fn mask_secret(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= 8 {
        return "*".repeat(n.max(4));
    }
    let head: String = chars[..2].iter().collect();
    let tail: String = chars[n - 2..].iter().collect();
    format!("{head}{}{tail}", "*".repeat(n - 4))
}

/// Accent color per setting category — used by the chips and row icons in
/// the new `/setting` panel.
fn category_color_for(c: crate::settings_panel::SettingCategory) -> ratatui::style::Color {
    use crate::settings_panel::SettingCategory;
    use ratatui::style::Color;
    match c {
        SettingCategory::Model => Color::Cyan,
        SettingCategory::Appearance => Color::Magenta,
        SettingCategory::Ai => Color::LightGreen,
        SettingCategory::Security => Color::LightRed,
        SettingCategory::Context => Color::LightBlue,
        SettingCategory::Remote => Color::LightMagenta,
        SettingCategory::Advanced => Color::Gray,
    }
}

/// Free-text/number editor for a single setting value.
///
/// Uses a cliclack input (prefilled with `current` for direct in-place editing)
/// for text/number fields, and a masked password input for secrets. Returns
/// the trimmed value, or `None` when the user cancels (Esc / Ctrl+C).
fn prompt_edit_value(label: &str, desc: &str, current: &str, secret: bool) -> Option<String> {
    let _guard = aish_tools::bash::acquire_interactive_input_guard();
    let prompt = if desc.is_empty() {
        label.to_string()
    } else {
        format!("{label}\n{}", theme::faint(desc))
    };
    if secret {
        let result: std::io::Result<String> = cliclack::password(&prompt).mask('•').interact();
        match result {
            Ok(v) => Some(v.trim().to_string()),
            Err(_) => None,
        }
    } else {
        let mut input = cliclack::input(&prompt);
        if !current.is_empty() {
            input = input.default_input(current);
        }
        let result: std::io::Result<String> = input.interact();
        match result {
            Ok(v) => Some(v.trim().to_string()),
            Err(_) => None,
        }
    }
}

/// Interactive menu picker built on the inline selection panel. Each option is
/// `(value, label, description)`. Returns the chosen value, or `None` on cancel.
/// REPL-safe (the underlying panel acquires the interactive input guard).
fn pick_menu(title: &str, options: &[(String, String, String)]) -> Option<String> {
    let opts: Vec<crate::tui::DialogOption> = options
        .iter()
        .map(|(v, l, d)| {
            let mut o = crate::tui::DialogOption::new(v.as_str(), l.as_str());
            if !d.is_empty() {
                o = o.with_description(d.as_str());
            }
            o
        })
        .collect();
    match crate::tui::show_selection_dialog(title, "", &opts, false, true) {
        crate::tui::DialogResult::Selected(v) => Some(v),
        _ => None,
    }
}

fn llm_session_from_config(config: &ConfigModel) -> LlmSession {
    let mut session = LlmSession::with_context(
        stream_context_from_config(config),
        Some(config.temperature),
        config.max_tokens,
    );

    // Multi-account quota rotation + model fallback. The top-level api_key is
    // account #0 (the primary); configured api_accounts extend the pool.
    let accounts = rotation_accounts_from_config(config);
    if !config.fallback_models.is_empty() || accounts.len() > 1 {
        let mut policy = aish_llm::RetryPolicy::default();
        policy.revert_on_cooldown = config.fallback_revert_on_cooldown;
        let state = aish_llm::RotationState::new(
            config.model.clone(),
            accounts,
            config.fallback_models.clone(),
            policy,
        );
        if state.is_active() {
            session.set_rotation(state);
        }
    }
    session
}

/// Build the rotation account pool from config. The primary `api_key` is always
/// index 0; each entry in `config.api_accounts` follows. Disabled when the key
/// is empty so rotation skips it instead of wasting an attempt.
fn rotation_accounts_from_config(config: &ConfigModel) -> Vec<aish_llm::ApiAccount> {
    let mut accounts = vec![aish_llm::ApiAccount {
        name: "primary".to_string(),
        api_key: config.api_key.clone(),
        api_base: None,
        weight: 1,
        disabled: config.api_key.trim().is_empty(),
    }];
    for acct in &config.api_accounts {
        accounts.push(aish_llm::ApiAccount {
            name: acct.name.clone(),
            api_key: acct.api_key.clone(),
            api_base: acct.api_base.clone(),
            weight: acct.weight.max(1),
            disabled: acct.disabled,
        });
    }
    accounts
}

fn stream_context_from_parts(
    api_base: &str,
    api_key: &str,
    model: &str,
    codex_auth_path: Option<&str>,
) -> StreamContext {
    StreamContext::new(api_base, api_key, model, codex_auth_path.map(PathBuf::from))
}

fn context_budget_policy_from_config(config: &ConfigModel) -> ContextBudgetPolicy {
    let compact = &config.context_auto_compact;
    let model_config_tokens = compact.model_context_windows.get(&config.model).copied();
    let context_window = resolve_context_window_tokens(
        compact.context_window_tokens,
        model_config_tokens,
        config.context_token_budget,
    );
    let mut policy =
        ContextBudgetPolicy::from_optional_budget(None, config.enable_token_estimation);
    policy.enabled = compact.enabled;
    policy.full_compact_enabled = compact.full_compact_enabled;
    policy.context_window_tokens = context_window.tokens;
    policy.context_window_source = context_window.source;
    if let Some(value) = compact.reserved_output_tokens {
        policy.reserved_output_tokens = value;
    }
    if let Some(value) = compact.auto_compact_buffer_tokens {
        policy.auto_compact_buffer_tokens = value;
    }
    if let Some(value) = compact.warning_buffer_tokens {
        policy.warning_buffer_tokens = value;
    }
    if let Some(value) = compact.blocking_buffer_tokens {
        policy.blocking_buffer_tokens = value;
    }
    policy.micro_keep_recent_messages = compact.micro_keep_recent_messages.max(1);
    policy.shell_keep_recent_commands = compact.shell_keep_recent_commands.max(1);
    policy.max_consecutive_failures = compact.max_consecutive_failures.max(1);
    policy.summary_max_tokens = compact.summary_max_tokens.max(256);
    if policy.context_window_tokens < context_window_hard_min_tokens() {
        if policy.full_compact_enabled {
            tracing::warn!(
                model = %config.model,
                context_window_tokens = policy.context_window_tokens,
                source = policy.context_window_source.as_str(),
                hard_min_tokens = context_window_hard_min_tokens(),
                "context window is too small for model-generated full compact; falling back to microcompact-only"
            );
        }
        policy.full_compact_enabled = false;
    } else if policy.context_window_tokens < context_window_warn_below_tokens() {
        tracing::warn!(
            model = %config.model,
            context_window_tokens = policy.context_window_tokens,
            source = policy.context_window_source.as_str(),
            warn_below_tokens = context_window_warn_below_tokens(),
            "context window is small; context compaction may trigger early"
        );
    }
    let thresholds = policy.thresholds();
    tracing::info!(
        model = %config.model,
        context_window_tokens = policy.context_window_tokens,
        context_window_source = policy.context_window_source.as_str(),
        reserved_output_tokens = policy.reserved_output_tokens,
        effective_reserved_output_tokens = effective_reserved_output_tokens(
            policy.context_window_tokens,
            policy.reserved_output_tokens,
        ),
        effective_context_window = thresholds.effective_context_window,
        warning_threshold = thresholds.warning_threshold,
        auto_compact_threshold = thresholds.auto_compact_threshold,
        blocking_threshold = thresholds.blocking_threshold,
        full_compact_enabled = policy.full_compact_enabled,
        "resolved context auto-compact budget"
    );
    policy
}

/// Resolve the current user's name via `getuid()` + `getpwuid_r()`.
/// Unlike `$USER`, this cannot be spoofed by setting an environment variable.
/// Falls back to the numeric UID string if the name cannot be resolved.
fn current_user_name() -> Option<String> {
    // SAFETY: [Category 8 — FFI] `getuid()` never fails (always returns the
    // real UID of the calling process) and has no side effects.
    let uid = unsafe { libc::getuid() };
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 4096];
    // SAFETY: [Category 8 — FFI] `getpwuid_r` writes into `pwd` and `buffer`;
    // both are valid mutable buffers of the expected sizes.
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len(),
            &mut result,
        )
    };
    if rc == 0 && !result.is_null() && !pwd.pw_name.is_null() {
        let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
            .to_string_lossy()
            .into_owned();
        Some(name)
    } else {
        // Fallback: numeric UID is still more reliable than $USER
        Some(uid.to_string())
    }
}

#[cfg(test)]
mod context_budget_tests {
    use super::*;

    #[test]
    fn model_context_window_mapping_sets_source() {
        let mut config = ConfigModel::default();
        config.model = "openai/glm-5.1".to_string();
        config.context_token_budget = Some(8_000);
        config
            .context_auto_compact
            .model_context_windows
            .insert(config.model.clone(), 128_000);

        let policy = context_budget_policy_from_config(&config);

        assert_eq!(policy.context_window_tokens, 128_000);
        assert_eq!(
            policy.context_window_source,
            aish_context::ContextWindowSource::ModelConfig
        );
        assert!(policy.full_compact_enabled);
    }

    #[test]
    fn tiny_context_window_disables_model_full_compact() {
        let mut config = ConfigModel::default();
        config.context_auto_compact.context_window_tokens = Some(2_000);
        config.context_auto_compact.full_compact_enabled = true;

        let policy = context_budget_policy_from_config(&config);

        assert_eq!(policy.context_window_tokens, 2_000);
        assert_eq!(
            policy.context_window_source,
            aish_context::ContextWindowSource::AutoCompactOverride
        );
        assert!(!policy.full_compact_enabled);
    }
}

/// Shell lifecycle phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPhase {
    /// Shell is initializing (loading config, skills, etc.)
    Booting,
    /// Shell is ready and waiting for user input
    Editing,
    /// A command has been submitted and is executing
    Running,
    /// Shell is shutting down
    Exiting,
}

impl std::fmt::Display for ShellPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellPhase::Booting => write!(f, "booting"),
            ShellPhase::Editing => write!(f, "editing"),
            ShellPhase::Running => write!(f, "running"),
            ShellPhase::Exiting => write!(f, "exiting"),
        }
    }
}

/// State of user interruption handling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InterruptionState {
    /// Normal operation
    #[default]
    Normal,
    /// User is providing input
    Inputting,
    /// A clear/exit is pending (Ctrl+C was pressed once)
    ClearPending,
    /// Exit has been confirmed (Ctrl+C pressed twice)
    ExitPending,
}

/// Main shell application that ties together the REPL loop, command routing,
/// AI handler, security manager, session store, skill manager, and memory manager.
pub struct AishShell {
    pub state: ShellState,
    pub config: ConfigModel,
    pub ai_handler: AiHandler,
    pub security_manager: SecurityManager,
    input_guard: aish_security::input_guard::InputGuard,
    secret_check_closure:
        std::sync::Arc<dyn Fn(&str) -> Option<aish_pty::SshSecretCheckResult> + Send + Sync>,
    secret_vault: std::sync::Arc<std::sync::Mutex<aish_security::secret::SecretVault>>,
    pub session_store: Option<SessionStore>,
    audit_store: Option<std::sync::Arc<aish_session::AuditStore>>,
    audit_user: Option<String>,
    audit_host: Option<String>,
    current_remote_host: Arc<Mutex<Option<String>>>,
    pub skill_manager: SkillManager,
    pub skill_hot_reloader: Option<SkillHotReloader>,
    pub memory_manager: SharedMemoryManager,
    pub version: String,
    pub operation_in_progress: bool,
    /// Persistent PTY session for executing all external commands.
    /// Wrapped in `Arc<Mutex<>>` so the readline completion handler can
    /// query the PTY bash for tab-completions.
    pty: Arc<Mutex<aish_pty::PersistentPty>>,
    /// UUID for the current session, used to associate history entries.
    session_uuid: String,
    /// Whether streaming has started printing content (to avoid double-printing).
    streamed_content: Arc<AtomicBool>,
    /// Current shell lifecycle phase
    phase: ShellPhase,
    /// Current interruption state
    interruption: InterruptionState,
    /// Timestamp of last Ctrl+C press (for double-press detection)
    last_ctrl_c: Option<std::time::Instant>,
    /// Shared animation spinner, stored so it can be stopped on cancellation.
    animation: Arc<SharedAnimation>,
    /// Session history of collapsed outputs for Ctrl+O browsing.
    expand_history: Arc<Mutex<crate::expand_history::ExpandHistory>>,
    /// Shared terminal session recorder for asciinema v2 recording.
    shared_recorder: crate::recorder::SharedRecorder,
    /// Inline AI completer (None when disabled). Stored so `/model` can
    /// update its model at runtime without restarting the shell.
    inline_ai: Option<Arc<crate::inline_completion::InlineCompleter>>,
    /// Session-scoped approval memory shared with the LLM session. Kept on the
    /// shell so slash commands (e.g. `/forget-approvals`) can reset it.
    approval_memory: Arc<Mutex<aish_llm::ApprovalMemory>>,
}

impl AishShell {
    /// Show expand panel for browsing collapsed output history.
    fn show_expand_panel(&self) {
        // Clone data out of the mutex before running the blocking TUI panel,
        // so the EscWatcher thread can still acquire the lock.
        let records: Vec<crate::expand_history::ExpandRecord> = {
            let history = self.expand_history.lock().unwrap();
            if history.is_empty() {
                return;
            }
            history.clone_records()
        };
        let history_records: Vec<aish_ui::HistoryRecord> = records
            .iter()
            .map(|r| aish_ui::HistoryRecord {
                command: r.command.clone(),
                line_count: r.line_count,
                time: r.time.clone(),
            })
            .collect();
        let mut panel_args = std::collections::HashMap::new();
        panel_args.insert("count".into(), history_records.len().to_string());
        let title = aish_i18n::t_with_args("shell.panel.history_title", &panel_args);
        let panel = aish_ui::HistoryPanel::new(&title, history_records)
            .with_footer_hint(aish_i18n::t("shell.panel.history_footer"))
            .with_lines_label(aish_i18n::t("shell.panel.lines_label"));
        if let Ok(aish_ui::PanelOutcome::Submitted(outcome)) =
            aish_ui::PanelRuntime::new().run(panel)
        {
            let idx = outcome.selected_index.min(records.len().saturating_sub(1));
            let rec = &records[idx];
            let mut expand_args = std::collections::HashMap::new();
            expand_args.insert("command".into(), rec.command.clone());
            expand_args.insert("count".into(), rec.line_count.to_string());
            let title = aish_i18n::t_with_args("shell.panel.expand_title", &expand_args);
            let expand = aish_ui::ExpandPanel::with_footer(
                &title,
                &rec.output,
                aish_i18n::t("shell.panel.expand_footer"),
            );
            let _ = aish_ui::PanelRuntime::new().run(expand);
        }
    }

    /// Show inline slash command popup with real-time filtering.
    fn run_slash_input_session(&self, prompt: &str) -> aish_ui::SlashInputOutcome {
        let commands: Vec<(String, String)> = crate::readline::SLASH_COMMANDS
            .iter()
            .map(|(name, _desc)| {
                let cmd = name
                    .strip_prefix('/')
                    .expect("slash commands must start with /");
                let i18n_key = format!("shell.slash.{cmd}");
                (name.to_string(), aish_i18n::t(&i18n_key))
            })
            .collect();
        let session = aish_ui::SlashInputSession::new(commands, prompt.to_string());
        // When triggered by Tab on a `/` prefix, pre-fill the popup with
        // the text the user had already typed.
        let session = {
            let prefill = crate::readline::take_slash_prefill();
            if prefill.is_empty() {
                session
            } else {
                session.with_input(prefill)
            }
        };
        match session.run() {
            Ok(outcome) => outcome,
            Err(_) => aish_ui::SlashInputOutcome::Cancelled,
        }
    }

    /// Run the `@path` file-mention popup, seeded with the readline state
    /// captured by `AtFileHandler`. `line` is the buffer without `@`;
    /// `at_pos` is where `@` was about to be inserted.
    fn run_file_mention_session(
        &self,
        prompt: &str,
        line: &str,
        at_pos: usize,
    ) -> aish_ui::FileMentionOutcome {
        let prefix = line[..at_pos].to_string();
        let cwd = std::path::PathBuf::from(&self.state.cwd);
        let session = aish_ui::FileMentionSession::new(&cwd, prompt.to_string(), prefix);
        session
            .run()
            .unwrap_or(aish_ui::FileMentionOutcome::Cancelled)
    }

    /// Security gate: check AI input for secrets and prompt the user.
    /// Returns `true` to continue, `false` if the user aborted (caller should skip).
    /// When secrets are detected and the user chooses "Redact", `question` is
    /// updated in-place with the redacted version.
    /// Display a yellow InputGuard warning and ask y/N confirmation.
    /// Returns `true` if the user explicitly confirms (y/yes).
    fn confirm_action(reason: &str, prompt_label: &str) -> bool {
        eprintln!("{}", theme::warning(reason));
        print!("{} [y/N] ", prompt_label);
        let _ = std::io::stdout().flush();

        // The shell prompt is in canonical mode with ISIG enabled, so a
        // bare std::io::stdin().read_line() would let Ctrl+C raise SIGINT
        // and kill aish.  Switch to raw mode for a single keystroke so we
        // can interpret Ctrl+C (0x03) as "cancel" instead of dying.
        let stdin_fd = libc::STDIN_FILENO;
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
        let saved = nix::sys::termios::tcgetattr(borrowed).ok();
        if let Some(ref saved) = saved {
            let mut raw = saved.clone();
            nix::sys::termios::cfmakeraw(&mut raw);
            raw.output_flags |=
                nix::sys::termios::OutputFlags::OPOST | nix::sys::termios::OutputFlags::ONLCR;
            let _ =
                nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSANOW, &raw);
        }

        let response = read_confirm_keystroke(stdin_fd);

        // Drain any trailing typeahead (Enter after `y`, full `yes<Enter>`,
        // etc.) before restoring the terminal. Leftover bytes would
        // otherwise be consumed by the next prompt as a fresh command.
        drain_stdin_trailing(stdin_fd);

        if let Some(ref saved) = saved {
            let _ =
                nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSADRAIN, saved);
        }

        match response {
            ConfirmResponse::Yes => {
                println!("y");
                true
            }
            ConfirmResponse::Cancel => {
                println!("^C");
                false
            }
            ConfirmResponse::No => {
                println!("n");
                false
            }
        }
    }

    /// Run InputGuard on `input` and handle the verdict uniformly:
    /// - Allow → proceed (return true)
    /// - Confirm / Unknown → ask user via confirm_action; return whether they confirmed
    /// - Block → reject (return false)
    ///
    /// On Block or user-decline: prints the verdict in red and, when
    /// `record_on_block` is true, records the input to history with
    /// exit code 1 (matching prior inline behavior at command sites).
    /// Returns false in both cases so the caller can `continue`.
    fn screen_input(
        &self,
        input: &str,
        context: aish_security::input_guard::InputContext,
        prompt_label: &str,
        record_on_block: bool,
    ) -> bool {
        let verdict = self.input_guard.check(input, context);
        match &verdict {
            aish_security::input_guard::InputVerdict::Block { .. } => {
                eprintln!("{}", theme::error(&verdict.format_display()));
                if record_on_block {
                    self.record_history(input, 1);
                }
                false
            }
            aish_security::input_guard::InputVerdict::Confirm { .. }
            | aish_security::input_guard::InputVerdict::Unknown { .. } => {
                let display = verdict.format_display();
                if !Self::confirm_action(&display, prompt_label) {
                    if record_on_block {
                        self.record_history(input, 1);
                    }
                    false
                } else {
                    true
                }
            }
            aish_security::input_guard::InputVerdict::Allow => true,
        }
    }

    /// Pre-screen an AI-bound prompt. Does not record history on Block
    /// (AI prompts aren't shell commands).
    fn screen_ai_prompt(&self, input: &str) -> bool {
        self.screen_input(
            input,
            aish_security::input_guard::InputContext::AiPrompt,
            "Send to AI anyway?",
            false,
        )
    }

    /// Pre-screen a shell command. Records history with exit code 1 when
    /// the user declines so the failed attempt shows up in `history`.
    fn screen_shell_command(&self, input: &str) -> bool {
        self.screen_input(
            input,
            aish_security::input_guard::InputContext::ShellCommand,
            "Execute anyway?",
            true,
        )
    }

    fn check_security_gate(&self, question: &mut String) -> bool {
        let decision = self.security_manager.check_ai_input(question);
        if !decision.require_confirmation {
            return true;
        }
        let reasons = decision
            .analysis
            .detected_secrets
            .as_ref()
            .map(|s| {
                s.iter()
                    .map(|m| m.format_reason())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            })
            .unwrap_or_default();
        let mut args = std::collections::HashMap::new();
        args.insert("reasons".to_string(), reasons);
        let title = t("shell.security.secret.title");
        let message = t_with_args("shell.security.secret.detected", &args);
        let choice = crate::tui::show_secret_dialog_tui(&title, &message);
        match choice {
            crate::tui::SecretDialogChoice::Abort => {
                let aborted = t("shell.security.secret.aborted");
                println!("{}", theme::warning(&aborted));
                false
            }
            crate::tui::SecretDialogChoice::Redact => {
                if let Some(secrets) = decision.analysis.detected_secrets {
                    let count = secrets.len();
                    let redacted = self.secret_vault.lock().unwrap().redact(&secrets, question);
                    let mut rargs = std::collections::HashMap::new();
                    rargs.insert("count".to_string(), count.to_string());
                    let msg = t_with_args("shell.security.secret.redacted", &rargs);
                    println!("{}", theme::warning(&msg));
                    *question = redacted;
                }
                true
            }
            crate::tui::SecretDialogChoice::Allow => true,
        }
    }

    /// Lock the PTY mutex, recovering from poison if a previous holder
    /// panicked. A poisoned PTY is still usable — the lock just means
    /// a prior operation failed, not that the PTY state is corrupt.
    fn lock_pty(&self) -> std::sync::MutexGuard<'_, aish_pty::PersistentPty> {
        match self.pty.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Create a new shell instance from the given configuration.
    pub fn new(config: ConfigModel) -> aish_core::Result<Self> {
        // Set terminal defaults and load bash environment
        environment::ensure_terminal_defaults();
        let _new_vars = environment::load_bash_env();
        // Honor config.output_language for AI responses (overrides LANG).
        crate::ai_handler::set_output_language_override(config.output_language.clone());

        let state = ShellState::new();

        // Initialize LLM session
        let mut llm_session = llm_session_from_config(&config);
        let context_budget_policy = context_budget_policy_from_config(&config);
        llm_session.set_context_budget_policy(context_budget_policy.clone());

        // Initialize Langfuse observability if configured
        if config.enable_langfuse {
            if let Some(lf_config) = LangfuseConfig::from_parts(
                config.langfuse_public_key.as_deref(),
                config.langfuse_secret_key.as_deref(),
                config.langfuse_host.as_deref(),
            ) {
                llm_session.set_langfuse(LangfuseClient::new(lf_config));
                tracing::info!("Langfuse observability enabled");
            }
        }

        // Initialize security manager (before tool registration)
        let mut policy = load_policy(None);
        // config.yaml mirrors several security_policy.yaml fields and takes
        // precedence, so users can toggle them from the familiar config file
        // (and via /setting) without editing security_policy.yaml.
        apply_config_security_overrides(&mut policy, &config);
        let security_manager = SecurityManager::new(policy);
        let input_guard =
            aish_security::input_guard::InputGuard::from_policy(security_manager.policy());

        // Register tools
        let mut tool_registry = ToolRegistry::new();
        // Shared PTY slot — will be populated after PersistentPty starts.
        let pty_slot: aish_tools::bash::PtySlot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut bash_tool = aish_tools::bash::BashTool::new();
        bash_tool.set_cancellation_token(llm_session.cancellation_token_arc());
        bash_tool.set_pty_slot(pty_slot.clone());

        // Shared secret vault slot — populated after AishShell construction.
        let vault_slot: aish_tools::bash::VaultSlot =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        bash_tool.set_secret_vault(vault_slot.clone());
        tool_registry.register(Box::new(bash_tool));
        tool_registry.register(Box::new(aish_tools::fs::ReadFileTool::new()));
        tool_registry.register(Box::new(aish_tools::fs::WriteFileTool::new()));
        tool_registry.register(Box::new(aish_tools::fs::EditFileTool::new()));
        tool_registry.register(Box::new(aish_tools::AskUserTool::with_runtime(Arc::new(
            crate::tui::run_ask_user_request,
        ))));
        tool_registry.register(Box::new(aish_tools::PythonTool::new()));
        tool_registry.register(Box::new(aish_tools::GlobTool::new()));
        tool_registry.register(Box::new(aish_tools::GrepTool::new()));
        tool_registry.register(Box::new(aish_tools::WebFetchTool::new(
            &config.api_base,
            &config.api_key,
            &config.model,
            Some(config.temperature),
            config.max_tokens,
        )));
        tool_registry.register(Box::new(aish_tools::EnterPlanModeTool::new()));
        tool_registry.register(Box::new(aish_tools::ExitPlanModeTool::new()));
        tool_registry.register(Box::new(aish_tools::ServiceSupervisorTool::new()));
        // AgentTool is registered after skill loading so the parent session already
        // has SkillTool when general-purpose / troubleshoot inherit the tool pool.

        // Initialize shared memory manager (best-effort)
        let memory_manager: SharedMemoryManager = Arc::new(Mutex::new(
            MemoryManager::new(MemoryManager::default_path()).ok(),
        ));

        // Create MemoryTool with real callbacks connected to the shared MemoryManager
        let mm_for_search = memory_manager.clone();
        let mm_for_store = memory_manager.clone();
        let mm_for_delete = memory_manager.clone();
        let mm_for_list = memory_manager.clone();

        let memory_tool = aish_tools::MemoryTool::new(
            // search callback
            Box::new(move |query, limit| {
                let mut guard = mm_for_search.lock().unwrap();
                if let Some(ref mut mm) = *guard {
                    let results = mm.recall(query, limit);
                    results
                        .into_iter()
                        .map(|e| aish_tools::MemorySearchResult {
                            id: e.id as usize,
                            content: e.content.clone(),
                            category: format!("{:?}", e.category).to_lowercase(),
                        })
                        .collect()
                } else {
                    vec![]
                }
            }),
            // store callback
            Box::new(move |content, category, source, importance| {
                let mut guard = mm_for_store.lock().unwrap();
                if let Some(ref mut mm) = *guard {
                    let cat = parse_category_str(category);
                    match mm.store(content, cat, source, importance as f64) {
                        Ok(id) => id.to_string(),
                        Err(e) => {
                            let mut args = std::collections::HashMap::new();
                            args.insert("error".to_string(), e.to_string());
                            t_with_args("shell.general_error", &args)
                        }
                    }
                } else {
                    "memory not available".to_string()
                }
            }),
            // delete callback
            Box::new(move |id| {
                let mut guard = mm_for_delete.lock().unwrap();
                if let Some(ref mut mm) = *guard {
                    mm.remove(id as i64).unwrap_or(false)
                } else {
                    false
                }
            }),
            // list callback
            Box::new(move |limit| {
                let guard = mm_for_list.lock().unwrap();
                if let Some(ref mm) = *guard {
                    mm.list()
                        .iter()
                        .rev()
                        .take(limit)
                        .map(|e| aish_tools::MemorySearchResult {
                            id: e.id as usize,
                            content: e.content.clone(),
                            category: format!("{:?}", e.category).to_lowercase(),
                        })
                        .collect()
                } else {
                    vec![]
                }
            }),
        );
        tool_registry.register(Box::new(memory_tool));
        // Load skills (best-effort, before tool registration so SkillTool can use real data)
        let mut skill_manager = SkillManager::new();
        // Skill/hook loading is gated by `enable_scripts`; when false the
        // directories are still known but nothing is loaded or hot-reloaded.
        if config.enable_scripts {
            let _ = skill_manager.load_all_skills();
        }
        let skill_count = skill_manager.list_skills().len();
        let seed_migration_notice = skill_manager.take_seed_migration_notice();

        // Start skill hot-reloader if skill directories exist
        let skill_hot_reloader = {
            let dirs = skill_manager.get_skill_dirs();
            if dirs.is_empty() {
                None
            } else {
                let reloader = SkillHotReloader::new(dirs);
                reloader.start();
                Some(reloader)
            }
        };

        // Wire SkillTool with real callbacks that look up skills from the loaded manager
        let skill_tool = {
            let skills_snapshot: std::collections::HashMap<String, aish_tools::SkillInfo> =
                skill_manager
                    .list_skills()
                    .iter()
                    .map(|s| {
                        (
                            s.metadata.name.clone(),
                            aish_tools::SkillInfo {
                                name: s.metadata.name.clone(),
                                content: s.content.clone(),
                                description: s.metadata.description.clone(),
                                base_dir: s.base_dir.clone(),
                                context: s.metadata.context,
                                agent: s.metadata.agent.clone(),
                                allowed_tools: s.metadata.allowed_tools.clone(),
                            },
                        )
                    })
                    .collect();
            let skill_names: Vec<String> = skills_snapshot.keys().cloned().collect();
            let lookup = Box::new(move |name: &str| skills_snapshot.get(name).cloned());
            let list = Box::new(move || skill_names.clone());
            aish_tools::SkillTool::new(lookup, list)
        };
        tool_registry.register(Box::new(skill_tool));
        tool_registry.register(Box::new(aish_tools::AgentTool::new()));

        let tools: Vec<(String, Box<dyn aish_llm::Tool>)> = tool_registry.drain_tools();
        for (_name, tool) in tools {
            llm_session.register_tool(tool);
        }

        // Open session store (best-effort)
        let session_store = match &config.session_db_path {
            Some(path) => SessionStore::open(Some(std::path::Path::new(path))).ok(),
            None => SessionStore::open(None).ok(),
        };

        // Create session record if store is available
        let session_uuid = if let Some(ref store) = session_store {
            match store.create_session(&config.model, Some(&config.api_base)) {
                Ok(record) => record.session_uuid,
                Err(_) => uuid::Uuid::new_v4().to_string(),
            }
        } else {
            uuid::Uuid::new_v4().to_string()
        };

        // Open audit store when audit is enabled in the security policy.
        let audit_enabled = security_manager.policy().audit_enabled;
        let audit_store = if audit_enabled {
            let audit_path = security_manager
                .policy()
                .audit_log_path
                .as_deref()
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    config
                        .session_db_path
                        .as_ref()
                        .map(|s| std::path::PathBuf::from(s))
                });
            let open_result = match audit_path {
                Some(ref p) => aish_session::AuditStore::open(Some(p)),
                None => aish_session::AuditStore::open(None),
            };
            match open_result {
                Ok(store) => Some(std::sync::Arc::new(store)),
                Err(e) => {
                    let msg = format!(
                        "audit is enabled in security policy but the audit store failed to open: {e}"
                    );
                    tracing::error!("{msg}");
                    eprintln!("WARNING: {msg}");
                    None
                }
            }
        } else {
            None
        };
        let audit_user = current_user_name();
        let audit_host = sysinfo::System::host_name();

        // Resolve memory config (use defaults if not specified)
        let memory_config = config.memory.clone().unwrap_or_default();

        // Track whether content was streamed for display coordination
        let streamed_content = Arc::new(AtomicBool::new(false));

        // Shared animation controlled by event callback
        let animation: Arc<SharedAnimation> = Arc::new(SharedAnimation::new());
        // Shared recorder for terminal session recording
        let shared_recorder = crate::recorder::new_shared_recorder();
        // Shared renderer for streaming markdown re-rendering
        let renderer = Arc::new(Mutex::new(ShellRenderer::new()));
        renderer
            .lock()
            .unwrap()
            .set_shared_recorder(shared_recorder.clone());
        let renderer_ref = renderer.clone();

        // Set up LLM event callback for real-time streaming display
        let streamed_flag = streamed_content.clone();
        let content_started = Arc::new(AtomicBool::new(false));
        let content_started_flag = content_started.clone();
        let reasoning_buf = Arc::new(Mutex::new(String::new()));
        let reasoning_buf_ref = reasoning_buf.clone();
        let animation_ref = animation.clone();
        // TTFT tracking state
        let thinking_start: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let ttft_recorded = Arc::new(AtomicBool::new(false));
        let ttft_value: Arc<Mutex<f64>> = Arc::new(Mutex::new(0.0));
        let thinking_start_ref = thinking_start.clone();
        let ttft_recorded_ref = ttft_recorded.clone();
        let ttft_value_ref = ttft_value.clone();
        // Reasoning display state: whether reasoning overlay is on-screen
        // and a frame counter for the spinner.
        let reasoning_active = Arc::new(AtomicBool::new(false));
        let reasoning_active_ref = reasoning_active.clone();
        let reasoning_frame = Arc::new(AtomicUsize::new(0));
        let reasoning_frame_ref = reasoning_frame.clone();
        let reasoning_lines_displayed = Arc::new(AtomicUsize::new(0));
        let reasoning_lines_displayed_ref = reasoning_lines_displayed.clone();
        let compaction_active = Arc::new(AtomicBool::new(false));
        let compaction_active_ref = compaction_active.clone();
        let compaction_notice_shown = Arc::new(AtomicBool::new(false));
        let compaction_notice_shown_ref = compaction_notice_shown.clone();
        let sub_agent_ui_active = Arc::new(AtomicBool::new(false));
        let sub_agent_ui_active_ref = sub_agent_ui_active.clone();
        let sub_agent_animation = Arc::new(SubAgentThinkingAnimation::new());
        let sub_agent_animation_ref = sub_agent_animation.clone();

        // Shared history of collapsed bash outputs for Ctrl+O browsing.
        let expand_history = Arc::new(Mutex::new(crate::expand_history::ExpandHistory::new()));
        let expand_history_ref = expand_history.clone();
        // Snapshot of (file_path, old_content) captured at ToolExecutionStart
        // for write_file/edit_file, used to render a colored diff at end.
        let file_edit_snapshot: Arc<parking_lot::Mutex<Option<(String, String)>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let file_edit_snapshot_ref = file_edit_snapshot.clone();

        // Set global Ctrl+O live handler for bash output viewing during
        // PTY command execution.
        aish_pty::ctrl_o::set_handler(Box::new(|buffer: &[u8]| {
            let content = String::from_utf8_lossy(buffer);
            let lines = content.lines().count();
            let title = format!("Live output ({} lines)", lines);
            let panel = aish_ui::ExpandPanel::new(&title, &content);
            let _ = aish_ui::PanelRuntime::new().run(panel);
        }));

        // Set global Ctrl+O browse handler for viewing collapsed output
        // history during AI streaming (called from the EscWatcher thread).
        {
            let history = expand_history_ref.clone();
            aish_pty::ctrl_o::set_browse_handler(Box::new(move || {
                // Clone data first to avoid holding MutexGuard during blocking TUI.
                let snapshot: Vec<crate::expand_history::ExpandRecord> = {
                    let hist = history.lock().unwrap();
                    if hist.is_empty() {
                        return;
                    }
                    hist.clone_records()
                };
                let records: Vec<aish_ui::HistoryRecord> = snapshot
                    .iter()
                    .map(|r| aish_ui::HistoryRecord {
                        command: r.command.clone(),
                        line_count: r.line_count,
                        time: r.time.clone(),
                    })
                    .collect();
                let mut panel_args = std::collections::HashMap::new();
                panel_args.insert("count".into(), records.len().to_string());
                let title = aish_i18n::t_with_args("shell.panel.history_title", &panel_args);
                let panel = aish_ui::HistoryPanel::new(&title, records)
                    .with_footer_hint(aish_i18n::t("shell.panel.history_footer"))
                    .with_lines_label(aish_i18n::t("shell.panel.lines_label"));
                if let Ok(aish_ui::PanelOutcome::Submitted(outcome)) =
                    aish_ui::PanelRuntime::new().run(panel)
                {
                    let idx = outcome.selected_index.min(snapshot.len().saturating_sub(1));
                    let rec = &snapshot[idx];
                    let mut expand_args = std::collections::HashMap::new();
                    expand_args.insert("command".into(), rec.command.clone());
                    expand_args.insert("count".into(), rec.line_count.to_string());
                    let title = aish_i18n::t_with_args("shell.panel.expand_title", &expand_args);
                    let expand = aish_ui::ExpandPanel::with_footer(
                        &title,
                        &rec.output,
                        aish_i18n::t("shell.panel.expand_footer"),
                    );
                    let _ = aish_ui::PanelRuntime::new().run(expand);
                }
            }));
        }

        let shared_recorder_cb = shared_recorder.clone();
        let event_callback: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync> =
            Arc::new(move |event: LlmEvent| {
                // Helper: clear multi-line reasoning overlay and reset state.
                let clear_reasoning = || {
                    let prev = reasoning_lines_displayed_ref.swap(0, Ordering::SeqCst);
                    if prev > 0 {
                        print!("\x1b[{}A", prev);
                        for _ in 0..prev {
                            print!("\r\x1b[K\n");
                        }
                        print!("\x1b[{}A", prev);
                        let _ = io::stdout().flush();
                    }
                    reasoning_active_ref.store(false, Ordering::SeqCst);
                };

                match event.event_type {
                    LlmEventType::OpStart => {
                        // Operation begins — start thinking animation
                        *thinking_start_ref.lock().unwrap() = Some(Instant::now());
                        ttft_recorded_ref.store(false, Ordering::SeqCst);
                        *ttft_value_ref.lock().unwrap() = 0.0;
                        reasoning_frame_ref.store(0, Ordering::SeqCst);
                        reasoning_lines_displayed_ref.store(0, Ordering::SeqCst);
                        reasoning_active_ref.store(false, Ordering::SeqCst);
                        compaction_active_ref.store(false, Ordering::SeqCst);
                        compaction_notice_shown_ref.store(false, Ordering::SeqCst);
                        animation_ref.start(&t("shell.status.thinking"));
                    }
                    LlmEventType::OpEnd => {
                        // Operation ends — stop animation and show timing
                        sub_agent_ui_active_ref.store(false, Ordering::SeqCst);
                        sub_agent_animation_ref.stop();
                        animation_ref.stop();
                        let ttft = *ttft_value_ref.lock().unwrap();
                        if ttft >= 0.1 {
                            let mut ttft_args = std::collections::HashMap::new();
                            ttft_args.insert("time".to_string(), format!("{:.1}", ttft));
                            println!(
                                "{}",
                                theme::faint(&aish_i18n::t_with_args(
                                    "shell.thinking_time",
                                    &ttft_args
                                ))
                            );
                        }
                        *thinking_start_ref.lock().unwrap() = None;
                    }
                    LlmEventType::ContextCompactionStart => {
                        if !compaction_active_ref.swap(true, Ordering::SeqCst) {
                            animation_ref.start(&t("shell.status.compacting_context"));
                        }
                    }
                    LlmEventType::ContextCompactionEnd => {
                        compaction_active_ref.store(false, Ordering::SeqCst);
                        animation_ref.stop();
                        if !compaction_notice_shown_ref.swap(true, Ordering::SeqCst) {
                            if let Some(message) = context_compaction_notice(&event) {
                                println!("{}", theme::faint(&message));
                            }
                        }
                    }
                    LlmEventType::GenerationStart if react_agent_llm_event(&event) => {}
                    LlmEventType::GenerationStart
                        if crate::llm_event_ui::sub_agent_llm_event(&event) =>
                    {
                        animation_ref.stop();
                        clear_reasoning();
                        if let Some(prefix) =
                            crate::llm_event_ui::sub_agent_thinking_animation_prefix(&event)
                        {
                            sub_agent_animation_ref.start(&prefix, &t("shell.sub_agent.thinking"));
                        }
                    }
                    LlmEventType::GenerationStart => {
                        compaction_active_ref.store(false, Ordering::SeqCst);
                        animation_ref.stop();
                        clear_reasoning();
                        // Reset streamed flag so it only reflects the CURRENT
                        // generation, not a previous iteration that included
                        // tool calls with interleaved content.  Without this
                        // reset, tool-call preview text sets the flag to true,
                        // and the final text-only response is never printed.
                        streamed_flag.store(false, Ordering::SeqCst);
                        content_started_flag.store(false, Ordering::SeqCst);
                        reasoning_buf_ref.lock().unwrap().clear();
                        reasoning_frame_ref.store(0, Ordering::SeqCst);
                        renderer_ref.lock().unwrap().reset();
                        if !sub_agent_ui_active_ref.load(Ordering::SeqCst) {
                            animation_ref.start(&t("shell.status.thinking"));
                        }
                    }
                    LlmEventType::GenerationEnd if react_agent_llm_event(&event) => {}
                    LlmEventType::GenerationEnd
                        if crate::llm_event_ui::sub_agent_llm_event(&event) =>
                    {
                        sub_agent_animation_ref.stop();
                    }
                    LlmEventType::GenerationEnd => {
                        animation_ref.stop();
                        clear_reasoning();
                        // Finalize streaming display (newline + reset)
                        if content_started_flag.load(Ordering::SeqCst) {
                            renderer_ref.lock().unwrap().finalize_stream();
                        }
                    }
                    LlmEventType::ContentDelta
                        if crate::llm_event_ui::sub_agent_llm_event(&event) => {}
                    LlmEventType::ContentDelta => {
                        if let Some(delta) = event.data.get("delta").and_then(|d| d.as_str()) {
                            if !delta.is_empty() {
                                animation_ref.stop();
                                if !ttft_recorded_ref.load(Ordering::SeqCst) {
                                    if let Some(start) = *thinking_start_ref.lock().unwrap() {
                                        let elapsed = start.elapsed().as_secs_f64();
                                        *ttft_value_ref.lock().unwrap() = elapsed;
                                        ttft_recorded_ref.store(true, Ordering::SeqCst);
                                    }
                                }
                                streamed_flag.store(true, Ordering::SeqCst);
                                clear_reasoning();
                                // Robot emoji prefix on first content chunk
                                if !content_started_flag.load(Ordering::SeqCst) {
                                    content_started_flag.store(true, Ordering::SeqCst);
                                    renderer_ref.lock().unwrap().render_separator();
                                    let ai_prefix = theme::muted("🤖 ");
                                    crate::recorder::shared_record_output(
                                        &shared_recorder_cb,
                                        &ai_prefix,
                                    );
                                    print!("{}", ai_prefix);
                                }
                                // Accumulate delta and print raw text
                                renderer_ref.lock().unwrap().append_delta(delta);
                            }
                        }
                    }
                    LlmEventType::ReasoningStart => {
                        animation_ref.stop();
                        reasoning_active_ref.store(true, Ordering::SeqCst);
                        reasoning_frame_ref.store(0, Ordering::SeqCst);
                        reasoning_lines_displayed_ref.store(0, Ordering::SeqCst);
                        reasoning_buf_ref.lock().unwrap().clear();
                    }
                    LlmEventType::ReasoningDelta => {
                        if let Some(delta) = event.data.get("delta").and_then(|d| d.as_str()) {
                            if !delta.is_empty() {
                                animation_ref.stop();
                                let mut buf = reasoning_buf_ref.lock().unwrap();
                                buf.push_str(delta);

                                // Get last 2 non-empty lines
                                let all_lines: Vec<&str> =
                                    buf.lines().filter(|l| !l.trim().is_empty()).collect();
                                let display_lines: Vec<&str> = all_lines
                                    .iter()
                                    .rev()
                                    .take(2)
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .rev()
                                    .copied()
                                    .collect();

                                let max_cols = crossterm::terminal::size()
                                    .map(|(_, cols)| cols as usize)
                                    .unwrap_or(80)
                                    .max(20)
                                    .saturating_sub(4);

                                let frame = reasoning_frame_ref.fetch_add(1, Ordering::SeqCst);
                                let spinner = crate::theme::spinner_frame(
                                    crate::theme::SPINNER_STATUS,
                                    frame,
                                );
                                let spinner = crate::theme::accent(spinner);

                                // Elapsed time for header
                                let elapsed_str = thinking_start_ref
                                    .lock()
                                    .unwrap()
                                    .map(|s| {
                                        let e = s.elapsed().as_secs_f64();
                                        if e >= 1.0 {
                                            let mut args = std::collections::HashMap::new();
                                            args.insert("elapsed".to_string(), format!("{:.1}", e));
                                            format!(
                                                " {}",
                                                aish_i18n::t_with_args(
                                                    "shell.session.thinking_elapsed",
                                                    &args
                                                )
                                            )
                                        } else {
                                            format!(" {}", aish_i18n::t("shell.session.thinking"))
                                        }
                                    })
                                    .unwrap_or_else(|| {
                                        format!(" {}", aish_i18n::t("shell.session.thinking"))
                                    });

                                let prev = reasoning_lines_displayed_ref.load(Ordering::SeqCst);
                                let new_count = 1 + display_lines.len();

                                // Move cursor up to overwrite previous display
                                if prev > 0 {
                                    print!("\x1b[{}A", prev);
                                }

                                // Header line
                                if display_lines.is_empty() {
                                    print!("\r\x1b[K{}{}...\n", spinner, elapsed_str);
                                } else {
                                    print!("\r\x1b[K{}{}\n", spinner, elapsed_str);
                                }

                                // Content lines
                                for line in &display_lines {
                                    let truncated = truncate_display_width(line.trim(), max_cols);
                                    print!("\r\x1b[K{}\n", crate::theme::muted(&truncated));
                                }

                                // Clear leftover lines from previous larger display
                                for _ in new_count..prev {
                                    print!("\r\x1b[K\n");
                                }

                                // Move cursor back up from extra cleared lines
                                if prev > new_count {
                                    print!("\x1b[{}A", prev - new_count);
                                }

                                reasoning_lines_displayed_ref.store(new_count, Ordering::SeqCst);
                                reasoning_active_ref.store(true, Ordering::SeqCst);
                                let _ = io::stdout().flush();
                            }
                        }
                    }
                    LlmEventType::ReasoningEnd => {
                        clear_reasoning();
                        reasoning_buf_ref.lock().unwrap().clear();
                    }
                    LlmEventType::ToolExecutionStart => {
                        if crate::llm_event_ui::is_parent_agent_spawn_tool_event(&event) {
                            sub_agent_ui_active_ref.store(true, Ordering::SeqCst);
                            animation_ref.stop();
                            sub_agent_animation_ref.stop();
                            clear_reasoning();
                        } else if let Some(name) =
                            event.data.get("tool_name").and_then(|n| n.as_str())
                        {
                            let is_sub_agent =
                                crate::llm_event_ui::sub_agent_context(&event).is_some();
                            if is_sub_agent {
                                sub_agent_animation_ref.stop();
                                animation_ref.stop();
                                clear_reasoning();
                            } else if !react_agent_llm_event(&event) {
                                animation_ref.stop();
                                clear_reasoning();
                            }
                            let args_preview = event
                                .data
                                .get("tool_args")
                                .map(|a| format_tool_args_for_display(name, a))
                                .unwrap_or_default();
                            // Snapshot old file content for write_file/edit_file
                            // so we can render a colored diff at ToolExecutionEnd.
                            if name == "write_file" || name == "edit_file" {
                                let path = event
                                    .data
                                    .get("tool_args")
                                    .and_then(|a| a.get("path"))
                                    .and_then(|p| p.as_str());
                                // Always reset the snapshot slot: Some on
                                // success / new-file, None on failure or when
                                // the tool call lacks a path argument.
                                *file_edit_snapshot_ref.lock() = match path {
                                    Some(path) => match std::fs::read_to_string(path) {
                                        Ok(c) if c.len() <= 64 * 1024 => {
                                            Some((path.to_string(), c))
                                        }
                                        Ok(_) => None,
                                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                            Some((path.to_string(), String::new()))
                                        }
                                        Err(_) => None,
                                    },
                                    None => None,
                                };
                            }
                            // Ensure we're on a fresh line after content streaming
                            if content_started_flag.load(Ordering::SeqCst) {
                                println!();
                            }
                            let tool_line =
                                if let Some(ctx) = crate::llm_event_ui::sub_agent_context(&event) {
                                    crate::llm_event_ui::format_sub_agent_tool_line(
                                        &ctx,
                                        name,
                                        &args_preview,
                                    )
                                } else {
                                    format!(
                                        "{} {} {} {}",
                                        theme::dim(theme::TOOL_BOX_TOP),
                                        theme::accent(theme::TOOL_PREFIX),
                                        theme::accent(name),
                                        theme::muted(&format!("({})", args_preview))
                                    )
                                };
                            crate::recorder::shared_record_output(
                                &shared_recorder_cb,
                                &format!("{}\n", tool_line),
                            );
                            println!("{}", tool_line);
                            let _ = io::stdout().flush();
                            // Start progress spinner for non-interactive tools.
                            // Skip animation when the tool needs interactive
                            // terminal input (sudo password, ssh, TUI apps) —
                            // the spinner's \r\x1b[K would erase the prompt.
                            let is_interactive_tool = matches!(name, "ask_user" | "resolve");
                            let bash_needs_tty = name == "bash"
                                && event
                                    .data
                                    .get("tool_args")
                                    .and_then(|a| a.get("command"))
                                    .and_then(|c| c.as_str())
                                    .is_some_and(aish_tools::bash::command_needs_interactive);
                            if crate::llm_event_ui::sub_agent_context(&event).is_none()
                                && !react_agent_llm_event(&event)
                                && !is_interactive_tool
                                && !bash_needs_tty
                            {
                                animation_ref.start(&theme::tool_status_label(name));
                            }
                        }
                    }
                    LlmEventType::ToolExecutionEnd => {
                        if crate::llm_event_ui::is_parent_agent_spawn_tool_event(&event) {
                            sub_agent_ui_active_ref.store(false, Ordering::SeqCst);
                        }
                        // Stop progress spinner started at ToolExecutionStart.
                        animation_ref.stop();
                        let tool_ok = event
                            .data
                            .get("ok")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(true);
                        if let Some(preview) =
                            event.data.get("output_preview").and_then(|p| p.as_str())
                        {
                            // Display collapsed output (first 2 lines) for bash tool,
                            // matching Python's _collapse_output_lines behavior.
                            let tool_name = event
                                .data
                                .get("tool_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            let interactive_bash = tool_name == "bash"
                                && event
                                    .data
                                    .get("tool_args")
                                    .and_then(|args| args.get("command"))
                                    .and_then(|command| command.as_str())
                                    .is_some_and(aish_tools::bash::command_needs_interactive);
                            if tool_name == "bash" && !preview.is_empty() && !interactive_bash {
                                let content = strip_tool_output_xml(preview);
                                if !content.is_empty() {
                                    // Use actual total line count from the full
                                    // output (not the truncated preview) for the
                                    // collapse hint.
                                    let total_lines = event
                                        .data
                                        .get("output_total_lines")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or_else(|| content.lines().count() as u64)
                                        as usize;
                                    let rail = theme::dim(theme::TOOL_BOX_MID);
                                    let collapsed = collapse_display_lines(&content, 2);
                                    let mut preview_ansi = collapsed
                                        .lines()
                                        .map(|l| format!("{}   {}", rail, theme::dim(l)))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    if total_lines > 2 {
                                        let hidden = total_lines - 2;
                                        let ctrl_o = theme::bold(&theme::accent("Ctrl+O"));
                                        preview_ansi.push_str(&format!(
                                            "\n{}   {}{} lines hidden ─── {}{}",
                                            rail,
                                            theme::dim("... "),
                                            hidden,
                                            ctrl_o,
                                            theme::dim(" to expand ...")
                                        ));
                                    }
                                    preview_ansi.push('\n');
                                    if let Some(ctx) =
                                        crate::llm_event_ui::sub_agent_context(&event)
                                    {
                                        preview_ansi =
                                            crate::llm_event_ui::indent_sub_agent_output_block(
                                                &ctx,
                                                preview_ansi.trim_end(),
                                            );
                                        preview_ansi.push('\n');
                                    }
                                    crate::recorder::shared_record_output(
                                        &shared_recorder_cb,
                                        &preview_ansi,
                                    );
                                    print!("{}", preview_ansi);
                                    let _ = io::stdout().flush();
                                    // Only record to expand history when output is
                                    // actually collapsed (total_lines > 2).
                                    if total_lines > 2 {
                                        let command = event
                                            .data
                                            .get("tool_args")
                                            .and_then(|args| args.get("command"))
                                            .and_then(|c| c.as_str())
                                            .unwrap_or("bash")
                                            .to_string();
                                        let full_output = event
                                            .data
                                            .get("output_full")
                                            .and_then(|v| v.as_str())
                                            .map(|s| strip_tool_output_xml(s))
                                            .unwrap_or(content);
                                        expand_history_ref
                                            .lock()
                                            .unwrap()
                                            .add(command, full_output);
                                    }
                                }
                            }
                        }
                        // Render colored diff for write_file/edit_file tools.
                        // Only consume the snapshot when this End event matches
                        // a write_file/edit_file call — otherwise leave it for
                        // the paired End event of the tool that stored it.
                        let end_tool = event
                            .data
                            .get("tool_name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        if end_tool == "write_file" || end_tool == "edit_file" {
                            let diff_snapshot = file_edit_snapshot_ref.lock().take();
                            if let Some((path, old_content)) = diff_snapshot {
                                if let Ok(new_content) = std::fs::read_to_string(&path) {
                                    if new_content.len() <= 64 * 1024 {
                                        let diff =
                                            theme::render_diff(&old_content, &new_content, 20);
                                        if !diff.is_empty() {
                                            let rail = theme::dim(theme::TOOL_BOX_MID);
                                            let diff_ansi: String = diff
                                                .lines()
                                                .map(|l| format!("{}   {}", rail, l))
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            println!("{}", diff_ansi);
                                            let _ = io::stdout().flush();
                                        }
                                    }
                                }
                            }
                        }
                        // Always print completion indicator with box bottom corner.
                        {
                            let status_line = if tool_ok {
                                format!(
                                    "{} {} {}",
                                    theme::dim(theme::TOOL_BOX_BOT),
                                    theme::success(theme::ICON_SUCCESS),
                                    theme::dim("done")
                                )
                            } else {
                                format!(
                                    "{} {} {}",
                                    theme::dim(theme::TOOL_BOX_BOT),
                                    theme::error(theme::ICON_ERROR),
                                    theme::dim("failed")
                                )
                            };
                            crate::recorder::shared_record_output(
                                &shared_recorder_cb,
                                &format!("{}\n", status_line),
                            );
                            println!("{}", status_line);
                            let _ = io::stdout().flush();
                        }
                    }
                    LlmEventType::Error => {
                        sub_agent_animation_ref.stop();
                        animation_ref.stop();
                        clear_reasoning();
                        let error_msg = event
                            .data
                            .get("error")
                            .or_else(|| event.data.get("error_message"))
                            .and_then(|e| e.as_str())
                            .unwrap_or("Unknown error");
                        let msg = {
                            let mut args = std::collections::HashMap::new();
                            args.insert("error".to_string(), error_msg.to_string());
                            t_with_args("shell.error.llm_error_message", &args)
                        };
                        eprintln!("{}", theme::error(&msg));
                    }
                    LlmEventType::Cancelled => {
                        sub_agent_animation_ref.stop();
                        animation_ref.stop();
                        clear_reasoning();
                        // Outer AI handlers print `shell.interrupted` once; avoid a second line.
                    }
                    LlmEventType::ToolConfirmationRequired => {
                        // Handled by separate confirmation_callback
                    }
                    LlmEventType::InteractionRequired => {
                        let prompt_text = event
                            .data
                            .get("prompt")
                            .and_then(|p| p.as_str())
                            .unwrap_or("");
                        if !prompt_text.is_empty() {
                            println!("{}", theme::accent(prompt_text));
                        }
                    }
                }
                None // Always continue
            });

        llm_session.set_event_callback(event_callback.clone());

        // Set up confirmation callback for tool approval flow
        let confirmation_callback: Arc<
            dyn Fn(&aish_llm::PreflightSecurityContext) -> aish_llm::ApprovalChoice + Send + Sync,
        > = Arc::new(|ctx: &aish_llm::PreflightSecurityContext| {
            let width = std::env::var("COLUMNS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(80);
            let inner_width = width.saturating_sub(4).max(20);
            let border = "─".repeat(inner_width);
            println!();
            println!("{}", theme::warning(&format!("╭{}╮", border)));
            let sec_title = theme::bold(&theme::warning(" ⚠  Security Confirmation Required"));
            print_panel_line(&sec_title, inner_width);
            print_panel_line("", inner_width);
            let tool_label = theme::bold(&theme::accent(&t("shell.confirm_dialog_tool")));
            print_panel_line(
                &format!("  {}   {}", tool_label, ctx.tool_name),
                inner_width,
            );
            let command = ctx.target.as_deref().unwrap_or("");
            if !command.is_empty() {
                let cmd_label = theme::bold(&theme::accent(&t("shell.confirm_dialog_command")));
                let safe_command = sanitize_for_display(command);
                let cmd_lines = wrap_text(&safe_command, width.saturating_sub(14));
                print_panel_line(
                    &format!("  {} {}", cmd_label, cmd_lines.lines().next().unwrap_or("")),
                    inner_width,
                );
                for line in cmd_lines.lines().skip(1) {
                    print_panel_line(&format!("         {}", line), inner_width);
                }
            }
            let reason_lines = wrap_text(&ctx.message, width.saturating_sub(14));
            let reason_label = theme::bold(&theme::accent("Reason:"));
            print_panel_line(
                &format!(
                    "  {} {}",
                    reason_label,
                    reason_lines.lines().next().unwrap_or("")
                ),
                inner_width,
            );
            for line in reason_lines.lines().skip(1) {
                print_panel_line(&format!("         {}", line), inner_width);
            }
            print_panel_line("", inner_width);
            print_panel_line(
                &format!("  {}", theme::accent(&t("shell.confirm_dialog_options"))),
                inner_width,
            );
            // Input line lives inside the panel so it stays visually attached
            // to the question and the pressed key is echoed back to the user.
            let prompt_text = t("shell.confirm_dialog_question");
            let prompt_vis = ansi_display_width(&prompt_text);
            print!(
                "{}  {} ",
                theme::warning("│"),
                theme::bold(&theme::accent(&prompt_text))
            );
            let _ = std::io::stdout().flush();
            let (pressed, choice) = read_approval_choice();
            let echo_ch = match pressed {
                b'\r' | b'\n' => '↵',
                0 => '?',
                c if (c as char).is_ascii_graphic() => c as char,
                _ => '·',
            };
            print!("{}", theme::accent(&echo_ch.to_string()));
            let used = 2 + prompt_vis + 1 + 1;
            let pad = inner_width.saturating_sub(used);
            println!("{}{}", " ".repeat(pad), theme::warning("│"));
            println!("{}", theme::warning(&format!("╰{}╯", border)));
            let verdict_key = match choice {
                aish_llm::ApprovalChoice::Once => "shell.confirm_choice_once",
                aish_llm::ApprovalChoice::RememberSession => "shell.confirm_choice_remember",
                aish_llm::ApprovalChoice::ReplyToAi => "shell.confirm_choice_reply",
                aish_llm::ApprovalChoice::Deny => "shell.confirm_choice_deny",
            };
            println!("  {} {}", theme::accent("→"), theme::bold(&t(verdict_key)));
            choice
        });

        llm_session.set_confirmation_callback(confirmation_callback);

        // Session-scoped approval memory: when the user approves a command with
        // "remember", equivalent commands (same host + normalized text) skip
        // confirmation — and the sandbox preflight — for the rest of the session.
        let approval_memory: Arc<Mutex<aish_llm::ApprovalMemory>> =
            Arc::new(Mutex::new(aish_llm::ApprovalMemory::new()));
        llm_session.set_approval_memory(approval_memory.clone());

        if let Some(ref audit) = audit_store {
            let scanner = security_manager.secret_scanner().clone();
            let redactor: Arc<dyn Fn(&str) -> String + Send + Sync> =
                Arc::new(move |text: &str| aish_security::secret::redact_secrets(text, &scanner));
            llm_session.set_audit_context(
                audit.clone() as Arc<dyn aish_core::AuditSink>,
                Some(redactor),
                session_uuid.clone(),
                audit_user.clone(),
                Some(Arc::new(Mutex::new(audit_host.clone()))),
            );
        }

        // Set iteration limit callback: ask user whether to continue after 20 tool-call rounds
        let iteration_limit_callback: Arc<dyn Fn(u32) -> bool + Send + Sync> =
            Arc::new(|iterations: u32| {
                let width = std::env::var("COLUMNS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(80);
                let inner_width = width.saturating_sub(4).max(20);
                let border = "─".repeat(inner_width);
                println!();
                println!("{}", theme::warning(&format!("╭{}╮", border)));
                let iter_title = theme::bold(&theme::warning(&aish_i18n::t(
                    "shell.session.iteration_limit_title",
                )));
                print_panel_line(&format!(" {}", iter_title), inner_width);
                print_panel_line("", inner_width);
                print_panel_line(
                    &format!(
                        "  {}",
                        aish_i18n::t_with_args("shell.session.iteration_limit_reached", &{
                            let mut m = std::collections::HashMap::new();
                            m.insert("count".to_string(), iterations.to_string());
                            m
                        },)
                    ),
                    inner_width,
                );
                print_panel_line("", inner_width);
                print_panel_line(
                    &format!(
                        "  {}",
                        theme::accent(&aish_i18n::t("shell.session.iteration_continue_prompt"))
                    ),
                    inner_width,
                );
                println!("{}", theme::warning(&format!("╰{}╯", border)));
                print!("  ");
                let _ = std::io::stdout().flush();

                read_raw_confirmation()
            });

        llm_session.set_iteration_limit_callback(iteration_limit_callback);

        // Build AI handler with all subsystems
        let ai_handler = AiHandler::new(
            llm_session,
            memory_manager.clone(),
            skill_manager,
            memory_config,
            config.max_llm_messages,
            config.max_shell_messages,
            config.context_token_budget,
            context_budget_policy,
        );

        // Note: event_callback is already set on the LlmSession before AiHandler takes ownership

        let version = env!("CARGO_PKG_VERSION").to_string();

        // Print welcome banner with changelog
        let changelog = aish_i18n::changelog::parse_current_changelog(&version);
        print!(
            "{}",
            prompt::render_welcome(&version, &config.model, skill_count, changelog.clone())
        );
        let _ = io::stdout().flush();

        // One-shot tip when this launch moved legacy install-seeded skills.
        if let Some(notice) = seed_migration_notice {
            eprintln!("{}", notice.user_message());
            let _ = io::stderr().flush();
        }

        // Store full changelog in expand_history so Ctrl+O can show all entries
        // when the welcome panel truncates them with "and N more".
        if changelog.len() > 2 {
            let full_text = prompt::format_changelog_full(&version, &changelog);
            let cl_title = prompt::changelog_title(&version);
            expand_history
                .lock()
                .unwrap()
                .add(format!("[changelog] {}", cl_title), full_text);
        }

        // Initialize persistent PTY session
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let pty = aish_pty::PersistentPty::start(&state.cwd, rows, cols).map_err(|e| {
            let mut args = std::collections::HashMap::new();
            args.insert(
                "error".to_string(),
                format!("failed to start persistent PTY: {e}"),
            );
            aish_core::AishError::Pty(t_with_args("shell.general_error", &args))
        })?;
        let pty = Arc::new(Mutex::new(pty));

        // Inject PersistentPty into the bash tool slot.
        {
            let mut slot = pty_slot.lock().unwrap();
            *slot = Some(pty.clone());
        }

        // Placeholder instances for struct fields.  The real subsystems live
        // inside AiHandler which needs mutable access during each turn.
        let shell_skill_manager = SkillManager::new();

        let secret_check_closure =
            Self::build_secret_check_closure(security_manager.secret_scanner().clone());

        let secret_vault = std::sync::Arc::new(std::sync::Mutex::new(
            aish_security::secret::SecretVault::new(),
        ));

        // Inject the vault into BashTool's slot so command execution can
        // restore $SECRET_* placeholders.
        {
            let mut guard = vault_slot.lock().unwrap();
            *guard = Some(secret_vault.clone());
        }

        Ok(Self {
            state,
            config,
            ai_handler,
            security_manager,
            input_guard,
            secret_check_closure,
            secret_vault,
            session_store,
            audit_store,
            audit_user,
            audit_host,
            current_remote_host: Arc::new(Mutex::new(None)),
            skill_manager: shell_skill_manager,
            skill_hot_reloader,
            memory_manager: memory_manager.clone(),
            version,
            operation_in_progress: false,
            pty,
            session_uuid,
            streamed_content,
            phase: ShellPhase::Booting,
            interruption: InterruptionState::default(),
            last_ctrl_c: None,
            animation,
            expand_history,
            shared_recorder,
            inline_ai: None,
            approval_memory,
        })
    }

    /// Create a new shell instance and immediately restore an existing session.
    pub fn resume(config: ConfigModel, session_id: &str) -> aish_core::Result<Self> {
        let mut shell = Self::new(config)?;
        let transient_session_uuid = shell.session_uuid.clone();
        if let Err(err) = shell.resume_session_with_options(session_id, false, false) {
            if let Some(ref store) = shell.session_store {
                let _ = store.delete_session(&transient_session_uuid);
            }
            return Err(err);
        }
        if shell.session_uuid != transient_session_uuid {
            if let Some(ref store) = shell.session_store {
                let _ = store.delete_session(&transient_session_uuid);
            }
        }
        Ok(shell)
    }

    /// Install a POSIX SIGINT handler that atomically sets the LLM
    /// session's cancellation flag. Returns the previous `SigAction`
    /// so it can be restored via `restore_ai_sigint_handler`.
    fn install_ai_sigint_handler(&self) -> Option<nix::sys::signal::SigAction> {
        use nix::sys::signal::{self, SigAction, SigHandler, SigSet, Signal};

        // Clear any leftover cancellation state from a previous operation.
        self.ai_handler.cancellation_token().reset();

        let token_ptr = self.ai_handler.cancellation_token() as *const CancellationToken;
        CANCEL_TOKEN_PTR.store(token_ptr as *mut (), Ordering::SeqCst);

        let action = SigAction::new(
            SigHandler::Handler(ai_sigint_handler),
            signal::SaFlags::empty(),
            SigSet::empty(),
        );
        unsafe { signal::sigaction(Signal::SIGINT, &action) }.ok()
    }

    /// Restore the SIGINT handler saved by `install_ai_sigint_handler`.
    fn restore_ai_sigint_handler(old: Option<nix::sys::signal::SigAction>) {
        CANCEL_TOKEN_PTR.store(std::ptr::null_mut(), Ordering::SeqCst);
        if let Some(old) = old {
            use nix::sys::signal::{self, Signal};
            let _ = unsafe { signal::sigaction(Signal::SIGINT, &old) };
        }
    }

    /// Run the main REPL loop.
    pub fn run(&mut self) -> aish_core::Result<()> {
        let runtime = tokio::runtime::Runtime::new()?;

        // Set up SIGTERM handler for graceful shutdown
        let sigterm_exit = Arc::new(AtomicBool::new(false));
        let sigterm_flag = sigterm_exit.clone();
        runtime.spawn(async move {
            let mut sigterm_stream =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(_) => return,
                };
            sigterm_stream.recv().await;
            sigterm_flag.store(true, Ordering::SeqCst);
        });

        // Build the shared AutoSuggest engine up here, before ShellReadline::new.
        let autosuggest = Arc::new(Mutex::new(crate::autosuggest::AutoSuggest::new(5000)));
        let runtime_handle = runtime.handle().clone();

        let inline_ai: Option<Arc<crate::inline_completion::InlineCompleter>> = {
            let cfg = &self.config.inline_completion;
            if !cfg.enabled {
                None
            } else if self.config.api_key.is_empty() {
                tracing::info!("inline_completion enabled but no api_key configured; skipping");
                None
            } else {
                let llm_client = Arc::new(aish_llm::LlmClient::new(
                    &self.config.api_base,
                    &self.config.api_key,
                    &self.config.model,
                ));
                let provider = crate::inline_completion::build_default_provider(
                    llm_client,
                    cfg.disable_thinking,
                    cfg.enforce_json,
                );
                Some(crate::inline_completion::InlineCompleter::new(
                    provider,
                    autosuggest.clone(),
                    cfg.clone(),
                    runtime_handle,
                ))
            }
        };

        // Store inline_ai on self so `/model` can update its model at runtime.
        self.inline_ai = inline_ai.clone();

        // Initialize readline with history, tab completion, and line editing
        let mut rl = ShellReadline::new(
            self.pty.clone(),
            autosuggest,
            inline_ai,
            self.config.history_size,
            self.config.auto_suggest,
        )
        .map_err(|e| {
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            aish_core::AishError::Config(t_with_args("shell.readline_init_failed", &args))
        })?;

        // Load history from default location
        let history_path = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("aish")
            .join("history.txt");
        if let Some(parent) = history_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        rl.load_history(&history_path);

        // Load recent history from SQLite across all sessions
        if let Some(ref store) = self.session_store {
            if let Ok(sessions) = store.list_sessions(5) {
                for session in sessions.iter() {
                    if let Ok(entries) = store.get_history(&session.session_uuid, 200) {
                        for entry in entries.iter().rev() {
                            rl.add_history_entry(&entry.command);
                        }
                    }
                }
            }
        }

        self.set_phase(ShellPhase::Editing);

        loop {
            if self.state.should_exit {
                break;
            }
            if sigterm_exit.load(Ordering::SeqCst) {
                break;
            }

            // Check for skill hot-reload changes
            if let Some(ref reloader) = self.skill_hot_reloader {
                let affected = reloader.apply_changes(&mut self.skill_manager);
                if !affected.is_empty() {
                    for name in &affected {
                        tracing::info!("Skill '{}' hot-reloaded", name);
                    }
                }
            }

            // Render prompt and read input via rustyline
            let mode = match self.ai_handler.plan_phase() {
                aish_core::PlanPhase::Planning => "plan",
                aish_core::PlanPhase::Normal => "aish",
            };
            let recording = self
                .shared_recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            let prompt_str = prompt::render_prompt(
                &self.state.cwd,
                &self.config.model,
                self.state.last_exit_code,
                mode,
                recording,
                self.config.prompt_style.as_deref(),
            );
            // Record prompt output if recording is active.
            // \r\x1b[2K clears the current line so re-rendered prompts
            // overwrite the previous one instead of appending after it.
            {
                crate::recorder::shared_record_output(
                    &self.shared_recorder,
                    &format!("\r\x1b[2K{}", prompt_str),
                );
            }
            let input = match rl.read_line(&prompt_str) {
                Ok(Some(line)) => line,
                Ok(None) => break, // EOF (Ctrl-D)
                Err(e) => {
                    // Check if Shift+Tab or F2 triggered the interrupt (mode toggle)
                    if matches!(e, rustyline::error::ReadlineError::Interrupted)
                        && rl.was_mode_toggle_requested()
                    {
                        let new_phase = self.ai_handler.toggle_plan_mode(&self.session_uuid);
                        match new_phase {
                            aish_core::PlanPhase::Planning => {
                                println!(
                                    "{}",
                                    theme::bold(&theme::warning(&t("shell.plan_mode_enabled")))
                                );
                                println!("{}", theme::faint(&t("shell.plan_mode_hint")));
                            }
                            aish_core::PlanPhase::Normal => {
                                println!("{}", theme::warning(&t("shell.plan_mode_disabled")));
                            }
                        }
                        continue;
                    }
                    // Check if Ctrl+O was pressed (expand collapsed output)
                    if matches!(e, rustyline::error::ReadlineError::Interrupted)
                        && rl.was_ctrl_o_requested()
                    {
                        if !self.expand_history.lock().unwrap().is_empty() {
                            self.show_expand_panel();
                        }
                        continue;
                    }
                    // Check if `/` on empty line triggered slash command popup
                    if matches!(e, rustyline::error::ReadlineError::Interrupted)
                        && rl.was_slash_requested()
                    {
                        match self.run_slash_input_session(&prompt_str) {
                            aish_ui::SlashInputOutcome::Command(cmd) => {
                                self.apply_slash_popup_command(&mut rl, &prompt_str, &cmd);
                            }
                            aish_ui::SlashInputOutcome::Dismissed(text) => {
                                if self.read_line_after_slash_dismiss(&mut rl, &prompt_str, &text) {
                                    break;
                                }
                            }
                            aish_ui::SlashInputOutcome::Cancelled => {}
                        }
                        continue;
                    }
                    // Check if `@` inside an AI prompt triggered the file-mention popup
                    if matches!(e, rustyline::error::ReadlineError::Interrupted)
                        && rl.was_at_file_requested()
                    {
                        if let Some((line, at_pos)) = crate::readline::take_at_file_context() {
                            match self.run_file_mention_session(&prompt_str, &line, at_pos) {
                                aish_ui::FileMentionOutcome::Selected(path) => {
                                    let before = &line[..at_pos];
                                    let after = &line[at_pos..];
                                    let new_line = format!("{before}@{path} {after}");
                                    if self.read_line_after_at_file(&mut rl, &prompt_str, &new_line)
                                    {
                                        break;
                                    }
                                }
                                aish_ui::FileMentionOutcome::Cancelled => {
                                    if self.read_line_after_at_file(&mut rl, &prompt_str, &line) {
                                        break;
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    // Interrupt (Ctrl-C) — handle double-press exit
                    if matches!(e, rustyline::error::ReadlineError::Interrupted) {
                        // Record newline so cast replay moves cursor to next line
                        // instead of filling the current line with spaces via \x1b[2K.
                        crate::recorder::shared_record_output(&self.shared_recorder, "\r\n");
                        if self.handle_ctrl_c() {
                            break;
                        }
                        continue;
                    }
                    eprintln!("{}", {
                        let mut args = std::collections::HashMap::new();
                        args.insert("error".to_string(), e.to_string());
                        t_with_args("shell.readline_error", &args)
                    });
                    break;
                }
            };
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            // Add to history
            self.state.history.push(input.to_string());

            // Reset streamed-content flag before each AI call
            self.streamed_content.store(false, Ordering::SeqCst);

            // Classify and route
            match input::classify_input(input) {
                crate::types::InputIntent::Empty => {}
                crate::types::InputIntent::Ai => {
                    let mut question = input::extract_ai_question(input);

                    // If just ";" with no question and there's a pending error,
                    // trigger error correction instead of a normal AI query.
                    if question.is_empty() && self.state.can_correct_error {
                        crate::recorder::shared_record_input(&self.shared_recorder, ";\n");
                        if let Some(ref cmd) = self.state.last_command.clone() {
                            let old_sigint = self.install_ai_sigint_handler();
                            let mut esc_watcher = CrosstermEscWatcher::start(
                                self.ai_handler.cancellation_token_arc(),
                            );
                            let token_ptr =
                                self.ai_handler.cancellation_token() as *const CancellationToken;
                            let result = runtime.block_on(async {
                                tokio::select! {
                                    r = self.ai_handler.handle_error_correction(
                                        cmd,
                                        self.state.last_exit_code,
                                        &self.state.last_output,
                                    ) => r,
                                    _ = poll_cancelled(token_ptr) => {
                                        Err(aish_core::AishError::Cancelled)
                                    }
                                }
                            });
                            esc_watcher.stop();
                            Self::restore_ai_sigint_handler(old_sigint);

                            match result {
                                Ok(correction) => {
                                    match &correction.command {
                                        Some(corrected) => {
                                            // Display corrected command and description
                                            let corrected_line = format!(
                                                "{} {}",
                                                t("shell.error_correction.corrected_command_title"),
                                                theme::accent(&theme::bold(corrected))
                                            );
                                            println!("{}", corrected_line);
                                            crate::recorder::shared_record_output(
                                                &self.shared_recorder,
                                                &format!("{}\r\n", corrected_line),
                                            );
                                            if let Some(ref desc) = correction.description {
                                                if !desc.is_empty() {
                                                    println!("   {}", desc);
                                                    crate::recorder::shared_record_output(
                                                        &self.shared_recorder,
                                                        &format!("   {}\r\n", desc),
                                                    );
                                                }
                                            }
                                            // Ask user confirmation: Y/n
                                            let prompt = format!(
                                                "{}{}{}",
                                                t("shell.error_correction.confirm_execute_prefix"),
                                                theme::accent(&theme::bold(corrected)),
                                                t("shell.error_correction.confirm_execute_suffix")
                                            );
                                            print!("{}", prompt);
                                            crate::recorder::shared_record_output(
                                                &self.shared_recorder,
                                                &prompt,
                                            );
                                            let _ = std::io::stdout().flush();
                                            let mut answer = String::new();
                                            if std::io::stdin().read_line(&mut answer).is_err() {
                                                continue;
                                            }
                                            crate::recorder::shared_record_input(
                                                &self.shared_recorder,
                                                &answer,
                                            );
                                            let answer = answer.trim().to_lowercase();
                                            if answer == "y" || answer == "yes" || answer.is_empty()
                                            {
                                                // InputGuard: AI-corrected commands must
                                                // clear the same gate as user-typed ones,
                                                // even after the Y/n approval above.
                                                if self.screen_shell_command(corrected) {
                                                    let exit_code =
                                                        self.execute_external_command(corrected);
                                                    self.record_history(corrected, exit_code);
                                                }
                                            }
                                            self.state.can_correct_error = false;
                                        }
                                        None => {
                                            // No valid command, show description if available
                                            let warn_line = theme::warning(&format!(
                                                "\u{26a0} {}",
                                                t("shell.error_correction.no_valid_command")
                                            ));
                                            println!("{}", warn_line);
                                            crate::recorder::shared_record_output(
                                                &self.shared_recorder,
                                                &format!("{}\r\n", warn_line),
                                            );
                                            if let Some(ref desc) = correction.description {
                                                let clean = desc
                                                    .split("Insufficient context")
                                                    .next()
                                                    .unwrap_or(desc)
                                                    .trim();
                                                if !clean.is_empty() {
                                                    println!("   {}", clean);
                                                    crate::recorder::shared_record_output(
                                                        &self.shared_recorder,
                                                        &format!("   {}\r\n", clean),
                                                    );
                                                }
                                            }
                                            let hint_line = format!(
                                                "   {}",
                                                theme::accent(&t(
                                                    "shell.error_correction.retry_hint"
                                                ))
                                            );
                                            println!("{}", hint_line);
                                            crate::recorder::shared_record_output(
                                                &self.shared_recorder,
                                                &format!("{}\r\n", hint_line),
                                            );
                                        }
                                    }
                                }
                                Err(aish_core::AishError::Cancelled) => {
                                    self.animation.stop();
                                    crate::recorder::shared_record_output(
                                        &self.shared_recorder,
                                        &format!("{}\r\n", theme::warning("Interrupted")),
                                    );
                                    println!("{}", theme::warning("Interrupted"));
                                }
                                Err(e) => {
                                    self.animation.stop();
                                    // Errors are already displayed via the LlmEventType::Error
                                    // event callback — avoid printing twice. Only handle
                                    // non-LLM errors that bypass the event system.
                                    if !matches!(e, aish_core::AishError::Llm(_)) {
                                        let msg = t("shell.error.llm_error_message")
                                            .replace("{error}", &e.to_string());
                                        eprintln!("{}", theme::error(&msg));
                                    }
                                }
                            }
                            continue;
                        }
                    }

                    // InputGuard pre-check for AI prompts
                    if !self.screen_ai_prompt(&question) {
                        continue;
                    }

                    // Security gate: detect secrets in AI input
                    if !self.check_security_gate(&mut question) {
                        continue;
                    }

                    // Record sanitized user input after security check passes.
                    // Use the original prefix (; or ；) so the cast replay shows
                    // the AI trigger character.
                    let ai_prefix = if input.starts_with('\u{ff1b}') {
                        "\u{ff1b}"
                    } else {
                        ";"
                    };
                    crate::recorder::shared_record_input(
                        &self.shared_recorder,
                        &format!("{}{}\n", ai_prefix, question),
                    );

                    let old_sigint = self.install_ai_sigint_handler();
                    let mut esc_watcher =
                        CrosstermEscWatcher::start(self.ai_handler.cancellation_token_arc());
                    let token_ptr =
                        self.ai_handler.cancellation_token() as *const CancellationToken;
                    let token_stats_before = self.ai_handler.session_token_stats();
                    let ai_start = std::time::Instant::now();
                    let result = runtime.block_on(async {
                        tokio::select! {
                            r = self.ai_handler.handle_question(&question) => r,
                            _ = poll_cancelled(token_ptr) => {
                                Err(aish_core::AishError::Cancelled)
                            }
                        }
                    });
                    let ai_elapsed = ai_start.elapsed().as_secs_f64();
                    esc_watcher.stop();
                    Self::restore_ai_sigint_handler(old_sigint);
                    self.sync_state_from_pty_cwd();

                    let did_stream = self.streamed_content.load(Ordering::SeqCst);

                    match result {
                        Ok(response) => {
                            if self.ai_handler.cancellation_token().is_cancelled() {
                                // Agent short-circuit cancel returns Ok("") — show the same
                                // user-facing line as Err(Cancelled). Do not treat as success
                                // (no plan approval / history-as-ok).
                                println!("{}", theme::warning(&t("shell.interrupted")));
                            } else {
                                if !did_stream && !response.trim().is_empty() {
                                    // Non-streaming fallback: print full response with formatting
                                    let mut sep_renderer = ShellRenderer::new();
                                    sep_renderer.set_shared_recorder(self.shared_recorder.clone());
                                    sep_renderer.render_separator();
                                    print_md_with_recording(&response, &self.shared_recorder);
                                    sep_renderer.render_separator();
                                } else if did_stream {
                                    // Streaming display already handled by event callback
                                    // No additional output needed here.
                                }

                                // Print response metadata footer (tokens + context usage).
                                {
                                    let stats = self.ai_handler.session_token_stats();
                                    let delta_in = stats
                                        .total_input
                                        .saturating_sub(token_stats_before.total_input);
                                    let delta_out = stats
                                        .total_output
                                        .saturating_sub(token_stats_before.total_output);
                                    let budget = self.ai_handler.context_budget_state();
                                    let prompt_est = self.ai_handler.last_prompt_estimate();
                                    let ctx_percent = if budget.effective_context_window > 0 {
                                        (prompt_est * 100 / budget.effective_context_window as u64)
                                            .min(100) as u8
                                    } else {
                                        0
                                    };
                                    let compaction = self.ai_handler.last_turn_compaction();
                                    let footer = theme::response_footer(
                                        &self.config.model,
                                        delta_in,
                                        delta_out,
                                        ctx_percent,
                                        compaction,
                                        Some(ai_elapsed),
                                    );
                                    crate::recorder::shared_record_output(
                                        &self.shared_recorder,
                                        &format!("{}\n", footer),
                                    );
                                    println!("{}", footer);
                                    let _ = std::io::stdout().flush();
                                }

                                self.persist_session_snapshot();

                                // Check if plan mode was exited during this AI turn.
                                // If exit_plan_mode tool was called, show plan approval UI.
                                let plan_state = self.ai_handler.plan_state();
                                if plan_state.phase == aish_core::PlanPhase::Normal
                                    && plan_state.plan_id.is_some()
                                {
                                    if let Some(artifact_path) = plan_state.artifact_path.as_ref() {
                                        // Plan was exited — read artifact and present for approval
                                        let artifact_text =
                                            aish_core::plan::read_artifact_text(artifact_path);

                                        // Use the enhanced plan approval flow
                                        use crate::wizard::plan_approval::{
                                            PlanApprovalDecision, PlanApprovalFlow,
                                        };
                                        let decision = PlanApprovalFlow::review_plan(
                                            &artifact_text,
                                            plan_state.summary.as_deref(),
                                            if plan_state.draft_revision > 0 {
                                                Some(plan_state.draft_revision)
                                            } else {
                                                None
                                            },
                                        );

                                        match decision {
                                            PlanApprovalDecision::Approved => {
                                                // Create approved snapshot and transition state
                                                let mut state = self.ai_handler.plan_state();
                                                if let Ok(_snapshot) =
                                                    aish_core::plan::create_approved_snapshot(
                                                        &mut state,
                                                    )
                                                {
                                                    println!(
                                                        "{}",
                                                        theme::success(&t("shell.plan_approved"))
                                                    );
                                                    println!(
                                                        "  {}",
                                                        theme::faint(&t_with_args(
                                                            "shell.plan_approved_hint",
                                                            &std::collections::HashMap::new()
                                                        ))
                                                    );
                                                }
                                            }
                                            PlanApprovalDecision::ChangesRequested { feedback } => {
                                                // Keep in planning phase — re-enter plan mode with feedback
                                                println!(
                                                    "{}",
                                                    theme::warning(&t(
                                                        "shell.plan_changes_requested"
                                                    ))
                                                );

                                                // Re-enter plan mode to let the AI revise
                                                self.ai_handler.enter_plan_mode(&self.session_uuid);

                                                // Set the approval status and feedback directly on the mutex
                                                {
                                                    let plan_state_lock =
                                                        self.ai_handler.plan_state_ptr();
                                                    let mut ps = plan_state_lock.lock().unwrap();
                                                    ps.approval_status =
                                                    aish_core::PlanApprovalStatus::ChangesRequested;
                                                    ps.approval_feedback_summary =
                                                        if feedback.is_empty() {
                                                            None
                                                        } else {
                                                            Some(feedback.clone())
                                                        };
                                                    // Bump revision since we're requesting changes
                                                    aish_core::plan::bump_draft_revision(&mut ps);
                                                    // Preserve the artifact path from the previous plan
                                                    ps.artifact_path =
                                                        plan_state.artifact_path.clone();
                                                    ps.plan_id = plan_state.plan_id.clone();
                                                }

                                                // If feedback was provided, send it back to the AI
                                                // by injecting it as context
                                                if !feedback.is_empty() {
                                                    let feedback_msg = format!(
                                                        "[Plan Review Feedback]\nThe user requested changes to the plan:\n{}\n\nPlease revise the plan accordingly and use exit_plan_mode when ready.",
                                                        feedback
                                                    );
                                                    self.ai_handler
                                                        .add_shell_context(&feedback_msg);
                                                    println!(
                                                        "  {}",
                                                        theme::faint(&t(
                                                            "shell.plan_feedback_sent"
                                                        ))
                                                    );
                                                }
                                            }
                                            PlanApprovalDecision::Cancelled => {
                                                println!(
                                                    "{}",
                                                    theme::warning(&t(
                                                        "shell.plan_review_cancelled"
                                                    ))
                                                );
                                                println!(
                                                    "{}",
                                                    theme::faint(&t("shell.plan_review_hint"))
                                                );
                                            }
                                        }
                                    }
                                }

                                self.record_history(input, 0);
                            }
                        }
                        Err(aish_core::AishError::Cancelled) => {
                            self.animation.stop();
                            println!("{}", theme::warning(&t("shell.interrupted")));
                        }
                        Err(e) => {
                            // Errors are already displayed via the LlmEventType::Error
                            // event callback — avoid printing twice.
                            if !matches!(e, aish_core::AishError::Llm(_)) {
                                let msg = t("shell.error.llm_error_message")
                                    .replace("{error}", &e.to_string());
                                eprintln!("{}", theme::error(&msg));
                            }
                            self.record_history(input, 1);
                        }
                    }
                }
                crate::types::InputIntent::Help => {
                    let result = self.state.handle_builtin("help", &[]);
                    if let Some(output) = result.output {
                        println!("{}", output);
                        if !output.is_empty() {
                            crate::recorder::shared_record_output(
                                &self.shared_recorder,
                                &format!("{}\n", output),
                            );
                        }
                    }
                    self.record_history(input, 0);
                }
                crate::types::InputIntent::BuiltinCommand => {
                    let parts: Vec<&str> = input.split_whitespace().collect();
                    if let Some(cmd) = parts.first() {
                        if *cmd == "setup" {
                            self.run_setup_wizard();
                            self.record_history(input, 0);
                            continue;
                        }
                        let result = self.state.handle_builtin(cmd, &parts[1..]);
                        if let Some(ref output) = result.output {
                            println!("{}", output);
                            if !output.is_empty() {
                                crate::recorder::shared_record_output(
                                    &self.shared_recorder,
                                    &format!("{}\n", output),
                                );
                            }
                        }
                        if result.should_exit {
                            self.record_history(input, 0);
                            break;
                        }
                        // PTY-required commands (su, sudo) — InputGuard
                        // check first, then route directly to PTY.
                        if result.route_to_pty {
                            if !self.screen_shell_command(input) {
                                continue;
                            }
                            if let Some(ref pty_cmd) = result.pty_command {
                                self.set_phase(ShellPhase::Running);
                                let exit_code = self.execute_external_command(pty_cmd);
                                self.set_phase(ShellPhase::Editing);
                                self.record_history(input, exit_code);
                                self.reset_interruption();
                                continue;
                            }
                        }
                        // State-modifying commands (cd, pushd, popd, export,
                        // unset) also need to be sent to the PTY bash process
                        // so that the persistent bash session stays in sync.
                        // Otherwise bash's CWD/env diverges from the Rust
                        // shell's tracking, causing the next external command
                        // to run in the wrong directory/environment.
                        // InputGuard MUST screen here too: bash will execute
                        // any command-substitution payloads embedded in the
                        // arguments (e.g. `export FOO=$(rm -rf /etc)`), so
                        // bypassing this check would let destructive code
                        // slip through unfiltered.
                        if crate::commands::is_state_modifying(cmd)
                            && !crate::commands::is_rejected(cmd)
                        {
                            if !self.screen_shell_command(input) {
                                // Destructive payload blocked — skip the sync
                                // so bash never sees it. State in Rust is
                                // already updated by handle_builtin above,
                                // but the destructive payload in the value
                                // never reaches bash.
                                self.record_history(input, 0);
                                continue;
                            }
                            self.sync_command_to_pty(input);
                        }

                        // Add builtin result to LLM context
                        let builtin_output = result.output.clone().unwrap_or_default();
                        let mut entry = format!(
                            "[Shell] {}\n<returncode>0</returncode>\n<output>{}</output>",
                            input, builtin_output
                        );
                        if crate::commands::is_state_modifying(cmd)
                            && !crate::commands::is_rejected(cmd)
                        {
                            entry.push_str(&format!("\n<cwd>{}</cwd>", self.state.cwd));
                        }
                        self.ai_handler.add_shell_context(&entry);
                    }
                    self.record_history(input, 0);
                }
                crate::types::InputIntent::SpecialCommand => {
                    if self.handle_special_command(input) {
                        self.record_history(input, 0);
                    }
                }
                crate::types::InputIntent::OperatorCommand | crate::types::InputIntent::Command => {
                    // NL detection runs BEFORE shell screening. Otherwise
                    // NL-looking input like "how do I kill a process?" would
                    // hit shell Confirm rules (kill) before the user gets a
                    // chance to route it to AI, where Confirm rules are
                    // intentionally skipped.
                    let nl_verdict = crate::nl_detect::detect(input);
                    if nl_verdict.is_natural_language {
                        let prompt_msg = t("shell.nl_detection.confirm_ask_ai");
                        print!("{} ", prompt_msg);
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        if std::io::stdin().read_line(&mut answer).is_err() {
                            // Fall through to command execution on read error
                        } else {
                            let ans = answer.trim().to_lowercase();
                            if ans != "n" && ans != "no" {
                                // Route to AI
                                let mut question = input.trim().to_string();

                                // Record user input for cast file
                                crate::recorder::shared_record_input(
                                    &self.shared_recorder,
                                    &format!("{}\n", question),
                                );

                                // InputGuard pre-check for NL-routed AI input
                                if !self.screen_ai_prompt(input) {
                                    continue;
                                }

                                // Security gate: same secret check as the normal AI path
                                if !self.check_security_gate(&mut question) {
                                    continue;
                                }

                                let old_sigint = self.install_ai_sigint_handler();
                                let mut esc_watcher = CrosstermEscWatcher::start(
                                    self.ai_handler.cancellation_token_arc(),
                                );
                                let token_ptr = self.ai_handler.cancellation_token()
                                    as *const CancellationToken;
                                let result = runtime.block_on(async {
                                    tokio::select! {
                                        r = self.ai_handler.handle_question(&question) => r,
                                        _ = poll_cancelled(token_ptr) => {
                                            Err(aish_core::AishError::Cancelled)
                                        }
                                    }
                                });
                                esc_watcher.stop();
                                Self::restore_ai_sigint_handler(old_sigint);
                                self.sync_state_from_pty_cwd();

                                let did_stream = self.streamed_content.load(Ordering::SeqCst);
                                match result {
                                    Ok(response) => {
                                        if self.ai_handler.cancellation_token().is_cancelled() {
                                            // Same as Err(Cancelled): do not persist/history as success.
                                            println!("{}", theme::warning(&t("shell.interrupted")));
                                        } else {
                                            if !did_stream && !response.trim().is_empty() {
                                                let mut sep_renderer = ShellRenderer::new();
                                                sep_renderer.set_shared_recorder(
                                                    self.shared_recorder.clone(),
                                                );
                                                sep_renderer.render_separator();
                                                print_md_with_recording(
                                                    &response,
                                                    &self.shared_recorder,
                                                );
                                                sep_renderer.render_separator();
                                            }
                                            self.persist_session_snapshot();
                                            self.record_history(input, 0);
                                        }
                                    }
                                    Err(aish_core::AishError::Cancelled) => {
                                        self.animation.stop();
                                        println!("{}", theme::warning(&t("shell.interrupted")));
                                    }
                                    Err(e) => {
                                        if !matches!(e, aish_core::AishError::Llm(_)) {
                                            let msg = t("shell.error.llm_error_message")
                                                .replace("{error}", &e.to_string());
                                            eprintln!("{}", theme::error(&msg));
                                        }
                                    }
                                }
                                continue;
                            }
                        }
                    }

                    // InputGuard pre-check for the (now confirmed) shell
                    // execution path. NL routing above already ran the AI
                    // variant; we only get here when the input will really
                    // execute as a shell command.
                    if !self.screen_shell_command(input) {
                        continue;
                    }

                    self.set_phase(ShellPhase::Running);
                    let exit_code = self.execute_external_command(input);
                    self.set_phase(ShellPhase::Editing);
                    self.record_history(input, exit_code);
                    self.reset_interruption();

                    self.track_command_failure_state(input, exit_code);
                }
                crate::types::InputIntent::ScriptCall => {
                    let exit_code = self.execute_script(input);
                    self.record_history(input, exit_code);
                    self.track_command_failure_state(input, exit_code);
                }
            }
        }

        // Save history on exit
        rl.save_history(&history_path);
        self.persist_session_snapshot();

        // Auto-stop recording if active
        {
            let mut guard = self
                .shared_recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut rec) = *guard {
                let path = rec.file_path().to_path_buf();
                let _ = rec.flush();
                *guard = None;
                println!(
                    "{}",
                    theme::warning(&t_with_args("shell.record.auto_saved", &{
                        let mut args = std::collections::HashMap::new();
                        args.insert("path".to_string(), path.display().to_string());
                        args
                    }))
                );
            }
        }

        println!(
            "{}",
            t_with_args("shell.resume.exit_command", &{
                let mut args = std::collections::HashMap::new();
                args.insert("session_id".to_string(), self.session_uuid.clone());
                args
            })
        );
        // In daemon mode, remind the user about Ctrl+Q detach + aish -c reconnect.
        if std::env::var("AISH_DAEMON_MODE")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            println!("{}", crate::theme::dim(&t("shell.exit_daemon_hint")));
        }

        drop(rl);
        self.shutdown();

        Ok(())
    }

    /// Echo prompt + submitted command the way rustyline would after Enter.
    fn echo_user_submitted_line(&self, prompt: &str, command: &str) {
        use std::io::Write;
        print!("{}", format_user_submitted_line(prompt, command));
        println!();
        let _ = std::io::stdout().flush();
        crate::recorder::shared_record_input(&self.shared_recorder, &format!("{command}\n"));
    }

    /// Execute a slash command selected from the popup (echo, history, handler).
    fn apply_slash_popup_command(&mut self, rl: &mut ShellReadline, prompt: &str, cmd: &str) {
        let input = cmd.trim();
        if input.is_empty() {
            return;
        }
        self.echo_user_submitted_line(prompt, input);
        rl.add_history_entry(input);
        self.state.history.push(input.to_string());
        if self.handle_special_command(input) {
            self.record_history(input, 0);
        }
    }

    /// Record Tab-dismissed popup text for cast replay (e.g. `/model ` after completion).
    fn record_slash_popup_dismissed(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        crate::recorder::shared_record_input(&self.shared_recorder, &format!("{text}\n"));
    }

    /// After slash popup dismisses into readline, read/edit/submit (supports nested `/`).
    /// Returns `true` when the main shell loop should exit.
    fn read_line_after_slash_dismiss(
        &mut self,
        rl: &mut ShellReadline,
        prompt: &str,
        initial: &str,
    ) -> bool {
        self.record_slash_popup_dismissed(initial);
        let mut prefill = initial.to_string();
        loop {
            match rl.read_line_with_initial(prompt, (&prefill, "")) {
                Ok(Some(line)) => return self.process_readline_submission(&line),
                Ok(None) => return false,
                Err(ref e) if matches!(e, rustyline::error::ReadlineError::Interrupted) => {
                    if rl.was_slash_requested() {
                        match self.run_slash_input_session(prompt) {
                            aish_ui::SlashInputOutcome::Command(cmd) => {
                                self.apply_slash_popup_command(rl, prompt, &cmd);
                                return false;
                            }
                            aish_ui::SlashInputOutcome::Dismissed(text) => {
                                self.record_slash_popup_dismissed(&text);
                                prefill = text;
                            }
                            aish_ui::SlashInputOutcome::Cancelled => return false,
                        }
                    } else {
                        crate::recorder::shared_record_output(&self.shared_recorder, "\r\n");
                        return self.handle_ctrl_c();
                    }
                }
                Err(_) => return false,
            }
        }
    }

    /// After the file-mention popup closes, re-enter readline with the
    /// spliced text pre-filled. Supports nested `@` and `/` popups.
    /// Returns `true` when the main shell loop should exit.
    fn read_line_after_at_file(
        &mut self,
        rl: &mut ShellReadline,
        prompt: &str,
        initial: &str,
    ) -> bool {
        let mut prefill = initial.to_string();
        loop {
            match rl.read_line_with_initial(prompt, (&prefill, "")) {
                Ok(Some(line)) => return self.process_readline_submission(&line),
                Ok(None) => return false,
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    if rl.was_at_file_requested() {
                        if let Some((line2, at_pos)) = crate::readline::take_at_file_context() {
                            match self.run_file_mention_session(prompt, &line2, at_pos) {
                                aish_ui::FileMentionOutcome::Selected(path) => {
                                    let before = &line2[..at_pos];
                                    let after = &line2[at_pos..];
                                    prefill = format!("{before}@{path} {after}");
                                }
                                aish_ui::FileMentionOutcome::Cancelled => {
                                    prefill = line2;
                                }
                            }
                        }
                    } else if rl.was_slash_requested() {
                        match self.run_slash_input_session(prompt) {
                            aish_ui::SlashInputOutcome::Command(cmd) => {
                                self.apply_slash_popup_command(rl, prompt, &cmd);
                                return false;
                            }
                            aish_ui::SlashInputOutcome::Dismissed(text) => {
                                prefill = text;
                            }
                            aish_ui::SlashInputOutcome::Cancelled => return false,
                        }
                    } else {
                        crate::recorder::shared_record_output(&self.shared_recorder, "\r\n");
                        return self.handle_ctrl_c();
                    }
                }
                Err(_) => return false,
            }
        }
    }

    /// Record last command outcome for `;` quick-fix and `/diagnose`.
    fn track_command_failure_state(&mut self, command: &str, exit_code: i32) {
        use aish_i18n::t;
        self.state.last_command = Some(command.to_string());
        self.state.last_exit_code = exit_code;
        self.state.can_correct_error = exit_code != 0 && exit_code != 130;
        if exit_code != 0 && exit_code != 130 {
            let hint = t("shell.error_correction.press_semicolon_hint");
            let hint_str = format!("{}\n", theme::muted(&format!("<{}>", hint)));
            crate::recorder::shared_record_output(&self.shared_recorder, &hint_str);
            eprintln!("{}", theme::muted(&format!("<{}>", hint)));
        }
    }

    /// Classify and run a readline submission. Returns `true` to break the main loop.
    fn process_readline_submission(&mut self, line: &str) -> bool {
        let input = line.trim();
        if input.is_empty() {
            return false;
        }
        self.state.history.push(input.to_string());
        match input::classify_input(input) {
            crate::types::InputIntent::SpecialCommand => {
                if self.handle_special_command(input) {
                    self.record_history(input, 0);
                }
                false
            }
            crate::types::InputIntent::Command | crate::types::InputIntent::OperatorCommand => {
                // InputGuard: slash-popup-dismissed submissions must
                // clear the same gate as main-loop submissions.
                if !self.screen_shell_command(input) {
                    return false;
                }
                self.set_phase(ShellPhase::Running);
                let exit_code = self.execute_external_command(input);
                self.set_phase(ShellPhase::Editing);
                self.record_history(input, exit_code);
                self.reset_interruption();
                self.track_command_failure_state(input, exit_code);
                false
            }
            crate::types::InputIntent::BuiltinCommand => {
                let parts: Vec<&str> = input.split_whitespace().collect();
                if let Some(cmd) = parts.first() {
                    if *cmd == "setup" {
                        self.run_setup_wizard();
                        self.record_history(input, 0);
                        return false;
                    }
                    let result = self.state.handle_builtin(cmd, &parts[1..]);
                    if let Some(ref output) = result.output {
                        println!("{}", output);
                    }
                    if result.should_exit {
                        self.record_history(input, 0);
                        return true;
                    }
                }
                self.record_history(input, 0);
                false
            }
            crate::types::InputIntent::Help => {
                let result = self.state.handle_builtin("help", &[]);
                if let Some(output) = result.output {
                    println!("{}", output);
                }
                self.record_history(input, 0);
                false
            }
            crate::types::InputIntent::ScriptCall => {
                // InputGuard: scripts run shell commands internally; gate
                // the call the same way as a direct shell command.
                if !self.screen_shell_command(input) {
                    return false;
                }
                let exit_code = self.execute_script(input);
                self.record_history(input, exit_code);
                self.track_command_failure_state(input, exit_code);
                false
            }
            crate::types::InputIntent::Ai => {
                // Popup-recovery AI path: the line was assembled inside a
                // slash/@ popup and submitted via `read_line_with_initial`,
                // so it does not flow through the main loop's Ai branch.
                // Mirror that branch's essentials (InputGuard, security
                // gate, handle_question, streaming/non-streaming render,
                // cancellation, history) so `@file` mentions reach the LLM.
                let mut question = input::extract_ai_question(input);
                if question.is_empty() {
                    return false;
                }
                if !self.screen_ai_prompt(&question) {
                    return false;
                }
                if !self.check_security_gate(&mut question) {
                    return false;
                }
                let ai_prefix = if input.starts_with('\u{ff1b}') {
                    "\u{ff1b}"
                } else {
                    ";"
                };
                crate::recorder::shared_record_input(
                    &self.shared_recorder,
                    &format!("{}{}\n", ai_prefix, question),
                );
                let old_sigint = self.install_ai_sigint_handler();
                let mut esc_watcher =
                    CrosstermEscWatcher::start(self.ai_handler.cancellation_token_arc());
                let token_ptr = self.ai_handler.cancellation_token() as *const CancellationToken;
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(err) => {
                        esc_watcher.stop();
                        Self::restore_ai_sigint_handler(old_sigint);
                        eprintln!("{}", theme::error(&format!("runtime error: {err}")));
                        return false;
                    }
                };
                let result = runtime.block_on(async {
                    tokio::select! {
                        r = self.ai_handler.handle_question(&question) => r,
                        _ = poll_cancelled(token_ptr) => {
                            Err(aish_core::AishError::Cancelled)
                        }
                    }
                });
                esc_watcher.stop();
                Self::restore_ai_sigint_handler(old_sigint);
                self.sync_state_from_pty_cwd();
                let did_stream = self.streamed_content.load(Ordering::SeqCst);
                match result {
                    Ok(response) => {
                        if self.ai_handler.cancellation_token().is_cancelled() {
                            println!("{}", theme::warning(&t("shell.interrupted")));
                        } else if !did_stream && !response.trim().is_empty() {
                            let mut sep = ShellRenderer::new();
                            sep.set_shared_recorder(self.shared_recorder.clone());
                            sep.render_separator();
                            print_md_with_recording(&response, &self.shared_recorder);
                            sep.render_separator();
                        }
                    }
                    Err(aish_core::AishError::Cancelled) => {
                        println!("{}", theme::warning(&t("shell.interrupted")));
                    }
                    Err(e) => {
                        if !matches!(e, aish_core::AishError::Llm(_)) {
                            let msg = t("shell.error.llm_error_message")
                                .replace("{error}", &e.to_string());
                            eprintln!("{}", theme::error(&msg));
                        }
                    }
                }
                self.record_history(input, 0);
                false
            }
            _ => {
                eprintln!("Unknown: {}", input);
                false
            }
        }
    }

    fn record_history(&self, command: &str, returncode: i32) {
        self.record_history_sourced(command, "user", returncode);
    }

    fn record_history_sourced(&self, command: &str, source: &str, returncode: i32) {
        if let Some(ref store) = self.session_store {
            let now = chrono::Utc::now();
            let _ = store.add_history_entry(&aish_session::HistoryEntry {
                id: None,
                session_uuid: self.session_uuid.clone(),
                command: command.to_string(),
                source: source.to_string(),
                returncode: Some(returncode),
                stdout: None,
                stderr: None,
                created_at: now,
            });
            let snapshot = self.session_state_snapshot(now);
            let _ = store.update_session_state(&self.session_uuid, &snapshot);
        }

        if let Some(ref audit) = self.audit_store {
            if self.security_manager.policy().audit_include_commands {
                let redacted = aish_security::secret::redact_secrets(
                    command,
                    self.security_manager.secret_scanner(),
                );
                let remote = self
                    .current_remote_host
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let host = remote
                    .as_deref()
                    .map(|h| h.split_once('@').map_or(h, |(_, host)| host).to_string())
                    .or_else(|| self.audit_host.clone());
                let user = remote
                    .as_deref()
                    .and_then(|h| h.split_once('@').map(|(u, _)| u.to_string()))
                    .or_else(|| self.audit_user.clone());
                audit.record(aish_core::AuditEvent::command(
                    chrono::Utc::now(),
                    Some(self.session_uuid.clone()),
                    user,
                    host,
                    redacted,
                    source.to_string(),
                    returncode,
                ));
            }
        }
    }

    fn persist_session_snapshot(&self) {
        if let Some(ref store) = self.session_store {
            let snapshot = self.session_state_snapshot(chrono::Utc::now());
            let _ = store.update_session_state(&self.session_uuid, &snapshot);
        }
    }

    fn session_state_snapshot(
        &self,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> SessionStateSnapshot {
        let context_messages = self.ai_handler.export_session_context_snapshot();
        SessionStateSnapshot {
            cwd: Some(self.state.cwd.clone()),
            summary_preview: summary_preview_from_context(&context_messages),
            context_messages_snapshot: context_messages,
            updated_at: Some(updated_at),
        }
    }

    /// Handle special slash commands (/model, /setup, /plan, etc.).
    fn handle_special_command(&mut self, input: &str) -> bool {
        let parts: Vec<&str> = input.split_whitespace().collect();
        match parts.first().copied() {
            Some("/help") => {
                let args: Vec<&str> = if parts.len() > 1 {
                    parts[1..].to_vec()
                } else {
                    vec![]
                };
                let result = self.state.handle_builtin("help", &args);
                if let Some(output) = result.output {
                    println!("{}", output);
                }
            }
            Some("/quit") => {
                self.state.should_exit = true;
                return false;
            }
            Some("/model") => self.handle_model_command(&parts),
            Some("/setting") => self.handle_setting_command(),
            Some("/setup") => self.run_setup_wizard(),
            Some("/plan") => self.handle_plan_command(&parts),
            Some("/token") => self.handle_token_command(),
            Some("/resume") => {
                self.handle_resume_command(&parts);
                return false;
            }
            Some("/feedback") => {
                crate::feedback::run_feedback(&self.config.model, &self.config.api_base);
            }
            Some("/record") => self.handle_record_command(&parts),
            Some("/doctor") => self.handle_doctor_command(&parts),
            Some("/diagnose") => self.handle_failure_diagnose_command(),
            Some("/status") => self.handle_status_command(),
            Some("/live_sessions") => self.handle_live_sessions_command(),
            Some("/kill_live_sessions") => self.handle_kill_live_sessions_command(&parts),
            Some("/audit") => self.handle_audit_command(&parts),
            Some("/forget-approvals") => self.handle_forget_approvals(),
            Some("/usage") => self.handle_usage_command(),
            Some("/accounts") => self.handle_accounts_command(&parts),
            Some("/fallback") => self.handle_fallback_command(&parts),
            Some("/fork") => self.handle_fork_command(),
            Some("/sessions") => self.handle_sessions_command(),
            Some("/export") => self.handle_export_command(&parts),
            _ => {
                eprintln!("{}", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("command".to_string(), input.to_string());
                    t_with_args("shell.unknown_command", &args)
                });
            }
        }
        true
    }

    /// Handle `/forget-approvals` — clear all session-scoped command approvals
    /// so previously "remembered" commands prompt for confirmation again.
    fn handle_forget_approvals(&mut self) {
        let count = {
            let mut memory = self.approval_memory.lock().unwrap();
            let n = memory.len();
            memory.clear();
            n
        };
        let mut args = std::collections::HashMap::new();
        args.insert("count".to_string(), count.to_string());
        println!(
            "\x1b[36m{}\x1b[0m",
            t_with_args("shell.forget_approvals_cleared", &args)
        );
    }

    /// `/fallback` — manage the ordered model fallback chain tried when the
    /// primary model hits a rate/usage limit or a hard error.
    fn handle_fallback_command(&mut self, parts: &[&str]) {
        // Direct subcommand (power-user) → dispatch.
        if parts.len() > 1 {
            return self.fallback_dispatch(parts);
        }
        // No args → interactive menu.
        let opts: Vec<(String, String, String)> = vec![
            ("list".to_string(), t("shell.menu.fallback.list"), t("shell.menu.fallback.list_desc")),
            ("add".to_string(), t("shell.menu.fallback.add"), t("shell.menu.fallback.add_desc")),
            ("remove".to_string(), t("shell.menu.fallback.remove"), t("shell.menu.fallback.remove_desc")),
            ("clear".to_string(), t("shell.menu.fallback.clear"), t("shell.menu.fallback.clear_desc")),
            ("revert".to_string(), t("shell.menu.fallback.revert"), t("shell.menu.fallback.revert_desc")),
        ];
        let action = match pick_menu(&t("shell.menu.fallback.title"), &opts) {
            Some(a) => a,
            None => return,
        };
        match action.as_str() {
            "list" => self.fallback_dispatch(&["/fallback", "list"]),
            "add" => self.fallback_add_interactive(),
            "remove" => self.fallback_remove_interactive(),
            "clear" => self.fallback_dispatch(&["/fallback", "clear"]),
            "revert" => self.fallback_revert_interactive(),
            _ => {}
        }
    }

    /// Direct subcommand dispatch for `/fallback` (power-user path).
    fn fallback_dispatch(&mut self, parts: &[&str]) {
        let sub = parts.get(1).copied().unwrap_or("list");
        match sub {
            "list" => {
                println!();
                println!("\x1b[1mFallback model chain\x1b[0m");
                println!("  primary : {}", self.config.model);
                if self.config.fallback_models.is_empty() {
                    println!("  fallback: (none configured)");
                } else {
                    for (i, m) in self.config.fallback_models.iter().enumerate() {
                        println!("  [{}]     {}", i + 1, m);
                    }
                }
                let pol = if self.config.fallback_revert_on_cooldown {
                    "auto-revert to primary on cooldown"
                } else {
                    "stay on fallback until manual switch"
                };
                println!("  policy  : {}", pol);
                println!();
            }
            "add" => {
                let model = match parts.get(2) {
                    Some(m) if !m.is_empty() => m.to_string(),
                    _ => {
                        eprintln!("usage: /fallback add <model>");
                        return;
                    }
                };
                if self.config.fallback_models.iter().any(|m| m == &model) {
                    eprintln!("'{}' is already in the fallback chain", model);
                    return;
                }
                self.config.fallback_models.push(model.clone());
                self.persist_config_and_rebuild_rotation();
                println!("\x1b[32madded fallback '{}'\x1b[0m", model);
            }
            "remove" | "rm" => {
                let model = match parts.get(2) {
                    Some(m) => m.to_string(),
                    None => {
                        eprintln!("usage: /fallback remove <model>");
                        return;
                    }
                };
                let before = self.config.fallback_models.len();
                self.config.fallback_models.retain(|m| m != &model);
                if self.config.fallback_models.len() == before {
                    eprintln!("'{}' not in the fallback chain", model);
                    return;
                }
                self.persist_config_and_rebuild_rotation();
                println!("\x1b[32mremoved fallback '{}'\x1b[0m", model);
            }
            "clear" => {
                self.config.fallback_models.clear();
                self.persist_config_and_rebuild_rotation();
                println!("\x1b[32mcleared fallback chain\x1b[0m");
            }
            "revert" => {
                let val = match parts.get(2).copied().unwrap_or("") {
                    "on" => true,
                    "off" => false,
                    _ => {
                        eprintln!("usage: /fallback revert on|off");
                        return;
                    }
                };
                self.config.fallback_revert_on_cooldown = val;
                self.persist_config_and_rebuild_rotation();
                println!(
                    "\x1b[32mrevert policy: {}\x1b[0m",
                    if val { "on (auto-revert)" } else { "off (stay)" }
                );
            }
            "help" | "-h" | "--help" => {
                println!("/fallback                  list the fallback model chain");
                println!("/fallback add <model>      append a fallback model");
                println!("/fallback remove <model>   remove a fallback model");
                println!("/fallback clear            empty the chain");
                println!("/fallback revert on|off    auto-revert to primary on cooldown");
                println!("(no args opens an interactive menu)");
            }
            other => eprintln!("unknown subcommand '{}'. try /fallback help", other),
        }
    }

    fn fallback_add_interactive(&mut self) {
        let model = match prompt_edit_value(
            "Fallback model",
            "model name tried when the primary fails (e.g. gpt-4o-mini)",
            "",
            false,
        ) {
            Some(m) if !m.trim().is_empty() => m.trim().to_string(),
            _ => {
                println!("cancelled");
                return;
            }
        };
        if self.config.fallback_models.iter().any(|m| m == &model) {
            eprintln!("'{}' is already in the fallback chain", model);
            return;
        }
        self.config.fallback_models.push(model.clone());
        self.persist_config_and_rebuild_rotation();
        println!("\x1b[32madded fallback '{}'\x1b[0m", model);
    }

    fn fallback_remove_interactive(&mut self) {
        if self.config.fallback_models.is_empty() {
            eprintln!("fallback chain is empty");
            return;
        }
        let models: Vec<String> = self.config.fallback_models.clone();
        let opts: Vec<(String, String, String)> =
            models.iter().map(|m| (m.clone(), m.clone(), String::new())).collect();
        let model = match pick_menu(&t("shell.menu.fallback.remove_title"), &opts) {
            Some(m) => m,
            None => return,
        };
        self.config.fallback_models.retain(|m| m != &model);
        self.persist_config_and_rebuild_rotation();
        println!("\x1b[32mremoved fallback '{}'\x1b[0m", model);
    }

    fn fallback_revert_interactive(&mut self) {
        let cur = self.config.fallback_revert_on_cooldown;
        let opts: Vec<(String, String, String)> = vec![
            ("on".to_string(), t("shell.menu.fallback.on"), t("shell.menu.fallback.on_desc")),
            ("off".to_string(), t("shell.menu.fallback.off"), t("shell.menu.fallback.off_desc")),
        ];
        let action = match pick_menu(&t("shell.menu.fallback.revert_title"), &opts) {
            Some(a) => a,
            None => return,
        };
        let val = action == "on";
        if val == cur {
            println!("revert policy already '{}'", action);
            return;
        }
        self.config.fallback_revert_on_cooldown = val;
        self.persist_config_and_rebuild_rotation();
        println!("\x1b[32mrevert policy: {}\x1b[0m", action);
    }

    /// `/fork` — branch the current session into a new one (copied context),
    /// then switch into the fork so the original is preserved at its branch point.
    fn handle_fork_command(&mut self) {
        // Persist the current context first so the fork copies the latest state.
        self.persist_session_snapshot();
        let parent_uuid = self.session_uuid.clone();
        let new_uuid = uuid::Uuid::new_v4().to_string();

        // fork_session borrows the store immutably; keep that borrow in a block so
        // the later `&mut self` resume call is not blocked by the live reference.
        let fork_result = {
            let Some(store) = self.session_store.as_ref() else {
                eprintln!("session store unavailable; cannot fork");
                return;
            };
            store.fork_session(&parent_uuid, None, &new_uuid)
        };

        let new_short = &new_uuid[..8.min(new_uuid.len())];
        let parent_short = &parent_uuid[..8.min(parent_uuid.len())];
        match fork_result {
            Ok(_) => {
                println!(
                    "\x1b[32mforked session\x1b[0m {} (from {})",
                    new_short, parent_short
                );
                if let Err(e) = self.resume_session_with_options(&new_uuid, false, true) {
                    eprintln!("fork created but switch failed: {e}");
                    eprintln!("use `/resume {}` to switch manually", new_short);
                }
            }
            Err(e) => eprintln!("fork failed: {e}"),
        }
    }

    /// `/sessions` — browse the session tree in an interactive panel (instead of
    /// flooding the screen) and switch to the chosen session on submit.
    fn handle_sessions_command(&mut self) {
        let Some(store) = self.session_store.as_ref() else {
            eprintln!("session store unavailable");
            return;
        };
        let roots = match store.session_roots() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed to list sessions: {e}");
                return;
            }
        };
        if roots.is_empty() {
            println!("(no saved sessions)");
            return;
        }

        // Flatten roots + direct children into panel rows with depth indentation
        // so the hierarchy stays visible without dumping every node to stdout.
        let mut items: Vec<aish_ui::SearchSelectItem> = Vec::new();
        for root in &roots {
            items.push(self.session_tree_item(root, 0));
            if let Ok(children) = store.list_children(&root.session_uuid) {
                for child in &children {
                    items.push(self.session_tree_item(child, 1));
                }
            }
        }

        let panel = aish_ui::SearchSelectPanel::new(
            aish_i18n::t("shell.resume.selector_title"),
            aish_i18n::t("shell.resume.search_placeholder"),
            items,
        );
        if let Ok(aish_ui::PanelOutcome::Submitted(
            aish_ui::SearchSelectOutcome::Selected(id),
        )) = aish_ui::PanelRuntime::new().run(panel)
        {
            if id != self.session_uuid {
                if let Err(e) = self.resume_session_with_options(&id, false, true) {
                    eprintln!("failed to switch session: {e}");
                }
            }
        }
    }

    /// Build one panel row for a session node, indenting forks beneath roots.
    fn session_tree_item(
        &self,
        session: &aish_session::SessionRecord,
        depth: usize,
    ) -> aish_ui::SearchSelectItem {
        let indent = if depth > 0 {
            format!("{}└ ", "  ".repeat(depth))
        } else {
            String::new()
        };
        let short = &session.session_uuid[..8.min(session.session_uuid.len())];
        let when = session.created_at.format("%m-%d %H:%M");
        let cur = if session.session_uuid == self.session_uuid {
            " *"
        } else {
            ""
        };
        let label = format!("{}{} {} {}{}", indent, short, session.model, when, cur);
        let snap = session.state_snapshot();
        let preview: String = snap
            .summary_preview
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        let search = format!("{} {} {}", short, session.model, preview);
        let mut item =
            aish_ui::SearchSelectItem::new(session.session_uuid.clone(), label).with_search_text(search);
        if !preview.is_empty() {
            item = item.with_detail(preview);
        }
        item
    }

    /// `/export [md]` — export the current session (AI conversation + command
    /// history) to a Markdown file for postmortem or sharing.
    fn handle_export_command(&self, parts: &[&str]) {
        let format = parts.get(1).copied().unwrap_or("md");
        if format != "md" && format != "markdown" {
            eprintln!("usage: /export [md]  (only markdown is supported)");
            return;
        }
        let Some(store) = self.session_store.as_ref() else {
            eprintln!("session store unavailable; cannot export");
            return;
        };
        let uuid = &self.session_uuid;
        let record = match store.get_session(uuid) {
            Ok(Some(r)) => r,
            Ok(None) => {
                eprintln!("current session not found in store");
                return;
            }
            Err(e) => {
                eprintln!("failed to load session: {e}");
                return;
            }
        };
        let history = store.get_history(uuid, 10000).unwrap_or_default();
        let snap = record.state_snapshot();

        let mut md = String::new();
        md.push_str("# aish session export\n\n");
        md.push_str(&format!("- **session**: `{}`\n", uuid));
        md.push_str(&format!("- **model**: {}\n", record.model));
        if let Some(base) = &record.api_base {
            md.push_str(&format!("- **api_base**: {}\n", base));
        }
        if let Some(parent) = &record.parent_session_uuid {
            md.push_str(&format!("- **forked from**: `{}`\n", parent));
        }
        md.push_str(&format!(
            "- **created**: {}\n",
            record.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        ));
        if let Some(cwd) = &snap.cwd {
            md.push_str(&format!("\n**working dir**: `{}`\n", cwd));
        }

        md.push_str("\n## Conversation\n\n");
        for msg in &snap.context_messages_snapshot {
            let role = match msg.role.as_str() {
                "user" => "User",
                "assistant" => "Assistant",
                "system" => "System",
                other => other,
            };
            md.push_str(&format!("**{}**\n\n{}\n\n", role, msg.content));
        }

        md.push_str("\n## Command history\n\n");
        md.push_str("| # | source | rc | command |\n|---|---|---|---|\n");
        for (i, entry) in history.iter().enumerate() {
            let cmd = entry.command.replace('|', "\\|").replace('\n', " ");
            let rc = entry
                .returncode
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into());
            md.push_str(&format!("| {} | {} | {} | `{}` |\n", i + 1, entry.source, rc, cmd));
        }

        let fname = format!("aish-session-{}.md", &uuid[..8.min(uuid.len())]);
        match std::fs::write(&fname, &md) {
            Ok(_) => println!(
                "\x1b[32mexported\x1b[0m {} ({} messages, {} commands)",
                fname,
                snap.context_messages_snapshot.len(),
                history.len()
            ),
            Err(e) => eprintln!("failed to write {}: {e}", fname),
        }
    }

    /// `/usage` — show multi-account quota rotation + model fallback status.
    fn handle_usage_command(&self) {
        match self.ai_handler.rotation_snapshot() {
            Some(snap) => {
                println!();
                println!("\x1b[1mAPI Quota Rotation & Fallback\x1b[0m");
                println!();
                println!("  primary model : {}", snap.primary_model);
                let fb = if snap.on_fallback {
                    "  \x1b[33m(fallback active)\x1b[0m"
                } else {
                    ""
                };
                println!("  active model  : {}{}", snap.active_model, fb);
                if !snap.fallback_models.is_empty() {
                    println!("  fallback chain: {}", snap.fallback_models.join(" -> "));
                }
                println!();
                println!(
                    "  accounts ({} total, {} available):",
                    snap.account_names.len(),
                    snap.available_accounts
                );
                for name in &snap.account_names {
                    let is_cur = snap.current_account.as_deref() == Some(name.as_str());
                    let is_cool = snap.cooled_accounts.iter().any(|c| c == name);
                    let (mark, state) = if is_cur {
                        ("\x1b[32m*\x1b[0m", "active")
                    } else if is_cool {
                        ("\x1b[31mx\x1b[0m", "cooling down")
                    } else {
                        ("o", "ready")
                    };
                    println!("    {} {:<16} {}", mark, name, state);
                }
                println!();
                println!("  rotations this session: {}", snap.total_rotations);
                println!();
            }
            None => {
                println!();
                println!("\x1b[33mMulti-account rotation is not active.\x1b[0m");
                println!("Add extra API keys with /accounts, or set fallback_models in config.yaml.");
                println!();
            }
        }
    }

    /// `/accounts` — manage multi-key quota rotation accounts at runtime.
    fn handle_accounts_command(&mut self, parts: &[&str]) {
        // Direct subcommand (power-user) → dispatch.
        if parts.len() > 1 {
            return self.accounts_dispatch(parts);
        }
        // No args → interactive menu.
        let opts: Vec<(String, String, String)> = vec![
            ("list".to_string(), t("shell.menu.accounts.list"), t("shell.menu.accounts.list_desc")),
            ("add".to_string(), t("shell.menu.accounts.add"), t("shell.menu.accounts.add_desc")),
            ("remove".to_string(), t("shell.menu.accounts.remove"), t("shell.menu.accounts.remove_desc")),
            ("toggle".to_string(), t("shell.menu.accounts.toggle"), t("shell.menu.accounts.toggle_desc")),
        ];
        let action = match pick_menu(&t("shell.menu.accounts.title"), &opts) {
            Some(a) => a,
            None => return,
        };
        match action.as_str() {
            "list" => self.accounts_list(),
            "add" => self.accounts_add_interactive(),
            "remove" => self.accounts_remove_interactive(),
            "toggle" => self.accounts_toggle_interactive(),
            _ => {}
        }
    }

    /// Direct subcommand dispatch for `/accounts` (power-user path).
    fn accounts_dispatch(&mut self, parts: &[&str]) {
        let sub = parts.get(1).copied().unwrap_or("list");
        match sub {
            "list" => self.accounts_list(),
            "add" => {
                let name = match parts.get(2) {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => {
                        eprintln!("usage: /accounts add <name> <api_key> [api_base]");
                        return;
                    }
                };
                let key = match parts.get(3) {
                    Some(k) if !k.is_empty() => k.to_string(),
                    _ => {
                        eprintln!("usage: /accounts add <name> <api_key> [api_base]");
                        return;
                    }
                };
                let base = parts.get(4).filter(|s| !s.is_empty()).map(|s| s.to_string());
                if self.config.api_accounts.iter().any(|a| a.name == name) {
                    eprintln!("account '{}' already exists", name);
                    return;
                }
                self.config.api_accounts.push(aish_config::ApiAccountConfig {
                    name: name.clone(),
                    api_key: key,
                    api_base: base,
                    weight: 1,
                    disabled: false,
                });
                self.persist_config_and_rebuild_rotation();
                println!(
                    "\x1b[32madded account '{}'\x1b[0m ({} extra account{})",
                    name,
                    self.config.api_accounts.len(),
                    if self.config.api_accounts.len() == 1 { "" } else { "s" }
                );
            }
            "remove" | "rm" | "delete" => {
                let name = match parts.get(2) {
                    Some(n) => n.to_string(),
                    None => {
                        eprintln!("usage: /accounts remove <name>");
                        return;
                    }
                };
                let before = self.config.api_accounts.len();
                self.config.api_accounts.retain(|a| a.name != name);
                if self.config.api_accounts.len() == before {
                    eprintln!("no account named '{}'", name);
                    return;
                }
                self.persist_config_and_rebuild_rotation();
                println!("\x1b[32mremoved account '{}'\x1b[0m", name);
            }
            "enable" | "disable" => {
                let disabled = sub == "disable";
                let name = match parts.get(2) {
                    Some(n) => n.to_string(),
                    None => {
                        eprintln!("usage: /accounts {} <name>", sub);
                        return;
                    }
                };
                match self.config.api_accounts.iter_mut().find(|a| a.name == name) {
                    Some(a) => {
                        a.disabled = disabled;
                        self.persist_config_and_rebuild_rotation();
                        println!(
                            "\x1b[32m{} account '{}'\x1b[0m",
                            if disabled { "disabled" } else { "enabled" },
                            name
                        );
                    }
                    None => eprintln!("no account named '{}'", name),
                }
            }
            "help" | "-h" | "--help" => {
                println!("/accounts                       list rotation accounts");
                println!("/accounts add <name> <key> [base]");
                println!("/accounts remove <name>");
                println!("/accounts enable|disable <name>");
                println!("(no args opens an interactive menu)");
            }
            other => eprintln!("unknown subcommand '{}'. try /accounts help", other),
        }
    }

    fn accounts_add_interactive(&mut self) {
        let name = match prompt_edit_value(
            "Account name",
            "short label, e.g. 'team-b'",
            "",
            false,
        ) {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => {
                println!("cancelled");
                return;
            }
        };
        let key = match prompt_edit_value("API key", "the key for this account", "", true) {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => {
                println!("cancelled");
                return;
            }
        };
        let base = prompt_edit_value(
            "API base (optional)",
            "override endpoint; blank = same as primary",
            "",
            false,
        )
        .filter(|s| !s.trim().is_empty());
        if self.config.api_accounts.iter().any(|a| a.name == name) {
            eprintln!("account '{}' already exists", name);
            return;
        }
        self.config.api_accounts.push(aish_config::ApiAccountConfig {
            name: name.clone(),
            api_key: key,
            api_base: base,
            weight: 1,
            disabled: false,
        });
        self.persist_config_and_rebuild_rotation();
        println!(
            "\x1b[32madded account '{}'\x1b[0m ({} extra account{})",
            name,
            self.config.api_accounts.len(),
            if self.config.api_accounts.len() == 1 { "" } else { "s" }
        );
    }

    fn accounts_remove_interactive(&mut self) {
        if self.config.api_accounts.is_empty() {
            eprintln!("no extra accounts to remove");
            return;
        }
        let names: Vec<String> = self.config.api_accounts.iter().map(|a| a.name.clone()).collect();
        let opts: Vec<(String, String, String)> =
            names.iter().map(|n| (n.clone(), n.clone(), String::new())).collect();
        let name = match pick_menu(&t("shell.menu.accounts.remove_title"), &opts) {
            Some(n) => n,
            None => return,
        };
        self.config.api_accounts.retain(|a| a.name != name);
        self.persist_config_and_rebuild_rotation();
        println!("\x1b[32mremoved account '{}'\x1b[0m", name);
    }

    fn accounts_toggle_interactive(&mut self) {
        if self.config.api_accounts.is_empty() {
            eprintln!("no extra accounts to toggle");
            return;
        }
        let names: Vec<String> = self.config.api_accounts.iter().map(|a| a.name.clone()).collect();
        let opts: Vec<(String, String, String)> =
            names.iter().map(|n| (n.clone(), n.clone(), String::new())).collect();
        let name = match pick_menu(&t("shell.menu.accounts.toggle_title"), &opts) {
            Some(n) => n,
            None => return,
        };
        if let Some(a) = self.config.api_accounts.iter_mut().find(|a| a.name == name) {
            a.disabled = !a.disabled;
            let now = a.disabled;
            self.persist_config_and_rebuild_rotation();
            println!(
                "\x1b[32m{} account '{}'\x1b[0m",
                if now { "disabled" } else { "enabled" },
                name
            );
        }
    }

    fn accounts_list(&self) {
        println!();
        println!("\x1b[1mRotation accounts\x1b[0m");
        let primary_preview: String = self.config.api_key.chars().take(6).collect();
        println!(
            "  [0] primary   key={}...  {}",
            primary_preview,
            if self.config.api_key.trim().is_empty() {
                "(unset)"
            } else {
                ""
            }
        );
        for (i, a) in self.config.api_accounts.iter().enumerate() {
            let preview: String = a.api_key.chars().take(6).collect();
            let state = if a.disabled { "disabled" } else { "enabled" };
            println!(
                "  [{}] {:<10} key={}...  {}  {}",
                i + 1,
                a.name,
                preview,
                a.api_base.as_deref().unwrap_or(""),
                state
            );
        }
        if self.config.api_accounts.is_empty() {
            println!("  (no extra accounts; add one with /accounts add <name> <key>)");
        }
        println!();
    }

    /// Persist the current config to disk, then rebuild the live rotation state.
    fn persist_config_and_rebuild_rotation(&mut self) {
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Err(e) = aish_config::ConfigLoader::save(&self.config, &config_path) {
            eprintln!("warning: failed to save config: {e}");
        }
        self.rebuild_rotation();
    }

    /// Rebuild the LLM session rotation state from the current config.
    fn rebuild_rotation(&mut self) {
        let accounts = rotation_accounts_from_config(&self.config);
        let state = if !self.config.fallback_models.is_empty() || accounts.len() > 1 {
            let mut policy = aish_llm::RetryPolicy::default();
            policy.revert_on_cooldown = self.config.fallback_revert_on_cooldown;
            let s = aish_llm::RotationState::new(
                self.config.model.clone(),
                accounts,
                self.config.fallback_models.clone(),
                policy,
            );
            if s.is_active() {
                Some(s)
            } else {
                None
            }
        } else {
            None
        };
        self.ai_handler.apply_rotation_state(state);
    }

    fn handle_audit_command(&self, parts: &[&str]) {
        let Some(ref audit) = self.audit_store else {
            eprintln!("audit is not enabled. Set 'audit.enabled: true' in security_policy.yaml.");
            return;
        };

        // No args → interactive menu (power-user keeps `/audit --user x ...`).
        if parts.len() == 1 {
            let opts: Vec<(String, String, String)> = vec![
                ("recent".to_string(), t("shell.menu.audit.recent"), t("shell.menu.audit.recent_desc")),
                ("user".to_string(), t("shell.menu.audit.user"), t("shell.menu.audit.user_desc")),
                ("host".to_string(), t("shell.menu.audit.host"), t("shell.menu.audit.host_desc")),
                ("type".to_string(), t("shell.menu.audit.type"), t("shell.menu.audit.type_desc")),
            ];
            let action = match pick_menu(&t("shell.menu.audit.title"), &opts) {
                Some(a) => a,
                None => return,
            };
            // Build owned args so the borrowed `&[&str]` outlives the match
            // arms; recurse into the same handler for the filter cases. For
            // "recent" (single element) fall through to the default query
            // below instead of recursing — otherwise the len()==1 menu would
            // re-trigger on itself.
            let owned: Vec<String> = match action.as_str() {
                "recent" => vec!["/audit".to_string()],
                "user" => match prompt_edit_value("Username", "filter by user", "", false) {
                    Some(u) if !u.is_empty() => vec!["/audit".into(), "--user".into(), u],
                    _ => return,
                },
                "host" => match prompt_edit_value("Host", "filter by host", "", false) {
                    Some(h) if !h.is_empty() => vec!["/audit".into(), "--host".into(), h],
                    _ => return,
                },
                "type" => match prompt_edit_value("Event type", "e.g. command_executed", "", false) {
                    Some(ty) if !ty.is_empty() => {
                        vec!["/audit".into(), "--event-type".into(), ty]
                    }
                    _ => return,
                },
                _ => return,
            };
            if owned.len() > 1 {
                let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
                return self.handle_audit_command(&refs);
            }
            // action == "recent": fall through to the default query.
        }

        let mut query = aish_session::AuditQuery::new();
        query.limit = 20;

        let mut i = 1;
        while i < parts.len() {
            match parts[i] {
                "--user" if i + 1 < parts.len() => {
                    query.user = Some(parts[i + 1].to_string());
                    i += 2;
                    continue;
                }
                "--host" if i + 1 < parts.len() => {
                    query.host = Some(parts[i + 1].to_string());
                    i += 2;
                    continue;
                }
                "--event-type" if i + 1 < parts.len() => {
                    match parts[i + 1].parse::<AuditEventType>() {
                        Ok(t) => query.event_type = Some(t),
                        Err(e) => {
                            eprintln!("invalid event type: {e}");
                            return;
                        }
                    }
                    i += 2;
                    continue;
                }
                "--since" if i + 1 < parts.len() => {
                    match chrono::DateTime::parse_from_rfc3339(parts[i + 1]) {
                        Ok(dt) => query.since = Some(dt.with_timezone(&chrono::Utc)),
                        Err(_) => {
                            eprintln!("invalid --since datetime (use RFC 3339, e.g. 2026-01-01T00:00:00Z)");
                            return;
                        }
                    }
                    i += 2;
                    continue;
                }
                "--limit" if i + 1 < parts.len() => {
                    if let Ok(n) = parts[i + 1].parse::<usize>() {
                        query.limit = n;
                    }
                    i += 2;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }

        // Access control: non-root users may not query another user's events
        // by name. Without --user, all events are shown (the user field in
        // audit records reflects the REMOTE SSH user, not the local OS user,
        // so filtering by local user would hide SSH session events).
        // SAFETY: getuid() never fails.
        let current_uid = unsafe { libc::getuid() };
        if current_uid != 0 {
            let me = self
                .audit_user
                .clone()
                .unwrap_or_else(|| current_uid.to_string());
            if let Some(ref requested) = query.user {
                if requested != &me {
                    eprintln!(
                        "permission denied: non-root users can only query their own audit events"
                    );
                    return;
                }
            }
        }

        let events = match audit.query(&query) {
            Ok(events) => events,
            Err(e) => {
                eprintln!("failed to query audit log: {e}");
                return;
            }
        };

        if events.is_empty() {
            println!("No audit events found.");
            return;
        }

        println!(
            "{:<26} {:<18} {:<10} {:<12} DETAILS",
            "TIMESTAMP", "EVENT", "USER", "HOST"
        );
        println!("{}", "─".repeat(100));
        for ev in &events {
            let detail = match ev.event_type {
                AuditEventType::Command => {
                    let cmd = ev.command.as_deref().unwrap_or("");
                    let source = ev.source.as_deref().unwrap_or("");
                    match ev.return_code {
                        Some(rc) if rc != 0 => format!("[{}] rc={} {}", source, rc, cmd),
                        _ => format!("[{}] {}", source, cmd),
                    }
                }
                AuditEventType::AiTool => {
                    let tool = ev.ai_tool.as_deref().unwrap_or("?");
                    let args_raw = ev.ai_args.as_deref().unwrap_or("");
                    let args_display = serde_json::from_str::<serde_json::Value>(args_raw)
                        .map(|v| format_tool_args_for_display(tool, &v))
                        .unwrap_or_else(|_| truncate_str(args_raw, 80).to_string());
                    format!("{} › {}", tool, args_display)
                }
                AuditEventType::SecurityDecision => {
                    let dec = ev.decision.as_deref().unwrap_or("?").to_uppercase();
                    let mut parts = vec![dec];
                    if let Some(choice) = ev.user_choice.as_deref().filter(|c| !c.is_empty()) {
                        parts.push(format!("user={}", choice));
                    }
                    if let Some(rule) = ev.matched_rule.as_deref().filter(|r| !r.is_empty()) {
                        parts.push(format!("rule={}", rule));
                    }
                    if let Some(cmd) = ev.command.as_deref().filter(|c| !c.is_empty()) {
                        parts.push(truncate_str(cmd, 60).to_string());
                    }
                    parts.join(" ")
                }
            };
            println!(
                "{:<26} {:<18} {:<10} {:<12} {}",
                ev.ts.format("%Y-%m-%d %H:%M:%S"),
                ev.event_type,
                ev.user.as_deref().unwrap_or("-"),
                ev.host.as_deref().unwrap_or("-"),
                detail
            );
        }
        println!("\n{} event(s) shown.", events.len());
    }

    fn handle_resume_command(&mut self, parts: &[&str]) {
        match parts.len() {
            1 => self.select_recent_session(),
            2 => self.resume_session(parts[1]),
            _ => eprintln!("{}", t("shell.resume.usage")),
        }
    }

    /// List all live PTY daemon sessions with interactive selection.
    /// Enter switches/attaches, Ctrl+E renames the highlighted session (then
    /// re-shows the refreshed list), Esc cancels.
    fn handle_live_sessions_command(&self) {
        use aish_i18n::{t, t_with_args};

        if !self.config.pty_daemon_enabled {
            eprintln!(
                "{}",
                theme::warning(&t("shell.live_sessions.daemon_disabled"))
            );
            eprintln!("{}", t("shell.live_sessions.daemon_disabled_hint"));
            return;
        }

        let current_id = std::env::var("AISH_SESSION_ID").ok();
        let home = dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();

        loop {
            let sessions = aish_pty::discover_sessions();
            if sessions.is_empty() {
                println!("{}", t("shell.live_sessions.none_active"));
                return;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Value of the current session's picker row, used to drive the
            // animated shimmer sweep on that row.
            let current_value = current_id.as_ref().and_then(|c| {
                sessions
                    .iter()
                    .enumerate()
                    .find(|(_, s)| c == &s.session_id || s.session_id.starts_with(c))
                    .map(|(i, _)| format!("session:{i}"))
            });

            let mut items: Vec<aish_ui::SearchSelectItem> = sessions
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let short = &s.session_id[..8.min(s.session_id.len())];
                    let name = s.name.as_deref().unwrap_or("");
                    let is_current = current_id
                        .as_ref()
                        .map(|c| c == &s.session_id || s.session_id.starts_with(c))
                        .unwrap_or(false);
                    let cwd_display = if !home.is_empty() && s.cwd.starts_with(&home) {
                        format!("~{}", &s.cwd[home.len()..])
                    } else {
                        s.cwd.clone()
                    };
                    let age_str = format_age(now.saturating_sub(s.started_at));
                    let detail = if is_current {
                        format!(
                            "{} {}",
                            t_with_args("shell.live_sessions.started_label", &age_args(age_str)),
                            t("shell.live_sessions.current_tag")
                        )
                    } else {
                        t_with_args("shell.live_sessions.started_label", &age_args(age_str))
                    };
                    // Label is short id + cwd; a custom name (if any) is the
                    // bold green highlight prefix so it stands out at a glance.
                    let label = format!("{short} {cwd_display}");
                    let search_text = format!("{name} {short} {cwd_display}");
                    let mut item = aish_ui::SearchSelectItem::new(format!("session:{}", i), label)
                        .with_detail(detail)
                        .with_search_text(search_text)
                        .with_badge(if is_current {
                            "\u{25cf}".to_string()
                        } else {
                            "\u{25cb}".to_string()
                        })
                        .with_renamable();
                    if !name.is_empty() {
                        item = item.with_highlight(name);
                    }
                    item
                })
                .collect();

            items.push(aish_ui::SearchSelectItem::new(
                "new",
                t("shell.live_sessions.create_new").to_string(),
            ));

            let panel = aish_ui::SearchSelectPanel::new(
                t("shell.live_sessions.panel_title"),
                t("shell.live_sessions.search_placeholder"),
                items,
            )
            .with_footer(t("shell.live_sessions.panel_footer"))
            .with_shimmer(current_value.as_deref());

            match aish_ui::PanelRuntime::new().run(panel) {
                Ok(aish_ui::PanelOutcome::Submitted(aish_ui::SearchSelectOutcome::Selected(
                    value,
                ))) => {
                    if value == "new" {
                        emit_osc("new");
                        return;
                    }
                    if let Some(idx_str) = value.strip_prefix("session:") {
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            if idx < sessions.len() {
                                let s = &sessions[idx];
                                let is_current = current_id
                                    .as_ref()
                                    .map(|c| c == &s.session_id)
                                    .unwrap_or(false);
                                if is_current {
                                    println!("{}", t("shell.live_sessions.already_current"));
                                } else {
                                    emit_osc(&format!(
                                        "switch:{}",
                                        &s.session_id[..8.min(s.session_id.len())]
                                    ));
                                }
                            }
                        }
                    }
                    return;
                }
                Ok(aish_ui::PanelOutcome::Submitted(aish_ui::SearchSelectOutcome::Rename(
                    value,
                ))) => {
                    if let Some(idx_str) = value.strip_prefix("session:") {
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            if idx < sessions.len() {
                                self.rename_live_session_interactive(&sessions[idx]);
                            }
                        }
                    }
                    // Refresh so the new name shows up, then re-show the picker.
                    continue;
                }
                _ => return,
            }
        }
    }

    /// Open a text-input panel to rename a live session in place. Used by the
    /// `/live_sessions` picker (Ctrl+E).
    fn rename_live_session_interactive(&self, session: &aish_pty::DaemonSessionInfo) {
        use aish_i18n::{t, t_with_args};
        use aish_ui::{ChoiceOutcome, ChoicePanel, PanelOutcome, PanelRuntime};
        use std::collections::HashMap;

        let short = &session.session_id[..8.min(session.session_id.len())];
        let current = session.name.as_deref().unwrap_or("");
        let mut id_args = HashMap::new();
        id_args.insert("id".to_string(), short.to_string());

        let question = if current.is_empty() {
            t("shell.live_sessions.rename_current_none")
        } else {
            let mut a = HashMap::new();
            a.insert("name".to_string(), current.to_string());
            t_with_args("shell.live_sessions.rename_current_set", &a)
        };

        let panel = ChoicePanel::new(
            t_with_args("shell.live_sessions.rename_title", &id_args),
            question,
            Vec::new(),
        )
        .with_custom_label(t("shell.live_sessions.rename_input_label"))
        .with_allow_cancel(true)
        .with_allow_empty_custom_input(true)
        .with_footer(t("shell.live_sessions.rename_input_footer"));

        if let Ok(PanelOutcome::Submitted(ChoiceOutcome::CustomInput(name))) =
            PanelRuntime::new().run(panel)
        {
            match aish_pty::rename_session(&session.session_id, &name) {
                Ok(()) => {
                    if name.trim().is_empty() {
                        println!(
                            "{}",
                            t_with_args("shell.live_sessions.rename_cleared", &id_args)
                        );
                    } else {
                        let mut a = id_args.clone();
                        a.insert("name".to_string(), name.trim().to_string());
                        println!("{}", t_with_args("shell.live_sessions.rename_done", &a));
                    }
                }
                Err(e) => {
                    let mut a = id_args.clone();
                    a.insert("err".to_string(), e.to_string());
                    eprintln!(
                        "{}",
                        theme::error(&t_with_args("shell.live_sessions.rename_failed", &a))
                    );
                }
            }
        }
    }

    /// Kill live PTY session(s) by ID prefix.
    ///
    /// - `/kill_live_sessions` with no args: list all active sessions.
    /// - `/kill_live_sessions <id> [<id> ...]`: kill each matching session.
    /// - `/kill_live_sessions all`: kill every active session except current.
    fn handle_kill_live_sessions_command(&self, parts: &[&str]) {
        use aish_i18n::{t, t_with_args};

        if !self.config.pty_daemon_enabled {
            eprintln!(
                "{}",
                theme::warning(&t("shell.kill_live_sessions.daemon_disabled"))
            );
            eprintln!("{}", t("shell.kill_live_sessions.daemon_disabled_hint"));
            return;
        }

        let sessions = aish_pty::discover_sessions();
        if sessions.is_empty() {
            println!("{}", t("shell.kill_live_sessions.none_active"));
            return;
        }

        let current_id = std::env::var("AISH_SESSION_ID").ok();
        let home = dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let format_session_line = |s: &aish_pty::DaemonSessionInfo| {
            let short = &s.session_id[..8.min(s.session_id.len())];
            let name = s.name.as_deref().unwrap_or("");
            let is_current = current_id
                .as_ref()
                .map(|c| c == &s.session_id || s.session_id.starts_with(c))
                .unwrap_or(false);
            let cwd_display = if !home.is_empty() && s.cwd.starts_with(&home) {
                format!("~{}", &s.cwd[home.len()..])
            } else {
                s.cwd.clone()
            };
            let age_str = format_age(now.saturating_sub(s.started_at));
            let current = if is_current {
                format!(
                    "  {}",
                    theme::success(&t("shell.live_sessions.current_tag"))
                )
            } else {
                String::new()
            };
            // Show name (if set) before short_id; otherwise just short_id.
            if !name.is_empty() {
                format!(
                    "  {} ({}) {}  {}{}",
                    name, short, cwd_display, age_str, current
                )
            } else {
                format!("  {} {}  {}{}", short, cwd_display, age_str, current)
            }
        };

        // No args: interactive menu of killable sessions.
        if parts.len() < 2 {
            let mut rows: Vec<(String, String)> = Vec::new();
            for s in &sessions {
                let current = current_id
                    .as_ref()
                    .map(|c| s.session_id == *c || s.session_id.starts_with(c))
                    .unwrap_or(false);
                if current {
                    continue;
                }
                rows.push((s.session_id.clone(), format_session_line(s)));
            }
            let mut menu: Vec<(String, String, String)> = rows
                .iter()
                .map(|(v, l)| (v.clone(), l.clone(), String::new()))
                .collect();
            menu.push(("all".to_string(), t("shell.menu.kill.all"), t("shell.menu.kill.all_desc")));
            let choice = match pick_menu(&t("shell.menu.kill.title"), &menu) {
                Some(c) => c,
                None => return,
            };
            return self.handle_kill_live_sessions_command(&[
                "/kill_live_sessions",
                choice.as_str(),
            ]);
        }

        // `/kill_live_sessions all`
        if parts[1] == "all" {
            let targets: Vec<_> = sessions
                .iter()
                .filter(|s| {
                    !current_id
                        .as_ref()
                        .map(|c| s.session_id == *c || s.session_id.starts_with(c.as_str()))
                        .unwrap_or(false)
                })
                .collect();
            if targets.is_empty() {
                println!("{}", t("shell.kill_live_sessions.no_others"));
                return;
            }
            for s in &targets {
                let short = &s.session_id[..8.min(s.session_id.len())];
                let mut kill_args = std::collections::HashMap::new();
                kill_args.insert("id".to_string(), short.to_string());
                print!(
                    "{}",
                    t_with_args("shell.kill_live_sessions.killing", &kill_args)
                );
                match aish_pty::kill_session(&s.socket_path) {
                    Ok(()) => {
                        println!(
                            "{}",
                            theme::success(&t("shell.kill_live_sessions.kill_done"))
                        )
                    }
                    Err(e) => {
                        let mut err_args = std::collections::HashMap::new();
                        err_args.insert("error".to_string(), e.to_string());
                        println!(
                            "{}",
                            theme::error(&t_with_args(
                                "shell.kill_live_sessions.kill_failed",
                                &err_args
                            ))
                        );
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut count_args = std::collections::HashMap::new();
            count_args.insert("count".to_string(), targets.len().to_string());
            println!(
                "\n{}",
                t_with_args("shell.kill_live_sessions.all_terminated", &count_args)
            );
            return;
        }

        // Kill each specified ID prefix.
        let mut killed = 0u32;
        let mut errors = 0u32;
        for id in &parts[1..] {
            let target = sessions.iter().find(|s| {
                s.session_id == *id
                    || s.session_id.starts_with(id)
                    || s.name.as_deref() == Some(*id)
            });
            match target {
                Some(s) => {
                    let short = &s.session_id[..8.min(s.session_id.len())];
                    let is_current = current_id
                        .as_ref()
                        .map(|c| s.session_id == *c || s.session_id.starts_with(c.as_str()))
                        .unwrap_or(false);
                    if is_current {
                        let mut cur_args = std::collections::HashMap::new();
                        cur_args.insert("id".to_string(), short.to_string());
                        eprintln!(
                            "{}",
                            theme::warning(&t_with_args(
                                "shell.kill_live_sessions.is_current",
                                &cur_args
                            ))
                        );
                        errors += 1;
                        continue;
                    }
                    let mut kill_args = std::collections::HashMap::new();
                    kill_args.insert("id".to_string(), short.to_string());
                    print!(
                        "{}",
                        t_with_args("shell.kill_live_sessions.killing", &kill_args)
                    );
                    match aish_pty::kill_session(&s.socket_path) {
                        Ok(()) => {
                            println!(
                                "{}",
                                theme::success(&t("shell.kill_live_sessions.kill_done"))
                            );
                            killed += 1;
                        }
                        Err(e) => {
                            let mut err_args = std::collections::HashMap::new();
                            err_args.insert("error".to_string(), e.to_string());
                            println!(
                                "{}",
                                theme::error(&t_with_args(
                                    "shell.kill_live_sessions.kill_failed",
                                    &err_args
                                ))
                            );
                            errors += 1;
                        }
                    }
                }
                None => {
                    let mut nf_args = std::collections::HashMap::new();
                    nf_args.insert("id".to_string(), id.to_string());
                    eprintln!(
                        "{}",
                        theme::error(&t_with_args("shell.kill_live_sessions.not_found", &nf_args))
                    );
                    errors += 1;
                }
            }
        }
        if killed > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if killed + errors > 1 {
            let mut sum_args = std::collections::HashMap::new();
            sum_args.insert("killed".to_string(), killed.to_string());
            sum_args.insert("failed".to_string(), errors.to_string());
            println!(
                "\n{}",
                t_with_args("shell.kill_live_sessions.summary", &sum_args)
            );
        }
    }

    fn handle_doctor_command(&mut self, parts: &[&str]) {
        // No args → interactive menu (power-user keeps `/doctor [--fix]`).
        let fix = if parts.len() == 1 {
            let opts: Vec<(String, String, String)> = vec![
                ("run".to_string(), t("shell.menu.doctor.run"), t("shell.menu.doctor.run_desc")),
                ("fix".to_string(), t("shell.menu.doctor.fix"), t("shell.menu.doctor.fix_desc")),
            ];
            match pick_menu(&t("shell.menu.doctor.title"), &opts) {
                Some(action) => action == "fix",
                None => return,
            }
        } else {
            parts.iter().skip(1).any(|arg| *arg == "--fix")
        };
        let doctor = crate::doctor::Doctor::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            doctor.run(fix).await;
        });
    }

    fn handle_failure_diagnose_command(&mut self) {
        use crate::ai_handler::{
            effective_verify_exit_code, format_failure_diagnose_error,
            print_failure_diagnose_report, should_offer_confirm_execute,
            summarize_verification_conclusion, verify_outcome_from_execution, DiagnoseParseOutcome,
            FailureDiagnoseConclusion, VerifyOutcome, VerifyStepResult,
        };
        use aish_i18n::{t, t_with_args};
        use aish_tools::bash::{BashTool, ReadOnlyVerdict};
        use std::collections::HashMap;

        if !self.state.can_correct_error {
            println!("{}", t("shell.failure_diagnose.no_failure"));
            return;
        }

        let Some(ref command) = self.state.last_command.clone() else {
            println!("{}", t("shell.failure_diagnose.no_failure"));
            return;
        };

        let command = command.clone();
        let exit_code = self.state.last_exit_code;
        let output = self.state.last_output.clone();
        let cwd = self.state.cwd.clone();
        let (safe_command, _) = self.secret_vault.lock().unwrap().redact_output(&command);
        let (safe_output, _) = self.secret_vault.lock().unwrap().redact_output(&output);

        let old_sigint = self.install_ai_sigint_handler();
        let mut esc_watcher = CrosstermEscWatcher::start(self.ai_handler.cancellation_token_arc());
        let token_ptr = self.ai_handler.cancellation_token() as *const aish_llm::CancellationToken;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let diagnose_result = rt.block_on(async {
            tokio::select! {
                r = self.ai_handler.handle_failure_diagnose(
                    &safe_command,
                    exit_code,
                    &safe_output,
                    &cwd,
                ) => r,
                _ = poll_cancelled(token_ptr) => {
                    Err(aish_core::AishError::Cancelled)
                }
            }
        });

        esc_watcher.stop();
        Self::restore_ai_sigint_handler(old_sigint);
        self.animation.stop();

        let report = match diagnose_result {
            Ok(parsed) => parsed,
            Err(aish_core::AishError::Cancelled) => {
                println!("{}", theme::warning(&t("shell.command_cancelled")));
                return;
            }
            Err(e) => {
                eprintln!("{}", theme::error(&format_failure_diagnose_error(&e)));
                return;
            }
        };

        print_failure_diagnose_report(&report);

        if report.outcome == DiagnoseParseOutcome::FormatError {
            self.state.can_correct_error = false;
            return;
        }

        let report = report.report;

        if let Some(ref fix_cmd) = report.suggested_fix {
            if !should_offer_confirm_execute(
                report.suggested_fix.as_deref(),
                report.has_alternatives,
            ) {
                if report.has_alternatives {
                    println!("{}", t("shell.failure_diagnose.fix_has_alternatives"));
                } else {
                    println!("{}", t("shell.failure_diagnose.fix_not_auto_executable"));
                }
                self.state.can_correct_error = false;
                return;
            }

            let prompt = format!(
                "{}{}{}",
                t("shell.failure_diagnose.confirm_fix_prefix"),
                theme::accent(&theme::bold(&fix_cmd)),
                t("shell.failure_diagnose.confirm_fix_suffix")
            );
            if !Self::confirm_action(&prompt, "") {
                let mut args = HashMap::new();
                args.insert("command".to_string(), fix_cmd.clone());
                println!(
                    "{}",
                    t_with_args("shell.failure_diagnose.fix_cancelled", &args)
                );
                self.state.can_correct_error = false;
                return;
            }

            if !self.screen_shell_command(fix_cmd) {
                self.state.can_correct_error = false;
                return;
            }

            let fix_exit = self.execute_external_command(fix_cmd);
            self.record_history(fix_cmd, fix_exit);

            if report.verify_commands.is_empty() {
                println!("{}", t("shell.failure_diagnose.no_verify_commands"));
                self.state.can_correct_error = false;
                return;
            }

            println!("{}", t("shell.failure_diagnose.verifying"));
            let mut steps = Vec::new();
            for verify_cmd in &report.verify_commands {
                let verdict = BashTool::classify_read_only(verify_cmd);
                if !matches!(verdict, ReadOnlyVerdict::ReadOnly) {
                    let reason = match verdict {
                        ReadOnlyVerdict::NotReadOnly { reason } => reason,
                        ReadOnlyVerdict::Unparseable => "unparseable command".to_string(),
                        ReadOnlyVerdict::ReadOnly => unreachable!(),
                    };
                    println!(
                        "{}",
                        t_with_args(
                            "shell.failure_diagnose.verify_blocked",
                            &HashMap::from([
                                ("command".to_string(), verify_cmd.clone()),
                                ("reason".to_string(), reason.clone()),
                            ])
                        )
                    );
                    steps.push(VerifyStepResult {
                        command: verify_cmd.clone(),
                        output: String::new(),
                        exit_code: -1,
                        outcome: VerifyOutcome::Blocked,
                        block_reason: Some(reason),
                    });
                    continue;
                }

                if !self.screen_shell_command(verify_cmd) {
                    steps.push(VerifyStepResult {
                        command: verify_cmd.clone(),
                        output: String::new(),
                        exit_code: -1,
                        outcome: VerifyOutcome::Blocked,
                        block_reason: Some("input guard declined".to_string()),
                    });
                    continue;
                }

                let code = self.execute_external_command(verify_cmd);
                let step_output = self.state.last_output.clone();
                let effective_code = effective_verify_exit_code(code, &step_output);
                let outcome = verify_outcome_from_execution(code, &step_output);
                let status_key = match outcome {
                    VerifyOutcome::Passed => "shell.failure_diagnose.verify_passed",
                    VerifyOutcome::Failed => "shell.failure_diagnose.verify_failed",
                    VerifyOutcome::Blocked => "shell.failure_diagnose.verify_blocked",
                };
                println!(
                    "{}",
                    t_with_args(
                        status_key,
                        &HashMap::from([
                            ("command".to_string(), verify_cmd.clone()),
                            ("exit_code".to_string(), effective_code.to_string()),
                        ])
                    )
                );
                if !step_output.is_empty() {
                    let preview: String = step_output.chars().take(500).collect();
                    println!("   {}", preview.trim());
                }
                steps.push(VerifyStepResult {
                    command: verify_cmd.clone(),
                    output: step_output,
                    exit_code: effective_code,
                    outcome,
                    block_reason: None,
                });
            }

            let conclusion = summarize_verification_conclusion(&steps);
            let conclusion_msg = match conclusion {
                FailureDiagnoseConclusion::Fixed => t("shell.failure_diagnose.conclusion_fixed"),
                FailureDiagnoseConclusion::PartialFailure => {
                    t("shell.failure_diagnose.conclusion_partial")
                }
                FailureDiagnoseConclusion::CannotDetermine => {
                    t("shell.failure_diagnose.conclusion_unknown")
                }
            };
            println!("{}", conclusion_msg);
        }

        self.state.can_correct_error = false;
    }

    fn handle_status_command(&mut self) {
        let live_id = std::env::var("AISH_SESSION_ID").ok();
        crate::status::run_status(
            &self.pty,
            &self.version,
            &self.session_uuid,
            live_id.as_deref(),
            &self.config.model,
        );
    }

    fn select_recent_session(&mut self) {
        let Some(store) = self.session_store.as_ref() else {
            eprintln!("{}", t("shell.resume.session_store_unavailable"));
            return;
        };

        let sessions = match store.list_sessions(RESUME_LIST_LIMIT) {
            Ok(sessions) if sessions.is_empty() => {
                println!("{}", t("shell.resume.no_sessions"));
                return;
            }
            Ok(sessions) => sessions,
            Err(err) => {
                eprintln!(
                    "{}",
                    t_with_args("shell.resume.list_failed", &{
                        let mut args = std::collections::HashMap::new();
                        args.insert("error".to_string(), err.to_string());
                        args
                    })
                );
                return;
            }
        };

        let items: Vec<ResumeSessionItem> = sessions
            .iter()
            .map(|session| ResumeSessionItem::from_record(session, &self.session_uuid))
            .collect();

        match select_resume_session(&items) {
            Ok(Some(session_id)) => self.resume_session(&session_id),
            Ok(None) => {}
            Err(_) => self.print_recent_sessions(),
        }
    }

    fn print_recent_sessions(&self) {
        let Some(store) = self.session_store.as_ref() else {
            eprintln!("{}", t("shell.resume.session_store_unavailable"));
            return;
        };

        match store.list_sessions(RESUME_LIST_LIMIT) {
            Ok(sessions) if sessions.is_empty() => {
                println!("{}", t("shell.resume.no_sessions"));
            }
            Ok(sessions) => {
                println!(
                    "{}",
                    t_with_args("shell.resume.recent_header", &{
                        let mut args = std::collections::HashMap::new();
                        args.insert("limit".to_string(), RESUME_LIST_LIMIT.to_string());
                        args
                    })
                );
                for session in sessions {
                    println!("{}", format_resume_session_row(&session));
                }
                println!("{}", t("shell.resume.list_hint"));
            }
            Err(err) => eprintln!(
                "{}",
                t_with_args("shell.resume.list_failed", &{
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), err.to_string());
                    args
                })
            ),
        }
    }

    fn resume_session(&mut self, session_id: &str) {
        if let Err(err) = self.resume_session_with_options(session_id, true, true) {
            eprintln!("{}", err);
        }
    }

    fn resume_session_with_options(
        &mut self,
        session_id: &str,
        persist_current: bool,
        print_success: bool,
    ) -> aish_core::Result<()> {
        if persist_current {
            self.persist_session_snapshot();
        }

        let Some(store) = self.session_store.as_ref() else {
            return Err(aish_core::AishError::Session(t(
                "shell.resume.session_store_unavailable",
            )));
        };

        let session = match store.get_session(session_id) {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(aish_core::AishError::Session(t_with_args(
                    "shell.resume.not_found",
                    &{
                        let mut args = std::collections::HashMap::new();
                        args.insert("session_id".to_string(), session_id.to_string());
                        args
                    },
                )))
            }
            Err(err) => return Err(err),
        };

        let snapshot = session.state_snapshot();
        let saved_cwd = snapshot.cwd.clone();
        self.ai_handler
            .restore_session_context_snapshot(snapshot.context_messages_snapshot);

        if self.config.model != session.model
            || session.api_base.as_deref() != Some(&self.config.api_base)
        {
            self.config.model = session.model.clone();
            if let Some(api_base) = session.api_base.clone() {
                self.config.api_base = api_base;
            }
            self.ai_handler.update_model(
                &self.config.model,
                Some(&self.config.api_base),
                Some(&self.config.api_key),
            );
            self.refresh_config_dependent_tools();
        }

        let target_cwd = saved_cwd
            .filter(|cwd| std::path::Path::new(cwd).is_dir())
            .unwrap_or_else(|| {
                if let Some(missing) = session.state_snapshot().cwd {
                    eprintln!(
                        "{}",
                        t_with_args("shell.resume.cwd_missing", &{
                            let mut args = std::collections::HashMap::new();
                            args.insert("cwd".to_string(), missing);
                            args
                        })
                    );
                }
                self.state.cwd.clone()
            });

        self.session_uuid = session.session_uuid.clone();
        if target_cwd != self.state.cwd {
            self.state.prev_cwd = Some(self.state.cwd.clone());
            self.state.cwd = target_cwd.clone();
            let _ = std::env::set_current_dir(&target_cwd);
        }

        self.restart_pty_with_notice(false)?;
        self.persist_session_snapshot();
        if print_success {
            println!(
                "{}",
                t_with_args("shell.resume.resumed", &{
                    let mut args = std::collections::HashMap::new();
                    args.insert("session_id".to_string(), self.session_uuid.clone());
                    args
                })
            );
        }
        Ok(())
    }

    /// Handle `/model [name]` — show current model or switch to a new one.
    fn refresh_config_dependent_tools(&mut self) {
        self.ai_handler
            .register_tool(Box::new(aish_tools::WebFetchTool::new(
                &self.config.api_base,
                &self.config.api_key,
                &self.config.model,
                Some(self.config.temperature),
                self.config.max_tokens,
            )));
    }

    fn handle_record_command(&mut self, parts: &[&str]) {
        // No args → interactive menu (power-user keeps `/record start|stop`).
        if parts.len() == 1 {
            let recording = self
                .shared_recorder
                .lock()
                .map(|g| g.is_some())
                .unwrap_or(false);
            let opts: Vec<(String, String, String)> = if recording {
                vec![("stop".to_string(), t("shell.menu.record.stop"), t("shell.menu.record.stop_desc"))]
            } else {
                vec![("start".to_string(), t("shell.menu.record.start"), t("shell.menu.record.start_desc"))]
            };
            let action = match pick_menu(&t("shell.menu.record.title"), &opts) {
                Some(a) => a,
                None => return,
            };
            return self.handle_record_command(&["/record", action.as_str()]);
        }
        let subcmd = parts.get(1).copied().unwrap_or("");
        let mut guard = self
            .shared_recorder
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        match subcmd {
            "start" => {
                if guard.is_some() {
                    eprintln!("{}", theme::warning(&t("shell.record.already_recording")));
                    return;
                }
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let file_path = crate::recorder::Recorder::generate_file_path();
                match crate::recorder::Recorder::new(file_path.clone(), term_size) {
                    Ok(recorder) => {
                        *guard = Some(recorder);
                        println!(
                            "{}",
                            theme::success(&t_with_args("shell.record.started", &{
                                let mut args = std::collections::HashMap::new();
                                args.insert("path".to_string(), file_path.display().to_string());
                                args
                            }))
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "{}",
                            theme::error(&t_with_args("shell.record.start_failed", &{
                                let mut args = std::collections::HashMap::new();
                                args.insert("error".to_string(), e.to_string());
                                args
                            }))
                        );
                    }
                }
            }
            "stop" => {
                let elapsed = guard.as_ref().map(|r| r.elapsed()).unwrap_or_default();
                if let Some(mut rec) = guard.take() {
                    let path = rec.file_path().to_path_buf();
                    let _ = rec.flush();
                    let metadata = std::fs::metadata(&path).ok();
                    let size = metadata.map(|m| m.len()).unwrap_or(0);
                    let secs = elapsed.as_secs();
                    let size_str = if size < 1024 {
                        format!("{} B", size)
                    } else {
                        format!("{:.1} KB", size as f64 / 1024.0)
                    };
                    println!(
                        "{}",
                        theme::success(&t_with_args("shell.record.stopped", &{
                            let mut args = std::collections::HashMap::new();
                            args.insert("path".to_string(), path.display().to_string());
                            args.insert("duration".to_string(), secs.to_string());
                            args.insert("size".to_string(), size_str);
                            args
                        }))
                    );
                } else {
                    eprintln!("{}", theme::warning(&t("shell.record.not_recording")));
                }
            }
            _ => {
                if let Some(ref rec) = *guard {
                    let elapsed = rec.elapsed();
                    let path = rec.file_path().display().to_string();
                    let secs = elapsed.as_secs();
                    println!(
                        "{}",
                        theme::warning(&t_with_args("shell.record.recording_status", &{
                            let mut args = std::collections::HashMap::new();
                            args.insert("path".to_string(), path);
                            args.insert("duration".to_string(), secs.to_string());
                            args
                        }))
                    );
                } else {
                    println!("{}", t("shell.record.usage"));
                }
            }
        }
    }

    fn handle_model_command(&mut self, parts: &[&str]) {
        if parts.len() == 1 {
            let mut args = std::collections::HashMap::new();
            args.insert("model".to_string(), self.config.model.clone());
            println!("{}", t_with_args("shell.model.current", &args));
            return;
        }

        if parts.len() > 1 && (parts[1] == "--help" || parts[1] == "-h") {
            println!("{}", theme::accent(&t("shell.model_usage")));
            return;
        }

        let new_model = parts[1..].join(" ");
        if new_model == self.config.model {
            let mut args = std::collections::HashMap::new();
            args.insert("model".to_string(), new_model);
            println!("{}", t_with_args("shell.model.switch_same", &args));
            return;
        }

        // Detect provider for the new model
        let _provider = aish_llm::detect_provider(&new_model, &self.config.api_base);

        // Update LLM session
        self.ai_handler.update_model(
            &new_model,
            Some(&self.config.api_base),
            Some(&self.config.api_key),
        );

        // Update inline completion model so it follows `/model` switches
        if let Some(ai) = &self.inline_ai {
            ai.update_model(&new_model);
        }

        // Update config
        self.config.model = new_model.clone();
        self.refresh_config_dependent_tools();

        // Persist to config file
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Err(e) = aish_config::ConfigLoader::save(&self.config, &config_path) {
            eprintln!(
                "{}",
                theme::warning(&{
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), e.to_string());
                    t_with_args("shell.config_save_warning", &args)
                })
            );
        }

        // Rebuild rotation so the new primary model takes effect at once;
        // otherwise the rotation loop keeps the model captured at setup.
        self.rebuild_rotation();

        let mut args = std::collections::HashMap::new();
        args.insert("model".to_string(), new_model);
        println!("{}", t_with_args("shell.model.switch_success", &args));
    }

    /// Handle `/setting` — single-screen settings panel.
    ///
    /// Flat layout: category chips on top, type-to-filter list, inline edit
    /// for Bool/Choice (Space toggle, `<`/`>` step, Enter cycle), and Enter
    /// on Text/Int/Float/Secret/StringList pops a one-shot external editor
    /// then returns to the panel at the same row. Esc exits; Ctrl+R resets
    /// the highlighted row to factory default.
    fn handle_setting_command(&mut self) {
        use crate::settings_panel::{self, SettingKind};
        use aish_ui::{PanelOutcome, PanelRuntime, SettingsOutcome, SettingsPanel};

        // Count of restart-required edits this session, for an exit summary.
        let mut restart_changes: usize = 0;
        let mut last_key: Option<String> = None;
        let mut last_category: usize = 0;
        let mut pending_error: Option<String> = None;

        loop {
            // Rebuild items every iteration so values refresh after each edit.
            // `last_key` restores the cursor; `last_category` keeps the user on
            // the same chip instead of bouncing back to "All" after each edit.
            let (cats, items) = Self::build_settings_items(&self.config);
            let panel = SettingsPanel::new(t("shell.setting.title").to_string(), cats, items)
                .with_search_placeholder(t("shell.setting.search_placeholder"))
                .with_footer_idle(t("shell.setting.footer_idle"))
                .with_active_category(last_category)
                .with_selected_key(last_key.as_deref())
                .with_error(pending_error.clone());

            let outcome = {
                let _guard = aish_tools::bash::acquire_interactive_input_guard();
                PanelRuntime::new().run(panel)
            };

            let outcome = match outcome {
                Ok(PanelOutcome::Submitted(o)) => o,
                Ok(PanelOutcome::Cancelled) => {
                    // Defensive: panel always submits Cancelled with a chip
                    // attached, but fall back to the snapshot if not.
                    break;
                }
                Err(_) => break,
            };
            // Remember the chip the user ended on — whether they applied an
            // edit, reset, opened a sub-editor, or cancelled out. This keeps
            // them on the same chip on the next panel re-open.
            last_category = outcome.active_category();

            match outcome {
                SettingsOutcome::Applied { key, value, .. } => {
                    let Some(k) = Self::resolve_setting_key(&key) else {
                        continue;
                    };
                    last_key = Some(key);
                    self.apply_or_queue_error(k, &value, &mut restart_changes, &mut pending_error);
                }
                SettingsOutcome::Reset { key, .. } => {
                    let Some(k) = Self::resolve_setting_key(&key) else {
                        continue;
                    };
                    let default_val = settings_panel::default_raw_of(k);
                    self.apply_or_queue_error(
                        k,
                        &default_val,
                        &mut restart_changes,
                        &mut pending_error,
                    );
                    last_key = Some(key);
                }
                SettingsOutcome::RequestExternalEdit { key, .. } => {
                    let Some(k) = Self::resolve_setting_key(&key) else {
                        continue;
                    };
                    last_key = Some(key.clone());
                    let def = settings_panel::find(k);
                    let label = setting_label(def);
                    let desc = setting_desc(def);
                    let cur_raw = settings_panel::current_raw(&self.config, k);
                    let secret = matches!(def.kind, SettingKind::Secret);
                    // Pop a single external input; we return to the panel
                    // afterwards regardless of outcome. Empty submission on
                    // a Secret prompt is a no-op (skip), not a wipe — mirrors
                    // the legacy edit_setting_value guard.
                    let submitted = prompt_edit_value(&label, &desc, &cur_raw, secret);
                    let skip = secret && submitted.as_deref().is_some_and(|v| v.is_empty());
                    if let Some(v) = submitted {
                        if !skip {
                            self.apply_or_queue_error(
                                k,
                                &v,
                                &mut restart_changes,
                                &mut pending_error,
                            );
                        }
                    }
                }
                SettingsOutcome::RequestChoiceSelect { key, .. } => {
                    let Some(k) = Self::resolve_setting_key(&key) else {
                        continue;
                    };
                    last_key = Some(key);
                    // Pop a one-shot choice list (shows every option with the
                    // current value tagged). Returns to the panel after.
                    if let Some(v) = self.prompt_choice_value(k) {
                        self.apply_or_queue_error(k, &v, &mut restart_changes, &mut pending_error);
                    }
                }
                SettingsOutcome::Cancelled { .. } => break,
            }
        }

        // Exit summary: surface how many changes need a restart.
        if restart_changes > 0 {
            println!(
                "{}",
                theme::warning(&t_with_args("shell.setting.restart_summary", &{
                    let mut a = std::collections::HashMap::new();
                    a.insert("count".to_string(), restart_changes.to_string());
                    a
                }))
            );
        }
    }

    /// Pop a one-shot selection list for a `Choice` setting so the user can
    /// see every option at a glance (instead of cycling blind with `<`/`>`).
    /// The current value is tagged. Returns `None` on Esc.
    fn prompt_choice_value(&self, key: crate::settings_panel::SettingKey) -> Option<String> {
        use crate::settings_panel::{self, SettingKind};
        use crate::tui::{show_selection_dialog, DialogOption, DialogResult};

        let def = settings_panel::find(key);
        let opts = match def.kind {
            SettingKind::Choice(o) => o,
            _ => return None,
        };
        let cur = settings_panel::current_raw(&self.config, key);
        let label = setting_label(def);
        let desc = setting_desc(def);
        let options: Vec<DialogOption> = opts
            .iter()
            .map(|o| {
                let mut d = DialogOption::new(*o, *o);
                if o.eq_ignore_ascii_case(&cur) {
                    d = d.with_description(t("shell.setting.current_tag"));
                }
                d
            })
            .collect();
        match show_selection_dialog(&label, &desc, &options, false, true) {
            DialogResult::Selected(v) => Some(v),
            _ => None,
        }
    }

    /// Resolve a settings-panel item key back to its `SettingKey`.
    /// Returns `None` if the catalog was mutated out-of-band (defensive).
    fn resolve_setting_key(name: &str) -> Option<crate::settings_panel::SettingKey> {
        use crate::settings_panel::SETTINGS;
        SETTINGS
            .iter()
            .find(|d| d.key.name() == name)
            .map(|d| d.key)
    }

    /// Apply `new_val` to `key` and either clear `pending_error` (on success)
    /// or replace it with the localized validation message (on failure).
    /// Centralizing this here guarantees the error shown always reflects the
    /// most recent edit's outcome — no stale messages can persist across
    /// edits, which was a real bug before this helper existed.
    fn apply_or_queue_error(
        &mut self,
        key: crate::settings_panel::SettingKey,
        new_val: &str,
        restart_changes: &mut usize,
        pending_error: &mut Option<String>,
    ) {
        *pending_error = None;
        if let Err(e) = self.apply_setting_value(key, new_val, restart_changes) {
            *pending_error = Some(t_with_args("shell.setting.invalid", &{
                let mut a = std::collections::HashMap::new();
                a.insert("error".to_string(), e);
                a
            }));
        }
    }

    /// Apply `new_val` to `key`: validate, persist, run live side-effects,
    /// emit a confirmation line. On validation failure returns `Err(msg)`
    /// so the caller can surface it in the panel without losing the cursor.
    fn apply_setting_value(
        &mut self,
        key: crate::settings_panel::SettingKey,
        new_val: &str,
        restart_changes: &mut usize,
    ) -> Result<(), String> {
        use crate::settings_panel::{self, LiveEffect, SettingKind};

        // Validate against a clone so a bad value can't partially mutate
        // the live config; only commit on success.
        let mut trial = self.config.clone();
        settings_panel::apply(&mut trial, key, new_val)?;
        self.config = trial;

        // Persist to disk.
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Err(e) = aish_config::ConfigLoader::save(&self.config, &config_path) {
            eprintln!(
                "{}",
                theme::warning(&{
                    let mut a = std::collections::HashMap::new();
                    a.insert("error".to_string(), e.to_string());
                    t_with_args("shell.config_save_warning", &a)
                })
            );
        }

        // Live side-effects so the change is felt without restart.
        match settings_panel::live_effect(key) {
            LiveEffect::ModelSession => {
                self.ai_handler.update_model(
                    &self.config.model,
                    Some(&self.config.api_base),
                    Some(&self.config.api_key),
                );
                if let Some(ai) = &self.inline_ai {
                    ai.update_model(&self.config.model);
                }
                self.refresh_config_dependent_tools();
            }
            LiveEffect::ToolsRefresh => self.refresh_config_dependent_tools(),
            LiveEffect::InputGuard => {
                self.input_guard
                    .set_enabled(self.config.input_guard_enabled);
            }
            LiveEffect::SecurityPolicy => {
                // Re-apply the security globals mirrored from config.yaml to
                // the running SecurityManager so changes take effect now,
                // not on next launch. `set_policy` rebuilds the sandbox
                // runner when `enable_sandbox` flips.
                let mut policy = self.security_manager.policy().clone();
                apply_config_security_overrides(&mut policy, &self.config);
                self.security_manager.set_policy(policy);

                // Also persist to security_policy.yaml (the file users
                // consider authoritative) so the change is visible there and
                // survives a restart. Writes to the resolved policy path;
                // a permission failure is logged but does not revert the
                // live update.
                self.sync_security_policy_file();
            }
            LiveEffect::None => {}
        }

        // Confirmation line below the panel.
        let def = settings_panel::find(key);
        let label = setting_label(def);
        if new_val.is_empty() {
            println!(
                "{}",
                theme::success(&t_with_args("shell.setting.applied_clear", &{
                    let mut a = std::collections::HashMap::new();
                    a.insert("label".to_string(), label);
                    a
                }))
            );
        } else {
            let shown = if matches!(def.kind, SettingKind::Secret) {
                mask_secret(new_val)
            } else {
                new_val.to_string()
            };
            println!(
                "{}",
                theme::success(&t_with_args("shell.setting.applied", &{
                    let mut a = std::collections::HashMap::new();
                    a.insert("label".to_string(), label);
                    a.insert("value".to_string(), shown);
                    a
                }))
            );
        }
        if settings_panel::requires_restart(key) {
            println!("{}", theme::faint(&t("shell.setting.restart_hint")));
            *restart_changes += 1;
        }
        Ok(())
    }

    /// Persist the current security globals to the resolved
    /// `security_policy.yaml` so `/setting` changes are visible in the file
    /// users consider authoritative and survive a restart. A write failure
    /// (e.g. root-owned system policy) is logged but does not revert the
    /// live update or block the `/setting` flow.
    fn sync_security_policy_file(&self) {
        let Some(path) = aish_security::resolve_security_policy_path(None) else {
            return;
        };
        let risk = match self.config.default_risk_level.to_lowercase().as_str() {
            "medium" => "MEDIUM",
            "high" => "HIGH",
            _ => "LOW",
        };
        let action = match self.config.sandbox_off_action.to_lowercase().as_str() {
            "confirm" => "CONFIRM",
            "block" => "BLOCK",
            _ => "ALLOW",
        };
        let timeout_str = self.config.sandbox_timeout_seconds.to_string();
        let updates: &[(&str, &str)] = &[
            (
                "enable_sandbox",
                if self.config.enable_sandbox {
                    "true"
                } else {
                    "false"
                },
            ),
            ("default_risk_level", risk),
            ("sandbox_off_action", action),
            ("sandbox_timeout_seconds", &timeout_str),
        ];
        if let Err(e) = aish_security::save_policy_globals(&path, updates) {
            eprintln!(
                "{}",
                theme::warning(&format!("could not write {}: {e}", path.display()))
            );
        }
    }

    /// Build the panel data from the live config. Pure function so it stays
    /// easy to test and avoids borrow conflicts against `self`.
    fn build_settings_items(
        config: &ConfigModel,
    ) -> (
        Vec<aish_ui::SettingsCategoryInfo>,
        Vec<aish_ui::SettingsItem>,
    ) {
        use crate::settings_panel::{self, SettingCategory, SettingKind, SETTINGS};
        use aish_ui::{SettingsCategoryInfo, SettingsItem, SettingsValueKind};

        let cats: Vec<SettingsCategoryInfo> = SettingCategory::ALL
            .iter()
            .map(|&c| SettingsCategoryInfo {
                label: t(&format!("shell.setting.cat.{}", c.name())).to_string(),
                icon: c.icon().to_string(),
                color: category_color_for(c),
            })
            .collect();

        let cat_index_of = |c: SettingCategory| {
            SettingCategory::ALL
                .iter()
                .position(|&x| x == c)
                .unwrap_or(0)
        };

        let items: Vec<SettingsItem> = SETTINGS
            .iter()
            .map(|def| {
                let cur_raw = settings_panel::current_raw(config, def.key);
                let display = setting_display_current(config, def);
                let (kind, options) = match def.kind {
                    SettingKind::Bool => (SettingsValueKind::Bool, Vec::new()),
                    SettingKind::Choice(opts) => (
                        SettingsValueKind::Choice,
                        opts.iter().map(|s| s.to_string()).collect(),
                    ),
                    SettingKind::Text => (SettingsValueKind::Text, Vec::new()),
                    SettingKind::Float => (SettingsValueKind::Float, Vec::new()),
                    SettingKind::Int => (SettingsValueKind::Int, Vec::new()),
                    SettingKind::Secret => (SettingsValueKind::Secret, Vec::new()),
                    SettingKind::StringList => (SettingsValueKind::StringList, Vec::new()),
                };
                SettingsItem {
                    key: def.key.name().to_string(),
                    label: setting_label(def),
                    desc: setting_desc(def),
                    current_raw: cur_raw.clone(),
                    display_value: display,
                    default_raw: settings_panel::default_raw_of(def.key).to_string(),
                    category_index: cat_index_of(def.category),
                    kind,
                    options,
                    changed: settings_panel::is_changed(def.key, &cur_raw),
                    restart_required: settings_panel::requires_restart(def.key),
                }
            })
            .collect();

        (cats, items)
    }

    /// Handle `/plan [start|status|exit]` — plan mode lifecycle.
    fn handle_plan_command(&mut self, parts: &[&str]) {
        use aish_core::PlanPhase;

        // No args → interactive menu (power-user keeps `/plan start|status|exit`).
        if parts.len() == 1 {
            let opts: Vec<(String, String, String)> = match self.ai_handler.plan_phase() {
                PlanPhase::Normal => vec![
                    ("start".to_string(), t("shell.menu.plan.start"), t("shell.menu.plan.start_desc")),
                    ("status".to_string(), t("shell.menu.plan.status"), t("shell.menu.plan.status_desc")),
                ],
                PlanPhase::Planning => vec![
                    ("status".to_string(), t("shell.menu.plan.status"), t("shell.menu.plan.status_desc")),
                    ("exit".to_string(), t("shell.menu.plan.exit"), t("shell.menu.plan.exit_desc")),
                ],
            };
            let action = match pick_menu(&t("shell.menu.plan.title"), &opts) {
                Some(a) => a,
                None => return,
            };
            return self.handle_plan_command(&["/plan", action.as_str()]);
        }

        if parts.len() > 1 && (parts[1] == "--help" || parts[1] == "-h") {
            println!("{}", theme::accent("Usage: /plan [start|status|exit]"));
            return;
        }

        let plan_state = self.ai_handler.plan_state();
        let current_phase = self.ai_handler.plan_phase();
        let subcommand = parts.get(1).copied().unwrap_or("");

        // Reject unknown subcommands
        if !subcommand.is_empty() && !["start", "status", "exit"].contains(&subcommand) {
            eprintln!(
                "{}",
                theme::error(&format!("Unknown /plan subcommand: {}", subcommand))
            );
            return;
        }

        match current_phase {
            PlanPhase::Planning => {
                match subcommand {
                    "exit" => {
                        self.ai_handler.exit_plan_mode();
                        println!("{}", theme::warning("Exited plan mode."));
                    }
                    _ => {
                        // Bare `/plan` or `/plan status` while planning → show status
                        let plan_id = plan_state.plan_id.as_deref().unwrap_or("unknown");
                        println!("{}", theme::accent(&theme::bold("Plan Mode (active)")));
                        println!("  Plan ID: {}", plan_id);
                        println!("  Approval: {}", plan_state.approval_status);
                        println!(
                            "  Artifact: {}",
                            plan_state.artifact_path.as_deref().unwrap_or("-")
                        );
                    }
                }
            }
            PlanPhase::Normal => {
                match subcommand {
                    "exit" => {
                        println!("mode=shell, approval_status=draft, artifact=-");
                    }
                    _ => {
                        // `/plan` or `/plan start` from shell mode → enter planning
                        self.ai_handler.enter_plan_mode(&self.session_uuid);
                        let plan_state = self.ai_handler.plan_state();
                        let plan_id = plan_state.plan_id.as_deref().unwrap_or("unknown");
                        println!("{}", theme::accent(&theme::bold("=== Plan Mode ===")));
                        println!("{}", theme::faint(&format!("Plan ID: {}", plan_id)));
                        println!("{}", theme::faint("During planning, the AI has access to read-only tools and write_file/edit_file for the plan artifact."));
                        println!(
                            "{}",
                            theme::faint("Type ; followed by your planning request to start.")
                        );
                    }
                }
            }
        }
    }

    /// Handle `/token` — show cumulative token usage statistics (last 7 days).
    fn handle_token_command(&self) {
        let stats = self.ai_handler.token_stats();
        let total = stats.total_input + stats.total_output;
        println!();
        println!("{}", aish_i18n::t("shell.token.title"));
        println!(
            "  {}  {}",
            aish_i18n::t("shell.token.input_tokens"),
            format_number(stats.total_input)
        );
        println!(
            "  {} {}",
            aish_i18n::t("shell.token.output_tokens"),
            format_number(stats.total_output)
        );
        println!(
            "  {}     {}",
            aish_i18n::t("shell.token.total"),
            format_number(total)
        );
        println!(
            "  {}  {}",
            aish_i18n::t("shell.token.api_calls"),
            format_number(stats.request_count)
        );
        println!();
    }

    /// Interactive setup wizard for configuring provider, API key, model, etc.
    fn run_setup_wizard(&mut self) {
        let config_dir = aish_config::ConfigLoader::default_config_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("aish")
            });

        let mut wizard = crate::wizard::SetupWizard::new(config_dir);
        match wizard.run() {
            Ok(new_config) => {
                self.config = crate::wizard::apply_setup_result(&self.config, new_config);
                // Update LLM session with new config
                self.ai_handler.update_model(
                    &self.config.model,
                    Some(&self.config.api_base),
                    Some(&self.config.api_key),
                );
                self.refresh_config_dependent_tools();
                let mut args = std::collections::HashMap::new();
                args.insert("model".to_string(), self.config.model.clone());
                println!(
                    "\n{}",
                    theme::success(&t_with_args("shell.setup.applied", &args))
                );
            }
            Err(aish_core::AishError::Cancelled) => {
                eprintln!("{}", theme::warning(&t("shell.setup.cancelled")));
            }
            Err(e) => {
                eprintln!("{}", theme::error(&e.to_string()));
            }
        }
    }

    /// Execute an external command via the persistent PTY session.
    fn execute_external_command(&mut self, command: &str) -> i32 {
        // Record user command input before execution
        crate::recorder::shared_record_input(&self.shared_recorder, &format!("{}\n", command));

        // Sync terminal size before each command
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            self.lock_pty().resize(rows, cols);
        }

        // Ensure the PTY is alive before sending a command.
        if !self.lock_pty().is_running() {
            self.restart_pty();
        }

        // Send command via PTY (release the lock inside the block so the
        // MutexGuard is dropped before any potential restart_pty() call).
        //
        // For session commands (ssh, telnet) the PTY output must be recorded
        // in real-time so that the cast file reflects the correct timing and
        // order.  For normal (short-lived) commands, real-time recording would
        // suppress output during the AI processing window inside
        // send_command_interactive, so we fall back to post-return recording
        // instead.
        let is_session = aish_pty::is_interactive_command(command);
        let remote_host = extract_remote_host(command);

        // Session entry banner: when the user runs ssh/telnet/mosh/etc, the
        // remote shell's own prompt (e.g. bash's `[root@host ~]#`) gives no
        // hint that the terminal has crossed into a remote session. Print a
        // yellow marker just before handing control to the PTY so the user
        // can see which host they are about to enter.
        if is_session {
            if let Some(host) = remote_host.as_deref() {
                let marker = theme::warning(&format!("[ssh:{}]", host));
                eprintln!("{}", marker);
                // Mirror to the session recorder so cast replay shows the
                // same banner as the live terminal — without this, replay
                // silently drops the marker and diverges from what the user
                // actually saw when entering the session.
                crate::recorder::shared_record_output(
                    &self.shared_recorder,
                    &format!("{}\r\n", marker),
                );
            }
        }

        let result = {
            let mut pty = self.lock_pty();
            if is_session {
                if let Some(ref host) = remote_host {
                    *self
                        .current_remote_host
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(host.clone());
                }
            }
            let shared_host = self.current_remote_host.clone();
            let ai_cb = Self::build_session_ai_callback(
                &self.config,
                &self.animation,
                shared_host.clone(),
                self.shared_recorder.clone(),
                self.audit_store.clone().map(|s| s as Arc<dyn AuditSink>),
                {
                    let scanner = self.security_manager.secret_scanner().clone();
                    Some(Arc::new(move |text: &str| {
                        aish_security::secret::redact_secrets(text, &scanner)
                    })
                        as Arc<dyn Fn(&str) -> String + Send + Sync>)
                },
                self.session_uuid.clone(),
                self.audit_user.clone(),
            );
            let on_output: Option<Box<dyn Fn(&str) + Send>> = if is_session {
                let recorder = self.shared_recorder.clone();
                Some(Box::new(move |data: &str| {
                    crate::recorder::shared_record_output(&recorder, data);
                }))
            } else {
                None
            };
            let version_for_status = self.version.clone();
            let session_for_status = self.session_uuid.clone();
            let model_for_status = self.config.model.clone();
            let status_cb: Option<Box<aish_pty::StatusCallback>> = if is_session {
                Some(Box::new(move |exec: &mut aish_pty::RemoteExecFn| {
                    crate::status::run_status_remote(
                        exec,
                        &version_for_status,
                        &session_for_status,
                        &model_for_status,
                    )
                }))
            } else {
                None
            };
            pty.send_command_interactive(
                command,
                ai_cb,
                status_cb,
                Some(shared_host),
                Some(self.secret_check_closure.clone()),
                Some(self.secret_vault.clone()),
                on_output,
                self.config.input_guard_enabled,
                self.config.enable_remote_git_prompt,
                self.config.remote_rich_prompt,
                self.config.remote_danger_patterns.clone(),
                self.config.remote_show_venv,
                self.config.remote_show_container,
                self.config.remote_show_kube,
            )
        };

        if is_session {
            *self
                .current_remote_host
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }

        let (exit_code, cwd, output) = match result {
            Ok(result) => result,
            Err(e) => {
                let message = {
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), e.to_string());
                    aish_i18n::t_with_args("shell.error.pty_error", &args)
                };
                eprintln!("{}", message);
                self.state.last_output = message;
                // PTY may have died, try restart
                self.restart_pty();
                return 1;
            }
        };

        // Store captured output for error correction and LLM context
        self.state.last_output = output.clone();

        // For normal (non-session) commands, record output after the PTY call
        // returns.  Session commands are recorded in real-time via on_output.
        if !is_session && !output.is_empty() {
            crate::recorder::shared_record_output(&self.shared_recorder, &output);
        }

        // Track whether CWD changed so we can include it in the context entry
        let mut cwd_changed_to: Option<String> = None;

        // Update CWD from PTY event
        if !cwd.is_empty() && cwd != self.state.cwd {
            cwd_changed_to = Some(cwd.clone());
            self.state.prev_cwd = Some(self.state.cwd.clone());
            self.state.cwd = cwd.clone();
            // Sync the actual process CWD so that any spawned subprocesses
            // (e.g., via AI tool execution) inherit the correct directory.
            let _ = std::env::set_current_dir(&cwd);
        }

        // Inject command result into LLM context so AI can reference
        // previous command output in follow-up questions.
        let output_preview = if output.len() > 4096 {
            // Safe UTF-8 truncation: find nearest char boundary
            let end = {
                let mut j = 4096;
                while j > 0 && !output.is_char_boundary(j) {
                    j -= 1;
                }
                j
            };
            &output[..end]
        } else {
            &output
        };

        // Redact secrets from shell context before injecting into LLM context.
        let (safe_output, _) = self
            .secret_vault
            .lock()
            .unwrap()
            .redact_output(output_preview);
        let (safe_command, _) = self.secret_vault.lock().unwrap().redact_output(command);

        let mut entry = format!(
            "[Shell] {}\n<returncode>{}</returncode>\n<output>{}</output>",
            safe_command, exit_code, safe_output
        );
        if let Some(ref new_cwd) = cwd_changed_to {
            entry.push_str(&format!("\n<cwd>{}</cwd>", new_cwd));
        }
        self.ai_handler.add_shell_context(&entry);

        // Check if PTY is still running, restart if not
        if !self.lock_pty().is_running() {
            self.restart_pty();
        }

        exit_code
    }

    /// Silently sync a state-modifying command (cd, export, etc.) to the
    /// persistent PTY bash process so that bash's CWD and env stay in sync
    /// with the Rust shell's tracking. Output is discarded.
    fn sync_command_to_pty(&mut self, command: &str) {
        if !self.lock_pty().is_running() {
            return;
        }
        let _ = self.pty.lock().unwrap().execute_command(
            command,
            std::time::Duration::from_secs(5),
            None,
            false,
        );
    }

    fn sync_state_from_pty_cwd(&mut self) {
        if !self.lock_pty().is_running() {
            return;
        }

        let result =
            self.lock_pty()
                .execute_command("pwd", std::time::Duration::from_secs(2), None, false);

        let Ok((_output, _exit_code, cwd)) = result else {
            return;
        };

        if !cwd.is_empty() && cwd != self.state.cwd {
            self.state.prev_cwd = Some(self.state.cwd.clone());
            self.state.cwd = cwd.clone();
            let _ = std::env::set_current_dir(&cwd);
        }
    }

    /// Restart the PTY session (e.g., after bash exits or crashes).
    fn restart_pty(&mut self) {
        let _ = self.restart_pty_with_notice(true);
    }

    pub fn shutdown(&mut self) {
        self.set_phase(ShellPhase::Exiting);
        self.lock_pty().stop();
    }

    fn restart_pty_with_notice(&mut self, show_notice: bool) -> aish_core::Result<()> {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        match aish_pty::PersistentPty::start(&self.state.cwd, rows, cols) {
            Ok(new_pty) => {
                *self.lock_pty() = new_pty;
                if show_notice {
                    println!("{}", theme::warning("bash session restarted"));
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("{}", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), e.to_string());
                    aish_i18n::t_with_args("shell.error.restart_bash_failed", &args)
                });
                self.state.should_exit = true;
                Err(e)
            }
        }
    }

    /// Get the current shell phase.
    pub fn phase(&self) -> &ShellPhase {
        &self.phase
    }

    /// Transition to a new phase.
    pub fn set_phase(&mut self, phase: ShellPhase) {
        tracing::debug!("Shell phase: {} → {}", self.phase, phase);
        self.phase = phase;
    }

    /// Handle a Ctrl+C interruption.
    /// Returns true if the shell should exit.
    pub fn handle_ctrl_c(&mut self) -> bool {
        let now = std::time::Instant::now();

        match self.interruption {
            InterruptionState::Normal | InterruptionState::Inputting => {
                self.interruption = InterruptionState::ClearPending;
                self.last_ctrl_c = Some(now);
                let msg = format!(
                    "{}\r\n",
                    theme::warning(&format!("({})", aish_i18n::t("shell.ctrl_c_again")))
                );
                crate::recorder::shared_record_output(&self.shared_recorder, &msg);
                println!(
                    "{}",
                    theme::warning(&format!("({})", aish_i18n::t("shell.ctrl_c_again")))
                );
                false
            }
            InterruptionState::ClearPending => {
                if let Some(last) = self.last_ctrl_c {
                    if now.duration_since(last).as_secs() < 1 {
                        self.interruption = InterruptionState::ExitPending;
                        let msg = format!("{}\r\n", theme::warning(&aish_i18n::t("shell.exiting")));
                        crate::recorder::shared_record_output(&self.shared_recorder, &msg);
                        println!("{}", theme::warning(&aish_i18n::t("shell.exiting")));
                        return true;
                    }
                }
                self.interruption = InterruptionState::ClearPending;
                self.last_ctrl_c = Some(now);
                let msg = format!(
                    "{}\r\n",
                    theme::warning(&format!("({})", aish_i18n::t("shell.ctrl_c_again")))
                );
                crate::recorder::shared_record_output(&self.shared_recorder, &msg);
                println!(
                    "{}",
                    theme::warning(&format!("({})", aish_i18n::t("shell.ctrl_c_again")))
                );
                false
            }
            InterruptionState::ExitPending => true,
        }
    }

    /// Reset interruption state to normal.
    pub fn reset_interruption(&mut self) {
        self.interruption = InterruptionState::Normal;
        self.last_ctrl_c = None;
    }

    /// Execute a .aish script file.
    fn execute_script(&mut self, input: &str) -> i32 {
        let parts: Vec<&str> = input.split_whitespace().collect();
        let script_path = match parts.first() {
            Some(p) => p,
            None => return 1,
        };

        // Try to load and parse the script
        let script =
            match aish_scripts::loader::parse_script_file(std::path::Path::new(script_path)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{}", {
                        let mut args = std::collections::HashMap::new();
                        args.insert("script".to_string(), script_path.to_string());
                        args.insert("error".to_string(), e.to_string());
                        aish_i18n::t_with_args("shell.error.load_script_failed", &args)
                    });
                    // Fall back to executing via bash
                    return self.execute_external_command(input);
                }
            };

        // InputGuard: pre-screen every non-comment, non-AI line in the
        // script before any of it reaches the bash executor. Scripts can
        // be downloaded (git clone) or shared, so their contents are NOT
        // trusted user input. Without this gate, an `evil.aish` containing
        // `rm -rf /etc` would execute unfiltered.
        //
        // N2: AI-prompt detection uses `is_ai_call_line` (strict quoted
        // form only). A loose `starts_with("ai ")` would let `ai $(rm -rf /)`
        // skip pre-screen AND fall through to bash, executing the
        // destructive payload.
        for line in script.content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // Only quoted `ai "..."` lines are AI prompts — they're passed
            // to the LLM, not bash, so InputGuard skips them. Anything else
            // starting with `ai` (e.g. `ai $(rm -rf /)`) is treated as a
            // shell command and screened.
            if is_ai_call_line(trimmed) {
                continue;
            }
            if !self.screen_shell_command(trimmed) {
                // Destructive content — abort the whole script. The user
                // gets one Block message naming the offending line.
                return 1;
            }
        }

        // Collect arguments
        let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

        // Check if the script contains any AI calls (uses the same
        // strict quoted-form predicate as the pre-screen — see N2 fix).
        let has_ai_calls = script
            .content
            .lines()
            .any(|line| is_ai_call_line(line.trim()));

        if !has_ai_calls {
            // No AI calls — use ScriptExecutor directly (faster, no async needed)
            let executor = aish_scripts::ScriptExecutor::new();
            let result = executor.execute(&script, &args);

            if !result.output.is_empty() {
                print!("{}", result.output);
            }
            if !result.error.is_empty() {
                eprint!("{}", result.error);
            }

            self.state.last_output = format!("{}{}", result.output, result.error);
            self.apply_script_result(&result);
            return if result.success { 0 } else { result.returncode };
        }

        // Script has AI calls — execute line by line, handling AI calls inline.
        // `ai_call_re()` (module-level helper) is shared with the pre-screen
        // loop above so the skip decision and inline-execution dispatch
        // cannot drift apart (N2 defense).
        let mut returncode = 0;

        // Build runtime env for variable substitution
        let mut script_env: std::collections::HashMap<String, String> = std::env::vars().collect();
        script_env.insert("AISH_SCRIPT_DIR".to_string(), script.base_dir.clone());
        script_env.insert("AISH_SCRIPT_NAME".to_string(), script.metadata.name.clone());
        for (i, arg) in args.iter().enumerate() {
            script_env.insert(format!("AISH_ARG_{}", i), arg.clone());
        }

        // Accumulate non-AI lines into segments and execute as bash
        let mut bash_segment = String::new();

        for line in script.content.lines() {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Check for AI call (uses the same strict quoted-form regex
            // as the pre-screen above — N2 defense).
            if let Some(caps) = ai_call_re().captures(trimmed) {
                // Flush any accumulated bash commands first
                if !bash_segment.is_empty() {
                    returncode = self.flush_bash_segment(&bash_segment, returncode);
                    bash_segment.clear();
                }

                // Execute AI call via the AI handler
                if let Some(prompt) = caps.get(1) {
                    let prompt_str = prompt.as_str();
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    match rt.block_on(self.ai_handler.handle_question(prompt_str)) {
                        Ok(response) => {
                            self.sync_state_from_pty_cwd();
                            print_md_with_recording(&response, &self.shared_recorder);
                            self.persist_session_snapshot();
                            script_env.insert("AISH_LAST_OUTPUT".to_string(), response);
                        }
                        Err(e) => {
                            eprintln!("{}", theme::error(&format!("AI error: {}", e)));
                            returncode = 1;
                        }
                    }
                }
                continue;
            }

            // Accumulate into bash segment
            bash_segment.push_str(line);
            bash_segment.push('\n');
        }

        // Flush remaining bash commands
        if !bash_segment.is_empty() {
            returncode = self.flush_bash_segment(&bash_segment, returncode);
        }

        returncode
    }

    /// Execute accumulated bash commands from a script segment.
    fn flush_bash_segment(&mut self, segment: &str, base_rc: i32) -> i32 {
        let (exit_code, cwd, output) = self
            .pty
            .lock()
            .unwrap()
            .send_command_interactive(
                segment,
                None,
                None,
                None,
                None,
                None,
                None,
                self.config.input_guard_enabled,
                false, // not an SSH session, no git prompt injection
                self.config.remote_rich_prompt,
                self.config.remote_danger_patterns.clone(),
                self.config.remote_show_venv,
                self.config.remote_show_container,
                self.config.remote_show_kube,
            )
            .unwrap_or((-1, self.state.cwd.clone(), String::new()));

        if !output.is_empty() {
            // Basic ANSI stripping: remove escape sequences for display
            let re = regex::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
            let clean = re.replace_all(&output, "").trim_end().to_string();
            if !clean.is_empty() {
                println!("{}", clean);
            }
        }

        if !cwd.is_empty() && cwd != self.state.cwd {
            self.state.prev_cwd = Some(self.state.cwd.clone());
            self.state.cwd = cwd;
        }
        self.state.last_output = output;
        self.state.last_exit_code = exit_code;

        if exit_code != 0 && base_rc == 0 {
            exit_code
        } else {
            base_rc
        }
    }

    /// Apply state changes from a ScriptExecutionResult.
    fn apply_script_result(&mut self, result: &aish_scripts::executor::ScriptExecutionResult) {
        if let Some(ref new_cwd) = result.new_cwd {
            let path = std::path::Path::new(new_cwd);
            if path.is_dir() {
                let _ = std::env::set_current_dir(path);
                self.state.prev_cwd = Some(self.state.cwd.clone());
                self.state.cwd = new_cwd.clone();
            }
        }
        for (key, value) in &result.env_changes {
            std::env::set_var(key, value);
            self.state.env_vars.insert(key.clone(), value.clone());
        }
    }

    /// Create a HostNoteTool with shared host state for SSH sessions.
    fn make_host_note_tool(current_host: Arc<Mutex<Option<String>>>) -> Box<dyn aish_llm::Tool> {
        Box::new(aish_tools::HostNoteTool::new(
            {
                let h = current_host.clone();
                Box::new(move |content: &str| {
                    let host = match h.lock().unwrap().clone() {
                        Some(h) if !h.is_empty() => h,
                        _ => return "No active remote host.".to_string(),
                    };
                    let mut profile = aish_hosts::get_or_create_profile(&host);
                    let id = profile.add_note(content.to_string());
                    match aish_hosts::save_profile(&profile) {
                        Ok(()) => {
                            let mut args = std::collections::HashMap::new();
                            args.insert("id".to_string(), id.to_string());
                            t_with_args("tools.host_note.stored", &args)
                        }
                        Err(e) => format!("Failed to save note: {}", e),
                    }
                })
            },
            {
                let h = current_host.clone();
                Box::new(move || {
                    let host = match h.lock().unwrap().clone() {
                        Some(h) if !h.is_empty() => h,
                        _ => return Vec::new(),
                    };
                    let profile = aish_hosts::get_or_create_profile(&host);
                    profile
                        .notes
                        .iter()
                        .map(|n| aish_tools::HostNoteEntry {
                            id: n.id,
                            content: n.content.clone(),
                        })
                        .collect()
                })
            },
            {
                let h = current_host.clone();
                Box::new(move |keyword: &str| {
                    let host = match h.lock().unwrap().clone() {
                        Some(h) if !h.is_empty() => h,
                        _ => return "No active remote host.".to_string(),
                    };
                    let mut profile = aish_hosts::get_or_create_profile(&host);
                    let removed = profile.remove_notes(keyword);
                    match aish_hosts::save_profile(&profile) {
                        Ok(()) if removed > 0 => {
                            let mut args = std::collections::HashMap::new();
                            args.insert("count".to_string(), removed.to_string());
                            t_with_args("tools.host_note.forgot", &args)
                        }
                        Ok(()) => t("tools.host_note.forgot_none"),
                        Err(e) => format!("Failed to save changes: {}", e),
                    }
                })
            },
        ))
    }

    /// Build an AI callback for session commands (SSH, telnet).
    /// Uses the same oracle prompt as local aish (with remote host context),
    /// streaming output, thinking animation, and ShellRenderer for display.
    /// Build a followup closure that can chain itself for multi-round tool use.
    /// Each invocation: call LLM with command output → render analysis → if
    /// the LLM suggests another command, return Some(AiResponse) with another
    /// followup closure (same builder, shared history).
    fn build_followup_closure(
        api_base: &str,
        api_key: &str,
        model: &str,
        codex_auth_path: Option<&str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        system_msg: &str,
        original_question: &str,
        animation: &Arc<crate::animation::SharedAnimation>,
        history: &Arc<Mutex<Vec<ChatMessage>>>,
        shared_host: Arc<Mutex<Option<String>>>,
        shared_recorder: crate::recorder::SharedRecorder,
    ) -> Box<aish_pty::FollowupCallback> {
        let api_base_f = api_base.to_string();
        let api_key_f = api_key.to_string();
        let model_f = model.to_string();
        let codex_auth_path_f = codex_auth_path.map(str::to_string);
        let system_msg_f = system_msg.to_string();
        let question_f = original_question.to_string();
        let anim_f = animation.clone();
        let history_f = history.clone();
        let shared_host_f = shared_host.clone();
        let shared_recorder = shared_recorder.clone();

        Box::new(
            move |output: &str, _offload_path: Option<&str>| -> Option<aish_pty::AiResponse> {
                let followup_prompt = format!(
                    "I ran the command on the remote host. Here is the output:\n\
                ```\n{}\n```\n\n\
                Original question: {}\n\
                Please analyze the command output. If further action is needed, \
                suggest the next bash command in a ```bash code block. \
                If no further action is needed, just provide a summary.",
                    output, question_f
                );

                // Channel for ask_user communication
                let (event_tx, event_rx) = std::sync::mpsc::channel::<aish_pty::AiEvent>();
                let (answer_tx, answer_rx) = std::sync::mpsc::channel::<aish_pty::AskUserAnswer>();

                // Done signal: LLM thread sends when all rendering is complete.
                let (llm_done_tx, llm_done_rx) = std::sync::mpsc::channel::<()>();

                // Cancellation support for Ctrl+C
                let (token_tx, token_rx) =
                    std::sync::mpsc::channel::<std::sync::Arc<CancellationToken>>();
                let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let cancelled_cb = cancelled.clone();
                let cancelled_fu = cancelled.clone();

                // Shared reasoning state for cleanup after cancellation
                let reasoning_active_main =
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let reasoning_lines_main =
                    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let reasoning_active_cb = reasoning_active_main.clone();
                let reasoning_lines_cb = reasoning_lines_main.clone();

                let followup_start = std::time::Instant::now();

                anim_f.start(&t("shell.status.thinking"));
                let history_snapshot = history_f.lock().unwrap().clone();

                // Spawn LLM thread with ChannelAskUserTool
                let api_base_th = api_base_f.clone();
                let codex_auth_path_th = codex_auth_path_f.clone();
                let api_key_th = api_key_f.clone();
                let model_th = model_f.clone();
                let system_msg_th = system_msg_f.clone();
                let anim_th = anim_f.clone();
                let followup_prompt_th = followup_prompt.clone();
                let question_th = question_f.clone();
                let conversation_history_th = history_f.clone();
                let shared_host_th_f = shared_host_f.clone();
                let shared_recorder_th = shared_recorder.clone();

                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Runtime::new() {
                        Ok(rt) => rt,
                        Err(e) => {
                            let _ = event_tx.send(aish_pty::AiEvent::Done(None));
                            let msg = format!(
                                "\r\n{}\r\n",
                                theme::error(&format!("Followup error: {}", e))
                            );
                            unsafe {
                                nix::libc::write(
                                    nix::libc::STDOUT_FILENO,
                                    msg.as_ptr() as *const nix::libc::c_void,
                                    msg.len(),
                                );
                            }
                            return;
                        }
                    };
                    let mut session = LlmSession::with_context(
                        stream_context_from_parts(
                            &api_base_th,
                            &api_key_th,
                            &model_th,
                            codex_auth_path_th.as_deref(),
                        ),
                        temperature,
                        max_tokens,
                    );
                    // Send cancellation token to main thread
                    let _ = token_tx.send(session.cancellation_token_arc());
                    let anim = anim_th.clone();
                    let reasoning_active = reasoning_active_cb.clone();
                    let reasoning_frame =
                        std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let reasoning_lines_displayed = reasoning_lines_cb.clone();
                    let reasoning_buf = std::sync::Mutex::new(String::new());
                    let compaction_active =
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let compaction_notice_shown =
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let thinking_start_followup =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(std::time::Instant::now())));
                    session.set_event_callback(std::sync::Arc::new(move |event| {
                        // Helper: clear reasoning overlay from terminal
                        let clear_reasoning = || {
                            if reasoning_active.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                let prev = reasoning_lines_displayed
                                    .swap(0, std::sync::atomic::Ordering::SeqCst);
                                if prev > 0 {
                                    use std::io::Write;
                                    print!("\x1b[{}A", prev);
                                    for _ in 0..prev {
                                        print!("\r\x1b[K\n");
                                    }
                                    print!("\x1b[{}A", prev);
                                    let _ = std::io::stdout().flush();
                                }
                                reasoning_buf.lock().unwrap().clear();
                            }
                        };
                        // Bail out if cancelled
                        if cancelled_cb.load(std::sync::atomic::Ordering::SeqCst) {
                            anim.stop();
                            clear_reasoning();
                            return None;
                        }
                        use aish_core::LlmEventType;
                        match event.event_type {
                            LlmEventType::ContextCompactionStart
                                if !compaction_active
                                    .swap(true, std::sync::atomic::Ordering::SeqCst) =>
                            {
                                anim.start(&t("shell.status.compacting_context"));
                            }
                            LlmEventType::ContextCompactionStart => {}
                            LlmEventType::ContextCompactionEnd => {
                                compaction_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                anim.stop();
                                if !compaction_notice_shown
                                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                                {
                                    if let Some(message) = context_compaction_notice(&event) {
                                        println!("{}", theme::faint(&message));
                                    }
                                }
                            }
                            LlmEventType::GenerationStart if react_agent_llm_event(&event) => {}
                            LlmEventType::GenerationStart
                                if crate::llm_event_ui::sub_agent_llm_event(&event) =>
                            {
                                crate::llm_event_ui::print_sub_agent_generation_start(&event);
                            }
                            LlmEventType::GenerationStart => {
                                compaction_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                anim.stop();
                                reasoning_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                reasoning_frame.store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_lines_displayed
                                    .store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_buf.lock().unwrap().clear();
                                anim.start(&t("shell.status.thinking"));
                            }
                            LlmEventType::GenerationEnd if react_agent_llm_event(&event) => {}
                            LlmEventType::GenerationEnd
                                if crate::llm_event_ui::sub_agent_llm_event(&event) => {}
                            LlmEventType::GenerationEnd => {
                                anim.stop();
                                clear_reasoning();
                            }
                            LlmEventType::ContentDelta
                                if crate::llm_event_ui::sub_agent_llm_event(&event) => {}
                            LlmEventType::ContentDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        clear_reasoning();
                                    }
                                }
                            }
                            LlmEventType::ReasoningDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        if !reasoning_active
                                            .load(std::sync::atomic::Ordering::SeqCst)
                                        {
                                            reasoning_active
                                                .store(true, std::sync::atomic::Ordering::SeqCst);
                                        }
                                        let mut buf = reasoning_buf.lock().unwrap();
                                        buf.push_str(delta);
                                        let all_lines: Vec<&str> =
                                            buf.lines().filter(|l| !l.trim().is_empty()).collect();
                                        let display_lines: Vec<&str> = all_lines
                                            .iter()
                                            .rev()
                                            .take(2)
                                            .collect::<Vec<_>>()
                                            .into_iter()
                                            .rev()
                                            .copied()
                                            .collect();
                                        let max_cols = 76usize;
                                        let frame = reasoning_frame
                                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        let spinner = crate::theme::spinner_frame(
                                            crate::theme::SPINNER_STATUS,
                                            frame,
                                        );
                                        let spinner = crate::theme::accent(spinner);
                                        let elapsed_str = thinking_start_followup
                                            .lock()
                                            .unwrap()
                                            .map(|s| {
                                                let e = s.elapsed().as_secs_f64();
                                                if e >= 1.0 {
                                                    let mut args = std::collections::HashMap::new();
                                                    args.insert(
                                                        "elapsed".to_string(),
                                                        format!("{:.1}", e),
                                                    );
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t_with_args(
                                                            "shell.session.thinking_elapsed",
                                                            &args
                                                        )
                                                    )
                                                } else {
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t("shell.session.thinking")
                                                    )
                                                }
                                            })
                                            .unwrap_or_else(|| {
                                                format!(
                                                    " {}",
                                                    aish_i18n::t("shell.session.thinking")
                                                )
                                            });
                                        let prev = reasoning_lines_displayed
                                            .load(std::sync::atomic::Ordering::SeqCst);
                                        let new_count = 1 + display_lines.len();
                                        if prev > 0 {
                                            print!("\x1b[{}A", prev);
                                        }
                                        if display_lines.is_empty() {
                                            print!("\r\x1b[K{}{}...\n", spinner, elapsed_str);
                                        } else {
                                            print!("\r\x1b[K{}{}\n", spinner, elapsed_str);
                                        }
                                        for line in &display_lines {
                                            let truncated =
                                                truncate_display_width(line.trim(), max_cols);
                                            print!("\r\x1b[K{}\n", crate::theme::muted(&truncated));
                                        }
                                        for _ in new_count..prev {
                                            print!("\r\x1b[K\n");
                                        }
                                        if prev > new_count {
                                            print!("\x1b[{}A", prev - new_count);
                                        }
                                        reasoning_lines_displayed
                                            .store(new_count, std::sync::atomic::Ordering::SeqCst);
                                        reasoning_active
                                            .store(true, std::sync::atomic::Ordering::SeqCst);
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            }
                            LlmEventType::ReasoningEnd => {
                                clear_reasoning();
                            }
                            LlmEventType::Error => {
                                anim.stop();
                                clear_reasoning();
                                let err = event
                                    .data
                                    .get("error")
                                    .or_else(|| event.data.get("error_message"))
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("Unknown error");
                                let msg = format!(
                                    "\r\n{}\r\n",
                                    theme::error(&format!("Followup LLM error: {}", err))
                                );
                                unsafe {
                                    nix::libc::write(
                                        nix::libc::STDOUT_FILENO,
                                        msg.as_ptr() as *const nix::libc::c_void,
                                        msg.len(),
                                    );
                                }
                            }
                            _ => {}
                        }
                        None
                    }));
                    // Register channel-based tools for SSH followup
                    session.register_tool(Box::new(aish_tools::ChannelBashTool::new(
                        event_tx.clone(),
                    )));
                    session.register_tool(Box::new(aish_tools::ChannelAskUserTool::new(
                        event_tx.clone(),
                        answer_rx,
                    )));
                    // Register host_note tool for SSH followup sessions
                    session.register_tool(Self::make_host_note_tool(shared_host_th_f.clone()));
                    let result = rt.block_on(async {
                        let user_msg = ChatMessage::user(&followup_prompt_th);
                        session
                            .process_input(&user_msg, &history_snapshot, Some(&system_msg_th), true)
                            .await
                    });
                    let process_result = result.ok();
                    let text = process_result.as_ref().map(|r| r.text.clone());

                    // Update conversation history
                    {
                        let mut h = conversation_history_th.lock().unwrap();
                        h.push(ChatMessage::user(&followup_prompt));
                        if let Some(ref pr) = process_result {
                            h.extend(pr.new_messages.clone());
                            if pr.new_messages.is_empty() {
                                h.push(ChatMessage::assistant(&pr.text));
                            }
                        }
                        truncate_ssh_history(&mut h);
                    }

                    // Render analysis
                    if let Some(ref t) = text {
                        if !t.trim().is_empty() {
                            let _ = std::io::stdout().flush();
                            let mut renderer = crate::renderer::ShellRenderer::new();
                            renderer.set_shared_recorder(shared_recorder_th.clone());
                            renderer.render_separator();
                            renderer.render_markdown(t);
                            let _ = std::io::stdout().flush();
                        }
                    }

                    // Build AiResponse with followup if another command was suggested
                    let next_cmd = text.as_ref().and_then(|t| extract_bash_command(t));
                    let ai_response = if next_cmd.is_some() {
                        let next_followup = Self::build_followup_closure(
                            &api_base_th,
                            &api_key_th,
                            &model_th,
                            codex_auth_path_th.as_deref(),
                            temperature,
                            max_tokens,
                            &system_msg_th,
                            &question_th,
                            &anim_th,
                            &conversation_history_th,
                            shared_host_th_f.clone(),
                            shared_recorder_th.clone(),
                        );
                        Some(aish_pty::AiResponse {
                            command: next_cmd,
                            display_text: String::new(),
                            followup: Some(next_followup),
                            ask_user: None,
                        })
                    } else {
                        None
                    };
                    let _ = event_tx.send(aish_pty::AiEvent::Done(ai_response));
                    let _ = llm_done_tx.send(()); // signal rendering complete
                });

                // Wait for result with Ctrl+C cancellation support
                let session_cancel_token = token_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .ok();
                let result = loop {
                    match event_rx.try_recv() {
                        Ok(aish_pty::AiEvent::Done(ai_response)) => {
                            break ai_response;
                        }
                        Ok(aish_pty::AiEvent::AskUser(request)) => {
                            break Some(aish_pty::AiResponse {
                                command: None,
                                display_text: String::new(),
                                followup: None,
                                ask_user: Some((
                                    request,
                                    aish_pty::AskUserChannel {
                                        answer_sender: answer_tx.clone(),
                                        event_receiver: event_rx,
                                    },
                                )),
                            });
                        }
                        Ok(aish_pty::AiEvent::BashExec {
                            command,
                            output_sender,
                        }) => {
                            let osender = output_sender.clone();
                            let done_rx = std::sync::Mutex::new(llm_done_rx);
                            let cancelled_f = cancelled_fu.clone();
                            let session_cancel_token_f = session_cancel_token.clone();
                            let anim_fu = anim_f.clone();
                            let followup: Box<aish_pty::FollowupCallback> = Box::new(
                            move |captured_output: &str,
                                  offload_path: Option<&str>|
                                  -> Option<aish_pty::AiResponse> {
                                // When the forwarding loop hard-aborts a
                                // followup (Ctrl+C during bashexec), it
                                // passes "Command cancelled by user".  Set
                                // the cancelled flag so the LLM event
                                // callback stops writing to stdout while
                                // the forwarding loop resumes shell I/O.
                                if captured_output.contains("cancelled by user") {
                                    cancelled_f.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                                let _ = osender.send(aish_pty::BashExecResult {
                                    output: captured_output.to_string(),
                                    offload_path: offload_path
                                        .map(|p| p.to_string()),
                                });
                                // Wait for LLM thread to finish rendering,
                                // monitoring stdin for Ctrl+C/ESC.
                                let wait_start = std::time::Instant::now();
                                let wait_timeout =
                                    std::time::Duration::from_secs(120);
                                loop {
                                    if done_rx
                                        .lock()
                                        .unwrap()
                                        .try_recv()
                                        .is_ok()
                                    {
                                        break;
                                    }
                                    if wait_start.elapsed() >= wait_timeout {
                                        break;
                                    }
                                    let mut rfds: nix::libc::fd_set =
                                        unsafe { std::mem::zeroed() };
                                    unsafe {
                                        nix::libc::FD_ZERO(&mut rfds);
                                        nix::libc::FD_SET(
                                            nix::libc::STDIN_FILENO,
                                            &mut rfds,
                                        );
                                    }
                                    let mut tv = nix::libc::timeval {
                                        tv_sec: 0,
                                        tv_usec: 100_000,
                                    };
                                    let sel = unsafe {
                                        nix::libc::select(
                                            nix::libc::STDIN_FILENO + 1,
                                            &mut rfds,
                                            std::ptr::null_mut(),
                                            std::ptr::null_mut(),
                                            &mut tv,
                                        )
                                    };
                                    if sel > 0 {
                                        let mut byte = [0u8; 1];
                                        if unsafe {
                                            nix::libc::read(
                                                nix::libc::STDIN_FILENO,
                                                byte.as_mut_ptr()
                                                    as *mut nix::libc::c_void,
                                                1,
                                            )
                                        } == 1
                                        {
                                            if byte[0] == 0x03 {
                                                cancelled_f.store(
                                                    true,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                                if let Some(ref token) =
                                                    session_cancel_token_f
                                                {
                                                    token.cancel();
                                                }
                                                anim_fu.stop();
                                                let msg = format!(
                                                    "\r{}\r\n",
                                                    theme::warning(&t("shell.command_cancelled"))
                                                );
                                                unsafe {
                                                    nix::libc::write(
                                                        nix::libc::STDOUT_FILENO,
                                                        msg.as_ptr()
                                                            as *mut nix::libc::c_void,
                                                        msg.len(),
                                                    );
                                                }
                                                break;
                                            } else if byte[0] == 0x1b {
                                                let mut ffds: nix::libc::fd_set =
                                                    unsafe { std::mem::zeroed() };
                                                unsafe {
                                                    nix::libc::FD_ZERO(&mut ffds);
                                                    nix::libc::FD_SET(
                                                        nix::libc::STDIN_FILENO,
                                                        &mut ffds,
                                                    );
                                                }
                                                let mut ftv = nix::libc::timeval {
                                                    tv_sec: 0,
                                                    tv_usec: 50_000,
                                                };
                                                let fsel = unsafe {
                                                    nix::libc::select(
                                                        nix::libc::STDIN_FILENO + 1,
                                                        &mut ffds,
                                                        std::ptr::null_mut(),
                                                        std::ptr::null_mut(),
                                                        &mut ftv,
                                                    )
                                                };
                                                if fsel == 0 {
                                                    cancelled_f.store(
                                                        true,
                                                        std::sync::atomic::Ordering::SeqCst,
                                                    );
                                                    if let Some(ref token) =
                                                        session_cancel_token_f
                                                    {
                                                        token.cancel();
                                                    }
                                                    anim_fu.stop();
                                                    let msg = format!(
                                                        "\r{}\r\n",
                                                        theme::warning(&t("shell.command_cancelled"))
                                                    );
                                                    unsafe {
                                                        nix::libc::write(
                                                            nix::libc::STDOUT_FILENO,
                                                            msg.as_ptr()
                                                                as *mut nix::libc::c_void,
                                                            msg.len(),
                                                        );
                                                    }
                                                    break;
                                                }
                                                let mut discard = [0u8; 16];
                                                unsafe {
                                                    nix::libc::read(
                                                        nix::libc::STDIN_FILENO,
                                                        discard.as_mut_ptr()
                                                            as *mut nix::libc::c_void,
                                                        discard.len(),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                None
                            },
                        );
                            break Some(aish_pty::AiResponse {
                                command: Some(command),
                                display_text: String::new(),
                                followup: Some(followup),
                                ask_user: None,
                            });
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    if aish_tools::bash::interactive_input_active() {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                    // Check for Ctrl+C on stdin (non-blocking)
                    let mut rfds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                    unsafe {
                        nix::libc::FD_ZERO(&mut rfds);
                        nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut rfds);
                    }
                    let mut tv = nix::libc::timeval {
                        tv_sec: 0,
                        tv_usec: 100_000,
                    };
                    let sel = unsafe {
                        nix::libc::select(
                            nix::libc::STDIN_FILENO + 1,
                            &mut rfds,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &mut tv,
                        )
                    };
                    if sel > 0 {
                        let mut byte = [0u8; 1];
                        if unsafe {
                            nix::libc::read(
                                nix::libc::STDIN_FILENO,
                                byte.as_mut_ptr() as *mut nix::libc::c_void,
                                1,
                            )
                        } == 1
                        {
                            if byte[0] == 0x03 {
                                // Ctrl+C — cancel
                                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                                if let Some(ref token) = session_cancel_token {
                                    token.cancel();
                                }
                                anim_f.stop();
                                let msg = format!(
                                    "\r{}\r\n",
                                    theme::warning(&t("shell.command_cancelled"))
                                );
                                unsafe {
                                    nix::libc::write(
                                        nix::libc::STDOUT_FILENO,
                                        msg.as_ptr() as *mut nix::libc::c_void,
                                        msg.len(),
                                    );
                                }
                                break None;
                            } else if byte[0] == 0x1b {
                                // ESC — check for follow-up bytes to
                                // distinguish from arrow/function keys.
                                let mut ffds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                                unsafe {
                                    nix::libc::FD_ZERO(&mut ffds);
                                    nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut ffds);
                                }
                                let mut ftv = nix::libc::timeval {
                                    tv_sec: 0,
                                    tv_usec: 50_000,
                                };
                                let fsel = unsafe {
                                    nix::libc::select(
                                        nix::libc::STDIN_FILENO + 1,
                                        &mut ffds,
                                        std::ptr::null_mut(),
                                        std::ptr::null_mut(),
                                        &mut ftv,
                                    )
                                };
                                if fsel == 0 {
                                    // Standalone ESC — cancel
                                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                                    if let Some(ref token) = session_cancel_token {
                                        token.cancel();
                                    }
                                    anim_f.stop();
                                    let msg = format!(
                                        "\r{}\r\n",
                                        theme::warning(&t("shell.command_cancelled"))
                                    );
                                    unsafe {
                                        nix::libc::write(
                                            nix::libc::STDOUT_FILENO,
                                            msg.as_ptr() as *mut nix::libc::c_void,
                                            msg.len(),
                                        );
                                    }
                                    break None;
                                }
                                // Follow-up bytes exist (ANSI escape
                                // sequence) — consume and discard them.
                                let mut discard = [0u8; 16];
                                unsafe {
                                    nix::libc::read(
                                        nix::libc::STDIN_FILENO,
                                        discard.as_mut_ptr() as *mut nix::libc::c_void,
                                        discard.len(),
                                    );
                                }
                            }
                        }
                    }
                    // Check timeout (60s)
                    if followup_start.elapsed() > std::time::Duration::from_secs(60) {
                        anim_f.stop();
                        let msg = format!("\r\n{}\r\n", theme::error("LLM timeout (60s)"));
                        unsafe {
                            nix::libc::write(
                                nix::libc::STDOUT_FILENO,
                                msg.as_ptr() as *const nix::libc::c_void,
                                msg.len(),
                            );
                        }
                        break None;
                    }
                };

                anim_f.stop();

                // Clear residual reasoning overlay
                if reasoning_active_main.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let prev = reasoning_lines_main.load(std::sync::atomic::Ordering::SeqCst);
                    if prev > 0 {
                        use std::io::Write;
                        print!("\x1b[{}A", prev);
                        for _ in 0..prev {
                            print!("\r\x1b[K\n");
                        }
                        print!("\x1b[{}A", prev);
                        reasoning_lines_main.store(0, std::sync::atomic::Ordering::SeqCst);
                        let _ = std::io::stdout().flush();
                    }
                }

                result
            },
        )
    }

    /// Build a closure that checks AI input for secrets.
    /// Returns Some(SshSecretCheckResult) if secrets found, None if clean.
    fn build_secret_check_closure(
        scanner: aish_security::secret::SecretScanner,
    ) -> std::sync::Arc<dyn Fn(&str) -> Option<aish_pty::SshSecretCheckResult> + Send + Sync> {
        std::sync::Arc::new(move |input: &str| {
            let secrets = scanner.scan(input);
            if secrets.is_empty() {
                return None;
            }
            let reasons = secrets
                .iter()
                .map(|s| s.format_reason())
                .collect::<Vec<_>>()
                .join("\n  ");
            let mut args = std::collections::HashMap::new();
            args.insert("reasons".to_string(), reasons);
            let title = aish_i18n::t("shell.security.secret.title");
            let message = aish_i18n::t_with_args("shell.security.secret.detected", &args);
            let warning = format!("{} {}", theme::warning(&format!("? {}:", title)), message);
            Some(aish_pty::SshSecretCheckResult {
                warning,
                detected_secrets: secrets,
            })
        })
    }

    fn build_session_ai_callback(
        config: &aish_config::ConfigModel,
        animation: &Arc<crate::animation::SharedAnimation>,
        shared_host: Arc<Mutex<Option<String>>>,
        shared_recorder: crate::recorder::SharedRecorder,
        audit_sink: Option<Arc<dyn AuditSink>>,
        audit_redactor: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
        audit_session_uuid: String,
        audit_user: Option<String>,
    ) -> Option<Box<aish_pty::AiCallback>> {
        let api_base = config.api_base.clone();
        let api_key = config.api_key.clone();
        let model = config.model.clone();
        let codex_auth_path = config.codex_auth_path.clone();
        let temperature = config.temperature;
        let max_tokens = config.max_tokens;
        let animation = animation.clone();
        let shared_recorder = shared_recorder.clone();
        let audit_sink_cb = audit_sink;
        let audit_redactor_cb = audit_redactor;
        let audit_session_uuid_cb = audit_session_uuid;
        let audit_user_cb = audit_user;

        // Load skills snapshot for SSH sessions (same as local session).
        let ssh_skills_snapshot: std::sync::Arc<
            std::collections::HashMap<String, aish_tools::SkillInfo>,
        > = {
            let mut sm = aish_skills::SkillManager::new();
            let _ = sm.load_all_skills();
            std::sync::Arc::new(
                sm.list_skills()
                    .iter()
                    .map(|s| {
                        (
                            s.metadata.name.clone(),
                            aish_tools::SkillInfo {
                                name: s.metadata.name.clone(),
                                content: s.content.clone(),
                                description: s.metadata.description.clone(),
                                base_dir: s.base_dir.clone(),
                                context: s.metadata.context,
                                agent: s.metadata.agent.clone(),
                                allowed_tools: s.metadata.allowed_tools.clone(),
                            },
                        )
                    })
                    .collect(),
            )
        };
        let ssh_skill_names: Vec<String> = ssh_skills_snapshot.keys().cloned().collect();
        let ssh_skills_description: String = ssh_skills_snapshot
            .values()
            .map(|s| format!("- **{}**: {}", s.name, s.description))
            .collect::<Vec<String>>()
            .join("\n");

        // Build oracle system prompt with remote host context
        let mut prompt_manager = aish_prompts::PromptManager::default_dir();
        prompt_manager.load_all();
        let role_prompt = prompt_manager.get("role").to_string();
        let mut vars = std::collections::HashMap::new();
        vars.insert("role_prompt".to_string(), role_prompt);
        vars.insert("uname_info".to_string(), crate::ai_handler::uname_info());
        vars.insert("user_nickname".to_string(), crate::ai_handler::whoami());
        vars.insert("os_info".to_string(), crate::ai_handler::os_info());
        vars.insert(
            "basic_env_info".to_string(),
            crate::ai_handler::basic_env_info(),
        );
        vars.insert(
            "output_language".to_string(),
            crate::ai_handler::output_language(),
        );
        vars.insert("cwd".to_string(), "~".to_string());
        // Static base prompt — SSH context and dossier are built dynamically
        // inside the closure so nested SSH host changes are reflected.
        let system_msg_base = prompt_manager.render("oracle", &vars);

        // Shared host reference — updated by the forwarding loop when
        // nested SSH connections are detected or disconnected.
        let dossier_host = shared_host.clone();

        // Pre-compute static values for error correction template
        let ec_role_prompt = {
            let mut pm = aish_prompts::PromptManager::default_dir();
            pm.load_all();
            pm.get("role").to_string()
        };
        let ec_uname_info = crate::ai_handler::uname_info();
        let ec_user_nickname = crate::ai_handler::whoami();
        let ec_os_info = crate::ai_handler::os_info();
        let ec_basic_env_info = crate::ai_handler::basic_env_info();
        let ec_output_language = crate::ai_handler::output_language();
        let conversation_history: Arc<Mutex<Vec<ChatMessage>>> = Arc::new(Mutex::new(Vec::new()));

        Some(Box::new(move |query: aish_pty::AiQuery| {
            // Detect error correction mode: user typed just `;` with no
            // question after a command failure.  In this case, the recent
            // output contains the error and we use a dedicated prompt.
            let is_error_correction = query.question.is_empty() && !query.recent_output.is_empty();

            // Build SSH context + dossier dynamically using the current host.
            // This ensures nested SSH host changes are reflected immediately.
            let current_host = dossier_host.lock().unwrap().clone();
            let mut system_msg_with_dossier = system_msg_base.clone();
            if let Some(ref host) = current_host {
                system_msg_with_dossier.push_str(&format!(
                    "\n\n**SSH Remote Session Context (overrides tool list above):** \
                     \n- The user is connected to a remote host **{host}** via SSH. \
                     \n- **Available tools:** `bash`, `ask_user`, `host_note`, `read_file`, `skill`, and `Agent`. \
                     \n- **DO NOT** call python_exec, write_file, edit_file, \
                     grep, glob, or any other tool — they do NOT exist in this session. \
                     \n- `bash` tool runs commands on the remote host. The command will be \
                     shown to the user for confirmation before execution. After execution, \
                     the output will be automatically returned to you for analysis. \
                     \n- `read_file` reads files on the LOCAL machine (where aish runs). \
                     Use it to read offload paths when bash output was offloaded to a local file. \
                     Do NOT use it for files on the remote host — use `bash` with `cat` for those. \
                     \n- `ask_user` asks the user a clarifying question (text_input or choice). \
                     \n- `host_note` tool saves, lists, or deletes notes about this remote host. \
                     When the user shares durable facts about this server (deployed services, \
                     known issues, key paths, assets, important warnings), \
                     proactively call `host_note` with action `store` to save them. These notes \
                     persist across sessions — next time the user connects to this host, you will \
                     automatically see them in the host dossier above. \
                     Do NOT save transient info like current directory, running process PIDs, \
                     or temporary file contents. \
                     \n- `skill` tool invokes a loaded skill plugin by name (use `skill_name` parameter). \
                     Do NOT use 'list' as a skill name — list the skills below instead. \
                     When the user's request matches a skill, invoke it BEFORE generating any other response. \
                     Skills configured with `context: subagent` run in an isolated sub-agent automatically. \
                     Use `Agent(subagent_type=troubleshoot)` for open-ended remote system diagnosis when no more specific skill applies. \
                     \n- **Available skills:**\n{ssh_skills_description} \
                     \n- **For reading/writing/searching files on the REMOTE host:** use `bash` tool with \
                     `cat`, `head`, `tail`, `echo`, `tee`, `grep`, `find`, `awk`, etc. \
                     \n- **For editing remote files:** use `bash` with `sed -i` for replacements, \
                     or `bash` with heredoc (`cat > file << 'EOF'`) for creating/overwriting files. \
                     \n- **IMPORTANT about offload:** when bash output is offloaded (status=offloaded), \
                     the file path is on the LOCAL machine. You MUST use `read_file` tool to read it. \
                     NEVER use `bash` to cat/wc/tail an offload path — that file does NOT exist on the remote host. \
                     \n- When the user asks to run a command, execute it, or check something, \
                     call the `bash` tool directly — do NOT just show the command in a code block."
                ));

                // Load host dossier on each invocation so notes added via
                // ;remember during this session are visible immediately.
                if let Some(profile) = aish_hosts::load_profile(host) {
                    let dossier = profile.format_for_prompt();
                    if !dossier.is_empty() {
                        system_msg_with_dossier.push_str(&format!("\n\n{}", dossier));
                    }
                }
            }

            let (context, effective_system_msg) = if is_error_correction {
                // Use aish's cmd_error template (same as local aish error correction)
                let mut pm = aish_prompts::PromptManager::default_dir();
                pm.load_all();

                // Extract the actual failed command from bash error output
                let failed_cmd = extract_failed_command(&query.recent_output);

                let mut ec_vars = std::collections::HashMap::new();
                ec_vars.insert("role_prompt".to_string(), ec_role_prompt.clone());
                ec_vars.insert("uname_info".to_string(), ec_uname_info.clone());
                ec_vars.insert("user_nickname".to_string(), ec_user_nickname.clone());
                ec_vars.insert("os_info".to_string(), ec_os_info.clone());
                ec_vars.insert("basic_env_info".to_string(), ec_basic_env_info.clone());
                ec_vars.insert("output_language".to_string(), ec_output_language.clone());
                ec_vars.insert("exit_code".to_string(), query.exit_code.to_string());

                // Add remote host environment info for SSH sessions
                let remote_env_info = if let Some(ref host) = current_host {
                    format!(
                        "\n- **Remote host:** {} (commands run on the remote host; analyze and correct based on the remote environment)",
                        host
                    )
                } else {
                    String::new()
                };
                ec_vars.insert("remote_env_info".to_string(), remote_env_info);

                let sys = pm.render("cmd_error", &ec_vars);

                // Include recent_output in the user message so LLM has the actual error
                let ctx = format!(
                    "<command_result>\nCommand: {}\nExit code: {}\n</command_result>\n\n\
                     Recent terminal output:\n{}\n\n\
                     Please analyze the error and suggest a fix.",
                    failed_cmd, query.exit_code, query.recent_output
                );
                (ctx, sys)
            } else if query.recent_output.is_empty() {
                (query.question.clone(), system_msg_with_dossier.clone())
            } else {
                // Put user question first, recent output as reference context
                let ctx = format!(
                    "{}\n\nFor reference, recent terminal output:\n{}",
                    query.question, query.recent_output
                );
                (ctx, system_msg_with_dossier.clone())
            };

            // Start thinking spinner
            animation.start(&t("shell.status.thinking"));
            let thinking_start =
                std::sync::Arc::new(std::sync::Mutex::new(Some(std::time::Instant::now())));

            // Channel for ask_user communication between LLM thread and callback
            let (event_tx, event_rx) = std::sync::mpsc::channel::<aish_pty::AiEvent>();
            let (answer_tx, answer_rx) = std::sync::mpsc::channel::<aish_pty::AskUserAnswer>();

            // Channel to send the session's cancellation token back to the
            // main thread so Ctrl+C can cancel the in-flight LLM request.
            let (token_tx, token_rx) =
                std::sync::mpsc::channel::<std::sync::Arc<CancellationToken>>();
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancelled_t = cancelled.clone();
            let cancelled_fu = cancelled.clone();

            // Done signal: LLM thread sends when all rendering is complete.
            // The followup closure waits on this so the forwarding loop only
            // requests a new PTY prompt AFTER the LLM output is on screen.
            let (llm_done_tx, llm_done_rx) = std::sync::mpsc::channel::<()>();

            // Shared reasoning state — needed by both the LLM event callback
            // (inside the thread) and the main thread (to clear residual
            // reasoning lines before ask_user / normal completion).
            let reasoning_active_main =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reasoning_lines_main = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let reasoning_active_cb = reasoning_active_main.clone();
            let reasoning_lines_cb = reasoning_lines_main.clone();

            let api_base_t = api_base.clone();
            let codex_auth_path_t = codex_auth_path.clone();
            let api_key_f = api_key.clone();
            let model_f = model.clone();
            let animation_t = animation.clone();
            let thinking_start_thread = thinking_start.clone();
            let context_messages_t = conversation_history.lock().unwrap().clone();
            let context_for_thread = context.clone();
            let conversation_history_t = conversation_history.clone();
            let system_msg_t = effective_system_msg.clone();
            let query_question_t = query.question.clone();
            let codex_auth_path_th = codex_auth_path.clone();
            let api_base_th = api_base.clone();
            let api_key_th = api_key.clone();
            let model_th = model.clone();
            let animation_th = animation.clone();
            let conversation_history_th = conversation_history.clone();
            let system_msg_th = effective_system_msg.clone();
            let shared_host_th = dossier_host.clone();
            let skills_snapshot_th = ssh_skills_snapshot.clone();
            let skill_names_th = ssh_skill_names.clone();
            let shared_recorder_th = shared_recorder.clone();
            let audit_sink_th = audit_sink_cb.clone();
            let audit_redactor_th = audit_redactor_cb.clone();
            let audit_session_uuid_th = audit_session_uuid_cb.clone();
            let audit_user_th = audit_user_cb.clone();
            let audit_host_th = dossier_host.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result = rt.block_on(async {
                    let mut session = LlmSession::with_context(
                        stream_context_from_parts(
                            &api_base_t,
                            &api_key_f,
                            &model_f,
                            codex_auth_path_t.as_deref(),
                        ),
                        Some(temperature),
                        max_tokens,
                    );

                    if let Some(ref sink) = audit_sink_th {
                        session.set_audit_context(
                            sink.clone(),
                            audit_redactor_th.clone(),
                            audit_session_uuid_th.clone(),
                            audit_user_th.clone(),
                            Some(audit_host_th.clone()),
                        );
                    }

                    // Send cancellation token to main thread
                    let _ = token_tx.send(session.cancellation_token_arc());
                    // Streaming event callback: only show reasoning overlay,
                    // collect content for formatted rendering after completion
                    let anim = animation_t.clone();
                    let cancelled_cb = cancelled_t.clone();
                    let reasoning_active = reasoning_active_cb.clone();
                    let reasoning_frame =
                        std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let reasoning_lines_displayed = reasoning_lines_cb.clone();
                    let reasoning_buf = std::sync::Mutex::new(String::new());
                    let compaction_active =
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let compaction_notice_shown =
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let thinking_start_r = thinking_start_thread.clone();
                    session.set_event_callback(std::sync::Arc::new(move |event| {
                        // Helper: clear reasoning overlay from terminal
                        let clear_reasoning = || {
                            if reasoning_active.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                let prev = reasoning_lines_displayed
                                    .swap(0, std::sync::atomic::Ordering::SeqCst);
                                if prev > 0 {
                                    use std::io::Write;
                                    print!("\x1b[{}A", prev);
                                    for _ in 0..prev {
                                        print!("\r\x1b[K\n");
                                    }
                                    print!("\x1b[{}A", prev);
                                    let _ = std::io::stdout().flush();
                                }
                                reasoning_buf.lock().unwrap().clear();
                            }
                        };
                        // Bail out if cancelled
                        if cancelled_cb.load(std::sync::atomic::Ordering::SeqCst) {
                            anim.stop();
                            clear_reasoning();
                            return None;
                        }
                        use aish_core::LlmEventType;
                        match event.event_type {
                            LlmEventType::ContextCompactionStart
                                if !compaction_active
                                    .swap(true, std::sync::atomic::Ordering::SeqCst) =>
                            {
                                anim.start(&t("shell.status.compacting_context"));
                            }
                            LlmEventType::ContextCompactionStart => {}
                            LlmEventType::ContextCompactionEnd => {
                                compaction_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                anim.stop();
                                if !compaction_notice_shown
                                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                                {
                                    if let Some(message) = context_compaction_notice(&event) {
                                        println!("{}", theme::faint(&message));
                                    }
                                }
                            }
                            LlmEventType::GenerationStart if react_agent_llm_event(&event) => {}
                            LlmEventType::GenerationStart
                                if crate::llm_event_ui::sub_agent_llm_event(&event) =>
                            {
                                crate::llm_event_ui::print_sub_agent_generation_start(&event);
                            }
                            LlmEventType::GenerationStart => {
                                compaction_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                anim.stop();
                                reasoning_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                reasoning_frame.store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_lines_displayed
                                    .store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_buf.lock().unwrap().clear();
                                anim.start(&t("shell.status.thinking"));
                            }
                            LlmEventType::GenerationEnd if react_agent_llm_event(&event) => {}
                            LlmEventType::GenerationEnd
                                if crate::llm_event_ui::sub_agent_llm_event(&event) => {}
                            LlmEventType::GenerationEnd => {
                                anim.stop();
                                clear_reasoning();
                            }
                            LlmEventType::ContentDelta
                                if crate::llm_event_ui::sub_agent_llm_event(&event) => {}
                            LlmEventType::ContentDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        clear_reasoning();
                                    }
                                }
                            }
                            LlmEventType::ReasoningDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        if !reasoning_active
                                            .load(std::sync::atomic::Ordering::SeqCst)
                                        {
                                            reasoning_active
                                                .store(true, std::sync::atomic::Ordering::SeqCst);
                                        }
                                        let mut buf = reasoning_buf.lock().unwrap();
                                        buf.push_str(delta);
                                        let all_lines: Vec<&str> =
                                            buf.lines().filter(|l| !l.trim().is_empty()).collect();
                                        let display_lines: Vec<&str> = all_lines
                                            .iter()
                                            .rev()
                                            .take(2)
                                            .collect::<Vec<_>>()
                                            .into_iter()
                                            .rev()
                                            .copied()
                                            .collect();
                                        let max_cols = 76usize;
                                        let frame = reasoning_frame
                                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        let spinner = crate::theme::spinner_frame(
                                            crate::theme::SPINNER_STATUS,
                                            frame,
                                        );
                                        let spinner = crate::theme::accent(spinner);
                                        let elapsed_str = thinking_start_r
                                            .lock()
                                            .unwrap()
                                            .map(|s| {
                                                let e = s.elapsed().as_secs_f64();
                                                if e >= 1.0 {
                                                    let mut args = std::collections::HashMap::new();
                                                    args.insert(
                                                        "elapsed".to_string(),
                                                        format!("{:.1}", e),
                                                    );
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t_with_args(
                                                            "shell.session.thinking_elapsed",
                                                            &args
                                                        )
                                                    )
                                                } else {
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t("shell.session.thinking")
                                                    )
                                                }
                                            })
                                            .unwrap_or_else(|| {
                                                format!(
                                                    " {}",
                                                    aish_i18n::t("shell.session.thinking")
                                                )
                                            });
                                        let prev = reasoning_lines_displayed
                                            .load(std::sync::atomic::Ordering::SeqCst);
                                        let new_count = 1 + display_lines.len();
                                        if prev > 0 {
                                            print!("\x1b[{}A", prev);
                                        }
                                        if display_lines.is_empty() {
                                            print!("\r\x1b[K{}{}...\n", spinner, elapsed_str);
                                        } else {
                                            print!("\r\x1b[K{}{}\n", spinner, elapsed_str);
                                        }
                                        for line in &display_lines {
                                            let truncated =
                                                truncate_display_width(line.trim(), max_cols);
                                            print!("\r\x1b[K{}\n", crate::theme::muted(&truncated));
                                        }
                                        for _ in new_count..prev {
                                            print!("\r\x1b[K\n");
                                        }
                                        if prev > new_count {
                                            print!("\x1b[{}A", prev - new_count);
                                        }
                                        reasoning_lines_displayed
                                            .store(new_count, std::sync::atomic::Ordering::SeqCst);
                                        reasoning_active
                                            .store(true, std::sync::atomic::Ordering::SeqCst);
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            }
                            LlmEventType::ReasoningEnd => {
                                clear_reasoning();
                            }
                            LlmEventType::Error => {
                                anim.stop();
                                clear_reasoning();
                                let error_msg = event
                                    .data
                                    .get("error")
                                    .or_else(|| event.data.get("error_message"))
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("Unknown error");
                                eprintln!("{}", theme::error(&format!("LLM error: {}", error_msg)));
                            }
                            LlmEventType::ToolExecutionStart => {
                                let tool_name = event
                                    .data
                                    .get("tool_name")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("unknown");
                                if tool_name == "read_file" {
                                    anim.stop();
                                    clear_reasoning();
                                    let path = event
                                        .data
                                        .get("tool_args")
                                        .and_then(|a| a.get("path"))
                                        .and_then(|p| p.as_str())
                                        .unwrap_or("?");
                                    use std::io::Write;
                                    println!(
                                        "{} {}({})",
                                        crate::theme::accent(crate::theme::TOOL_PREFIX),
                                        crate::theme::muted("read_file"),
                                        path
                                    );
                                    let _ = std::io::stdout().flush();
                                }
                            }
                            LlmEventType::ToolExecutionEnd => {
                                let tool_name = event
                                    .data
                                    .get("tool_name")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("unknown");
                                if tool_name == "read_file" {
                                    let ok = event
                                        .data
                                        .get("ok")
                                        .and_then(|b| b.as_bool())
                                        .unwrap_or(false);
                                    if !ok {
                                        let preview = event
                                            .data
                                            .get("output_preview")
                                            .and_then(|p| p.as_str())
                                            .unwrap_or("error");
                                        use std::io::Write;
                                        println!("{}", theme::error(preview));
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            }
                            _ => {}
                        }
                        None
                    }));

                    // Register channel-based tools for SSH sessions
                    session.register_tool(Box::new(aish_tools::ChannelBashTool::new(
                        event_tx.clone(),
                    )));
                    session.register_tool(Box::new(aish_tools::ChannelAskUserTool::new(
                        event_tx.clone(),
                        answer_rx,
                    )));
                    // Register host_note tool for SSH sessions
                    session.register_tool(Self::make_host_note_tool(shared_host_th.clone()));
                    // Register read_file (restricted to offload paths) for
                    // reading LOCAL offload files in SSH sessions.
                    session.register_tool(Box::new(aish_tools::fs::SshReadFileTool::new()));
                    // Register skill tool with loaded skills snapshot
                    {
                        let snap = skills_snapshot_th.clone();
                        let names = skill_names_th.clone();
                        let lookup = Box::new(move |name: &str| snap.get(name).cloned());
                        let list = Box::new(move || names.clone());
                        session.register_tool(Box::new(aish_tools::SkillTool::new(lookup, list)));
                    }
                    session.register_tool(Box::new(aish_tools::AgentTool::new()));

                    let user_msg_t = ChatMessage::user(&context_for_thread);
                    session
                        .process_input(&user_msg_t, &context_messages_t, Some(&system_msg_t), true)
                        .await
                });
                if cancelled_t.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let process_result = result.ok();
                let text = process_result.as_ref().map(|r| r.text.clone());

                // Update conversation history
                {
                    let mut h = conversation_history_t.lock().unwrap();
                    h.push(ChatMessage::user(&context_for_thread));
                    if let Some(ref pr) = process_result {
                        h.extend(pr.new_messages.clone());
                        if pr.new_messages.is_empty() {
                            h.push(ChatMessage::assistant(&pr.text));
                        }
                    }
                    truncate_ssh_history(&mut h);
                }

                // Build AiResponse from text
                let ai_response = match text {
                    Some(ref t) if is_error_correction => {
                        // Render description
                        let ec_result = crate::ai_handler::parse_error_correction_response(t);
                        if let Some(ref desc) = ec_result.description {
                            if !desc.trim().is_empty() {
                                let _ = std::io::stdout().flush();
                                let mut renderer = crate::renderer::ShellRenderer::new();
                                renderer.set_shared_recorder(shared_recorder_th.clone());
                                renderer.render_separator();
                                renderer.render_markdown(desc);
                                let _ = std::io::stdout().flush();
                            }
                        }
                        Some(aish_pty::AiResponse {
                            command: ec_result.command,
                            display_text: String::new(),
                            followup: None,
                            ask_user: None,
                        })
                    }
                    Some(t) => {
                        // Render formatted markdown
                        if !t.trim().is_empty() {
                            let _ = std::io::stdout().flush();
                            let mut renderer = crate::renderer::ShellRenderer::new();
                            renderer.set_shared_recorder(shared_recorder_th.clone());
                            renderer.render_separator();
                            renderer.render_markdown(t.trim());
                            let elapsed = thinking_start_thread
                                .lock()
                                .unwrap()
                                .map(|s| s.elapsed().as_secs_f64())
                                .unwrap_or(0.0);
                            if elapsed >= 0.1 {
                                let mut elapsed_args = std::collections::HashMap::new();
                                elapsed_args.insert("time".to_string(), format!("{:.1}", elapsed));
                                println!(
                                    "{}",
                                    theme::faint(&aish_i18n::t_with_args(
                                        "shell.thinking_time",
                                        &elapsed_args
                                    ))
                                );
                            }
                            renderer.render_separator();
                            let _ = std::io::stdout().flush();
                        }
                        let command = extract_bash_command(&t);
                        let followup = command.as_ref().map(|_cmd| {
                            Self::build_followup_closure(
                                &api_base_th,
                                &api_key_th,
                                &model_th,
                                codex_auth_path_th.as_deref(),
                                Some(temperature),
                                max_tokens,
                                &system_msg_th,
                                &query_question_t,
                                &animation_th,
                                &conversation_history_th,
                                shared_host_th.clone(),
                                shared_recorder_th.clone(),
                            )
                        });
                        Some(aish_pty::AiResponse {
                            command,
                            display_text: String::new(),
                            followup,
                            ask_user: None,
                        })
                    }
                    None => Some(aish_pty::AiResponse {
                        command: None,
                        display_text: theme::warning(&aish_i18n::t("shell.session.ai_error")),
                        followup: None,
                        ask_user: None,
                    }),
                };
                let _ = event_tx.send(aish_pty::AiEvent::Done(ai_response));
                let _ = llm_done_tx.send(()); // signal rendering complete
            });

            // Wait for result with Ctrl+C cancellation support
            // Also handles ask_user events from the LLM thread
            enum CallbackEvent {
                Done(Option<aish_pty::AiResponse>),
                AskUser(aish_pty::AskUserRequest, aish_pty::AskUserChannel),
                BashExec {
                    command: String,
                    output_sender: std::sync::mpsc::Sender<aish_pty::BashExecResult>,
                    event_receiver: std::sync::mpsc::Receiver<aish_pty::AiEvent>,
                },
            }
            // Receive the session's cancellation token (sent by the LLM thread)
            let session_cancel_token = token_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .ok();
            let cb_event = loop {
                match event_rx.try_recv() {
                    Ok(aish_pty::AiEvent::Done(ai_response)) => {
                        break Some(CallbackEvent::Done(ai_response));
                    }
                    Ok(aish_pty::AiEvent::AskUser(request)) => {
                        break Some(CallbackEvent::AskUser(
                            request,
                            aish_pty::AskUserChannel {
                                answer_sender: answer_tx.clone(),
                                event_receiver: event_rx,
                            },
                        ));
                    }
                    Ok(aish_pty::AiEvent::BashExec {
                        command,
                        output_sender,
                    }) => {
                        break Some(CallbackEvent::BashExec {
                            command,
                            output_sender,
                            event_receiver: event_rx,
                        });
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                if aish_tools::bash::interactive_input_active() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                // Check for Ctrl+C on stdin (non-blocking)
                let mut rfds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                unsafe {
                    nix::libc::FD_ZERO(&mut rfds);
                    nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut rfds);
                }
                let mut tv = nix::libc::timeval {
                    tv_sec: 0,
                    tv_usec: 100_000,
                }; // 100ms
                let sel = unsafe {
                    nix::libc::select(
                        nix::libc::STDIN_FILENO + 1,
                        &mut rfds,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut tv,
                    )
                };
                if sel > 0 {
                    let mut byte = [0u8; 1];
                    if unsafe {
                        nix::libc::read(
                            nix::libc::STDIN_FILENO,
                            byte.as_mut_ptr() as *mut nix::libc::c_void,
                            1,
                        )
                    } == 1
                    {
                        if byte[0] == 0x03 {
                            // Ctrl+C pressed — cancel the LLM request
                            cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                            if let Some(ref token) = session_cancel_token {
                                token.cancel();
                            }
                            animation.stop();
                            println!("{}", theme::warning(&t("shell.command_cancelled")));
                            break None;
                        } else if byte[0] == 0x1b {
                            // ESC — check for follow-up bytes (arrow/function
                            // keys send ESC + additional bytes).
                            let mut ffds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                            unsafe {
                                nix::libc::FD_ZERO(&mut ffds);
                                nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut ffds);
                            }
                            let mut ftv = nix::libc::timeval {
                                tv_sec: 0,
                                tv_usec: 50_000,
                            };
                            let fsel = unsafe {
                                nix::libc::select(
                                    nix::libc::STDIN_FILENO + 1,
                                    &mut ffds,
                                    std::ptr::null_mut(),
                                    std::ptr::null_mut(),
                                    &mut ftv,
                                )
                            };
                            if fsel == 0 {
                                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                                if let Some(ref token) = session_cancel_token {
                                    token.cancel();
                                }
                                animation.stop();
                                println!("\r\n{}", theme::warning(&t("shell.command_cancelled")));
                                break None;
                            }
                            // Follow-up bytes exist (ANSI escape
                            // sequence) — consume and discard them.
                            let mut discard = [0u8; 16];
                            unsafe {
                                nix::libc::read(
                                    nix::libc::STDIN_FILENO,
                                    discard.as_mut_ptr() as *mut nix::libc::c_void,
                                    discard.len(),
                                );
                            }
                        }
                        // Other bytes during AI processing — discard.
                    }
                }
                // Check timeout (60s)
                if thinking_start
                    .lock()
                    .unwrap()
                    .is_some_and(|s| s.elapsed() > std::time::Duration::from_secs(60))
                {
                    animation.stop();
                    eprintln!("{}", theme::error("LLM timeout (60s)"));
                    break None;
                }
            };

            animation.stop();

            // Clear any residual reasoning lines left on screen (the LLM
            // event callback may have shown reasoning deltas that were not
            // erased because GenerationEnd / ReasoningEnd haven't fired yet).
            if reasoning_active_main.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let prev = reasoning_lines_main.load(std::sync::atomic::Ordering::SeqCst);
                if prev > 0 {
                    use std::io::Write;
                    print!("\x1b[{}A", prev);
                    for _ in 0..prev {
                        print!("\r\x1b[K\n");
                    }
                    print!("\x1b[{}A", prev);
                    reasoning_lines_main.store(0, std::sync::atomic::Ordering::SeqCst);
                    let _ = std::io::stdout().flush();
                }
            }

            // Handle the callback event
            match cb_event {
                // Ask_user — return response with ask_user channel
                Some(CallbackEvent::AskUser(request, channel)) => Some(aish_pty::AiResponse {
                    command: None,
                    display_text: String::new(),
                    followup: None,
                    ask_user: Some((request, channel)),
                }),
                // Bash_exec — return command for execution on remote host,
                // with multi-round chaining support.
                Some(CallbackEvent::BashExec {
                    command,
                    output_sender,
                    event_receiver,
                }) => {
                    let shared_event_rx =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(event_receiver)));
                    let shared_done_rx =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(llm_done_rx)));
                    let answer_tx_f = answer_tx.clone();

                    fn make_chain_followup(
                        event_rx: std::sync::Arc<
                            std::sync::Mutex<Option<std::sync::mpsc::Receiver<aish_pty::AiEvent>>>,
                        >,
                        done_rx: std::sync::Arc<
                            std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
                        >,
                        answer_tx: std::sync::mpsc::Sender<aish_pty::AskUserAnswer>,
                        output_sender: std::sync::mpsc::Sender<aish_pty::BashExecResult>,
                        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
                        cancel_token: Option<std::sync::Arc<CancellationToken>>,
                        animation: std::sync::Arc<crate::animation::SharedAnimation>,
                    ) -> Box<aish_pty::FollowupCallback> {
                        Box::new(
                            move |captured_output: &str,
                                  offload_path: Option<&str>|
                                  -> Option<aish_pty::AiResponse> {
                                // When the forwarding loop hard-aborts (Ctrl+C
                                // during bashexec) or the user rejects the
                                // command, cancel the LLM session and stop
                                // the tool chain immediately.
                                if captured_output.contains("cancelled by user")
                                    || captured_output.contains("rejected by user")
                                {
                                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                                    let _ = output_sender.send(aish_pty::BashExecResult {
                                        output: captured_output.to_string(),
                                        offload_path: offload_path.map(|p| p.to_string()),
                                    });
                                    if let Some(ref token) = cancel_token {
                                        token.cancel();
                                    }
                                    // Drain remaining events so the LLM thread
                                    // can finish cleanly.
                                    if let Some(rx) = event_rx.lock().unwrap().take() {
                                        drop(rx);
                                    }
                                    return None;
                                }
                                let _ = output_sender.send(aish_pty::BashExecResult {
                                    output: captured_output.to_string(),
                                    offload_path: offload_path.map(|p| p.to_string()),
                                });

                                let rx = match event_rx.lock().unwrap().take() {
                                    Some(rx) => rx,
                                    None => return None,
                                };

                                let chain_start = std::time::Instant::now();
                                let chain_timeout = std::time::Duration::from_secs(120);
                                let mut chain_result: Option<Option<aish_pty::AiResponse>> = None;
                                while chain_start.elapsed() < chain_timeout {
                                    match rx.try_recv() {
                                        Ok(aish_pty::AiEvent::BashExec {
                                            command,
                                            output_sender: new_sender,
                                        }) => {
                                            *event_rx.lock().unwrap() = Some(rx);
                                            chain_result = Some(Some(aish_pty::AiResponse {
                                                command: Some(command),
                                                display_text: String::new(),
                                                followup: Some(make_chain_followup(
                                                    event_rx.clone(),
                                                    done_rx.clone(),
                                                    answer_tx.clone(),
                                                    new_sender,
                                                    cancelled.clone(),
                                                    cancel_token.clone(),
                                                    animation.clone(),
                                                )),
                                                ask_user: None,
                                            }));
                                            break;
                                        }
                                        Ok(aish_pty::AiEvent::AskUser(request)) => {
                                            chain_result = Some(Some(aish_pty::AiResponse {
                                                command: None,
                                                display_text: String::new(),
                                                followup: None,
                                                ask_user: Some((
                                                    request,
                                                    aish_pty::AskUserChannel {
                                                        answer_sender: answer_tx.clone(),
                                                        event_receiver: rx,
                                                    },
                                                )),
                                            }));
                                            break;
                                        }
                                        Ok(aish_pty::AiEvent::Done(_)) => {
                                            if let Some(drx) = done_rx.lock().unwrap().take() {
                                                let done_start = std::time::Instant::now();
                                                loop {
                                                    if drx.try_recv().is_ok() {
                                                        break;
                                                    }
                                                    if done_start.elapsed() >= chain_timeout {
                                                        break;
                                                    }
                                                    let mut dfds: nix::libc::fd_set =
                                                        unsafe { std::mem::zeroed() };
                                                    unsafe {
                                                        nix::libc::FD_ZERO(&mut dfds);
                                                        nix::libc::FD_SET(
                                                            nix::libc::STDIN_FILENO,
                                                            &mut dfds,
                                                        );
                                                    }
                                                    let mut dtv = nix::libc::timeval {
                                                        tv_sec: 0,
                                                        tv_usec: 100_000,
                                                    };
                                                    let dsel = unsafe {
                                                        nix::libc::select(
                                                            nix::libc::STDIN_FILENO + 1,
                                                            &mut dfds,
                                                            std::ptr::null_mut(),
                                                            std::ptr::null_mut(),
                                                            &mut dtv,
                                                        )
                                                    };
                                                    if dsel > 0 {
                                                        let mut byte = [0u8; 1];
                                                        if unsafe {
                                                            nix::libc::read(
                                                                nix::libc::STDIN_FILENO,
                                                                byte.as_mut_ptr()
                                                                    as *mut nix::libc::c_void,
                                                                1,
                                                            )
                                                        } == 1
                                                        {
                                                            if byte[0] == 0x03 {
                                                                cancelled.store(
                                                                    true,
                                                                    std::sync::atomic::Ordering::SeqCst,
                                                                );
                                                                if let Some(ref token) =
                                                                    cancel_token
                                                                {
                                                                    token.cancel();
                                                                }
                                                                animation.stop();
                                                                let msg = format!(
                                                                    "\r{}\r\n",
                                                                    theme::warning(&t(
                                                                        "shell.command_cancelled"
                                                                    ))
                                                                );
                                                                unsafe {
                                                                    nix::libc::write(
                                                                        nix::libc::STDOUT_FILENO,
                                                                        msg.as_ptr()
                                                                            as *mut nix::libc::c_void,
                                                                        msg.len(),
                                                                    );
                                                                }
                                                                break;
                                                            } else if byte[0] == 0x1b {
                                                                let mut ffds: nix::libc::fd_set =
                                                                    unsafe { std::mem::zeroed() };
                                                                unsafe {
                                                                    nix::libc::FD_ZERO(&mut ffds);
                                                                    nix::libc::FD_SET(
                                                                        nix::libc::STDIN_FILENO,
                                                                        &mut ffds,
                                                                    );
                                                                }
                                                                let mut ftv = nix::libc::timeval {
                                                                    tv_sec: 0,
                                                                    tv_usec: 50_000,
                                                                };
                                                                let fsel = unsafe {
                                                                    nix::libc::select(
                                                                        nix::libc::STDIN_FILENO + 1,
                                                                        &mut ffds,
                                                                        std::ptr::null_mut(),
                                                                        std::ptr::null_mut(),
                                                                        &mut ftv,
                                                                    )
                                                                };
                                                                if fsel == 0 {
                                                                    cancelled.store(
                                                                        true,
                                                                        std::sync::atomic::Ordering::SeqCst,
                                                                    );
                                                                    if let Some(ref token) =
                                                                        cancel_token
                                                                    {
                                                                        token.cancel();
                                                                    }
                                                                    animation.stop();
                                                                    let msg = format!(
                                                                        "\r{}\r\n",
                                                                        theme::warning(&t("shell.command_cancelled"))
                                                                    );
                                                                    unsafe {
                                                                        nix::libc::write(
                                                                            nix::libc::STDOUT_FILENO,
                                                                            msg.as_ptr()
                                                                                as *mut nix::libc::c_void,
                                                                            msg.len(),
                                                                        );
                                                                    }
                                                                    break;
                                                                }
                                                                let mut discard = [0u8; 16];
                                                                unsafe {
                                                                    nix::libc::read(
                                                                        nix::libc::STDIN_FILENO,
                                                                        discard.as_mut_ptr()
                                                                            as *mut nix::libc::c_void,
                                                                        discard.len(),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            chain_result = Some(None);
                                            break;
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                            chain_result = Some(None);
                                            break;
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                                    }
                                    let mut rfds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                                    unsafe {
                                        nix::libc::FD_ZERO(&mut rfds);
                                        nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut rfds);
                                    }
                                    let mut tv = nix::libc::timeval {
                                        tv_sec: 0,
                                        tv_usec: 100_000,
                                    };
                                    let sel = unsafe {
                                        nix::libc::select(
                                            nix::libc::STDIN_FILENO + 1,
                                            &mut rfds,
                                            std::ptr::null_mut(),
                                            std::ptr::null_mut(),
                                            &mut tv,
                                        )
                                    };
                                    if sel > 0 {
                                        let mut byte = [0u8; 1];
                                        if unsafe {
                                            nix::libc::read(
                                                nix::libc::STDIN_FILENO,
                                                byte.as_mut_ptr() as *mut nix::libc::c_void,
                                                1,
                                            )
                                        } == 1
                                        {
                                            if byte[0] == 0x03 {
                                                cancelled.store(
                                                    true,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                                if let Some(ref token) = cancel_token {
                                                    token.cancel();
                                                }
                                                animation.stop();
                                                let msg = format!(
                                                    "\r{}\r\n",
                                                    theme::warning(&t("shell.command_cancelled"))
                                                );
                                                unsafe {
                                                    nix::libc::write(
                                                        nix::libc::STDOUT_FILENO,
                                                        msg.as_ptr() as *mut nix::libc::c_void,
                                                        msg.len(),
                                                    );
                                                }
                                                chain_result = Some(None);
                                                break;
                                            } else if byte[0] == 0x1b {
                                                let mut ffds: nix::libc::fd_set =
                                                    unsafe { std::mem::zeroed() };
                                                unsafe {
                                                    nix::libc::FD_ZERO(&mut ffds);
                                                    nix::libc::FD_SET(
                                                        nix::libc::STDIN_FILENO,
                                                        &mut ffds,
                                                    );
                                                }
                                                let mut ftv = nix::libc::timeval {
                                                    tv_sec: 0,
                                                    tv_usec: 50_000,
                                                };
                                                let fsel = unsafe {
                                                    nix::libc::select(
                                                        nix::libc::STDIN_FILENO + 1,
                                                        &mut ffds,
                                                        std::ptr::null_mut(),
                                                        std::ptr::null_mut(),
                                                        &mut ftv,
                                                    )
                                                };
                                                if fsel == 0 {
                                                    cancelled.store(
                                                        true,
                                                        std::sync::atomic::Ordering::SeqCst,
                                                    );
                                                    if let Some(ref token) = cancel_token {
                                                        token.cancel();
                                                    }
                                                    animation.stop();
                                                    let msg = format!(
                                                        "\r{}\r\n",
                                                        theme::warning(&t(
                                                            "shell.command_cancelled"
                                                        ))
                                                    );
                                                    unsafe {
                                                        nix::libc::write(
                                                            nix::libc::STDOUT_FILENO,
                                                            msg.as_ptr() as *mut nix::libc::c_void,
                                                            msg.len(),
                                                        );
                                                    }
                                                    chain_result = Some(None);
                                                    break;
                                                }
                                                let mut discard = [0u8; 16];
                                                unsafe {
                                                    nix::libc::read(
                                                        nix::libc::STDIN_FILENO,
                                                        discard.as_mut_ptr()
                                                            as *mut nix::libc::c_void,
                                                        discard.len(),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                chain_result.unwrap_or(None)
                            },
                        )
                    }

                    let followup = make_chain_followup(
                        shared_event_rx,
                        shared_done_rx,
                        answer_tx_f,
                        output_sender,
                        cancelled_fu,
                        session_cancel_token,
                        animation.clone(),
                    );
                    Some(aish_pty::AiResponse {
                        command: Some(command),
                        display_text: String::new(),
                        followup: Some(followup),
                        ask_user: None,
                    })
                }
                // Normal completion — AiResponse already built by LLM thread
                Some(CallbackEvent::Done(ai_response)) => ai_response,
                None => None,
            }
        }))
    }
}

/// Cached regex for stripping complete XML tags from tool output.
static TOOL_XML_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
/// Cached regex for removing trailing tool metadata blocks.
static TOOL_XML_TRAILING_METADATA_RE: std::sync::OnceLock<regex::Regex> =
    std::sync::OnceLock::new();
/// Cached regex for removing incomplete tags from truncation.
static TOOL_XML_INCOMPLETE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

/// Cached regex identifying an AI-prompt line in `.aish` scripts. Only
/// strict quoted form (`ai "..."` / `ai '...'`) qualifies; anything else
/// starting with `ai` (e.g. `ai $(rm -rf /)`) is treated as a shell
/// command and routed through InputGuard. Hoisted to module scope + cached
/// so it can be unit-tested directly and isn't recompiled per script run.
static AI_CALL_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

/// Returns the shared `AI_CALL_RE`, compiling it on first use.
fn ai_call_re() -> &'static regex::Regex {
    AI_CALL_RE.get_or_init(|| {
        regex::Regex::new(r#"^\s*ai\s+["']([^"']+)["']\s*$"#).expect("AI_CALL_RE pattern is valid")
    })
}

/// Returns true iff `line` is a strict AI-prompt line (`ai "..."`/`ai '...'`).
/// Used by `execute_script`'s pre-screen skip and inline-execution dispatch
/// so the two decisions cannot drift apart (N2 defense).
fn is_ai_call_line(line: &str) -> bool {
    ai_call_re().is_match(line)
}

#[cfg(test)]
mod ai_call_line_tests {
    use super::is_ai_call_line;

    // --- N2 regression: malformed `ai ...` lines must NOT be flagged as
    //     AI prompts. If they were, they'd skip InputGuard pre-screening
    //     AND fall through to bash, executing the destructive payload. ---

    #[test]
    fn rejects_command_substitution_form() {
        // The headline N2 bypass: `ai $(rm -rf /)` looks like an AI call
        // under a loose `starts_with("ai ")` check, but is a destructive
        // shell command. Must be rejected so InputGuard screens it.
        assert!(!is_ai_call_line("ai $(rm -rf /)"));
    }

    #[test]
    fn rejects_unquoted_payload() {
        // No quotes → not a strict AI prompt → must screen.
        assert!(!is_ai_call_line("ai rm -rf /"));
    }

    #[test]
    fn rejects_trailing_content_after_close_quote() {
        // `ai "x" ; rm -rf /` would be a two-statement attack if accepted
        // as an AI line. Must reject so the second statement is screened.
        assert!(!is_ai_call_line("ai \"x\" ; rm -rf /"));
    }

    #[test]
    fn rejects_lookalike_commands() {
        // `aid` and `aim` are real shell commands, not AI prompts.
        assert!(!is_ai_call_line("aid --help"));
        assert!(!is_ai_call_line("aim commit"));
    }

    // --- Positive cases: legitimate strict-quoted AI prompts MUST be
    //     recognized so they reach the LLM, not bash. ---

    #[test]
    fn accepts_double_quoted_prompt() {
        assert!(is_ai_call_line("ai \"summarize this\""));
    }

    #[test]
    fn accepts_single_quoted_prompt() {
        assert!(is_ai_call_line("ai 'summarize this'"));
    }

    #[test]
    fn accepts_leading_whitespace() {
        assert!(is_ai_call_line("   ai \"hi\""));
        assert!(is_ai_call_line("\tai \"hi\""));
    }
}

/// User's response to a single-key `[y/N]` confirmation prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmResponse {
    /// User pressed `y` / `Y`.
    Yes,
    /// User pressed `n`, Enter, or any non-confirming key.
    No,
    /// User pressed Ctrl+C or ESC — cancel and stay in the shell.
    Cancel,
}

/// Map a raw keystroke byte from a `[y/N]` prompt to a [`ConfirmResponse`].
/// Kept as a pure function so it can be unit-tested without touching termios.
pub(crate) fn interpret_confirm_byte(byte: u8) -> ConfirmResponse {
    match byte {
        b'y' | b'Y' => ConfirmResponse::Yes,
        0x03 | 0x1b => ConfirmResponse::Cancel,
        _ => ConfirmResponse::No,
    }
}

/// Read one raw byte from `stdin_fd` with EINTR retry, then classify it.
fn read_confirm_keystroke(stdin_fd: libc::c_int) -> ConfirmResponse {
    loop {
        let mut byte = [0u8; 1];
        let n = unsafe { libc::read(stdin_fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
        match n {
            1 => return interpret_confirm_byte(byte[0]),
            -1 => {
                // Portable errno access (mirrors persistent.rs:61). Avoids
                // glibc/musl-specific `__errno_location`.
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return ConfirmResponse::No;
            }
            _ => return ConfirmResponse::No,
        }
    }
}

/// Drain any trailing typeahead from stdin (e.g. user typed `yes<Enter>`
/// but we only consumed the `y`). Without this, the leftover bytes are
/// consumed by the next prompt as a fresh command.
fn drain_stdin_trailing(stdin_fd: libc::c_int) {
    let mut buf = [0u8; 64];
    loop {
        let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(stdin_fd, &mut fds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 10_000,
        };
        let ready = unsafe {
            libc::select(
                stdin_fd + 1,
                &mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if ready <= 0 {
            break;
        }
        let n = unsafe { libc::read(stdin_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

/// Strip XML tags from tool output to extract plain text content for terminal display.
/// Handles multi-line <offload>JSON</offload> blocks, <return_code>, <stdout>,
/// <stderr>, and any incomplete tags from truncation.
fn strip_tool_output_xml(output: &str) -> String {
    let re_trailing_metadata = TOOL_XML_TRAILING_METADATA_RE.get_or_init(|| {
        regex::Regex::new(
            r"(?ms)(?:^|\n)<(?:offload|return_code|exit-code)>\n.*?\n</(?:offload|return_code|exit-code)>\s*$",
        )
        .unwrap()
    });
    let mut cleaned = output.trim().to_string();
    loop {
        let next = re_trailing_metadata.replace(&cleaned, "").to_string();
        if next == cleaned {
            break;
        }
        cleaned = next.trim_end().to_string();
    }
    // Remove incomplete tags (e.g. "<stdo" from truncation)
    let re_incomplete =
        TOOL_XML_INCOMPLETE_RE.get_or_init(|| regex::Regex::new(r"<[^>]*$").unwrap());
    let cleaned = re_incomplete.replace_all(&cleaned, "").to_string();
    // Remove remaining single-line XML tags
    let re = TOOL_XML_RE.get_or_init(|| {
        regex::Regex::new(r"</?(?:stdout|stderr|return_code|exit-code)/?>").unwrap()
    });
    let cleaned = re.replace_all(&cleaned, "").to_string();
    // Trim leading/trailing whitespace but preserve blank lines
    // (they are part of the original output, e.g. YAML section breaks)
    cleaned.trim().to_string()
}

/// Collapse output to first N lines for terminal display, matching Python's
/// `_collapse_output_lines` behavior: show first `max_lines` lines and append
/// " ..." if truncated.
fn collapse_display_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_string();
    }
    let collapsed: Vec<&str> = lines.iter().take(max_lines).copied().collect();
    format!("{} ...", collapsed.join("\n"))
}

/// Collapse long output for display, showing first/last N lines with a truncation notice.
pub fn collapse_output(
    output: &str,
    offload_path: Option<&str>,
    threshold_lines: usize,
    context_lines: usize,
) -> String {
    let all_lines: Vec<&str> = output.lines().collect();
    if all_lines.len() <= threshold_lines {
        return output.to_string();
    }

    let first: Vec<&str> = all_lines.iter().take(context_lines).copied().collect();
    let last: Vec<&str> = all_lines
        .iter()
        .rev()
        .take(context_lines)
        .rev()
        .copied()
        .collect();
    let omitted = all_lines.len() - first.len() - last.len();

    let mut result = first.join("\n");
    result.push_str(&format!(
        "\n{}",
        theme::faint(&format!(
            "... ({} lines truncated{})",
            omitted,
            offload_path
                .map(|p| format!(", see {}", p))
                .unwrap_or_default(),
        ))
    ));
    result.push('\n');
    result.push_str(&last.join("\n"));

    result
}

#[cfg(test)]
mod submitted_line_tests {
    use super::format_user_submitted_line;

    #[test]
    fn submitted_line_includes_prompt_and_command() {
        let line = format_user_submitted_line("◆ aish ", "/help");
        assert_eq!(line, "◆ aish /help");
    }
}

#[cfg(test)]
mod collapsing_tests {
    use super::*;

    #[test]
    fn test_strip_tool_output_xml_removes_return_code_block() {
        let output = "<stdout>\nconfig.yaml\n</stdout>\n<return_code>\n0\n</return_code>";
        assert_eq!(strip_tool_output_xml(output), "config.yaml");
    }

    #[test]
    fn test_strip_tool_output_xml_preserves_return_code_text_in_stdout() {
        let output = "<stdout>\nliteral <return_code>\n0\n</return_code> block\n</stdout>\n<return_code>\n0\n</return_code>";
        assert_eq!(strip_tool_output_xml(output), "literal \n0\n block");
    }

    #[test]
    fn test_collapse_output_short() {
        let output = "line1\nline2\nline3";
        let result = collapse_output(output, None, 20, 5);
        assert_eq!(result, output);
    }

    #[test]
    fn test_collapse_output_long() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {}", i)).collect();
        let output = lines.join("\n");
        let result = collapse_output(&output, None, 20, 5);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 29"));
        assert!(result.contains("truncated"));
        assert!(!result.contains("line 10"));
    }

    #[test]
    fn test_collapse_output_with_offload() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {}", i)).collect();
        let output = lines.join("\n");
        let result = collapse_output(&output, Some("/tmp/offload.raw"), 20, 5);
        assert!(result.contains("/tmp/offload.raw"));
    }
}

/// Format tool arguments for display in the streaming output.
/// Skips large fields like content, shows single values for single-key dicts,
/// and truncates long strings.
fn format_tool_args_for_display(tool_name: &str, args: &serde_json::Value) -> String {
    // For write_file, skip the content field
    if tool_name == "write_file" {
        if let Some(obj) = args.as_object() {
            let display: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .filter(|(k, _)| *k != "content")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            return truncate_str(&serde_json::Value::Object(display).to_string(), 120);
        }
    }

    // For single-key dicts, show just the value
    if let Some(obj) = args.as_object() {
        if obj.len() == 1 {
            if let Some(v) = obj.values().next() {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                return truncate_str(&s, 120);
            }
        }
    }

    if let Some(obj) = args.as_object() {
        let primary = match tool_name {
            "bash" | "secure_bash" => "command",
            "read_file" | "edit_file" => "path",
            "grep" | "glob" => "pattern",
            "web_fetch" => "url",
            "ask_user" | "agent" => "prompt",
            _ => "",
        };
        if !primary.is_empty() {
            if let Some(val) = obj.get(primary) {
                let main = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let extras: Vec<String> = obj
                    .iter()
                    .filter(|(k, _)| *k != primary && *k != "content")
                    .filter_map(|(k, v)| {
                        let s = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        if s.is_empty() {
                            None
                        } else {
                            Some(format!("{}={}", k, truncate_str(&s, 30)))
                        }
                    })
                    .collect();
                if extras.is_empty() {
                    return truncate_str(&main, 120).to_string();
                }
                return format!("{} ({})", truncate_str(&main, 80), extras.join(", "));
            }
        }
    }

    truncate_str(&args.to_string(), 120)
}

/// Truncate a string to max_len *display columns*, accounting for CJK double-width chars.
fn truncate_display_width(s: &str, max_cols: usize) -> String {
    let mut cols = 0usize;
    let mut end = 0usize;
    for (i, ch) in s.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + w > max_cols {
            break;
        }
        cols += w;
        end = i + ch.len_utf8();
    }
    let truncated = &s[..end];
    if truncated.len() < s.len() && max_cols > 3 {
        // Re-truncate to leave room for "..."
        let mut cols2 = 0usize;
        let mut end2 = 0usize;
        for (i, ch) in s.char_indices() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cols2 + w > max_cols - 3 {
                break;
            }
            cols2 += w;
            end2 = i + ch.len_utf8();
        }
        format!("{}...", &s[..end2])
    } else {
        truncated.to_string()
    }
}

/// Truncate a string to max_len *characters* (UTF-8 safe), appending "..." if truncated.
/// Uses char count instead of byte count to avoid panicking on multi-byte characters.
fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s.to_string();
    }
    if max_len <= 3 {
        return s.chars().take(max_len).collect();
    }
    let truncated: String = s.chars().take(max_len - 3).collect();
    format!("{}...", truncated)
}

fn print_panel_line(content: &str, inner_width: usize) {
    let rendered = truncate_ansi_display_width(content, inner_width);
    let visible = ansi_display_width(&rendered);
    let padding = inner_width.saturating_sub(visible);
    println!(
        "{}{}{}{}",
        theme::warning("│"),
        rendered,
        " ".repeat(padding),
        theme::warning("│")
    );
}

fn truncate_ansi_display_width(s: &str, max_cols: usize) -> String {
    if ansi_display_width(s) <= max_cols {
        return s.to_string();
    }

    let ellipsis = if max_cols > 3 { "..." } else { "" };
    let target = max_cols.saturating_sub(ellipsis.len());
    let mut width = 0usize;
    let mut output = String::new();
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            output.push(ch);
            if let Some(next) = chars.next() {
                output.push(next);
            }
            for code_ch in chars.by_ref() {
                output.push(code_ch);
                if code_ch.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }

        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target {
            break;
        }
        width += ch_width;
        output.push(ch);
    }

    output.push_str(ellipsis);
    output.push_str("\x1b[0m");
    output
}

fn ansi_display_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for code_ch in chars.by_ref() {
                if code_ch.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    width
}

/// Wrap text to the given width, preserving word boundaries.
fn wrap_text(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    let mut line_width = 0usize;
    for word in text.split_whitespace() {
        let word_width = unicode_width::UnicodeWidthStr::width(word);
        if line_width == 0 {
            result.push_str(word);
            line_width = word_width;
        } else if line_width + 1 + word_width <= max_width {
            result.push(' ');
            result.push_str(word);
            line_width += 1 + word_width;
        } else {
            result.push('\n');
            result.push_str(word);
            line_width = word_width;
        }
    }
    result
}

fn summary_preview_from_context(messages: &[SessionContextMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find_map(|message| {
            if message.content.starts_with("<conversation-summary") {
                first_summary_line(&message.content)
            } else {
                None
            }
        })
        .or_else(|| {
            messages.iter().rev().find_map(|message| {
                (message.memory_type == aish_core::MemoryType::Llm && message.role == "user")
                    .then(|| truncate_resume_field(&message.content, 120))
            })
        })
}

/// Emit an OSC 5151 escape sequence to signal the outer PTY client.
/// The sequence is invisible (stripped by the outer client's scanner)
/// and triggers session switch/new/detach operations.
fn emit_osc(op: &str) {
    use std::io::Write;
    // OSC 5151 ; <op> BEL
    let _ = write!(std::io::stdout(), "\x1b]5151;{}\x07", op);
    let _ = std::io::stdout().flush();
}

/// Format a duration in seconds as a localized "N s/min/h ago" string.
fn format_age(secs: u64) -> String {
    use aish_i18n::t_with_args;
    let mut args = std::collections::HashMap::new();
    if secs < 60 {
        args.insert("seconds".to_string(), secs.to_string());
        t_with_args("shell.time.seconds_ago", &args)
    } else if secs < 3600 {
        args.insert("minutes".to_string(), (secs / 60).to_string());
        t_with_args("shell.time.minutes_ago", &args)
    } else {
        args.insert("hours".to_string(), (secs / 3600).to_string());
        t_with_args("shell.time.hours_ago", &args)
    }
}

/// Build a single-entry `{age => val}` map for `t_with_args`.
fn age_args(age: String) -> std::collections::HashMap<String, String> {
    let mut args = std::collections::HashMap::new();
    args.insert("age".to_string(), age);
    args
}

fn first_summary_line(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("<conversation-summary")
                && !line.starts_with("</conversation-summary")
                && *line != "Summary:"
        })
        .map(|line| line.trim_start_matches("- "))
        .map(|line| truncate_resume_field(line, 120))
}

fn format_resume_session_row(session: &SessionRecord) -> String {
    let snapshot = session.state_snapshot();
    let updated_at = snapshot.updated_at.unwrap_or(session.created_at);
    let cwd = snapshot.cwd.as_deref().unwrap_or("-");
    let summary = snapshot.summary_preview.as_deref().unwrap_or("-");
    format!(
        "{}  {}  {}  {}  {}",
        updated_at.format("%Y-%m-%d %H:%M:%S UTC"),
        session.session_uuid,
        session.model,
        truncate_resume_field(cwd, 48),
        truncate_resume_field(summary, 96)
    )
}

fn truncate_resume_field(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut truncated: String = normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect();
    truncated.push_str("...");
    truncated
}

/// Parse a category string (from the LLM tool call) into a MemoryCategory.
/// Falls back to `Other` for unrecognized values.
fn parse_category_str(s: &str) -> MemoryCategory {
    match s.to_lowercase().as_str() {
        "preference" => MemoryCategory::Preference,
        "environment" => MemoryCategory::Environment,
        "solution" => MemoryCategory::Solution,
        "pattern" => MemoryCategory::Pattern,
        _ => MemoryCategory::Other,
    }
}

/// Format a number with thousand separators.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Strip terminal control sequences (CSI/OSC/SS3 and other C0 control bytes)
/// from tool-provided text before rendering it, so a command containing
/// escape sequences cannot alter the terminal (clipboard, hyperlink, title)
/// before the user approves it. Common whitespace (`\n`, `\r`, `\t`) and all
/// printable characters are preserved.
fn sanitize_for_display(s: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(concat!(
            // OSC: ESC ] ... (BEL | ST)
            r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)",
            // CSI: ESC [ params letter
            r"|\x1b\[[0-9;?]*[A-Za-z]",
            // Other single-char ESC sequences
            r"|\x1b[@-Z\\-_]",
            // Stray C0 control bytes (excluding \t \n \r) and DEL
            r"|[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]",
        ))
        .unwrap()
    });
    RE.replace_all(s, "").to_string()
}

#[cfg(test)]
mod sanitize_for_display_tests {
    use super::sanitize_for_display;

    #[test]
    fn strips_csi_color_sequences() {
        assert_eq!(sanitize_for_display("\x1b[31mrm\x1b[0m /tmp"), "rm /tmp");
    }

    #[test]
    fn strips_osc_clipboard_sequence() {
        // OSC 52 (clipboard) must not execute on display.
        assert_eq!(
            sanitize_for_display("\x1b]52;c;Zm1lbQ==\x07echo hi"),
            "echo hi"
        );
    }

    #[test]
    fn preserves_common_whitespace_and_plain_text() {
        assert_eq!(sanitize_for_display("a\tb\nc\rd"), "a\tb\nc\rd");
        assert_eq!(
            sanitize_for_display("systemctl restart nginx"),
            "systemctl restart nginx"
        );
    }
}

/// Read a single-byte confirmation from stdin in raw mode.
///
/// Acquires the interactive input guard (pauses esc_watcher), reads one
/// byte, and drains any trailing bytes (e.g. `\r` after `y`).  Returns
/// `true` for 'y', 'Y', Enter.
fn read_raw_confirmation() -> bool {
    let _ig = aish_tools::bash::acquire_interactive_input_guard();

    let mut byte = [0u8; 1];
    let approved = match io::stdin().read(&mut byte) {
        Ok(1) => {
            let ch = byte[0];
            println!();
            ch == b'y' || ch == b'Y' || ch == b'\r' || ch == b'\n'
        }
        _ => false,
    };
    // Drain trailing bytes with timeout (e.g. \r after 'y').
    let mut drain_buf = [0u8; 64];
    let stdin_fd = libc::STDIN_FILENO;
    loop {
        let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(stdin_fd, &mut fds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 10_000,
        };
        let sel = unsafe {
            libc::select(
                stdin_fd + 1,
                &mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel <= 0 {
            break;
        }
        match io::stdin().read(&mut drain_buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => continue,
        }
    }
    approved
}

/// Read a single-byte approval choice from stdin in raw mode.
///
/// Returns the raw byte the user pressed plus the mapped [`ApprovalChoice`].
/// The caller owns echo and newline rendering so the choice can be shown
/// inside the confirmation panel. Reads exactly one byte, then drains any
/// trailing bytes (e.g. \r after 'y') with a short timeout, like
/// [`read_raw_confirmation`].
fn read_approval_choice() -> (u8, aish_llm::ApprovalChoice) {
    let _ig = aish_tools::bash::acquire_interactive_input_guard();

    let mut byte = [0u8; 1];
    let (pressed, choice) = match io::stdin().read(&mut byte) {
        Ok(1) => {
            let ch = byte[0];
            let choice = match ch {
                b'y' | b'Y' => aish_llm::ApprovalChoice::Once,
                b'a' | b'A' => aish_llm::ApprovalChoice::RememberSession,
                b'r' | b'R' => aish_llm::ApprovalChoice::ReplyToAi,
                _ => aish_llm::ApprovalChoice::Deny,
            };
            (ch, choice)
        }
        _ => (0, aish_llm::ApprovalChoice::Deny),
    };
    // Drain trailing bytes with timeout (e.g. \r after 'y').
    let mut drain_buf = [0u8; 64];
    let stdin_fd = libc::STDIN_FILENO;
    loop {
        let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(stdin_fd, &mut fds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 10_000,
        };
        let sel = unsafe {
            libc::select(
                stdin_fd + 1,
                &mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel <= 0 {
            break;
        }
        match io::stdin().read(&mut drain_buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => continue,
        }
    }
    (pressed, choice)
}

/// Render markdown-formatted text to the terminal using richrs.
/// Print markdown with recording support.
fn print_md_with_recording(text: &str, shared_recorder: &crate::recorder::SharedRecorder) {
    use crate::renderer::ShellRenderer;
    let mut renderer = ShellRenderer::new();
    renderer.set_shared_recorder(shared_recorder.clone());
    renderer.render_markdown(text);
}

/// Extract the first ```bash code block from AI response text.
/// Extract the remote host from an SSH/telnet command.
/// Forward-parsing version that correctly skips SSH options with arguments.
/// e.g. "ssh -p 2222 user@example.com" → "user@example.com"
/// e.g. "ssh -o StrictHostKeyChecking=no root@host" → "root@host"
fn extract_remote_host(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let cmd = parts[0];
    if !matches!(cmd, "ssh" | "telnet" | "mosh" | "sftp" | "nc" | "netcat") {
        return None;
    }
    let opts_with_arg: &[&str] = &[
        "-D", "-E", "-F", "-I", "-J", "-L", "-O", "-Q", "-R", "-S", "-W", "-b", "-c", "-e", "-i",
        "-l", "-m", "-o", "-p", "-w",
    ];
    let mut iter = parts.iter().skip(1).peekable();
    while let Some(part) = iter.next() {
        if part.starts_with('-') {
            let opt_name = if part.contains('=') {
                part.split('=').next().unwrap()
            } else {
                *part
            };
            if opts_with_arg.contains(&opt_name) && !part.contains('=') {
                iter.next();
            }
            continue;
        }
        return Some(part.to_string());
    }
    None
}

#[cfg(test)]
mod extract_remote_host_tests {
    use super::extract_remote_host;

    // Regression: ssh -q is the quiet flag (no argument). Treating -q as an
    // option-with-arg caused `ssh -q host` to skip `host` and return None,
    // disabling the [ssh:host] banner and remote PS1 marker.
    #[test]
    fn ssh_quiet_flag_does_not_consume_host() {
        assert_eq!(
            extract_remote_host("ssh -q 10.10.17.243"),
            Some("10.10.17.243".to_string())
        );
        assert_eq!(
            extract_remote_host("ssh -q user@host"),
            Some("user@host".to_string())
        );
    }

    #[test]
    fn ssh_port_option_still_consumes_argument() {
        assert_eq!(
            extract_remote_host("ssh -p 2222 user@host"),
            Some("user@host".to_string())
        );
    }

    #[test]
    fn ssh_flags_and_argument_options_parse_host() {
        // -K (GSSAPI auth) is a flag — host must be found, not consumed.
        assert_eq!(
            extract_remote_host("ssh -K root@host uptime"),
            Some("root@host".to_string())
        );
        // -Q takes a query_option argument.
        assert_eq!(
            extract_remote_host("ssh -Q cipher root@host"),
            Some("root@host".to_string())
        );
        // -D (dynamic forward) takes a port argument.
        assert_eq!(
            extract_remote_host("ssh -D 1080 root@host"),
            Some("root@host".to_string())
        );
    }
}

/// Each variant appears with and without the leading login-shell dash.
const SHELL_ERROR_PREFIXES: &[&str] = &[
    "-bash: ", "bash: ", "-ksh: ", "ksh: ", "-zsh: ", "zsh: ", "-ash: ", "ash: ", "-dash: ",
    "dash: ", "-fish: ", "fish: ", "-csh: ", "csh: ", "-tcsh: ", "tcsh: ", "-sh: ", "sh: ",
];

/// Extract the failed command from PTY output after a command error.
/// Strategy 1: Find the full command from the prompt line just before the
/// shell error (preserves pipes, args, etc.).
/// Strategy 2: Extract the command name from the shell error message.
fn extract_failed_command(output: &str) -> String {
    static ANSI_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ANSI_RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap());
    let clean = re.replace_all(output, "").to_string();
    let lines: Vec<&str> = clean.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_shell_error = SHELL_ERROR_PREFIXES
            .iter()
            .any(|prefix| trimmed.starts_with(prefix));

        if is_shell_error && i > 0 {
            // Look at the line before the error — it should be the prompt + command
            let prev = lines[i - 1].trim();
            // Extract command after the last "# " or "$ " (common prompt endings)
            if let Some(idx) = prev.rfind("# ") {
                let full_cmd = prev[idx + 2..].trim();
                if !full_cmd.is_empty() {
                    return full_cmd.to_string();
                }
            }
            if let Some(idx) = prev.rfind("$ ") {
                let full_cmd = prev[idx + 2..].trim();
                if !full_cmd.is_empty() {
                    return full_cmd.to_string();
                }
            }
        }
    }

    // Fallback: extract command name from the error message itself
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        let rest = SHELL_ERROR_PREFIXES
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix));
        if let Some(rest) = rest {
            // Use rfind to get the LAST ": " — fish errors like
            // "Unknown command: foo" have multiple colons.
            if let Some(colon_pos) = rest.rfind(": ") {
                let cmd = rest[colon_pos + 2..].trim();
                if !cmd.is_empty() {
                    return cmd.to_string();
                }
            }
        }
    }
    "(remote command)".to_string()
}

fn extract_bash_command(text: &str) -> Option<String> {
    let marker = "```bash";
    let start = text.find(marker)?;
    let content_start = start + marker.len();
    let content_end = text[content_start..].find("```")?;
    let cmd = text[content_start..content_start + content_end]
        .trim()
        .to_string();
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
}

#[cfg(test)]
mod phase_tests {
    use super::*;

    #[test]
    fn test_shell_phase_display() {
        assert_eq!(ShellPhase::Booting.to_string(), "booting");
        assert_eq!(ShellPhase::Editing.to_string(), "editing");
        assert_eq!(ShellPhase::Running.to_string(), "running");
        assert_eq!(ShellPhase::Exiting.to_string(), "exiting");
    }

    #[test]
    fn test_interruption_default() {
        assert_eq!(InterruptionState::default(), InterruptionState::Normal);
    }

    #[test]
    fn test_phase_equality() {
        assert_eq!(ShellPhase::Booting, ShellPhase::Booting);
        assert_ne!(ShellPhase::Booting, ShellPhase::Editing);
    }
}

#[cfg(test)]
mod confirm_action_tests {
    use super::*;

    #[test]
    fn yes_lower_confirms() {
        assert_eq!(interpret_confirm_byte(b'y'), ConfirmResponse::Yes);
    }

    #[test]
    fn yes_upper_confirms() {
        assert_eq!(interpret_confirm_byte(b'Y'), ConfirmResponse::Yes);
    }

    #[test]
    fn no_rejects() {
        assert_eq!(interpret_confirm_byte(b'n'), ConfirmResponse::No);
    }

    #[test]
    fn ctrl_c_cancels() {
        assert_eq!(interpret_confirm_byte(0x03), ConfirmResponse::Cancel);
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(interpret_confirm_byte(0x1b), ConfirmResponse::Cancel);
    }

    #[test]
    fn enter_rejects() {
        // Default action is No: bare Enter must not confirm.
        assert_eq!(interpret_confirm_byte(b'\r'), ConfirmResponse::No);
        assert_eq!(interpret_confirm_byte(b'\n'), ConfirmResponse::No);
    }

    #[test]
    fn arbitrary_byte_rejects() {
        assert_eq!(interpret_confirm_byte(b'x'), ConfirmResponse::No);
        assert_eq!(interpret_confirm_byte(b' '), ConfirmResponse::No);
    }
}
