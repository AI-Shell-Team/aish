//! Setup wizard for interactive LLM provider configuration.
//!
//! Guides users through selecting a provider, entering credentials, choosing a model,
//! and verifying connectivity and tool support.

mod clack_filter;
pub mod clack_log;
pub mod clack_theme;
pub mod endpoints;
pub mod free_key;
pub mod model_fetch;
pub mod plan_approval;
pub mod plan_display;
pub mod prompts;
pub mod ui;
pub mod verification;

use std::path::PathBuf;

use aish_config::ConfigModel;
use aish_core::AishError;
use aish_i18n::{t, t_with_args};
use aish_llm::{normalize_model_for_provider, resolve_model_for_api};

use crate::tui::{DialogOption, DialogResult, CUSTOM_DIALOG_VALUE};
use ui::{show_searchable_selection, show_selection};

enum SaveNotice {
    Normal,
    WithWarning,
}

fn format_config_path(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

/// Merge wizard-produced setup fields into an existing config, preserving unrelated settings.
pub fn apply_setup_result(existing: &ConfigModel, setup: ConfigModel) -> ConfigModel {
    let mut merged = existing.clone();
    merged.model = setup.model;
    merged.api_base = setup.api_base;
    merged.api_key = setup.api_key;
    merged.is_free_key = setup.is_free_key;
    merged
}

/// Default OpenAI-compatible API base for providers without a preset URL.
fn default_api_base_for_provider(provider_key: &str) -> Option<String> {
    match provider_key {
        "openai" => Some("https://api.openai.com/v1".to_string()),
        "deepseek" => Some("https://api.deepseek.com/v1".to_string()),
        "gemini" | "google" => {
            Some("https://generativelanguage.googleapis.com/v1beta/openai".to_string())
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Provider definitions
// ---------------------------------------------------------------------------

/// Provider configuration with API base and display info.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub key: String,
    pub label: String,
    pub api_base: Option<String>,
    pub requires_api_base: bool,
    pub allow_custom_model: bool,
    pub env_key: Option<String>,
}

impl ProviderInfo {
    fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            api_base: None,
            requires_api_base: false,
            allow_custom_model: true,
            env_key: None,
        }
    }

    fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = Some(base.into());
        self.requires_api_base = false;
        self
    }

    fn with_custom_api_base(mut self) -> Self {
        self.requires_api_base = true;
        self
    }

    fn with_env_key(mut self, key: impl Into<String>) -> Self {
        self.env_key = Some(key.into());
        self
    }
}

/// Get all available providers for the wizard (matches Python's _PROVIDER_PRIORITY order).
pub fn get_all_providers() -> Vec<ProviderInfo> {
    vec![
        // 1. OpenRouter
        ProviderInfo::new("openrouter", "OpenRouter")
            .with_api_base("https://openrouter.ai/api/v1")
            .with_env_key("OPENROUTER_API_KEY"),
        // 2. OpenAI
        ProviderInfo::new("openai", "OpenAI").with_env_key("OPENAI_API_KEY"),
        // 3. Anthropic
        ProviderInfo::new("anthropic", "Anthropic")
            .with_custom_api_base()
            .with_env_key("ANTHROPIC_API_KEY"),
        // 4. DeepSeek
        ProviderInfo::new("deepseek", "DeepSeek").with_env_key("DEEPSEEK_API_KEY"),
        // 5. Gemini
        ProviderInfo::new("gemini", "Gemini").with_env_key("GOOGLE_API_KEY"),
        // 6. Google
        ProviderInfo::new("google", "Google").with_env_key("GOOGLE_API_KEY"),
        // 7. xAI
        ProviderInfo::new("xai", "xAI (Grok)")
            .with_api_base("https://api.x.ai/v1")
            .with_env_key("XAI_API_KEY"),
        // 8. MiniMax (multi-endpoint)
        ProviderInfo::new("minimax", "MiniMax").with_env_key("MINIMAX_API_KEY"),
        // 9. Moonshot AI (multi-endpoint)
        ProviderInfo::new("moonshot", "Moonshot AI").with_env_key("MOONSHOT_API_KEY"),
        // 10. Z.AI (multi-endpoint, requires_api_base)
        ProviderInfo::new("zai", "Z.AI")
            .with_custom_api_base()
            .with_env_key("ZAI_API_KEY"),
        // 11. Baidu Qianfan
        ProviderInfo::new("qianfan", "Baidu Qianfan")
            .with_api_base("https://qianfan.baidubce.com/v2")
            .with_env_key("QIANFAN_API_KEY"),
        // 12. Mistral AI
        ProviderInfo::new("mistral", "Mistral AI")
            .with_api_base("https://api.mistral.ai/v1")
            .with_env_key("MISTRAL_API_KEY"),
        // 13. Together AI
        ProviderInfo::new("together", "Together AI")
            .with_api_base("https://api.together.xyz/v1")
            .with_env_key("TOGETHER_API_KEY"),
        // 14. HuggingFace
        ProviderInfo::new("huggingface", "HuggingFace")
            .with_api_base("https://api-inference.huggingface.co/v1")
            .with_env_key("HUGGINGFACE_API_KEY"),
        // 15. Qwen (Alibaba)
        ProviderInfo::new("qwen", "Qwen (Alibaba)")
            .with_api_base("https://dashscope.aliyuncs.com/compatible-mode/v1")
            .with_env_key("DASHSCOPE_API_KEY"),
        // 16. Kilo Gateway
        ProviderInfo::new("kilocode", "Kilo Gateway")
            .with_api_base("https://api.kilocode.ai/v1")
            .with_env_key("KILOCODE_API_KEY"),
        // 17. Ollama (local)
        ProviderInfo::new("ollama", "Ollama")
            .with_api_base("http://127.0.0.1:11434/v1")
            .with_env_key(""),
        // 18. vLLM (local)
        ProviderInfo::new("vllm", "vLLM")
            .with_api_base("http://127.0.0.1:8000/v1")
            .with_env_key(""),
        // 19. Vercel AI Gateway
        ProviderInfo::new("ai_gateway", "Vercel AI Gateway")
            .with_api_base("https://gateway.vercel.ai/api/v1")
            .with_env_key("AI_GATEWAY_API_KEY"),
        // 20. Azure (requires custom api_base)
        ProviderInfo::new("azure", "Azure")
            .with_custom_api_base()
            .with_env_key("AZURE_API_KEY"),
        // 21. Bedrock (requires custom api_base)
        ProviderInfo::new("bedrock", "Bedrock").with_custom_api_base(),
        // 22. Custom (OpenAI-compatible)
        ProviderInfo::new("custom", "Custom").with_custom_api_base(),
    ]
}

