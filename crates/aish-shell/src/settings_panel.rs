//! Interactive settings panel (`/setting`).
//!
//! The raw `ConfigModel` exposes 50+ fields, which is overwhelming to edit by
//! hand. This module curates the commonly-tweaked subset into a categorized
//! catalog where every entry carries a human label and a one-line explanation,
//! and exposes helpers to read the current value, validate/apply a new value,
//! and signal which live side-effects the shell must run after a change.
//!
//! The interactive loop itself lives in `app.rs` (`handle_setting_command`);
//! this module is intentionally free of terminal I/O so it stays unit-testable.

use aish_config::ConfigModel;
use aish_security::{RiskLevel, SandboxOffAction, SecurityPolicy};

// ---------------------------------------------------------------------------
// Live-effect signal
// ---------------------------------------------------------------------------

/// What the shell must do after a setting is applied so the change takes effect
/// in the running session (beyond persisting to disk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEffect {
    /// No live action needed; the field is read on demand from `self.config`.
    /// Security keys also map here — they are applied via the dedicated
    /// `is_security_setting` path in `app.rs`, not through this enum.
    None,
    /// Rebuild the model-dependent tools (WebFetch) — e.g. after temperature or
    /// `max_tokens` changes.
    ToolsRefresh,
    /// Re-initialize the whole LLM session + tools + inline completer — needed
    /// when `model` / `api_base` / `api_key` change.
    ModelSession,
}

// ---------------------------------------------------------------------------
// Setting categories and value kinds
// ---------------------------------------------------------------------------

/// Top-level group shown in the first panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingCategory {
    Model,
    Appearance,
    Ai,
    Security,
    Context,
    Remote,
    Advanced,
}

impl SettingCategory {
    /// Stable identifier used to build i18n keys and option values.
    pub fn name(self) -> &'static str {
        match self {
            SettingCategory::Model => "model",
            SettingCategory::Appearance => "appearance",
            SettingCategory::Ai => "ai",
            SettingCategory::Security => "security",
            SettingCategory::Context => "context",
            SettingCategory::Remote => "remote",
            SettingCategory::Advanced => "advanced",
        }
    }

    /// All categories in display order.
    pub const ALL: &'static [SettingCategory] = &[
        SettingCategory::Model,
        SettingCategory::Appearance,
        SettingCategory::Ai,
        SettingCategory::Security,
        SettingCategory::Context,
        SettingCategory::Remote,
        SettingCategory::Advanced,
    ];
}

impl SettingCategory {
    /// Short glyph shown next to the category label in chips and row icons.
    /// Pure ASCII-ish unicode so it renders in any terminal.
    pub fn icon(self) -> &'static str {
        match self {
            SettingCategory::Model => "◆",
            SettingCategory::Appearance => "◐",
            SettingCategory::Ai => "✦",
            SettingCategory::Security => "▓",
            SettingCategory::Context => "▣",
            SettingCategory::Remote => "⌘",
            SettingCategory::Advanced => "⚙",
        }
    }
}

/// How a value is edited in the panel.
#[derive(Debug, Clone, Copy)]
pub enum SettingKind {
    /// On/off toggle.
    Bool,
    /// Pick one of a fixed set of values.
    Choice(&'static [&'static str]),
    /// Arbitrary single-line text.
    Text,
    /// Floating-point number.
    Float,
    /// Non-negative integer (blank clears to the default / unset).
    Int,
    /// Like `Text` but the current value is masked on display (API keys).
    Secret,
    /// A list of strings edited as a comma-separated single line; apply()
    /// splits on commas. Used for pattern lists like remote_danger_patterns.
    StringList,
}

// ---------------------------------------------------------------------------
// Setting key — exhaustive identifier for each catalog entry

