use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Helper functions for serde defaults
// ---------------------------------------------------------------------------

fn default_recall_token_budget() -> usize {
    512
}

fn default_theme() -> String {
    "dark".into()
}

fn default_true() -> bool {
    true
}

fn default_max_lines() -> usize {
    5
}

fn default_max_chars() -> usize {
    100
}

fn default_max_items() -> usize {
    10
}

fn default_max_llm_messages() -> usize {
    50
}

fn default_max_shell_messages() -> usize {
    20
}

fn default_history_size() -> usize {
    1000
}

fn default_terminal_resize_mode() -> String {
    "full".into()
}

fn default_micro_keep_recent_messages() -> usize {
    6
}

fn default_shell_keep_recent_commands() -> usize {
    8
}

fn default_max_consecutive_compact_failures() -> usize {
    3
}

fn default_summary_max_tokens() -> usize {
    4000
}

fn default_remote_danger_patterns() -> Vec<String> {
    vec![
        "^prod-".into(),
        "^prod\\.".into(),
        "^prd-".into(),
        "^prd\\.".into(),
        "-prod\\.".into(),
        "^production".into(),
        "^release-".into(),
        "^live-".into(),
    ]
}

// ---------------------------------------------------------------------------
// Memory sub-config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub auto_recall: bool,
    pub auto_retain: bool,
    pub recall_limit: usize,
    #[serde(default = "default_recall_token_budget")]
    pub recall_token_budget: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            auto_recall: true,
            auto_retain: true,
            recall_limit: 5,
            recall_token_budget: default_recall_token_budget(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool argument preview sub-config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArgPreviewConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_lines")]
    pub max_lines: usize,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    #[serde(default = "default_max_items")]
    pub max_items: usize,
}

impl Default for ToolArgPreviewConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_lines: default_max_lines(),
            max_chars: default_max_chars(),
            max_items: default_max_items(),
        }
    }
}

// ---------------------------------------------------------------------------
// Output offload sub-config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputOffloadConfig {
    pub base_dir: Option<String>,
}

// ---------------------------------------------------------------------------
// Context auto-compact sub-config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextAutoCompactConfig {
    pub enabled: bool,
    pub full_compact_enabled: bool,
    pub context_window_tokens: Option<usize>,
    pub model_context_windows: HashMap<String, usize>,
    pub reserved_output_tokens: Option<usize>,
    pub auto_compact_buffer_tokens: Option<usize>,
    pub warning_buffer_tokens: Option<usize>,
    pub blocking_buffer_tokens: Option<usize>,
    #[serde(default = "default_micro_keep_recent_messages")]
    pub micro_keep_recent_messages: usize,
    #[serde(default = "default_shell_keep_recent_commands")]
    pub shell_keep_recent_commands: usize,
    #[serde(default = "default_max_consecutive_compact_failures")]
    pub max_consecutive_failures: usize,
    #[serde(default = "default_summary_max_tokens")]
    pub summary_max_tokens: usize,
}

impl Default for ContextAutoCompactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            full_compact_enabled: true,
            context_window_tokens: None,
            model_context_windows: HashMap::new(),
            reserved_output_tokens: None,
            auto_compact_buffer_tokens: None,
            warning_buffer_tokens: None,
            blocking_buffer_tokens: None,
            micro_keep_recent_messages: default_micro_keep_recent_messages(),
            shell_keep_recent_commands: default_shell_keep_recent_commands(),
            max_consecutive_failures: default_max_consecutive_compact_failures(),
            summary_max_tokens: default_summary_max_tokens(),
        }
    }
}

// ---------------------------------------------------------------------------
// Inline AI completion sub-config
// ---------------------------------------------------------------------------