// ---------------------------------------------------------------------------
// Model lists (static for common providers)
// ---------------------------------------------------------------------------

/// Get predefined models for a provider (matches Python's constants).
pub fn get_provider_models(provider_key: &str) -> Vec<String> {
    match provider_key {
        "openai" => vec![
            "gpt-4o".into(),
            "gpt-4o-mini".into(),
            "gpt-4-turbo".into(),
            "gpt-3.5-turbo".into(),
        ],
        "anthropic" => vec![
            "claude-3-5-sonnet-20241022".into(),
            "claude-3-5-haiku-20241022".into(),
            "claude-3-opus-20240229".into(),
            "claude-3-sonnet-20240229".into(),
        ],
        "gemini" | "google" => vec![
            "gemini-2.5-flash-preview".into(),
            "gemini-2.5-flash".into(),
            "gemini-2.0-flash-exp".into(),
            "gemini-1.5-pro".into(),
        ],
        "deepseek" => vec!["deepseek-chat".into(), "deepseek-coder".into()],
        "xai" => vec!["grok-4".into()],
        "openai-codex" => vec!["gpt-5.4".into()],
        "ollama" => vec![
            "llama3.2".into(),
            "llama3.1".into(),
            "qwen2.5".into(),
            "deepseek-r1".into(),
            "mistral".into(),
            "codellama".into(),
        ],
        "minimax" => vec![
            "MiniMax-M2.5".into(),
            "MiniMax-M2.5-highspeed".into(),
            "MiniMax-M2.5-Lightning".into(),
        ],
        "moonshot" => vec![
            "kimi-k2.5".into(),
            "kimi-k2-turbo-preview".into(),
            "k2p5".into(),
        ],
        "zai" => vec![
            "glm-5".into(),
            "glm-4.7".into(),
            "glm-4.7-flash".into(),
            "glm-4.7-flashx".into(),
        ],
        "qianfan" => vec![
            "deepseek-v3.2".into(),
            "ernie-5.0-thinking-preview".into(),
            "ernie-4.0-8k".into(),
            "ernie-4.0-turbo-8k".into(),
            "ernie-3.5-8k".into(),
        ],
        "mistral" => vec![
            "mistral-large-latest".into(),
            "mistral-large-2411".into(),
            "pixtral-12b-2409".into(),
            "mistral-nemo".into(),
            "open-mistral-7b".into(),
            "open-mixtral-8x7b".into(),
            "open-mixtral-8x22b".into(),
        ],
        "together" => vec![
            "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo".into(),
            "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo".into(),
            "Qwen/Qwen2.5-72B-Instruct-Turbo".into(),
            "mistralai/Mixtral-8x22B-Instruct-v0.1".into(),
            "deepseek-ai/DeepSeek-V3".into(),
            "google/gemma-2-27b-it".into(),
        ],
        "huggingface" => vec![
            "meta-llama/Llama-3.1-70B-Instruct".into(),
            "meta-llama/Llama-3.1-8B-Instruct".into(),
            "Qwen/Qwen2.5-72B-Instruct".into(),
            "mistralai/Mistral-7B-Instruct-v0.3".into(),
            "bigcode/starcoder2-15b".into(),
        ],
        "qwen" => vec![
            "qwen-max".into(),
            "qwen-plus".into(),
            "qwen-turbo".into(),
            "qwen-long".into(),
            "qwen-vl-max".into(),
            "qwen-vl-plus".into(),
        ],
        "kilocode" => vec![
            "openai/gpt-4o".into(),
            "openai/gpt-4o-mini".into(),
            "openai/gpt-4-turbo".into(),
            "anthropic/claude-3-5-sonnet-20241022".into(),
            "anthropic/claude-3-5-haiku-20241022".into(),
            "google/gemini-2.0-flash-exp".into(),
            "meta-llama/llama-3.1-405b-instruct".into(),
        ],
        "vllm" => vec![
            "meta-llama/Llama-3.2-3B-Instruct".into(),
            "meta-llama/Llama-3.1-8B-Instruct".into(),
            "Qwen/Qwen2.5-7B-Instruct".into(),
            "mistralai/Mistral-7B-Instruct-v0.3".into(),
        ],
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Wizard state machine
// ---------------------------------------------------------------------------

/// Wizard execution state.
#[derive(Debug, Clone, PartialEq)]
pub enum WizardState {
    ProviderSelection,
    ApiKeyInput,
    ModelSelection,
    Verification,
    Complete,
}

/// Wizard configuration and state.
pub struct SetupWizard {
    config_dir: PathBuf,
    state: WizardState,
    selected_provider: Option<ProviderInfo>,
    api_base: Option<String>,
    api_key: Option<String>,
    selected_model: Option<String>,
    is_free_key: bool,
}

/// Mask a secret string, showing only first 4 and last 4 characters.
fn mask_secret(s: &str) -> String {
    if s.len() <= 8 {
        return "*".repeat(s.len());
    }
    format!("{}...{}", &s[..4], &s[s.len() - 4..])
}

impl SetupWizard {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            config_dir,
            state: WizardState::ProviderSelection,
            selected_provider: None,
            api_base: None,
            api_key: None,
            selected_model: None,
            is_free_key: false,
        }
    }

    /// Get the config directory path.
    pub fn config_dir(&self) -> &PathBuf {
        &self.config_dir
    }

    fn set_normalized_model(&mut self, raw: &str) {
        let provider_key = self
            .selected_provider
            .as_ref()
            .map(|p| p.key.as_str())
            .unwrap_or("custom");
        let normalized = normalize_model_for_provider(provider_key, raw);
        if normalized != raw.trim() {
            let mut args = std::collections::HashMap::new();
            args.insert("model".to_string(), normalized.clone());
            println!(
                "  \x1b[2m{}\x1b[0m",
                t_with_args("cli.setup.model_custom_saved_as", &args)
            );
        }
        self.selected_model = Some(normalized);
    }

    /// Prompt the user to choose setup entry mode.
    fn select_entry_mode(&self) -> Result<String, AishError> {
        let mut options = vec![
            DialogOption::new("manual", t("cli.setup.action_manual_setup")),
            DialogOption::new("exit", t("cli.setup.action_exit")),
        ];

        // Prepend free_key option when the binary is available.
        if free_key::has_free_key_module() {
            options.insert(
                0,
                DialogOption::new("free_key", t("cli.setup.action_use_free_key")),
            );
        }

        let result = show_selection(
            &t("cli.setup.entry_title"),
            &t("cli.setup.entry_header"),
            &options,
        )?;

        match result {
            DialogResult::Selected(key) => Ok(key),
            DialogResult::Cancelled => Err(AishError::Cancelled),
            _ => Ok("manual".to_string()),
        }
    }

    /// Run the wizard interactively.
    pub fn run(&mut self) -> Result<ConfigModel, AishError> {
        let entry_mode = self.select_entry_mode()?;

        if entry_mode == "exit" {
            return Err(AishError::Cancelled);
        }

        // Free key flow: register, then jump straight to verification.
        if entry_mode == "free_key" {
            return self.run_free_key_flow();
        }

        // Manual setup flow.
        while self.state != WizardState::Complete {
            self.step()?;
        }
        self.build_config()
    }

    /// Handle the free key registration flow.
    ///
    /// 1. Show privacy notice and get consent.
    /// 2. Detect geo location.
    /// 3. Register via the `aish_freekey_bin` binary.
    /// 4. On success, verify connectivity and save.
    /// 5. On failure, offer retry / fallback to manual.
    fn run_free_key_flow(&mut self) -> Result<ConfigModel, AishError> {
        loop {
            // Show free key header.
            println!("\n{}", t("cli.setup.step_free_key"));
            println!("  {}", t("cli.setup.free_key_header"));

            // Privacy notice.
            println!("  {}", t("cli.setup.free_key_privacy_title"));
            println!("  {}", t("cli.setup.free_key_privacy_notice"));

            let consent_options = vec![
                DialogOption::new("agree", t("cli.setup.action_agree")),
                DialogOption::new("disagree", t("cli.setup.action_disagree")),
            ];
            let consent =
                show_selection(&t("cli.setup.free_key_privacy_title"), "", &consent_options)?;

            match consent {
                DialogResult::Selected(key) if key == "agree" => {}
                _ => {
                    // Disagreed or cancelled → fallback to manual.
                    break self.run_manual_flow();
                }
            }

            // Detect geo location.
            println!("  {}", t("cli.setup.free_key_detecting_location"));
            let location = free_key::detect_geo_location();
            let location_display = if location == "cn" {
                t("cli.setup.free_key_location_cn")
            } else {
                t("cli.setup.free_key_location_overseas")
            };
            println!(
                "  {}",
                t("cli.setup.free_key_location_detected").replace("{location}", &location_display)
            );

            // Register.
            println!("  {}", t("cli.setup.free_key_registering"));
            match free_key::register_free_key() {
                Ok(result) if result.success => {
                    println!("  {}", t("cli.setup.free_key_success"));
                    if result.already_registered {
                        println!("  {}", t("cli.setup.free_key_already_registered"));
                    }

                    // Populate config from registration result.
                    self.api_key = Some(result.api_key);
                    self.api_base = if result.api_base.is_empty() {
                        None
                    } else {
                        Some(result.api_base)
                    };
                    self.selected_provider = Some(ProviderInfo::new(
                        "free_key",
                        t("cli.setup.free_key_provider_label"),
                    ));
                    if result.model.is_empty() {
                        self.selected_model = None;
                    } else {
                        self.set_normalized_model(&result.model);
                    }
                    self.is_free_key = true;

                    // If all fields are present, verify and save.
                    if self.api_key.is_some()
                        && self.api_base.is_some()
                        && self.selected_model.is_some()
                    {
                        self.state = WizardState::Verification;
                        while self.state != WizardState::Complete {
                            self.step()?;
                        }
                        return self.build_config();
                    }

                    // Missing fields → fall through to manual for the gaps.
                    break self.run_manual_flow();
                }
                Ok(result) => {
                    // Registration failed.
                    let default_reason = t("cli.setup.verify_failed_unknown");
                    let reason = result.error_message.as_deref().unwrap_or(&default_reason);
                    println!(
                        "  {}",
                        t("cli.setup.free_key_failed_with_reason").replace("{reason}", reason)
                    );

                    if !self.offer_free_key_retry()? {
                        break self.run_manual_flow();
                    }
                    // retry → continue loop
                }
                Err(e) => {
                    println!(
                        "  {}",
                        t("cli.setup.free_key_failed_with_reason")
                            .replace("{reason}", &e.to_string())
                    );

                    if !self.offer_free_key_retry()? {
                        break self.run_manual_flow();
                    }
                }
            }
        }
    }

    /// Offer retry / fallback after free key registration failure.
    ///
    /// Returns `true` if the user wants to retry, `false` to fallback to manual.
    fn offer_free_key_retry(&self) -> Result<bool, AishError> {
        let options = vec![
            DialogOption::new("retry", t("cli.setup.action_retry_free_key")),
            DialogOption::new("manual", t("cli.setup.action_fallback_manual")),
            DialogOption::new("exit", t("cli.setup.action_exit")),
        ];

        let result = show_selection(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
        )?;

        match result {
            DialogResult::Selected(key) => match key.as_str() {
                "retry" => Ok(true),
                "manual" => Ok(false),
                _ => Err(AishError::Cancelled),
            },
            _ => Err(AishError::Cancelled),
        }
    }

    /// Run the manual setup flow (normal wizard steps).
    fn run_manual_flow(&mut self) -> Result<ConfigModel, AishError> {
        self.state = WizardState::ProviderSelection;
        while self.state != WizardState::Complete {
            self.step()?;
        }
        self.build_config()
    }

    /// Execute a single wizard step based on current state.
    ///
    /// Each step function may update `self.state` internally (e.g. to go back
    /// to a previous step on retry).  We only advance to the default next
    /// state when the step function did NOT change the state itself.
    fn step(&mut self) -> Result<(), AishError> {
        match self.state {
            WizardState::ProviderSelection => {
                self.select_provider()?;
                // select_provider never changes state internally
                self.state = WizardState::ApiKeyInput;
            }
            WizardState::ApiKeyInput => {
                self.prompt_api_key()?;
                // prompt_api_key never changes state internally
                self.state = WizardState::ModelSelection;
            }
            WizardState::ModelSelection => {
                let prev = self.state.clone();
                self.select_model()?;
                // select_model may set ProviderSelection on "back"
                if self.state == prev {
                    self.state = WizardState::Verification;
                }
            }
            WizardState::Verification => {
                self.verify_and_save()?;
                // verify_and_save manages its own state transitions
                // (Complete on success, or back to earlier steps on retry)
            }
            WizardState::Complete => {}
        }
        Ok(())
    }

    /// Step 1: Provider selection.
    fn select_provider(&mut self) -> Result<(), AishError> {
        let title = t("cli.setup.provider_header");

        let providers = get_all_providers();
        let options: Vec<DialogOption> = providers
            .iter()
            .map(|p| {
                let hint_key = format!("cli.setup.provider_hints.{}", p.key);
                let hint = t(&hint_key);
                let description = if hint != hint_key {
                    hint
                } else if p.requires_api_base {
                    t("cli.setup.provider_custom_note")
                } else if p.api_base.is_some() {
                    t("cli.setup.provider_preset_base")
                } else {
                    String::new()
                };
                let mut opt = DialogOption::new(&p.key, p.label.clone());
                if !description.is_empty() {
                    opt = opt.with_description(description);
                }
                opt
            })
            .collect();

        let result = show_searchable_selection(
            &title,
            &t("cli.setup.provider_filter_prompt"),
            &t("cli.setup.provider_filter_hint"),
            &options,
            None,
            false,
        )?;

        match result {
            DialogResult::Selected(key) => {
                let provider = providers
                    .iter()
                    .find(|p| p.key == key)
                    .ok_or_else(|| AishError::Config(format!("Provider not found: {}", key)))?;
                self.selected_provider = Some(provider.clone());

                // Check for alternative endpoints
                let eps = endpoints::get_provider_endpoints(&key);
                if !eps.is_empty() {
                    self.api_base = Some(self.select_endpoint(&eps)?);
                } else if provider.requires_api_base {
                    self.api_base = Some(self.prompt_api_base(&key)?);
                } else {
                    self.api_base = provider
                        .api_base
                        .clone()
                        .or_else(|| default_api_base_for_provider(&key));
                }
            }
            DialogResult::Cancelled => {
                return Err(AishError::Cancelled);
            }
            _ => {
                return Err(AishError::Cancelled);
            }
        }

        Ok(())
    }

    /// Prompt for custom API base URL.
    fn prompt_api_base(&self, _provider_key: &str) -> Result<String, AishError> {
        prompts::prompt_api_base_url()
    }

    /// Let the user select an endpoint from a list of alternatives.
    fn select_endpoint(&self, eps: &[endpoints::EndpointInfo]) -> Result<String, AishError> {
        let provider = self
            .selected_provider
            .as_ref()
            .ok_or(AishError::Cancelled)?;
        let mut args = std::collections::HashMap::new();
        args.insert("provider".to_string(), provider.label.clone());
        let title = t_with_args("cli.setup.provider_endpoint_header", &args);

        let options: Vec<DialogOption> = eps
            .iter()
            .map(|e| {
                DialogOption::new(&e.api_base, e.label.clone())
                    .with_description(format!("{} — {}", e.hint, e.api_base))
            })
            .collect();

        let result = show_searchable_selection(
            &title,
            &t("cli.setup.provider_filter_prompt"),
            &t("cli.setup.provider_filter_hint"),
            &options,
            None,
            false,
        )?;

        match result {
            DialogResult::Selected(key) => Ok(key),
            DialogResult::Cancelled => Err(AishError::Cancelled),
            _ => Err(AishError::Cancelled),
        }
    }

    /// Step 2: API key input.
    fn prompt_api_key(&mut self) -> Result<(), AishError> {
        let provider = self
            .selected_provider
            .as_ref()
            .ok_or(AishError::Cancelled)?;

        // Check environment variable
        let env_value = provider.env_key.as_ref().and_then(|k| {
            if k.is_empty() {
                None
            } else {
                std::env::var(k).ok()
            }
        });

        if let Some(ref value) = env_value {
            let masked = mask_secret(value);
            println!(
                "  {}",
                t("cli.setup.api_key_env_found")
                    .replace("{env_key}", provider.env_key.as_deref().unwrap_or(""))
                    .replace("{masked}", &masked)
            );
            println!("  {}", t("cli.setup.api_key_env_hint"));
        }

        let api_key = prompts::prompt_api_key_value(env_value.as_deref())?;
        self.api_key = Some(api_key);
        Ok(())
    }

    /// Step 3: Model selection (with dynamic fetch from provider API).
    fn select_model(&mut self) -> Result<(), AishError> {
        let provider = self
            .selected_provider
            .as_ref()
            .ok_or(AishError::Cancelled)?;

        let mut args = std::collections::HashMap::new();
        args.insert("provider".to_string(), provider.label.clone());
        let title = t_with_args("cli.setup.model_header", &args);

        let api_base = self.api_base.as_deref().unwrap_or("");

        // Try dynamic model fetch first; fall back to static list.
        let dynamic_models = if let Some(api_key) = &self.api_key {
            model_fetch::get_models_for_provider(&provider.key, api_base, Some(api_key.as_str()))
        } else if provider.key == "ollama" || provider.key == "vllm" {
            model_fetch::get_models_for_provider(&provider.key, api_base, None)
        } else {
            vec![]
        };

        let models = if dynamic_models.is_empty() {
            get_provider_models(&provider.key)
        } else {
            dynamic_models
        };

        if !models.is_empty() {
            let options: Vec<DialogOption> = models
                .iter()
                .map(|m| DialogOption::new(m, m.clone()))
                .collect();

            let result = show_searchable_selection(
                &title,
                &t("cli.setup.model_filter_prompt"),
                &t("cli.setup.model_filter_hint"),
                &options,
                Some(&t("cli.setup.model_custom_option")),
                true,
            )?;

            match result {
                DialogResult::Selected(model) if model == CUSTOM_DIALOG_VALUE => {
                    return self.prompt_custom_model();
                }
                DialogResult::Selected(model) => {
                    self.set_normalized_model(&model);
                    return Ok(());
                }
                DialogResult::Cancelled => {
                    return Err(AishError::Cancelled);
                }
                _ => {
                    return Err(AishError::Cancelled);
                }
            }
        }

        self.prompt_custom_model()
    }

    fn prompt_custom_model(&mut self) -> Result<(), AishError> {
        let model = prompts::prompt_custom_model_name()?;
        if model.eq_ignore_ascii_case("back") || model.eq_ignore_ascii_case("b") {
            self.state = WizardState::ProviderSelection;
            return Ok(());
        }
        self.set_normalized_model(&model);
        Ok(())
    }

    /// Step 4: Verify and save configuration.
    fn verify_and_save(&mut self) -> Result<(), AishError> {
        let _provider = self
            .selected_provider
            .as_ref()
            .ok_or(AishError::Cancelled)?;
        let model = self
            .selected_model
            .as_ref()
            .ok_or(AishError::Cancelled)?
            .clone();
        let api_base = self
            .api_base
            .as_ref()
            .ok_or_else(|| AishError::Config("No API base configured".to_string()))?
            .clone();
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| AishError::Config("No API key configured".to_string()))?
            .clone();
        let api_model = resolve_model_for_api(&model, &api_base);

        let connectivity_msg = t("cli.setup.verify_connectivity_in_progress");
        clack_log::step(&connectivity_msg);
        let conn = clack_log::with_spinner("...", || {
            verification::check_connectivity(
                &api_base,
                &api_key,
                &api_model,
                verification::DEFAULT_CONNECTIVITY_TIMEOUT_S,
            )
        });

        if !conn.ok {
            let unknown = t("cli.setup.verify_failed_unknown");
            let err_msg = conn.error.as_deref().unwrap_or(&unknown);
            let reason =
                t("cli.setup.verify_simple_failed_with_reason").replace("{reason}", err_msg);
            clack_log::error(&reason);
            return self.handle_connectivity_failure();
        }

        let latency = conn.latency_ms.unwrap_or(0);
        clack_log::success(&t("cli.setup.connectivity_ok").replace("{}", &latency.to_string()));

        let tool_msg = t("cli.setup.verify_tool_in_progress");
        clack_log::step(&tool_msg);
        let tool_result = clack_log::with_spinner("...", || {
            verification::check_tool_support(
                &api_base,
                &api_key,
                &api_model,
                verification::DEFAULT_TOOL_SUPPORT_TIMEOUT_S,
            )
        });

        if tool_result.supports {
            clack_log::success(&t("cli.setup.verify_simple_success"));
            self.save_config(SaveNotice::Normal)?;
            self.state = WizardState::Complete;
            return Ok(());
        }

        // Tool support not detected - check if it's a definitive failure or inconclusive
        let not_detected = t("cli.setup.verify_tool_not_detected");
        let reason = tool_result.error.as_deref().unwrap_or(&not_detected);
        let full_reason =
            t("cli.setup.verify_simple_failed_with_reason").replace("{reason}", reason);
        clack_log::error(&full_reason);

        if tool_result.error.is_none() {
            // Inconclusive result - offer "Continue anyway"
            return self.handle_inconclusive_tool_support();
        }

        // Definitive failure
        self.handle_tool_support_failure()
    }

    /// Handle a connectivity failure (Layer 1): offer specific retry options.
    fn handle_connectivity_failure(&mut self) -> Result<(), AishError> {
        let options = vec![
            DialogOption::new("retry_api_base", t("cli.setup.action_retry_api_base")),
            DialogOption::new("retry_model", t("cli.setup.action_retry_model")),
            DialogOption::new("retry_api_key", t("cli.setup.action_retry_api_key")),
            DialogOption::new("change_provider", t("cli.setup.action_change_provider")),
            DialogOption::new("exit", t("cli.setup.action_exit")),
        ];

        let result = show_selection(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
        )?;

        match result {
            DialogResult::Selected(key) => match key.as_str() {
                "retry_api_base" => {
                    let provider_key = self
                        .selected_provider
                        .as_ref()
                        .map(|p| p.key.as_str())
                        .unwrap_or("custom");
                    match self.prompt_api_base(provider_key) {
                        Ok(new_base) => {
                            self.api_base = Some(new_base);
                            self.verify_and_save()
                        }
                        Err(e) => Err(e),
                    }
                }
                "retry_model" => {
                    self.state = WizardState::ModelSelection;
                    Ok(())
                }
                "retry_api_key" => {
                    self.state = WizardState::ApiKeyInput;
                    Ok(())
                }
                "change_provider" => {
                    self.state = WizardState::ProviderSelection;
                    Ok(())
                }
                _ => Err(AishError::Cancelled),
            },
            DialogResult::Cancelled => Err(AishError::Cancelled),
            _ => Err(AishError::Cancelled),
        }
    }

    /// Handle a definitive tool-support failure (Layer 2).
    fn handle_tool_support_failure(&mut self) -> Result<(), AishError> {
        let options = vec![
            DialogOption::new("retry_model", t("cli.setup.action_retry_model")),
            DialogOption::new("change_provider", t("cli.setup.action_change_provider")),
            DialogOption::new("exit", t("cli.setup.action_exit")),
        ];

        let result = show_selection(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
        )?;

        match result {
            DialogResult::Selected(key) => match key.as_str() {
                "retry_model" => {
                    self.state = WizardState::ModelSelection;
                    Ok(())
                }
                "change_provider" => {
                    self.state = WizardState::ProviderSelection;
                    Ok(())
                }
                _ => Err(AishError::Cancelled),
            },
            DialogResult::Cancelled => Err(AishError::Cancelled),
            _ => Err(AishError::Cancelled),
        }
    }

    /// Handle an inconclusive tool-support result (Layer 2 - could not determine).
    fn handle_inconclusive_tool_support(&mut self) -> Result<(), AishError> {
        let options = vec![
            DialogOption::new("retry_model", t("cli.setup.action_retry_model")),
            DialogOption::new("change_provider", t("cli.setup.action_change_provider")),
            DialogOption::new("continue", t("cli.setup.action_continue")),
            DialogOption::new("exit", t("cli.setup.action_exit")),
        ];

        let result = show_selection(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
        )?;

        match result {
            DialogResult::Selected(key) => match key.as_str() {
                "retry_model" => {
                    self.state = WizardState::ModelSelection;
                    Ok(())
                }
                "change_provider" => {
                    self.state = WizardState::ProviderSelection;
                    Ok(())
                }
                "continue" => {
                    self.save_config(SaveNotice::WithWarning)?;
                    self.state = WizardState::Complete;
                    Ok(())
                }
                _ => Err(AishError::Cancelled),
            },
            DialogResult::Cancelled => Err(AishError::Cancelled),
            _ => Err(AishError::Cancelled),
        }
    }

    /// Load existing config from disk, or defaults when missing.
    fn load_existing_config(&self) -> ConfigModel {
        let config_path = self.config_dir.join("config.yaml");
        aish_config::ConfigLoader::load(Some(&config_path)).unwrap_or_default()
    }

    /// Save configuration to disk.
    fn save_config(&self, notice: SaveNotice) -> Result<(), AishError> {
        let _provider = self
            .selected_provider
            .as_ref()
            .ok_or_else(|| AishError::Config("No provider selected".to_string()))?;
        let model = self
            .selected_model
            .as_ref()
            .ok_or_else(|| AishError::Config("No model selected".to_string()))?;
        let api_base = self
            .api_base
            .as_ref()
            .ok_or_else(|| AishError::Config("No API base configured".to_string()))?;
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| AishError::Config("No API key configured".to_string()))?;

        let mut config = self.load_existing_config();
        config.model = model.clone();
        config.api_base = api_base.clone();
        config.api_key = api_key.clone();
        config.is_free_key = self.is_free_key;

        // Save to config file.
        let config_path = self.config_dir.join("config.yaml");
        let yaml_content = serde_yaml::to_string(&config)
            .map_err(|e| AishError::Config(format!("Failed to serialize config: {}", e)))?;

        std::fs::write(&config_path, yaml_content)
            .map_err(|e| AishError::Config(format!("Failed to write config: {}", e)))?;

        let path = format_config_path(&config_path);
        let mut args = std::collections::HashMap::new();
        args.insert("path".to_string(), path);
        let message_key = match notice {
            SaveNotice::Normal => "cli.setup.saved_to",
            SaveNotice::WithWarning => "cli.setup.saved_with_warning_to",
        };
        clack_log::success(&t_with_args(message_key, &args));

        Ok(())
    }

    /// Build the final ConfigModel.
    fn build_config(&self) -> Result<ConfigModel, AishError> {
        let mut config = self.load_existing_config();
        config.model = self
            .selected_model
            .as_ref()
            .ok_or_else(|| AishError::Config("No model selected".to_string()))?
            .clone();
        config.api_base = self
            .api_base
            .as_ref()
            .ok_or_else(|| AishError::Config("No API base configured".to_string()))?
            .clone();
        config.api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| AishError::Config("No API key configured".to_string()))?
            .clone();
        config.is_free_key = self.is_free_key;
        Ok(config)
    }
}