/// Identifier for a single configurable setting.
///
/// Order here defines display order within a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    // Model & Provider
    Model,
    ApiBase,
    ApiKey,
    Temperature,
    MaxTokens,
    // Appearance
    PromptStyle,
    OutputLanguage,
    AutoSuggest,
    // AI Behavior
    InlineCompletion,
    InlineDebounceMs,
    InlineContextLines,
    InlineMaxTokens,
    InlineMinInputChars,
    InlineTimeoutSecs,
    InlineDisableThinking,
    InlineEnforceJson,
    MaxLlmMessages,
    EnableTokenEstimation,
    // Security
    InputGuardEnabled,
    EnableSandbox,
    DefaultRiskLevel,
    SandboxOffAction,
    SandboxTimeout,
    // Context & Memory
    MemoryAutoRecall,
    MemoryAutoRetain,
    ContextAutoCompact,
    CompactFullEnabled,
    CompactContextWindowTokens,
    CompactMicroKeepRecent,
    CompactShellKeepRecent,
    CompactSummaryMaxTokens,
    CompactMaxConsecutiveFailures,
    CompactReservedOutputTokens,
    HistorySize,
    MaxShellMessages,
    ContextTokenBudget,
    // Remote Prompt
    EnableRemoteGitPrompt,
    RemoteRichPrompt,
    RemoteShowVenv,
    RemoteShowContainer,
    RemoteShowKube,
    RemoteDangerPatterns,
    // Advanced
    LogLevel,
    EnableLangfuse,
    LangfusePublicKey,
    LangfuseSecretKey,
    LangfuseHost,
    EnableScripts,
    PtyDaemonEnabled,
    CodexAuthPath,
    SessionDbPath,
    SkillAutoSearch,
    SkillRegistries,
}
impl SettingKey {
    /// Stable string used for i18n keys (`shell.setting.k.{name}.label|desc`).
    pub fn name(self) -> &'static str {
        match self {
            SettingKey::Model => "model",
            SettingKey::ApiBase => "api_base",
            SettingKey::ApiKey => "api_key",
            SettingKey::Temperature => "temperature",
            SettingKey::MaxTokens => "max_tokens",
            SettingKey::PromptStyle => "prompt_style",
            SettingKey::OutputLanguage => "output_language",
            SettingKey::AutoSuggest => "auto_suggest",
            SettingKey::InlineCompletion => "inline_completion",
            SettingKey::InlineDebounceMs => "inline_debounce_ms",
            SettingKey::InlineContextLines => "inline_context_lines",
            SettingKey::InlineMaxTokens => "inline_max_tokens",
            SettingKey::InlineMinInputChars => "inline_min_input_chars",
            SettingKey::InlineTimeoutSecs => "inline_timeout_secs",
            SettingKey::InlineDisableThinking => "inline_disable_thinking",
            SettingKey::InlineEnforceJson => "inline_enforce_json",
            SettingKey::MaxLlmMessages => "max_llm_messages",
            SettingKey::EnableTokenEstimation => "enable_token_estimation",
            SettingKey::InputGuardEnabled => "input_guard_enabled",
            SettingKey::EnableSandbox => "enable_sandbox",
            SettingKey::DefaultRiskLevel => "default_risk_level",
            SettingKey::SandboxOffAction => "sandbox_off_action",
            SettingKey::SandboxTimeout => "sandbox_timeout_seconds",
            SettingKey::MemoryAutoRecall => "memory_auto_recall",
            SettingKey::MemoryAutoRetain => "memory_auto_retain",
            SettingKey::ContextAutoCompact => "context_auto_compact",
            SettingKey::CompactFullEnabled => "compact_full_enabled",
            SettingKey::CompactContextWindowTokens => "compact_context_window_tokens",
            SettingKey::CompactMicroKeepRecent => "compact_micro_keep_recent",
            SettingKey::CompactShellKeepRecent => "compact_shell_keep_recent",
            SettingKey::CompactSummaryMaxTokens => "compact_summary_max_tokens",
            SettingKey::CompactMaxConsecutiveFailures => "compact_max_consecutive_failures",
            SettingKey::CompactReservedOutputTokens => "compact_reserved_output_tokens",
            SettingKey::HistorySize => "history_size",
            SettingKey::MaxShellMessages => "max_shell_messages",
            SettingKey::ContextTokenBudget => "context_token_budget",
            SettingKey::EnableRemoteGitPrompt => "enable_remote_git_prompt",
            SettingKey::RemoteRichPrompt => "remote_rich_prompt",
            SettingKey::RemoteShowVenv => "remote_show_venv",
            SettingKey::RemoteShowContainer => "remote_show_container",
            SettingKey::RemoteShowKube => "remote_show_kube",
            SettingKey::RemoteDangerPatterns => "remote_danger_patterns",
            SettingKey::LogLevel => "log_level",
            SettingKey::EnableLangfuse => "enable_langfuse",
            SettingKey::LangfusePublicKey => "langfuse_public_key",
            SettingKey::LangfuseSecretKey => "langfuse_secret_key",
            SettingKey::LangfuseHost => "langfuse_host",
            SettingKey::EnableScripts => "enable_scripts",
            SettingKey::PtyDaemonEnabled => "pty_daemon_enabled",
            SettingKey::CodexAuthPath => "codex_auth_path",
            SettingKey::SessionDbPath => "session_db_path",
            SettingKey::SkillAutoSearch => "skill_auto_search",
            SettingKey::SkillRegistries => "skill_registries",
        }
    }
}

// ---------------------------------------------------------------------------
// Catalog entry
// ---------------------------------------------------------------------------

/// A single curated setting.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub key: SettingKey,
    pub category: SettingCategory,
    pub kind: SettingKind,
}