/// Configuration for inline AI completion (the gray ghost-text suggestions
/// shown when the user types a `;`/`;`-prefixed AI prompt).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InlineCompletionConfig {
    /// Master toggle. When false, no InlineCompleter is constructed.
    pub enabled: bool,

    /// How long after the user stops typing before we fire a request.
    pub debounce_ms: u64,

    /// Number of recent shell-history lines to include as context.
    pub context_lines: usize,

    /// Hard cap on tokens for the suggested suffix.
    pub max_tokens: u32,

    /// Minimum non-prefix character count required to trigger.
    pub min_input_chars: usize,

    /// When true, send a bundle of "skip reasoning" flags in the request
    /// body: `thinking: {type: disabled}` (Anthropic), `enable_thinking:
    /// false` (Qwen/DeepSeek custom), `chat_template_kwargs: {enable_thinking:
    /// false}` (vLLM), `reasoning_effort: low` (OpenAI o1-style).
    /// Default is **false**: many gateways either ignore these fields
    /// (DeepSeek still generates reasoning) or process them slowly
    /// (moonshot response time doubles). Enable only if your model is a
    /// Qwen3/vLLM deployment that genuinely honors `enable_thinking`.
    pub disable_thinking: bool,

    /// When true, send `{"response_format": {"type": "json_object"}}` to
    /// force OpenAI-compatible JSON-mode output. Some gateways reject this
    /// field with HTTP 400, so default is false — the system prompt alone
    /// asks the model for JSON. Enable if your provider supports JSON mode
    /// and you want stricter output enforcement.
    pub enforce_json: bool,

    /// Hard cap (seconds) on a single inline-completion LLM call. If the
    /// model doesn't respond within this window, the request is abandoned
    /// silently. The underlying `LlmClient` has its own 120s timeout —
    /// this overrides that for inline completion because waiting two
    /// minutes for a hint is unacceptable. Bump this if your gateway is
    /// consistently slow.
    pub timeout_secs: u64,
}

impl Default for InlineCompletionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debounce_ms: 400,
            context_lines: 5,
            // Reasoning models (DeepSeek-R1, Qwen3, GLM, etc.) spend a lot
            // of tokens on chain-of-thought before emitting visible content.
            // `enable_thinking: false` is sent but many gateways (notably
            // DeepSeek) ignore it and still produce 1000-2000 chars of
            // reasoning_content. We need a budget large enough for the
            // full reasoning + the JSON answer: 512 covers the ~480-token
            // reasoning we've observed in the wild. cap_suffix_width()
            // keeps the ghost text short regardless of token consumption.
            max_tokens: 512,
            min_input_chars: 3,
            // Default OFF: many gateways either ignore these flags (DeepSeek
            // still generates reasoning) or process them slowly (moonshot
            // response time doubles). See the field doc above for details.
            disable_thinking: false,
            enforce_json: false,
            timeout_secs: 30,
        }
    }
}

// ---------------------------------------------------------------------------
// Skill registry sub-config
// ---------------------------------------------------------------------------

/// A single skill registry source entry.
///
/// Users can configure multiple registries; each maps to an adapter type
/// that knows how to search and install skills from that source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RegistrySource {
    /// Human-readable name used in UI and as reference key
    /// (e.g. "skills_sh", "skillhub_cn", "uniontech").
    pub name: String,

    /// Adapter type: "skills_sh", "skillhub", or "clawhub".
    /// Determines which search API format and install mechanism is used.
    #[serde(rename = "type")]
    pub registry_type: String,

    /// Whether this registry is queried during search.
    pub enabled: bool,

    /// Base URL of the registry API.
    /// skills.sh: "https://skills.sh"
    /// skillhub:  "https://api.skillhub.cn"
    /// clawhub:   "https://clawhub.ai"
    pub url: String,
}

impl Default for RegistrySource {
    fn default() -> Self {
        Self {
            name: String::new(),
            registry_type: String::new(),
            enabled: true,
            url: String::new(),
        }
    }
}

