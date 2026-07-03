//! Provider authentication flows (OAuth, device-code, etc.).

use std::path::PathBuf;

use aish_config::ConfigModel;
use aish_i18n::{t, t_with_args};
use aish_llm::providers::codex::{
    ensure_codex_auth, login_codex_device_code, resolve_codex_auth_path, CODEX_DEFAULT_BASE_URL,
    CODEX_PROVIDER,
};

#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum AuthFlow {
    Browser,
    DeviceCode,
    CodexCli,
}

struct AuthProviderInfo {
    id: String,
    _display_name: String,
    default_model: String,
}

fn get_auth_capable_providers() -> Vec<AuthProviderInfo> {
    vec![AuthProviderInfo {
        id: CODEX_PROVIDER.to_string(),
        _display_name: "Codex".to_string(),
        default_model: "gpt-5.4".to_string(),
    }]
}

fn is_auth_capable(provider_id: &str) -> bool {
    get_auth_capable_providers()
        .iter()
        .any(|p| p.id == provider_id)
}

pub fn run_models_auth(
    config: &mut ConfigModel,
    provider: Option<&str>,
    model: &str,
    set_default: bool,
    auth_flow: AuthFlow,
    force: bool,
    open_browser: bool,
    _callback_port: u16,
) {
    let provider_id = match provider {
        Some(p) => {
            let normalized = p.to_lowercase().replace('_', "-");
            if !is_auth_capable(&normalized) {
                let supported: Vec<String> = get_auth_capable_providers()
                    .iter()
                    .map(|p| p.id.clone())
                    .collect();
                eprintln!(
                    "\x1b[31mProvider '{}' does not support auth flows.\x1b[0m",
                    normalized
                );
                eprintln!("\x1b[2mSupported: {}\x1b[0m", supported.join(", "));
                std::process::exit(1);
            }
            normalized
        }
        None => {
            eprintln!("\x1b[31m--provider is required.\x1b[0m");
            eprintln!("\x1b[2mExample: aish models auth --provider openai-codex\x1b[0m");
            std::process::exit(1);
        }
    };

    println!("\x1b[1;36m{}\x1b[0m\n", {
        let mut args = std::collections::HashMap::new();
        args.insert("provider".to_string(), provider_id.to_string());
        t_with_args("cli.models_auth_title", &args)
    });

    if !force {
        println!("\x1b[2m{}\x1b[0m", t("cli.checking_existing_auth"));
    }

    let auth_path = config
        .codex_auth_path
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| Some(resolve_codex_auth_path(None)));

    let auth_path_ref = auth_path.as_deref();

    let auth_result = match auth_flow {
        AuthFlow::Browser => ensure_codex_auth(auth_path_ref, open_browser),
        AuthFlow::DeviceCode | AuthFlow::CodexCli => login_codex_device_code(auth_path_ref),
    };

    match auth_result {
        Ok(auth) => {
            if let Some(path) = auth_path {
                config.codex_auth_path = Some(path.display().to_string());
            }
            config.api_key.clear();

            let resolved_model = if model.is_empty() {
                get_auth_capable_providers()
                    .iter()
                    .find(|p| p.id == provider_id)
                    .map(|p| format!("{}/{}", p.id, p.default_model))
                    .unwrap_or_else(|| format!("{}/gpt-5.4", CODEX_PROVIDER))
            } else if model.contains('/') {
                model.to_string()
            } else {
                format!("{}/{}", provider_id, model)
            };

            if set_default {
                config.model = resolved_model;
                config.api_base = CODEX_DEFAULT_BASE_URL.to_string();
            }

            println!("\n\x1b[32m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("provider".to_string(), provider_id.to_string());
                t_with_args("cli.auth_configured", &args)
            });
            if set_default {
                println!("\x1b[32m{}\x1b[0m", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("model".to_string(), config.model.clone());
                    t_with_args("cli.default_model_set_success", &args)
                });
            }
            println!(
                "\x1b[2mAccount: {} | auth: {}\x1b[0m",
                auth.account_id,
                auth.auth_path.display()
            );
        }
        Err(e) => {
            eprintln!("\x1b[31m{}\x1b[0m", e);
            std::process::exit(1);
        }
    }

    let config_path = aish_config::ConfigLoader::default_config_path();
    match aish_config::ConfigLoader::save(config, &config_path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("\x1b[31m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.save_config_failed", &args)
            });
        }
    }
}