/// The full curated catalog. Every entry below is consumed by the running
/// shell (live or at next startup) — vestigial config fields that nothing
/// reads are deliberately excluded. The remaining `ConfigModel` fields stay
/// in the YAML for power users.
pub const SETTINGS: &[SettingDef] = &[
    // --- Model & Provider ---
    SettingDef {
        key: SettingKey::Model,
        category: SettingCategory::Model,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::ApiBase,
        category: SettingCategory::Model,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::ApiKey,
        category: SettingCategory::Model,
        kind: SettingKind::Secret,
    },
    SettingDef {
        key: SettingKey::Temperature,
        category: SettingCategory::Model,
        kind: SettingKind::Float,
    },
    SettingDef {
        key: SettingKey::MaxTokens,
        category: SettingCategory::Model,
        kind: SettingKind::Int,
    },
    // --- Appearance ---
    SettingDef {
        key: SettingKey::PromptStyle,
        category: SettingCategory::Appearance,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::OutputLanguage,
        category: SettingCategory::Appearance,
        kind: SettingKind::Choice(&["zh-CN", "en-US", "ja-JP", "de-DE", "fr-FR", "es-ES"]),
    },
    // --- AI Behavior ---
    SettingDef {
        key: SettingKey::InlineCompletion,
        category: SettingCategory::Ai,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::InlineDebounceMs,
        category: SettingCategory::Ai,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::InlineContextLines,
        category: SettingCategory::Ai,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::InlineMaxTokens,
        category: SettingCategory::Ai,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::InlineMinInputChars,
        category: SettingCategory::Ai,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::InlineTimeoutSecs,
        category: SettingCategory::Ai,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::InlineDisableThinking,
        category: SettingCategory::Ai,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::InlineEnforceJson,
        category: SettingCategory::Ai,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::AutoSuggest,
        category: SettingCategory::Ai,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::MaxLlmMessages,
        category: SettingCategory::Ai,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::EnableTokenEstimation,
        category: SettingCategory::Ai,
        kind: SettingKind::Bool,
    },
    // --- Security ---
    SettingDef {
        key: SettingKey::InputGuardEnabled,
        category: SettingCategory::Security,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::EnableSandbox,
        category: SettingCategory::Security,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::DefaultRiskLevel,
        category: SettingCategory::Security,
        kind: SettingKind::Choice(&["low", "medium", "high"]),
    },
    SettingDef {
        key: SettingKey::SandboxOffAction,
        category: SettingCategory::Security,
        kind: SettingKind::Choice(&["allow", "confirm", "block"]),
    },
    SettingDef {
        key: SettingKey::SandboxTimeout,
        category: SettingCategory::Security,
        kind: SettingKind::Float,
    },
    // --- Context & Memory ---
    SettingDef {
        key: SettingKey::MemoryAutoRecall,
        category: SettingCategory::Context,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::MemoryAutoRetain,
        category: SettingCategory::Context,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::ContextAutoCompact,
        category: SettingCategory::Context,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::CompactFullEnabled,
        category: SettingCategory::Context,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::CompactContextWindowTokens,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::CompactMicroKeepRecent,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::CompactShellKeepRecent,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::CompactSummaryMaxTokens,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::CompactMaxConsecutiveFailures,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::CompactReservedOutputTokens,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::HistorySize,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::MaxShellMessages,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    SettingDef {
        key: SettingKey::ContextTokenBudget,
        category: SettingCategory::Context,
        kind: SettingKind::Int,
    },
    // --- Remote Prompt ---
    SettingDef {
        key: SettingKey::EnableRemoteGitPrompt,
        category: SettingCategory::Remote,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::RemoteRichPrompt,
        category: SettingCategory::Remote,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::RemoteShowVenv,
        category: SettingCategory::Remote,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::RemoteShowContainer,
        category: SettingCategory::Remote,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::RemoteShowKube,
        category: SettingCategory::Remote,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::RemoteDangerPatterns,
        category: SettingCategory::Remote,
        kind: SettingKind::StringList,
    },
    // --- Advanced ---
    SettingDef {
        key: SettingKey::EnableLangfuse,
        category: SettingCategory::Advanced,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::LangfusePublicKey,
        category: SettingCategory::Advanced,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::LangfuseSecretKey,
        category: SettingCategory::Advanced,
        kind: SettingKind::Secret,
    },
    SettingDef {
        key: SettingKey::LangfuseHost,
        category: SettingCategory::Advanced,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::EnableScripts,
        category: SettingCategory::Advanced,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::LogLevel,
        category: SettingCategory::Advanced,
        kind: SettingKind::Choice(&["error", "warn", "info", "debug", "trace"]),
    },
    SettingDef {
        key: SettingKey::PtyDaemonEnabled,
        category: SettingCategory::Advanced,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::CodexAuthPath,
        category: SettingCategory::Advanced,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::SessionDbPath,
        category: SettingCategory::Advanced,
        kind: SettingKind::Text,
    },
    SettingDef {
        key: SettingKey::SkillAutoSearch,
        category: SettingCategory::Advanced,
        kind: SettingKind::Bool,
    },
    SettingDef {
        key: SettingKey::SkillRegistries,
        category: SettingCategory::Advanced,
        kind: SettingKind::StringList,
    },
];

/// All entries belonging to `category`, in catalog order.
pub fn entries_for(category: SettingCategory) -> impl Iterator<Item = &'static SettingDef> {
    SETTINGS.iter().filter(move |s| s.category == category)
}

/// Look up a definition by key.
pub fn find(key: SettingKey) -> &'static SettingDef {
    SETTINGS
        .iter()
        .find(|s| s.key == key)
        .expect("SettingKey is exhaustive over SETTINGS")
}

/// The factory-default raw value for `key`.
///
/// Security keys derive from `SecurityPolicy::default()` (policy SSOT);
/// everything else from `ConfigModel::default()`.
pub fn default_raw_of(key: SettingKey) -> String {
    raw_value(&ConfigModel::default(), &SecurityPolicy::default(), key)
}

/// Whether the current raw value differs from the factory default.
pub fn is_changed(key: SettingKey, current_raw: &str) -> bool {
    current_raw != default_raw_of(key)
}

// ---------------------------------------------------------------------------
// Security settings (security_policy.yaml SSOT)
// ---------------------------------------------------------------------------

/// Security category keys live in `security_policy.yaml`, not `config.yaml`.
pub fn is_security_setting(key: SettingKey) -> bool {
    matches!(
        key,
        SettingKey::InputGuardEnabled
            | SettingKey::EnableSandbox
            | SettingKey::DefaultRiskLevel
            | SettingKey::SandboxOffAction
            | SettingKey::SandboxTimeout
    )
}

/// Read the current raw value from the correct SSOT (`SecurityPolicy` or
/// `ConfigModel`).
pub fn raw_value(cfg: &ConfigModel, policy: &SecurityPolicy, key: SettingKey) -> String {
    if is_security_setting(key) {
        security_current_raw(policy, key)
    } else {
        current_raw(cfg, key)
    }
}

/// Read a Security setting from the live policy.
fn risk_level_raw(level: RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

fn sandbox_off_action_raw(action: SandboxOffAction) -> &'static str {
    match action {
        SandboxOffAction::Allow => "allow",
        SandboxOffAction::Confirm => "confirm",
        SandboxOffAction::Block => "block",
    }
}