/// Skill marketplace configuration.
///
/// Controls auto-search behavior and the list of registry sources.
/// Pre-configured with skills.sh and skillhub.cn as defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// When true, the AI automatically searches registries if no loaded
    /// skill matches the user's request.
    pub auto_search: bool,

    /// Name of the default registry (must match an entry in `registries`).
    /// Used as the primary source for CLI and AI-driven searches.
    pub default_registry: String,

    /// Ordered list of registry sources. Higher entries take priority.
    pub registries: Vec<RegistrySource>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            auto_search: true,
            default_registry: "skills_sh".into(),
            registries: vec![
                RegistrySource {
                    name: "skills_sh".into(),
                    registry_type: "skills_sh".into(),
                    enabled: true,
                    url: "https://skills.sh".into(),
                },
                RegistrySource {
                    name: "skillhub_cn".into(),
                    registry_type: "skillhub".into(),
                    enabled: true,
                    url: "https://api.skillhub.cn".into(),
                },
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level config model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigModel {
    pub model: String,
    pub api_base: String,
    pub api_key: String,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    pub prompt_theme: String,

    // Per-tool argument preview settings
    #[serde(default)]
    pub tool_arg_preview: ToolArgPreviewConfig,
    pub tool_arg_preview_max_length: usize,

    pub bash_output_offload: Option<OutputOffloadConfig>,
    pub pty_output_keep_bytes: usize,
    pub memory: Option<MemoryConfig>,
    pub session_db_path: Option<String>,
    pub enable_sandbox: bool,

    /// Inject git branch awareness into remote bash prompts during SSH/telnet
    /// sessions. When true, aish prepends a `|branch` marker (magenta, matching
    /// local prompt style) to the remote PS1 by installing a PROMPT_COMMAND
    /// hook. Default: true. Set false if you use starship/oh-my-posh on remote
    /// machines and want to avoid duplicate branch display.
    #[serde(default = "default_true")]
    pub enable_remote_git_prompt: bool,

    /// Master switch for environment-aware PS1 injection. When false, the
    /// legacy `[ssh:host]` literal is used with no probe, no jump chain,
    /// no container/kube segments. Default: true.
    #[serde(default = "default_true")]
    pub remote_rich_prompt: bool,

    /// Hostname regex patterns that escalate the PS1 marker to Danger
    /// color.
    ///
    /// - Missing key in YAML: built-in defaults are applied.
    /// - Empty list `[]`: NO danger patterns — every host renders with
    ///   non-danger color (explicit disable, no fallback to defaults).
    /// - Non-empty list: replaces defaults entirely.
    ///
    /// Built-in defaults: `^prod-`, `^prod\.`, `^prd-`, `^prd\.`, `-prod\.`,
    /// `^production`,
    /// `^release-`, `^live-`.
    #[serde(default = "default_remote_danger_patterns")]
    pub remote_danger_patterns: Vec<String>,

    /// Show venv/conda name segment in remote PS1. Default: true.
    #[serde(default = "default_true")]
    pub remote_show_venv: bool,

    /// Show container segment (`docker`/`podman`/etc) in remote PS1. Default: true.
    #[serde(default = "default_true")]
    pub remote_show_container: bool,

    /// Show kube context segment in remote PS1. Default: true.
    #[serde(default = "default_true")]
    pub remote_show_kube: bool,

    pub sandbox_off_action: String,
    pub sandbox_timeout_seconds: f64,
    pub default_risk_level: String,

    /// Master switch for InputGuard (BLOCKED / Confirm prompts).
    /// When false, all input passes through unchecked. Mirrors the
    /// `input_guard.enabled` field in security_policy.yaml so users
    /// can toggle it from the more familiar config.yaml.
    #[serde(default = "default_true")]
    pub input_guard_enabled: bool,
    pub langfuse_public_key: Option<String>,
    pub langfuse_secret_key: Option<String>,
    pub langfuse_host: Option<String>,
    pub log_level: String,
    pub log_file: Option<String>,

    // --- New fields (Phase 5) ---
    /// Prompt style character (e.g. "🚀", "→", "$")
    #[serde(default)]
    pub prompt_style: Option<String>,

    /// UI theme: "dark" or "light"
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Enable auto-suggest completions
    #[serde(default)]
    pub auto_suggest: bool,

    /// Preferred output language for AI responses
    #[serde(default)]
    pub output_language: Option<String>,

    /// Path to OpenAI Codex auth.json
    #[serde(default)]
    pub codex_auth_path: Option<String>,

    /// Whether the current configuration uses a free API key
    #[serde(default)]
    pub is_free_key: bool,

    /// Enable Langfuse integration for LLM observability
    #[serde(default)]
    pub enable_langfuse: bool,

    /// Pre-approved AI commands that skip confirmation
    #[serde(default)]
    pub approved_ai_commands: Vec<String>,

    /// Maximum number of LLM conversation messages to keep in context
    #[serde(default = "default_max_llm_messages")]
    pub max_llm_messages: usize,

    /// Maximum number of shell history entries to keep in context
    #[serde(default = "default_max_shell_messages")]
    pub max_shell_messages: usize,

    /// Optional token budget limit for context
    #[serde(default)]
    pub context_token_budget: Option<usize>,

    /// Enable tiktoken-based token estimation for context trimming
    #[serde(default = "default_true")]
    pub enable_token_estimation: bool,

    /// Automatic context pressure handling and compaction settings
    #[serde(default)]
    pub context_auto_compact: ContextAutoCompactConfig,

    /// Enable script system (hooks, hot-reload, custom prompts)
    #[serde(default = "default_true")]
    pub enable_scripts: bool,

    /// Maximum command history size
    #[serde(default = "default_history_size")]
    pub history_size: usize,

    /// Terminal resize handling mode: full, pty_only, or off
    #[serde(default = "default_terminal_resize_mode")]
    pub terminal_resize_mode: String,

    /// Inline AI completion (ghost-text suggestions in AI mode).
    #[serde(default)]
    pub inline_completion: InlineCompletionConfig,

    /// Enable PTY daemon session persistence (tmux-like).
    /// When true, aish runs inside a daemon-managed PTY that survives
    /// terminal disconnects. Sessions can be resumed with 'aish' or
    /// listed with 'aish sessions'. Default: true.
    /// When false, aish runs standalone (old behavior: exit = close).
    #[serde(default = "default_true")]
    pub pty_daemon_enabled: bool,
    /// Skill marketplace: auto-search + registry sources.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Additional API accounts for multi-key quota rotation under the same
    /// provider. The top-level `api_key` is always the primary (account #0);
    /// entries here are tried in order when the primary hits a rate/usage limit.
    #[serde(default)]
    pub api_accounts: Vec<ApiAccountConfig>,
    /// Ordered fallback model names tried when the primary model fails
    /// (rate-limit / usage-limit / exhausted 5xx). Each runs with the active account.
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Models the user has switched to via /model, most-recent-first. Surfaced
    /// at the top of the picker so switching back is one keystroke.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_models: Vec<String>,
    /// Restore the primary model after its cooldown window expires following
    /// a fallback. Default: true.
    #[serde(default = "default_true")]
    pub fallback_revert_on_cooldown: bool,
}

impl Default for ConfigModel {
    fn default() -> Self {
        Self {
            model: String::new(),
            api_base: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            temperature: 0.3,
            max_tokens: None,
            prompt_theme: "default".to_string(),
            tool_arg_preview: ToolArgPreviewConfig::default(),
            tool_arg_preview_max_length: 200,
            bash_output_offload: None,
            pty_output_keep_bytes: 4096,
            memory: None,
            session_db_path: None,
            enable_sandbox: false,
            enable_remote_git_prompt: true,
            remote_rich_prompt: default_true(),
            remote_danger_patterns: default_remote_danger_patterns(),
            remote_show_venv: default_true(),
            remote_show_container: default_true(),
            remote_show_kube: default_true(),
            sandbox_off_action: "allow".to_string(),
            sandbox_timeout_seconds: 10.0,
            default_risk_level: "low".to_string(),
            input_guard_enabled: default_true(),
            langfuse_public_key: None,
            langfuse_secret_key: None,
            langfuse_host: None,
            log_level: "warn".to_string(),
            log_file: None,
            prompt_style: None,
            theme: default_theme(),
            auto_suggest: false,
            output_language: None,
            codex_auth_path: None,
            is_free_key: false,
            enable_langfuse: false,
            approved_ai_commands: vec![],
            max_llm_messages: default_max_llm_messages(),
            max_shell_messages: default_max_shell_messages(),
            context_token_budget: None,
            enable_token_estimation: default_true(),
            context_auto_compact: ContextAutoCompactConfig::default(),
            enable_scripts: default_true(),
            history_size: default_history_size(),
            terminal_resize_mode: default_terminal_resize_mode(),
            inline_completion: InlineCompletionConfig::default(),
            pty_daemon_enabled: default_true(),
            skills: SkillsConfig::default(),
            api_accounts: vec![],
            fallback_models: vec![],
            recent_models: vec![],
            fallback_revert_on_cooldown: default_true(),
        }
    }
}

/// One additional API credential for multi-key quota rotation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiAccountConfig {
    /// Human-readable label shown in the rotation UI.
    pub name: String,
    pub api_key: String,
    /// Optional override of the provider base URL for this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Optional per-account model override. When set, this account uses its
    /// own model regardless of the global `model` / fallback chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Relative selection weight (advisory; higher = preferred).
    #[serde(default = "default_one")]
    pub weight: u32,
    /// Skip this account entirely when true.
    #[serde(default)]
    pub disabled: bool,
}

