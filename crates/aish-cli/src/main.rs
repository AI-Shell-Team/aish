// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments
)]

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod models_auth;
mod uninstall;
mod update;

/// AI Shell - A shell with built-in LLM capabilities
#[derive(Parser)]
#[command(
    name = "aish",
    version,
    about = "AI Shell - A shell with built-in LLM capabilities"
)]
struct Cli {
    /// LLM model to use
    #[arg(long, short = 'm')]
    model: Option<String>,

    /// API key for the LLM provider
    #[arg(long)]
    api_key: Option<String>,

    /// API base URL for the LLM provider
    #[arg(long)]
    api_base: Option<String>,

    /// Path to configuration file
    #[arg(long)]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the AI Shell (default)
    Run,

    /// Show information about AI Shell
    Info,

    /// Run interactive setup
    Setup,

    /// Show model usage status
    ModelsUsage,

    /// Check tool calling support for a model
    CheckToolSupport {
        /// Model name to check
        #[arg(long)]
        model: Option<String>,

        /// API base URL
        #[arg(long)]
        api_base: Option<String>,

        /// API key
        #[arg(long)]
        api_key: Option<String>,
    },

    /// Check Langfuse observability connectivity
    CheckLangfuse {
        /// Langfuse public key
        #[arg(long)]
        public_key: Option<String>,

        /// Langfuse secret key
        #[arg(long)]
        secret_key: Option<String>,

        /// Langfuse host URL
        #[arg(long)]
        host: Option<String>,
    },

    /// Update aish to the latest version
    Update {
        #[arg(long)]
        check_only: bool,
        #[arg(long, short = 'p')]
        pre_release: bool,
    },

    /// Uninstall aish
    Uninstall {
        #[arg(long)]
        purge: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Manage provider authentication
    ModelsAuth {
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, default_value = "")]
        model: String,
        #[arg(long, default_value = "true")]
        set_default: bool,
        #[arg(long, default_value = "browser")]
        auth_flow: models_auth::AuthFlow,
        #[arg(long, default_value = "false")]
        force: bool,
        #[arg(long, default_value = "true")]
        open_browser: bool,
        #[arg(long, default_value_t = 8402)]
        callback_port: u16,
        #[arg(long)]
        config: Option<String>,
    },
}

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Load configuration
    let config_path = cli.config.as_deref().map(std::path::Path::new);
    let mut config = match aish_config::ConfigLoader::load(config_path) {
        Ok(config) => config,
        Err(e) => {
            tracing::debug!("Config load failed (using defaults): {}", e);
            aish_config::ConfigModel::default()
        }
    };

    // Apply CLI arg overrides
    if let Some(model) = cli.model {
        config.model = model;
    }
    if let Some(api_key) = cli.api_key {
        config.api_key = api_key;
    }
    if let Some(api_base) = cli.api_base {
        config.api_base = api_base;
    }

    match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => run_shell(config),
        Commands::Info => show_info(&config),
        Commands::Setup => run_setup(&mut config),
        Commands::ModelsUsage => show_models_usage(&config),
        Commands::CheckToolSupport {
            model,
            api_base,
            api_key,
        } => check_tool_support(&config, model, api_base, api_key),
        Commands::CheckLangfuse {
            public_key,
            secret_key,
            host,
        } => check_langfuse(&config, public_key, secret_key, host),
        Commands::Update {
            check_only,
            pre_release,
        } => {
            update::run_update(check_only, pre_release);
        }
        Commands::Uninstall { purge, yes } => {
            uninstall::run_uninstall(purge, yes);
        }
        Commands::ModelsAuth {
            provider,
            model,
            set_default,
            auth_flow,
            force,
            open_browser,
            callback_port,
            config: auth_config,
        } => {
            let mut cfg = load_config(auth_config.as_deref());
            models_auth::run_models_auth(
                &mut cfg,
                provider.as_deref(),
                &model,
                set_default,
                auth_flow,
                force,
                open_browser,
                callback_port,
            );
        }
    }
}

fn load_config(config_path: Option<&str>) -> aish_config::ConfigModel {
    let path = config_path.map(std::path::Path::new);
    aish_config::ConfigLoader::load(path).unwrap_or_default()
}