fn parse_risk_level(value: &str) -> RiskLevel {
    match value {
        "medium" => RiskLevel::Medium,
        "high" => RiskLevel::High,
        _ => RiskLevel::Low,
    }
}

fn parse_sandbox_off_action(value: &str) -> SandboxOffAction {
    match value {
        "confirm" => SandboxOffAction::Confirm,
        "block" => SandboxOffAction::Block,
        _ => SandboxOffAction::Allow,
    }
}

pub fn security_current_raw(policy: &SecurityPolicy, key: SettingKey) -> String {
    if !is_security_setting(key) {
        panic!("security_current_raw called with non-security key {key:?}");
    }
    let raw = match key {
        SettingKey::InputGuardEnabled => bool_str(policy.input_guard.enabled),
        SettingKey::EnableSandbox => bool_str(policy.enable_sandbox),
        SettingKey::DefaultRiskLevel => risk_level_raw(policy.default_risk_level).to_string(),
        SettingKey::SandboxOffAction => sandbox_off_action_raw(policy.sandbox_off_action).to_string(),
        SettingKey::SandboxTimeout => format!("{}", policy.sandbox_timeout_seconds),
        _ => unreachable!("non-security key rejected above"),
    };
    if let SettingKind::Choice(options) = find(key).kind {
        if let Some(canonical) = options.iter().find(|o| o.eq_ignore_ascii_case(&raw)) {
            return (*canonical).to_string();
        }
    }
    raw
}

