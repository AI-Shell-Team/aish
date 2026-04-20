//! Setup wizard for interactive LLM provider configuration.
//!
//! Guides users through selecting a provider, entering credentials, choosing a model,
//! and verifying connectivity and tool support.

pub mod endpoints;
pub mod free_key;
pub mod model_fetch;
pub mod plan_approval;
pub mod plan_display;
pub mod verification;

use std::path::PathBuf;

use aish_config::ConfigModel;
use aish_core::AishError;
use aish_i18n::t;

use crate::tui::{DialogOption, DialogResult};

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
            .with_env_key("OPENAI_API_KEY"),
        // 2. OpenAI
        ProviderInfo::new("openai", "OpenAI").with_env_key("OPENAI_API_KEY"),
        // 3. Anthropic
        ProviderInfo::new("anthropic", "Anthropic").with_env_key("ANTHROPIC_API_KEY"),
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
        }
    }

    /// Get the config directory path.
    pub fn config_dir(&self) -> &PathBuf {
        &self.config_dir
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

        let result = crate::tui::show_selection_dialog(
            &t("cli.setup.entry_title"),
            &t("cli.setup.entry_header"),
            &options,
            false,
            true,
        );

        match result {
            DialogResult::Selected(key) => Ok(key),
            DialogResult::Cancelled => Err(AishError::Config(t("cli.setup.cancelled"))),
            _ => Ok("manual".to_string()),
        }
    }

    /// Run the wizard interactively.
    pub fn run(&mut self) -> Result<ConfigModel, AishError> {
        let entry_mode = self.select_entry_mode()?;

        if entry_mode == "exit" {
            return Err(AishError::Config(t("cli.setup.cancelled")));
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
            let consent = crate::tui::show_selection_dialog(
                &t("cli.setup.free_key_privacy_title"),
                "",
                &consent_options,
                false,
                true,
            );

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
                    self.selected_model = if result.model.is_empty() {
                        None
                    } else {
                        Some(model_fetch::normalize_model_name(&result.model))
                    };
                    self.selected_provider = Some(ProviderInfo::new(
                        "free_key",
                        t("cli.setup.free_key_provider_label"),
                    ));

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

        let result = crate::tui::show_selection_dialog(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
            false,
            true,
        );

        match result {
            DialogResult::Selected(key) => match key.as_str() {
                "retry" => Ok(true),
                "manual" => Ok(false),
                _ => Err(AishError::Config(t("cli.setup.cancelled"))),
            },
            _ => Err(AishError::Config(t("cli.setup.cancelled"))),
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
        let title = t("cli.setup.step_provider");
        let question = t("cli.setup.provider_header");

        let providers = get_all_providers();
        let options: Vec<DialogOption> = providers
            .iter()
            .map(|p| {
                let note = if p.requires_api_base {
                    t("cli.setup.provider_custom_note")
                } else if p.api_base.is_some() {
                    t("cli.setup.provider_preset_base")
                } else {
                    String::new()
                };
                let label = if note.is_empty() {
                    p.label.clone()
                } else {
                    format!("{}  [{}]", p.label, note)
                };
                DialogOption::new(&p.key, label)
            })
            .collect();

        let result = crate::tui::show_selection_dialog(
            &title, &question, &options, true, // allow custom provider (custom API base)
            true, // allow cancel
        );

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
                    self.api_base = Some(self.select_endpoint(&eps));
                } else if provider.requires_api_base {
                    self.api_base = Some(self.prompt_api_base(&key)?);
                } else {
                    self.api_base = provider.api_base.clone();
                }
            }
            DialogResult::CustomInput(input) => {
                // Custom provider: treat API base as provider key.
                self.api_base = Some(input.clone());
                self.selected_provider = Some(ProviderInfo {
                    key: input.clone(),
                    label: format!("Custom ({})", input),
                    api_base: Some(input),
                    requires_api_base: false,
                    allow_custom_model: true,
                    env_key: None,
                });
            }
            DialogResult::Cancelled => {
                return Err(AishError::Config(t("cli.setup.cancelled")));
            }
        }

        Ok(())
    }

    /// Prompt for custom API base URL.
    fn prompt_api_base(&self, _provider_key: &str) -> Result<String, AishError> {
        println!("\n{}", t("cli.setup.custom_api_base_title"));
        println!("  {}", t("cli.setup.custom_api_base_header"));

        let prompt_label = t("cli.setup.provider_custom_api_base");
        let required_msg = t("cli.setup.provider_custom_api_base_required");
        let invalid_msg = t("cli.setup.provider_custom_api_base_invalid");

        loop {
            let result = inquire::Text::new(&format!("{}:", prompt_label))
                .prompt()
                .map_err(|_| AishError::Config(t("cli.setup.cancelled")))?;

            let trimmed = result.trim().to_string();
            if trimmed.is_empty() {
                println!("  {}", required_msg);
                continue;
            }

            if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                println!("  {}", invalid_msg);
                continue;
            }

            return Ok(trimmed);
        }
    }

    /// Let the user select an endpoint from a list of alternatives.
    fn select_endpoint(&self, eps: &[endpoints::EndpointInfo]) -> String {
        let provider_label = self
            .selected_provider
            .as_ref()
            .map(|p| p.label.as_str())
            .unwrap_or("Provider");
        let title = t("cli.setup.step_provider_endpoint");
        let question =
            t("cli.setup.provider_endpoint_header").replace("{provider}", provider_label);

        let options: Vec<DialogOption> = eps
            .iter()
            .map(|e| DialogOption::new(&e.api_base, format!("{}  [{}]", e.label, e.hint)))
            .collect();

        let result = crate::tui::show_selection_dialog(&title, &question, &options, false, true);

        match result {
            DialogResult::Selected(key) => key,
            _ => eps.first().map(|e| e.api_base.clone()).unwrap_or_default(),
        }
    }

    /// Step 2: API key input.
    fn prompt_api_key(&mut self) -> Result<(), AishError> {
        let provider = self
            .selected_provider
            .as_ref()
            .ok_or_else(|| AishError::Config(t("cli.setup.cancelled")))?;

        // Show step header
        println!("\n{}", t("cli.setup.step_key"));

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
            println!("  {}", t("cli.setup.api_key_hint"));
        }

        // Loop until we get a valid key
        let prompt_label = t("cli.setup.api_key_prompt");
        let required_msg = t("cli.setup.api_key_required");
        loop {
            let result = inquire::Text::new(&format!("{}:", prompt_label))
                .prompt()
                .map_err(|_| AishError::Config(t("cli.setup.cancelled")))?;

            let trimmed = result.trim().to_string();

            if trimmed.is_empty() {
                // If env value exists, use it
                if let Some(ref value) = env_value {
                    self.api_key = Some(value.clone());
                    return Ok(());
                }
                println!("  {}", required_msg);
                continue;
            }

            self.api_key = Some(trimmed);
            return Ok(());
        }
    }

    /// Step 3: Model selection (with dynamic fetch from provider API).
    fn select_model(&mut self) -> Result<(), AishError> {
        let provider = self
            .selected_provider
            .as_ref()
            .ok_or_else(|| AishError::Config(t("cli.setup.cancelled")))?;

        let title = t("cli.setup.step_model");
        let question = t("cli.setup.model_header").replace("{provider}", &provider.label);

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

            let result = crate::tui::show_selection_dialog(
                &title, &question, &options, true, // allow custom model
                true, // allow cancel
            );

            match result {
                DialogResult::Selected(model) => {
                    self.selected_model = Some(model_fetch::normalize_model_name(&model));
                    return Ok(());
                }
                DialogResult::CustomInput(model) => {
                    let model = model.trim().to_string();
                    if model.is_empty() {
                        println!("  {}", t("cli.setup.model_custom_required"));
                        // Fall through to manual input
                    } else {
                        self.selected_model = Some(model_fetch::normalize_model_name(&model));
                        return Ok(());
                    }
                }
                DialogResult::Cancelled => {
                    return Err(AishError::Config(t("cli.setup.cancelled")));
                }
            }
        }

        // Manual model input (no models found or custom input was empty)
        let model_prompt = t("cli.setup.model_prompt");
        let model_required_msg = t("cli.setup.model_custom_required");
        loop {
            let result = inquire::Text::new(&format!("{}: {}", title, model_prompt))
                .prompt()
                .map_err(|_| AishError::Config(t("cli.setup.cancelled")))?;

            let model = result.trim().to_string();
            if model.is_empty() {
                println!("  {}", model_required_msg);
                continue;
            }
            if model.eq_ignore_ascii_case("back") || model.eq_ignore_ascii_case("b") {
                self.state = WizardState::ProviderSelection;
                return Ok(());
            }
            self.selected_model = Some(model_fetch::normalize_model_name(&model));
            return Ok(());
        }
    }

    /// Step 4: Verify and save configuration.
    fn verify_and_save(&mut self) -> Result<(), AishError> {
        let _provider = self
            .selected_provider
            .as_ref()
            .ok_or_else(|| AishError::Config(t("cli.setup.cancelled")))?;
        let model = self
            .selected_model
            .as_ref()
            .ok_or_else(|| AishError::Config(t("cli.setup.cancelled")))?
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

        println!("\n{}", t("cli.setup.verify_header"));

        // --- Layer 1: Connectivity check ---
        println!("  {}", t("cli.setup.verify_connectivity_in_progress"));

        let conn = verification::check_connectivity(
            &api_base,
            &api_key,
            &model,
            verification::DEFAULT_CONNECTIVITY_TIMEOUT_S,
        );

        if !conn.ok {
            let err_msg = conn.error.as_deref().unwrap_or("Unknown error");
            let reason =
                t("cli.setup.verify_simple_failed_with_reason").replace("{reason}", err_msg);
            println!("\n  {}", reason);

            return self.handle_connectivity_failure();
        }

        let latency = conn.latency_ms.unwrap_or(0);
        println!(
            "  {}",
            t("cli.setup.connectivity_ok").replace("{}", &latency.to_string())
        );

        // --- Layer 2: Tool support check ---
        println!("  {}", t("cli.setup.verify_tool_in_progress"));

        let tool_result = verification::check_tool_support(
            &api_base,
            &api_key,
            &model,
            verification::DEFAULT_TOOL_SUPPORT_TIMEOUT_S,
        );

        if tool_result.supports {
            println!("  {}", t("cli.setup.verify_simple_success"));
            self.save_config()?;
            println!("\n  {}", t("cli.setup.saved"));
            self.state = WizardState::Complete;
            return Ok(());
        }

        // Tool support not detected - check if it's a definitive failure or inconclusive
        let reason = tool_result.error.as_deref().unwrap_or("not detected");
        let full_reason =
            t("cli.setup.verify_simple_failed_with_reason").replace("{reason}", reason);
        println!("\n  {}", full_reason);

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

        let result = crate::tui::show_selection_dialog(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
            false,
            true,
        );

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
                _ => Err(AishError::Config(t("cli.setup.cancelled"))),
            },
            DialogResult::Cancelled => Err(AishError::Config(t("cli.setup.cancelled"))),
            _ => Err(AishError::Config(t("cli.setup.cancelled"))),
        }
    }

    /// Handle a definitive tool-support failure (Layer 2).
    fn handle_tool_support_failure(&mut self) -> Result<(), AishError> {
        let options = vec![
            DialogOption::new("retry_model", t("cli.setup.action_retry_model")),
            DialogOption::new("change_provider", t("cli.setup.action_change_provider")),
            DialogOption::new("exit", t("cli.setup.action_exit")),
        ];

        let result = crate::tui::show_selection_dialog(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
            false,
            true,
        );

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
                _ => Err(AishError::Config(t("cli.setup.cancelled"))),
            },
            DialogResult::Cancelled => Err(AishError::Config(t("cli.setup.cancelled"))),
            _ => Err(AishError::Config(t("cli.setup.cancelled"))),
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

        let result = crate::tui::show_selection_dialog(
            &t("cli.setup.verify_title"),
            &t("cli.setup.action_header"),
            &options,
            false,
            true,
        );

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
                    self.save_config()?;
                    println!("\n  {}", t("cli.setup.saved_with_warning"));
                    self.state = WizardState::Complete;
                    Ok(())
                }
                _ => Err(AishError::Config(t("cli.setup.cancelled"))),
            },
            DialogResult::Cancelled => Err(AishError::Config(t("cli.setup.cancelled"))),
            _ => Err(AishError::Config(t("cli.setup.cancelled"))),
        }
    }

    /// Save configuration to disk.
    fn save_config(&self) -> Result<(), AishError> {
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

        // Build ConfigModel: start from defaults, override wizard-collected values.
        let mut config = ConfigModel::default();
        config.model = model.clone();
        config.api_base = api_base.clone();
        config.api_key = api_key.clone();
        config.temperature = 0.7;
        config.max_tokens = Some(4096);

        // Save to config file.
        let config_path = self.config_dir.join("config.yaml");
        let yaml_content = serde_yaml::to_string(&config)
            .map_err(|e| AishError::Config(format!("Failed to serialize config: {}", e)))?;

        std::fs::write(&config_path, yaml_content)
            .map_err(|e| AishError::Config(format!("Failed to write config: {}", e)))?;

        println!("\n  {}", t("cli.setup.saved"));
        println!(
            "  {}",
            t("cli.setup.config_path").replace("{}", &config_path.display().to_string())
        );

        Ok(())
    }

    /// Build the final ConfigModel.
    fn build_config(&self) -> Result<ConfigModel, AishError> {
        let mut config = ConfigModel::default();
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
        config.temperature = 0.7;
        config.max_tokens = Some(4096);
        Ok(config)
    }
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