/// Print the post-setup outro (for CLI `aish setup`).
pub fn print_setup_complete_hint() {
    clack_log::outro(&t("cli.setup.setup_complete_hint"));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_info_creation() {
        let provider = ProviderInfo::new("test", "Test Provider")
            .with_api_base("https://api.example.com/v1")
            .with_env_key("TEST_API_KEY");

        assert_eq!(provider.key, "test");
        assert_eq!(provider.label, "Test Provider");
        assert_eq!(
            provider.api_base,
            Some("https://api.example.com/v1".to_string())
        );
        assert!(!provider.requires_api_base);
        assert_eq!(provider.env_key, Some("TEST_API_KEY".to_string()));
    }

    #[test]
    fn test_get_all_providers() {
        let providers = get_all_providers();
        assert!(!providers.is_empty());
        // Check providers from the updated Python-aligned list
        assert!(providers.iter().any(|p| p.key == "openrouter"));
        assert!(providers.iter().any(|p| p.key == "openai"));
        assert!(providers.iter().any(|p| p.key == "anthropic"));
        assert!(providers.iter().any(|p| p.key == "qianfan"));
        assert!(providers.iter().any(|p| p.key == "mistral"));
        assert!(providers.iter().any(|p| p.key == "ollama"));
        assert!(providers.iter().any(|p| p.key == "custom"));
        // Verify Python priority order: openrouter is first
        assert_eq!(providers.first().unwrap().key, "openrouter");
    }

    #[test]
    fn test_get_provider_models() {
        let openai_models = get_provider_models("openai");
        assert!(!openai_models.is_empty());
        assert!(openai_models.contains(&"gpt-4o".to_string()));

        let anthropic_models = get_provider_models("anthropic");
        assert!(!anthropic_models.is_empty());
        assert!(anthropic_models.iter().any(|m| m.starts_with("claude-")));

        // Verify updated xai model
        let xai_models = get_provider_models("xai");
        assert!(xai_models.contains(&"grok-4".to_string()));

        // Verify new providers have models
        let qianfan_models = get_provider_models("qianfan");
        assert!(!qianfan_models.is_empty());

        let mistral_models = get_provider_models("mistral");
        assert!(!mistral_models.is_empty());

        let empty_models = get_provider_models("nonexistent");
        assert!(empty_models.is_empty());
    }

    #[test]
    fn test_wizard_state_transitions() {
        let wizard = SetupWizard::new(PathBuf::from("/tmp/test"));
        assert_eq!(wizard.state, WizardState::ProviderSelection);
        assert!(wizard.selected_provider.is_none());
        assert!(wizard.selected_model.is_none());
    }
}
