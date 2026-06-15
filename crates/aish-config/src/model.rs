use std::collections::HashMap;

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
// Status bar sub-config
// ---------------------------------------------------------------------------

fn default_statusbar_visible() -> bool {
    true
}

fn default_statusbar_style() -> String {
    "single".into()
}

fn default_refresh_interval_ms() -> u64 {
    500
}

/// Per-model pricing override entry for cost estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingEntry {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
}

/// Configuration for the bottom status bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusBarConfig {
    /// Whether the status bar is visible on startup.
    #[serde(default = "default_statusbar_visible")]
    pub visible: bool,
    /// Style: "single" (detailed 1-line) or "minimal" (compact 1-line).
    #[serde(default = "default_statusbar_style")]
    pub style: String,
    /// Refresh interval in milliseconds for idle-state updates.
    #[serde(default = "default_refresh_interval_ms")]
    pub refresh_interval_ms: u64,
    /// Optional per-model pricing overrides (USD per 1K tokens).
    pub pricing: HashMap<String, PricingEntry>,
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            visible: default_statusbar_visible(),
            style: default_statusbar_style(),
            refresh_interval_ms: default_refresh_interval_ms(),
            pricing: HashMap::new(),
        }
    }
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
    pub sandbox_off_action: String,
    pub sandbox_timeout_seconds: f64,
    pub default_risk_level: String,
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

    /// Status bar configuration (token usage display).
    #[serde(default)]
    pub statusbar: StatusBarConfig,
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
            sandbox_off_action: "allow".to_string(),
            sandbox_timeout_seconds: 10.0,
            default_risk_level: "low".to_string(),
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
            statusbar: StatusBarConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
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
}