fn default_one() -> u32 {
    1
}

/// Compile the `remote_danger_patterns` into `Regex` objects, logging each
/// invalid pattern at warn level and skipping it. Patterns come from config
/// and don't change during a session, so callers should compile once and
/// cache the result rather than recompiling on every prompt injection.
///
/// `tracing::warn!` makes typo'd patterns visible to users (previously they
/// were silently ignored by `Regex::new(...).ok()`).
pub fn compile_remote_danger_patterns(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| {
            if p.is_empty() {
                return None;
            }
            match Regex::new(p) {
                Ok(re) => Some(re),
                Err(e) => {
                    tracing::warn!(
                        pattern = p.as_str(),
                        error = %e,
                        "skipping invalid remote_danger_patterns regex"
                    );
                    None
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_has_theme() {
        let config = ConfigModel::default();
        assert_eq!(config.theme, "dark");
    }

    #[test]
    fn test_default_tool_arg_preview() {
        let preview = ToolArgPreviewConfig::default();
        assert!(preview.enabled);
        assert_eq!(preview.max_lines, 5);
        assert_eq!(preview.max_chars, 100);
        assert_eq!(preview.max_items, 10);
    }

    #[test]
    fn test_config_deserialize_with_new_fields() {
        let yaml = r#"
model: gpt-4
api_base: https://api.example.com/v1
api_key: sk-test
temperature: 0.7
prompt_style: "🚀"
theme: light
auto_suggest: true
output_language: zh-CN
tool_arg_preview:
  enabled: false
  max_lines: 3
  max_chars: 80
  max_items: 5
"#;
        let config: ConfigModel = serde_yaml::from_str(yaml).expect("failed to parse YAML");
        assert_eq!(config.model, "gpt-4");
        assert_eq!(config.prompt_style.as_deref(), Some("🚀"));
        assert_eq!(config.theme, "light");
        assert!(config.auto_suggest);
        assert_eq!(config.output_language.as_deref(), Some("zh-CN"));
        assert!(!config.tool_arg_preview.enabled);
        assert_eq!(config.tool_arg_preview.max_lines, 3);
        assert_eq!(config.tool_arg_preview.max_chars, 80);
        assert_eq!(config.tool_arg_preview.max_items, 5);
    }

    #[test]
    fn test_config_defaults_without_new_fields() {
        let yaml = r#"
model: gpt-4
api_base: https://api.example.com/v1
api_key: sk-test
"#;
        let config: ConfigModel = serde_yaml::from_str(yaml).expect("failed to parse YAML");
        assert_eq!(config.theme, "dark");
        assert!(!config.auto_suggest);
        assert!(config.prompt_style.is_none());
        assert!(config.output_language.is_none());
        assert!(config.tool_arg_preview.enabled);
        assert_eq!(config.tool_arg_preview.max_lines, 5);
        assert_eq!(config.tool_arg_preview.max_chars, 100);
        assert_eq!(config.tool_arg_preview.max_items, 10);
    }

    #[test]
    fn test_new_fields_default_values() {
        let config = ConfigModel::default();
        assert!(config.codex_auth_path.is_none());
        assert!(!config.is_free_key);
        assert!(!config.enable_langfuse);
        assert!(config.approved_ai_commands.is_empty());
        assert_eq!(config.max_llm_messages, 50);
        assert_eq!(config.max_shell_messages, 20);
        assert!(config.context_token_budget.is_none());
        assert!(config.enable_token_estimation);
        assert!(config.context_auto_compact.enabled);
        assert!(config.context_auto_compact.full_compact_enabled);
        assert_eq!(config.context_auto_compact.micro_keep_recent_messages, 6);
        assert_eq!(config.context_auto_compact.shell_keep_recent_commands, 8);
        assert!(config.enable_scripts);
        assert_eq!(config.history_size, 1000);
        assert_eq!(config.terminal_resize_mode, "full");
    }

    #[test]
    fn test_new_fields_deserialize() {
        let yaml = r#"
model: gpt-4
api_base: https://api.example.com/v1
api_key: sk-test
codex_auth_path: /tmp/auth.json
is_free_key: true
enable_langfuse: true
approved_ai_commands:
  - "ls"
  - "git status"
max_llm_messages: 30
max_shell_messages: 10
context_token_budget: 8000
enable_token_estimation: false
context_auto_compact:
    enabled: true
    full_compact_enabled: false
    context_window_tokens: 32000
    model_context_windows:
      openai/glm-5.1: 128000
    reserved_output_tokens: 4000
    auto_compact_buffer_tokens: 3000
    warning_buffer_tokens: 2000
    blocking_buffer_tokens: 500
    micro_keep_recent_messages: 4
    shell_keep_recent_commands: 3
    max_consecutive_failures: 2
    summary_max_tokens: 1200
enable_scripts: false
history_size: 500
terminal_resize_mode: pty_only
"#;
        let config: ConfigModel = serde_yaml::from_str(yaml).expect("failed to parse YAML");
        assert_eq!(config.codex_auth_path.as_deref(), Some("/tmp/auth.json"));
        assert!(config.is_free_key);
        assert!(config.enable_langfuse);
        assert_eq!(config.approved_ai_commands, vec!["ls", "git status"]);
        assert_eq!(config.max_llm_messages, 30);
        assert_eq!(config.max_shell_messages, 10);
        assert_eq!(config.context_token_budget, Some(8000));
        assert!(!config.enable_token_estimation);
        assert!(config.context_auto_compact.enabled);
        assert!(!config.context_auto_compact.full_compact_enabled);
        assert_eq!(
            config.context_auto_compact.context_window_tokens,
            Some(32000)
        );
        assert_eq!(
            config
                .context_auto_compact
                .model_context_windows
                .get("openai/glm-5.1"),
            Some(&128000)
        );
        assert_eq!(
            config.context_auto_compact.reserved_output_tokens,
            Some(4000)
        );
        assert_eq!(
            config.context_auto_compact.auto_compact_buffer_tokens,
            Some(3000)
        );
        assert_eq!(
            config.context_auto_compact.warning_buffer_tokens,
            Some(2000)
        );
        assert_eq!(
            config.context_auto_compact.blocking_buffer_tokens,
            Some(500)
        );
        assert_eq!(config.context_auto_compact.micro_keep_recent_messages, 4);
        assert_eq!(config.context_auto_compact.shell_keep_recent_commands, 3);
        assert_eq!(config.context_auto_compact.max_consecutive_failures, 2);
        assert_eq!(config.context_auto_compact.summary_max_tokens, 1200);
        assert!(!config.enable_scripts);
        assert_eq!(config.history_size, 500);
        assert_eq!(config.terminal_resize_mode, "pty_only");
    }

    #[test]
    fn test_new_fields_missing_means_defaults() {
        let yaml = r#"
model: gpt-4
api_base: https://api.example.com/v1
api_key: sk-test
"#;
        let config: ConfigModel = serde_yaml::from_str(yaml).expect("failed to parse YAML");
        assert!(config.codex_auth_path.is_none());
        assert!(!config.is_free_key);
        assert!(!config.enable_langfuse);
        assert!(config.approved_ai_commands.is_empty());
        assert_eq!(config.max_llm_messages, 50);
        assert_eq!(config.max_shell_messages, 20);
        assert!(config.context_token_budget.is_none());
        assert!(config.enable_token_estimation);
        assert!(config.context_auto_compact.enabled);
        assert!(config.context_auto_compact.full_compact_enabled);
        assert!(config.context_auto_compact.context_window_tokens.is_none());
        assert!(config.context_auto_compact.model_context_windows.is_empty());
        assert!(config.enable_scripts);
        assert_eq!(config.history_size, 1000);
        assert_eq!(config.terminal_resize_mode, "full");
    }

    #[test]
    fn test_context_auto_compact_defaults() {
        let compact = ContextAutoCompactConfig::default();
        assert!(compact.enabled);
        assert!(compact.full_compact_enabled);
        assert!(compact.context_window_tokens.is_none());
        assert!(compact.model_context_windows.is_empty());
        assert_eq!(compact.micro_keep_recent_messages, 6);
        assert_eq!(compact.shell_keep_recent_commands, 8);
        assert_eq!(compact.max_consecutive_failures, 3);
        assert_eq!(compact.summary_max_tokens, 4000);
    }

    #[test]
    fn test_enable_remote_git_prompt_defaults_to_true() {
        let cfg: ConfigModel = serde_yaml::from_str("model: test\n").unwrap();
        assert!(
            cfg.enable_remote_git_prompt,
            "enable_remote_git_prompt must default to true"
        );
    }

    #[test]
    fn test_enable_remote_git_prompt_can_be_disabled() {
        let yaml = "model: test\nenable_remote_git_prompt: false\n";
        let cfg: ConfigModel = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.enable_remote_git_prompt);
    }

    #[test]
    fn test_remote_danger_patterns_defaults_nonempty() {
        let cfg = ConfigModel::default();
        assert!(
            !cfg.remote_danger_patterns.is_empty(),
            "must ship sensible defaults"
        );
        // Must catch common prod naming.
        let any_matches = cfg
            .remote_danger_patterns
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .any(|re| re.is_match("prod-web-03"));
        assert!(any_matches, "defaults must match prod-web-03");
    }

    #[test]
    fn test_remote_rich_prompt_defaults_true() {
        assert!(ConfigModel::default().remote_rich_prompt);
    }

    #[test]
    fn test_remote_show_flags_default_true() {
        let cfg = ConfigModel::default();
        assert!(cfg.remote_show_venv);
        assert!(cfg.remote_show_container);
        assert!(cfg.remote_show_kube);
    }

    #[test]
    fn test_remote_danger_patterns_empty_stays_empty() {
        // Explicit empty list must NOT fall back to defaults — it means
        // "disable danger escalation entirely". Locks the serde semantic
        // so future "fixes" don't silently reintroduce fallback behavior.
        let yaml = "model: test\nremote_danger_patterns: []\n";
        let cfg: ConfigModel = serde_yaml::from_str(yaml).expect("parse");
        assert!(
            cfg.remote_danger_patterns.is_empty(),
            "explicit empty must stay empty, no fallback"
        );
    }

    #[test]
    fn test_remote_config_overrides_apply() {
        let yaml = "model: test\nremote_rich_prompt: false\nremote_danger_patterns: ['^my-prod-']\nremote_show_venv: false\nremote_show_container: false\nremote_show_kube: false\n";
        let cfg: ConfigModel = serde_yaml::from_str(yaml).expect("parse");
        assert!(!cfg.remote_rich_prompt);
        assert!(!cfg.remote_show_venv);
        assert!(!cfg.remote_show_container);
        assert!(!cfg.remote_show_kube);
        assert_eq!(cfg.remote_danger_patterns, vec!["^my-prod-".to_string()]);
    }

    #[test]
    fn test_legacy_yaml_without_remote_config_loads() {
        let yaml = "model: test\napi_base: https://x\napi_key: k\n";
        let cfg: ConfigModel = serde_yaml::from_str(yaml).expect("parse");
        assert!(cfg.remote_rich_prompt);
        assert!(!cfg.remote_danger_patterns.is_empty());
    }

    #[test]
    fn test_compile_remote_danger_patterns_skips_invalid() {
        // Valid patterns compile; invalid ones are skipped without panic.
        let patterns = vec![
            "^prod-".to_string(),
            "".to_string(),  // empty skipped silently
            "[".to_string(), // invalid regex skipped with warn
            "-prod\\.".to_string(),
        ];
        let compiled = compile_remote_danger_patterns(&patterns);
        // 2 valid + 1 empty-skipped + 1 invalid-skipped => 2 compiled.
        assert_eq!(
            compiled.len(),
            2,
            "invalid and empty patterns must be skipped"
        );
        assert!(compiled[0].is_match("prod-web-03"));
    }

    #[test]
    fn test_compile_remote_danger_patterns_empty_input() {
        let compiled = compile_remote_danger_patterns(&[]);
        assert!(compiled.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Inline completion
    // ---------------------------------------------------------------------------

    #[test]
    fn inline_completion_defaults_are_off() {
        let cfg = InlineCompletionConfig::default();
        assert!(!cfg.enabled, "must be opt-in (off by default)");
        assert_eq!(cfg.debounce_ms, 400);
        assert_eq!(cfg.context_lines, 5);
        assert_eq!(cfg.max_tokens, 512);
        assert_eq!(cfg.min_input_chars, 3);
        assert!(
            !cfg.disable_thinking,
            "should be OFF by default — extras hurt non-reasoning models"
        );
        assert!(
            !cfg.enforce_json,
            "JSON mode should be opt-in (off by default)"
        );
        assert_eq!(cfg.timeout_secs, 30);
    }

    #[test]
    fn config_model_default_includes_inline_completion() {
        let m = ConfigModel::default();
        assert!(!m.inline_completion.enabled);
    }

    #[test]
    fn inline_completion_deserializes_from_yaml() {
        let yaml = r#"
inline_completion:
  enabled: true
  debounce_ms: 250
  context_lines: 8
  max_tokens: 16
  min_input_chars: 2
"#;
        let m: ConfigModel = serde_yaml::from_str(yaml).unwrap();
        assert!(m.inline_completion.enabled);
        assert_eq!(m.inline_completion.debounce_ms, 250);
        assert_eq!(m.inline_completion.context_lines, 8);
        assert_eq!(m.inline_completion.max_tokens, 16);
        assert_eq!(m.inline_completion.min_input_chars, 2);
    }

    #[test]
    fn inline_completion_partial_uses_defaults_for_missing_fields() {
        let yaml = r#"
inline_completion:
  enabled: true
"#;
        let m: ConfigModel = serde_yaml::from_str(yaml).unwrap();
        assert!(m.inline_completion.enabled);
        assert_eq!(m.inline_completion.debounce_ms, 400);
        assert_eq!(m.inline_completion.max_tokens, 512);
    }
}