fn run_shell(mut config: aish_config::ConfigModel) {
    // Auto-trigger setup wizard on first run if config is incomplete
    if aish_shell::needs_interactive_setup(&config) {
        println!("\x1b[33mConfiguration incomplete — launching setup wizard.\x1b[0m\n");
        run_setup(&mut config);
        // Reload config with the saved values
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Ok(loaded) = aish_config::ConfigLoader::load(Some(&config_path)) {
            config = loaded;
        }
    }

    match aish_shell::AishShell::new(config) {
        Ok(mut shell) => {
            if let Err(e) = shell.run() {
                eprintln!("Shell error: {}", e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to initialize shell: {}", e);
            std::process::exit(1);
        }
    }
}

fn show_info(config: &aish_config::ConfigModel) {
    println!("AI Shell v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!(
        "  Model:     {}",
        if config.model.is_empty() {
            "(not set)"
        } else {
            &config.model
        }
    );
    println!("  API Base:  {}", config.api_base);
    println!("  Config:    ~/.config/aish/config.yaml");
    println!();
    println!(
        "  Platform:  {}-{}",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
}

fn run_setup(config: &mut aish_config::ConfigModel) {
    let config_dir = aish_config::ConfigLoader::default_config_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("aish")
        });

    let mut wizard = aish_shell::wizard::SetupWizard::new(config_dir);
    match wizard.run() {
        Ok(new_config) => {
            *config = new_config;
            println!("\n{}", aish_i18n::t("cli.setup.setup_complete_hint"));
        }
        Err(e) => {
            eprintln!(
                "\n\x1b[33m{}: {}\x1b[0m",
                aish_i18n::t("cli.setup.cancelled"),
                e
            );
            eprintln!("~/.config/aish/config.yaml");
        }
    }
}

fn show_models_usage(config: &aish_config::ConfigModel) {
    let model = &config.model;
    let api_base = &config.api_base;
    let api_key_set = !config.api_key.is_empty();

    let provider = aish_llm::detect_provider(model, api_base);

    println!("\x1b[1mModel Configuration\x1b[0m");
    println!("  Model:      \x1b[36m{}\x1b[0m", model);
    println!("  Provider:   \x1b[36m{}\x1b[0m", provider.display_name);
    println!("  API Base:   \x1b[2m{}\x1b[0m", api_base);
    println!(
        "  API Key:    {}",
        if api_key_set {
            "\x1b[32mset\x1b[0m"
        } else {
            "\x1b[31mnot set\x1b[0m"
        }
    );

    println!();
    println!("\x1b[1mProvider Capabilities\x1b[0m");
    println!(
        "  Streaming:      {}",
        if provider.supports_streaming {
            "\x1b[32myes\x1b[0m"
        } else {
            "\x1b[33mno\x1b[0m"
        }
    );
    println!(
        "  Tool Calling:   {}",
        if provider.supports_tools {
            "\x1b[32myes\x1b[0m"
        } else {
            "\x1b[33mno\x1b[0m"
        }
    );

    if let Some(dashboard) = &provider.dashboard_url {
        println!();
        println!("\x1b[1mDashboard\x1b[0m");
        println!("  \x1b[4m{}\x1b[0m", dashboard);
    }

    println!();
    println!("\x1b[2mConfig file: ~/.config/aish/config.yaml\x1b[0m");
    println!("\x1b[2mOverride:    AISH_MODEL, AISH_API_KEY, AISH_API_BASE\x1b[0m");
}

fn check_tool_support(
    config: &aish_config::ConfigModel,
    model: Option<String>,
    api_base: Option<String>,
    api_key: Option<String>,
) {
    let model = model.unwrap_or_else(|| config.model.clone());
    let api_base = api_base.unwrap_or_else(|| config.api_base.clone());
    let api_key = api_key.unwrap_or_else(|| config.api_key.clone());

    if model.is_empty() {
        eprintln!("Error: No model specified. Use --model or set it in config.");
        std::process::exit(1);
    }
    if api_key.is_empty() {
        eprintln!("Error: No API key specified. Use --api-key or set it in config.");
        std::process::exit(1);
    }

    println!("Checking tool calling support for: {}", model);
    println!("API Base: {}", api_base);

    // Send a simple request with a tool definition to test support
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        let client = aish_llm::LlmClient::new(&api_base, &api_key, &model);
        let messages = vec![aish_llm::ChatMessage::user(
            "Reply with just 'ok'. Do not use any tools.",
        )];
        let tool = aish_llm::ToolSpec {
            r#type: "function".to_string(),
            function: aish_llm::FunctionSpec {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "input": {"type": "string"}
                    }
                }),
            },
        };
        client
            .chat_completion(&messages, Some(&[tool]), false, Some(0.0), Some(10))
            .await
    });

    match result {
        Ok(_) => {
            println!("\x1b[32mTool calling is supported.\x1b[0m");
        }
        Err(e) => {
            println!("\x1b[31mTool calling may not be supported: {}\x1b[0m", e);
        }
    }
}

fn check_langfuse(
    config: &aish_config::ConfigModel,
    public_key: Option<String>,
    secret_key: Option<String>,
    host: Option<String>,
) {
    let public_key = public_key
        .or_else(|| config.langfuse_public_key.clone())
        .or_else(|| std::env::var("LANGFUSE_PUBLIC_KEY").ok());
    let secret_key = secret_key
        .or_else(|| config.langfuse_secret_key.clone())
        .or_else(|| std::env::var("LANGFUSE_SECRET_KEY").ok());
    let host = host
        .or_else(|| config.langfuse_host.clone())
        .or_else(|| std::env::var("LANGFUSE_HOST").ok());

    match (public_key, secret_key) {
        (Some(pk), Some(sk)) => {
            let lf_config =
                aish_llm::LangfuseConfig::from_parts(Some(&pk), Some(&sk), host.as_deref());
            match lf_config {
                Some(cfg) => {
                    let base_url = cfg.base_url.clone();
                    let _client = aish_llm::LangfuseClient::new(cfg);
                    println!("Langfuse configuration found.");
                    println!("  Host: {}", base_url);
                    if pk.len() > 8 {
                        println!("  Public Key: {}...{}", &pk[..4], &pk[pk.len() - 4..]);
                    } else {
                        println!(
                            "  Public Key: {}...{}",
                            &pk[..2.min(pk.len())],
                            &pk[(pk.len() - 2).min(pk.len())..]
                        );
                    }
                    println!("\x1b[32mLangfuse is configured and ready.\x1b[0m");
                }
                None => {
                    eprintln!("Langfuse configuration is incomplete.");
                }
            }
        }
        (None, _) | (_, None) => {
            eprintln!("Langfuse is not configured.");
            eprintln!("Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY environment variables,");
            eprintln!("or add langfuse_public_key and langfuse_secret_key to config.yaml.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_show_models_usage_does_not_panic() {
        let config = aish_config::ConfigModel::default();
        show_models_usage(&config);
    }
}