/// Apply a validated Security setting onto a policy in memory.
pub fn apply_security(
    policy: &mut SecurityPolicy,
    key: SettingKey,
    value: &str,
) -> Result<(), String> {
    if !is_security_setting(key) {
        return Err(format!("not a security setting: {key:?}"));
    }
    if let SettingKind::Choice(options) = find(key).kind {
        if !options.contains(&value) {
            return Err(format!("expected one of {:?}, got `{value}`", options));
        }
    }
    match key {
        SettingKey::InputGuardEnabled => {
            policy.input_guard.enabled = parse_bool(value)?;
        }
        SettingKey::EnableSandbox => {
            policy.enable_sandbox = parse_bool(value)?;
        }
        SettingKey::DefaultRiskLevel => {
            policy.default_risk_level = parse_risk_level(value);
        }
        SettingKey::SandboxOffAction => {
            policy.sandbox_off_action = parse_sandbox_off_action(value);
        }
        SettingKey::SandboxTimeout => {
            policy.sandbox_timeout_seconds = parse_f64(value, 1.0..=300.0)?;
        }
        _ => unreachable!("non-security key rejected above"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Read / write the live config
// ---------------------------------------------------------------------------

/// Read the current raw value as a string (what would be written to YAML).
/// Option fields render as `""` when unset.
pub fn current_raw(cfg: &ConfigModel, key: SettingKey) -> String {
    let raw = match key {
        SettingKey::Model => cfg.model.clone(),
        SettingKey::ApiBase => cfg.api_base.clone(),
        SettingKey::ApiKey => cfg.api_key.clone(),
        SettingKey::Temperature => format!("{}", cfg.temperature),
        SettingKey::MaxTokens => cfg.max_tokens.map(|n| n.to_string()).unwrap_or_default(),
        SettingKey::PromptStyle => cfg.prompt_style.clone().unwrap_or_default(),
        SettingKey::OutputLanguage => cfg.output_language.clone().unwrap_or_default(),
        SettingKey::AutoSuggest => bool_str(cfg.auto_suggest),
        SettingKey::InlineCompletion => bool_str(cfg.inline_completion.enabled),
        SettingKey::InlineDebounceMs => cfg.inline_completion.debounce_ms.to_string(),
        SettingKey::InlineContextLines => cfg.inline_completion.context_lines.to_string(),
        SettingKey::InlineMaxTokens => cfg.inline_completion.max_tokens.to_string(),
        SettingKey::InlineMinInputChars => cfg.inline_completion.min_input_chars.to_string(),
        SettingKey::InlineTimeoutSecs => cfg.inline_completion.timeout_secs.to_string(),
        SettingKey::InlineDisableThinking => bool_str(cfg.inline_completion.disable_thinking),
        SettingKey::InlineEnforceJson => bool_str(cfg.inline_completion.enforce_json),
        SettingKey::MaxLlmMessages => cfg.max_llm_messages.to_string(),
        SettingKey::EnableTokenEstimation => bool_str(cfg.enable_token_estimation),
        SettingKey::InputGuardEnabled
        | SettingKey::EnableSandbox
        | SettingKey::DefaultRiskLevel
        | SettingKey::SandboxOffAction
        | SettingKey::SandboxTimeout => {
            panic!("security setting {key:?} must use security_current_raw")
        }
        SettingKey::MemoryAutoRecall => cfg
            .memory
            .as_ref()
            .map(|m| bool_str(m.auto_recall))
            .unwrap_or_else(|| bool_str(true)),
        SettingKey::MemoryAutoRetain => cfg
            .memory
            .as_ref()
            .map(|m| bool_str(m.auto_retain))
            .unwrap_or_else(|| bool_str(true)),
        SettingKey::ContextAutoCompact => bool_str(cfg.context_auto_compact.enabled),
        SettingKey::CompactFullEnabled => bool_str(cfg.context_auto_compact.full_compact_enabled),
        SettingKey::CompactContextWindowTokens => cfg
            .context_auto_compact
            .context_window_tokens
            .map(|n| n.to_string())
            .unwrap_or_default(),
        SettingKey::CompactMicroKeepRecent => cfg
            .context_auto_compact
            .micro_keep_recent_messages
            .to_string(),
        SettingKey::CompactShellKeepRecent => cfg
            .context_auto_compact
            .shell_keep_recent_commands
            .to_string(),
        SettingKey::CompactSummaryMaxTokens => {
            cfg.context_auto_compact.summary_max_tokens.to_string()
        }
        SettingKey::CompactMaxConsecutiveFailures => cfg
            .context_auto_compact
            .max_consecutive_failures
            .to_string(),
        SettingKey::CompactReservedOutputTokens => cfg
            .context_auto_compact
            .reserved_output_tokens
            .map(|n| n.to_string())
            .unwrap_or_default(),
        SettingKey::HistorySize => cfg.history_size.to_string(),
        SettingKey::MaxShellMessages => cfg.max_shell_messages.to_string(),
        SettingKey::ContextTokenBudget => cfg
            .context_token_budget
            .map(|n| n.to_string())
            .unwrap_or_default(),
        SettingKey::EnableRemoteGitPrompt => bool_str(cfg.enable_remote_git_prompt),
        SettingKey::RemoteRichPrompt => bool_str(cfg.remote_rich_prompt),
        SettingKey::RemoteShowVenv => bool_str(cfg.remote_show_venv),
        SettingKey::RemoteShowContainer => bool_str(cfg.remote_show_container),
        SettingKey::RemoteShowKube => bool_str(cfg.remote_show_kube),
        SettingKey::RemoteDangerPatterns => cfg.remote_danger_patterns.join(", "),
        SettingKey::LogLevel => cfg.log_level.clone(),
        SettingKey::EnableLangfuse => bool_str(cfg.enable_langfuse),
        SettingKey::LangfusePublicKey => cfg.langfuse_public_key.clone().unwrap_or_default(),
        SettingKey::LangfuseSecretKey => cfg.langfuse_secret_key.clone().unwrap_or_default(),
        SettingKey::LangfuseHost => cfg.langfuse_host.clone().unwrap_or_default(),
        SettingKey::EnableScripts => bool_str(cfg.enable_scripts),
        SettingKey::SkillRegistries => cfg
            .skills
            .registries
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
        SettingKey::CodexAuthPath => cfg.codex_auth_path.clone().unwrap_or_default(),
        SettingKey::SessionDbPath => cfg.session_db_path.clone().unwrap_or_default(),
        SettingKey::PtyDaemonEnabled => bool_str(cfg.pty_daemon_enabled),
        SettingKey::SkillAutoSearch => bool_str(cfg.skills.auto_search),
    };
    // Normalize Choice values to their canonical option so the display and
    // "current" marker line up even if the stored value differs by case
    // (e.g. security_policy.yaml merging in "LOW" vs the list's "low").
    if let SettingKind::Choice(options) = find(key).kind {
        if let Some(canonical) = options.iter().find(|o| o.eq_ignore_ascii_case(&raw)) {
            return (*canonical).to_string();
        }
    }
    raw
}

/// Apply a validated raw string value to the config in place.
///
/// `value` is already trimmed by the caller. Empty string clears Option
/// fields to `None`. Returns `Err(message)` on a parse failure.
pub fn apply(cfg: &mut ConfigModel, key: SettingKey, value: &str) -> Result<(), String> {
    // Choice fields must be one of the declared options — reject typos like
    // `log_level: warb` early so they can't silently disable logging.
    if let SettingKind::Choice(options) = find(key).kind {
        if !options.contains(&value) {
            return Err(format!("expected one of {:?}, got `{value}`", options));
        }
    }
    match key {
        SettingKey::Model => cfg.model = value.to_string(),
        SettingKey::ApiBase => cfg.api_base = value.to_string(),
        SettingKey::ApiKey => cfg.api_key = value.to_string(),
        SettingKey::Temperature => {
            cfg.temperature = parse_f32(value, 0.0..=2.0)?;
        }
        SettingKey::MaxTokens => {
            cfg.max_tokens = if value.is_empty() {
                None
            } else {
                Some(parse_u32(value)?)
            };
        }
        SettingKey::PromptStyle => cfg.prompt_style = opt_string(value),
        SettingKey::OutputLanguage => cfg.output_language = opt_string(value),
        SettingKey::AutoSuggest => cfg.auto_suggest = parse_bool(value)?,
        SettingKey::InlineCompletion => cfg.inline_completion.enabled = parse_bool(value)?,
        SettingKey::InlineDebounceMs => {
            cfg.inline_completion.debounce_ms = parse_usize(value)? as u64;
        }
        SettingKey::InlineContextLines => {
            cfg.inline_completion.context_lines = parse_usize(value)?;
        }
        SettingKey::InlineMaxTokens => {
            cfg.inline_completion.max_tokens = parse_u32(value)?;
        }
        SettingKey::InlineMinInputChars => {
            cfg.inline_completion.min_input_chars = parse_usize(value)?;
        }
        SettingKey::InlineTimeoutSecs => {
            cfg.inline_completion.timeout_secs = parse_usize(value)? as u64;
        }
        SettingKey::InlineDisableThinking => {
            cfg.inline_completion.disable_thinking = parse_bool(value)?;
        }
        SettingKey::InlineEnforceJson => {
            cfg.inline_completion.enforce_json = parse_bool(value)?;
        }
        SettingKey::MaxLlmMessages => cfg.max_llm_messages = parse_usize(value)?,
        SettingKey::EnableTokenEstimation => cfg.enable_token_estimation = parse_bool(value)?,
        SettingKey::InputGuardEnabled
        | SettingKey::EnableSandbox
        | SettingKey::DefaultRiskLevel
        | SettingKey::SandboxOffAction
        | SettingKey::SandboxTimeout => {
            panic!("security setting {key:?} must use apply_security")
        }
        SettingKey::MemoryAutoRecall => {
            ensure_memory(cfg).auto_recall = parse_bool(value)?;
        }
        SettingKey::MemoryAutoRetain => {
            ensure_memory(cfg).auto_retain = parse_bool(value)?;
        }
        SettingKey::ContextAutoCompact => cfg.context_auto_compact.enabled = parse_bool(value)?,
        SettingKey::HistorySize => cfg.history_size = parse_usize(value)?,
        SettingKey::CompactFullEnabled => {
            cfg.context_auto_compact.full_compact_enabled = parse_bool(value)?;
        }
        SettingKey::CompactContextWindowTokens => {
            cfg.context_auto_compact.context_window_tokens = if value.is_empty() {
                None
            } else {
                Some(parse_usize(value)?)
            };
        }
        SettingKey::CompactMicroKeepRecent => {
            cfg.context_auto_compact.micro_keep_recent_messages = parse_usize(value)?;
        }
        SettingKey::CompactShellKeepRecent => {
            cfg.context_auto_compact.shell_keep_recent_commands = parse_usize(value)?;
        }
        SettingKey::CompactSummaryMaxTokens => {
            cfg.context_auto_compact.summary_max_tokens = parse_usize(value)?;
        }
        SettingKey::CompactMaxConsecutiveFailures => {
            cfg.context_auto_compact.max_consecutive_failures = parse_usize(value)?;
        }
        SettingKey::CompactReservedOutputTokens => {
            cfg.context_auto_compact.reserved_output_tokens = if value.is_empty() {
                None
            } else {
                Some(parse_usize(value)?)
            };
        }
        SettingKey::MaxShellMessages => cfg.max_shell_messages = parse_usize(value)?,
        SettingKey::ContextTokenBudget => {
            cfg.context_token_budget = if value.is_empty() {
                None
            } else {
                Some(parse_usize(value)?)
            };
        }
        SettingKey::EnableRemoteGitPrompt => cfg.enable_remote_git_prompt = parse_bool(value)?,
        SettingKey::RemoteRichPrompt => cfg.remote_rich_prompt = parse_bool(value)?,
        SettingKey::RemoteShowVenv => cfg.remote_show_venv = parse_bool(value)?,
        SettingKey::RemoteShowContainer => cfg.remote_show_container = parse_bool(value)?,
        SettingKey::RemoteShowKube => cfg.remote_show_kube = parse_bool(value)?,
        SettingKey::RemoteDangerPatterns => {
            cfg.remote_danger_patterns = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        SettingKey::LogLevel => cfg.log_level = value.to_string(),
        SettingKey::EnableLangfuse => cfg.enable_langfuse = parse_bool(value)?,
        SettingKey::LangfusePublicKey => cfg.langfuse_public_key = opt_string(value),
        SettingKey::LangfuseSecretKey => cfg.langfuse_secret_key = opt_string(value),
        SettingKey::LangfuseHost => cfg.langfuse_host = opt_string(value),
        SettingKey::EnableScripts => cfg.enable_scripts = parse_bool(value)?,
        SettingKey::PtyDaemonEnabled => cfg.pty_daemon_enabled = parse_bool(value)?,
        SettingKey::CodexAuthPath => cfg.codex_auth_path = opt_string(value),
        SettingKey::SessionDbPath => cfg.session_db_path = opt_string(value),
        SettingKey::SkillAutoSearch => cfg.skills.auto_search = parse_bool(value)?,
        // SkillRegistries is managed via interactive dialog, not text edit.
        SettingKey::SkillRegistries => {}
    }
    Ok(())
}
/// What live side-effect the shell must run after `key` changes.
pub fn live_effect(key: SettingKey) -> LiveEffect {
    match key {
        SettingKey::Model | SettingKey::ApiBase | SettingKey::ApiKey => LiveEffect::ModelSession,
        SettingKey::Temperature | SettingKey::MaxTokens => LiveEffect::ToolsRefresh,
        // Security keys are applied via `is_security_setting` in app.rs.
        _ => LiveEffect::None,
    }
}

/// Whether the running session must be restarted for `key`'s new value to take
/// effect. The model/credential fields are applied live via `update_model`,
/// temperature/max_tokens rebuild tools live, Security settings are pushed into
/// the running `SecurityManager` + InputGuard, and `prompt_style` is read from
/// `self.config` on each render — everything else is consumed only at startup.
pub fn requires_restart(key: SettingKey) -> bool {
    if is_security_setting(key) {
        return false;
    }
    match key {
        SettingKey::Model
        | SettingKey::ApiBase
        | SettingKey::ApiKey
        | SettingKey::Temperature
        | SettingKey::MaxTokens
        | SettingKey::PromptStyle => false,
        // SkillAutoSearch is read once at AiHandler construction, so a change
        // only takes effect after restart (live toggling silently failed
        // before). SkillRegistries likewise (AI tools capture a config
        // snapshot at registration time).
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bool_str(b: bool) -> String {
    if b {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

fn opt_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Lazily initialize the nullable memory sub-config so its fields can be edited.
fn ensure_memory(cfg: &mut ConfigModel) -> &mut aish_config::MemoryConfig {
    cfg.memory.get_or_insert_with(Default::default)
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "on" | "1" | "yes" | "y" => Ok(true),
        "false" | "off" | "0" | "no" | "n" => Ok(false),
        other => Err(format!("expected on/off, got `{other}`")),
    }
}

fn parse_f32(value: &str, range: std::ops::RangeInclusive<f32>) -> Result<f32, String> {
    let n: f32 = value
        .parse()
        .map_err(|_| format!("expected a number, got `{value}`"))?;
    if !range.contains(&n) {
        return Err(format!(
            "{} is out of range {}..={}",
            n,
            range.start(),
            range.end()
        ));
    }
    Ok(n)
}

fn parse_f64(value: &str, range: std::ops::RangeInclusive<f64>) -> Result<f64, String> {
    let n: f64 = value
        .parse()
        .map_err(|_| format!("expected a number, got `{value}`"))?;
    if !range.contains(&n) {
        return Err(format!(
            "{} is out of range {}..={}",
            n,
            range.start(),
            range.end()
        ));
    }
    Ok(n)
}

fn parse_u32(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("expected a non-negative integer, got `{value}`"))
}

fn parse_usize(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("expected a non-negative integer, got `{value}`"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> ConfigModel {
        ConfigModel::default()
    }

    #[test]
    fn catalog_is_nonempty_and_covers_all_categories() {
        assert!(SETTINGS.len() >= 15, "catalog should be curated");
        for cat in SettingCategory::ALL {
            assert!(
                entries_for(*cat).count() > 0,
                "category {:?} has no entries",
                cat
            );
        }
    }

    #[test]
    fn each_key_resolves_to_a_definition() {
        // Every SettingKey variant must map to exactly one catalog entry.
        let all_keys = [
            SettingKey::Model,
            SettingKey::ApiBase,
            SettingKey::ApiKey,
            SettingKey::Temperature,
            SettingKey::MaxTokens,
            SettingKey::PromptStyle,
            SettingKey::OutputLanguage,
            SettingKey::AutoSuggest,
            SettingKey::InlineCompletion,
            SettingKey::InlineDebounceMs,
            SettingKey::InlineContextLines,
            SettingKey::InlineMaxTokens,
            SettingKey::InlineMinInputChars,
            SettingKey::InlineTimeoutSecs,
            SettingKey::InlineDisableThinking,
            SettingKey::InlineEnforceJson,
            SettingKey::MaxLlmMessages,
            SettingKey::EnableTokenEstimation,
            SettingKey::InputGuardEnabled,
            SettingKey::EnableSandbox,
            SettingKey::DefaultRiskLevel,
            SettingKey::SandboxOffAction,
            SettingKey::SandboxTimeout,
            SettingKey::MemoryAutoRecall,
            SettingKey::MemoryAutoRetain,
            SettingKey::ContextAutoCompact,
            SettingKey::CompactFullEnabled,
            SettingKey::CompactContextWindowTokens,
            SettingKey::CompactMicroKeepRecent,
            SettingKey::CompactShellKeepRecent,
            SettingKey::CompactSummaryMaxTokens,
            SettingKey::CompactMaxConsecutiveFailures,
            SettingKey::CompactReservedOutputTokens,
            SettingKey::HistorySize,
            SettingKey::MaxShellMessages,
            SettingKey::ContextTokenBudget,
            SettingKey::EnableRemoteGitPrompt,
            SettingKey::RemoteRichPrompt,
            SettingKey::RemoteShowVenv,
            SettingKey::RemoteShowContainer,
            SettingKey::RemoteShowKube,
            SettingKey::RemoteDangerPatterns,
            SettingKey::LogLevel,
            SettingKey::EnableLangfuse,
            SettingKey::LangfusePublicKey,
            SettingKey::LangfuseSecretKey,
            SettingKey::LangfuseHost,
            SettingKey::EnableScripts,
            SettingKey::PtyDaemonEnabled,
            SettingKey::CodexAuthPath,
            SettingKey::SessionDbPath,
            SettingKey::SkillAutoSearch,
            SettingKey::SkillRegistries,
        ];
        assert_eq!(all_keys.len(), SETTINGS.len());
        for k in all_keys {
            assert_eq!(find(k).key, k);
        }
    }

    #[test]
    fn choice_current_value_is_case_normalized() {
        // Security enums always render as canonical lowercase options.
        let mut policy = SecurityPolicy::default();
        policy.default_risk_level = RiskLevel::Low;
        assert_eq!(
            security_current_raw(&policy, SettingKey::DefaultRiskLevel),
            "low"
        );
        let mut cfg = default_cfg();
        cfg.log_level = "DEBUG".into();
        assert_eq!(current_raw(&cfg, SettingKey::LogLevel), "debug");
        // A value outside the option set is passed through unchanged.
        cfg.log_level = "weird".into();
        assert_eq!(current_raw(&cfg, SettingKey::LogLevel), "weird");
    }

    #[test]
    fn security_sandbox_roundtrip_uses_policy_not_config() {
        let mut policy = SecurityPolicy::default();
        assert_eq!(
            security_current_raw(&policy, SettingKey::EnableSandbox),
            "false"
        );
        apply_security(&mut policy, SettingKey::EnableSandbox, "on").unwrap();
        assert!(policy.enable_sandbox);
        apply_security(&mut policy, SettingKey::SandboxOffAction, "block").unwrap();
        assert_eq!(policy.sandbox_off_action, SandboxOffAction::Block);
        assert_eq!(
            security_current_raw(&policy, SettingKey::SandboxOffAction),
            "block"
        );
    }

    #[test]
    fn roundtrip_bool_toggle() {
        let mut policy = SecurityPolicy::default();
        assert_eq!(
            security_current_raw(&policy, SettingKey::InputGuardEnabled),
            "true"
        );
        apply_security(&mut policy, SettingKey::InputGuardEnabled, "off").unwrap();
        assert!(!policy.input_guard.enabled);
        assert_eq!(
            security_current_raw(&policy, SettingKey::InputGuardEnabled),
            "false"
        );
    }

    #[test]
    fn temperature_rejects_out_of_range() {
        let mut cfg = default_cfg();
        assert!(apply(&mut cfg, SettingKey::Temperature, "3.0").is_err());
        apply(&mut cfg, SettingKey::Temperature, "0.8").unwrap();
        assert!((cfg.temperature - 0.8).abs() < 1e-6);
    }

    #[test]
    fn choice_rejects_unknown_value() {
        // Typos must be rejected so they can't silently disable logging or
        // mis-grade security risk.
        let mut cfg = default_cfg();
        assert!(apply(&mut cfg, SettingKey::LogLevel, "warb").is_err());
        let mut policy = SecurityPolicy::default();
        assert!(apply_security(&mut policy, SettingKey::DefaultRiskLevel, "banana").is_err());
        // Valid values pass through.
        apply(&mut cfg, SettingKey::LogLevel, "debug").unwrap();
        assert_eq!(cfg.log_level, "debug");
        apply_security(&mut policy, SettingKey::DefaultRiskLevel, "high").unwrap();
        assert_eq!(policy.default_risk_level, RiskLevel::High);
    }

    #[test]
    fn requires_restart_classification() {
        // Live settings must not claim a restart is needed.
        assert!(!requires_restart(SettingKey::Model));
        assert!(!requires_restart(SettingKey::InputGuardEnabled));
        assert!(!requires_restart(SettingKey::PromptStyle));
        // Security globals are pushed live into the SecurityManager.
        assert!(!requires_restart(SettingKey::EnableSandbox));
        assert!(!requires_restart(SettingKey::DefaultRiskLevel));
        assert!(!requires_restart(SettingKey::SandboxOffAction));
        assert!(!requires_restart(SettingKey::SandboxTimeout));
        // Startup-only settings do require a restart.
        assert!(requires_restart(SettingKey::LogLevel));
        assert!(requires_restart(SettingKey::HistorySize));
        assert!(requires_restart(SettingKey::SkillAutoSearch));
    }

    #[test]
    fn max_tokens_clears_on_empty() {
        let mut cfg = default_cfg();
        apply(&mut cfg, SettingKey::MaxTokens, "4096").unwrap();
        assert_eq!(cfg.max_tokens, Some(4096));
        apply(&mut cfg, SettingKey::MaxTokens, "").unwrap();
        assert_eq!(cfg.max_tokens, None);
    }

    #[test]
    fn memory_fields_lazy_init() {
        let mut cfg = default_cfg();
        assert!(cfg.memory.is_none());
        apply(&mut cfg, SettingKey::MemoryAutoRecall, "false").unwrap();
        let m = cfg.memory.as_ref().expect("memory initialized");
        assert!(!m.auto_recall);
    }

    #[test]
    fn nested_inline_completion_toggle() {
        let mut cfg = default_cfg();
        assert!(!cfg.inline_completion.enabled);
        apply(&mut cfg, SettingKey::InlineCompletion, "on").unwrap();
        assert!(cfg.inline_completion.enabled);
    }

    #[test]
    fn model_fields_signal_session_refresh() {
        assert_eq!(live_effect(SettingKey::Model), LiveEffect::ModelSession);
        assert_eq!(live_effect(SettingKey::ApiKey), LiveEffect::ModelSession);
        // Security keys are handled outside LiveEffect.
        assert_eq!(live_effect(SettingKey::InputGuardEnabled), LiveEffect::None);
        assert_eq!(live_effect(SettingKey::EnableSandbox), LiveEffect::None);
        assert_eq!(live_effect(SettingKey::DefaultRiskLevel), LiveEffect::None);
        assert_eq!(live_effect(SettingKey::SandboxOffAction), LiveEffect::None);
        assert_eq!(live_effect(SettingKey::SandboxTimeout), LiveEffect::None);
    }

    /// Regression: `default_raw_of` must track factory defaults. Security keys
    /// come from `SecurityPolicy::default()`; the rest from `ConfigModel`.
    #[test]
    fn default_raw_of_matches_factory_defaults_for_all_keys() {
        let cfg = ConfigModel::default();
        let policy = SecurityPolicy::default();
        for def in SETTINGS {
            let expected = raw_value(&cfg, &policy, def.key);
            let actual = default_raw_of(def.key);
            assert_eq!(
                actual, expected,
                "default_raw_of({:?}) drifts from factory default",
                def.key
            );
        }
    }

    /// The RemoteDangerPatterns default specifically — non-empty and matches
    /// the factory list. Catches the original P1 wipe bug directly.
    #[test]
    fn default_raw_of_remote_danger_patterns_is_factory_list() {
        let d = default_raw_of(SettingKey::RemoteDangerPatterns);
        assert!(!d.is_empty(), "danger patterns default must not be empty");
        assert!(d.contains("^prod-"));
        assert!(d.contains("^production"));
        // Fresh config matches default → is_changed must be false.
        let fresh = current_raw(&ConfigModel::default(), SettingKey::RemoteDangerPatterns);
        assert!(!is_changed(SettingKey::RemoteDangerPatterns, &fresh));
    }
}
