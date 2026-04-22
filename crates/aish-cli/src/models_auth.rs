//! Provider authentication flows (OAuth, device-code, etc.).

use std::io::{self, Write};

use aish_config::ConfigModel;
use aish_i18n::{t, t_with_args};

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
        id: "openai-codex".to_string(),
        _display_name: "OpenAI Codex".to_string(),
        default_model: "codex-mini".to_string(),
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
    _auth_flow: AuthFlow,
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

    println!("\x1b[33m{}\x1b[0m", t("cli.oauth_not_implemented"));
    println!("{}\n", t("cli.oauth_hint"));

    if !open_browser {
        println!("{}\x1b[0m", t("cli.skipping_browser"));
    }

    print!("Auth token: ");
    io::stdout().flush().unwrap();
    let mut token = String::new();
    if io::stdin().read_line(&mut token).is_err() {
        eprintln!("\x1b[31m{}\x1b[0m", t("cli.token_read_failed"));
        return;
    }
    let token = token.trim();

    if token.is_empty() {
        eprintln!("\x1b[31m{}\x1b[0m", t("cli.token_empty"));
        return;
    }

    let resolved_model = if model.is_empty() {
        get_auth_capable_providers()
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.default_model.clone())
            .unwrap_or_else(|| "default".to_string())
    } else {
        model.to_string()
    };

    config.api_key = token.to_string();

    if set_default {
        config.model = format!("{}/{}", provider_id, resolved_model);
    }

    let config_path = aish_config::ConfigLoader::default_config_path();
    match aish_config::ConfigLoader::save(config, &config_path) {
        Ok(()) => {
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
        }
        Err(e) => {
            eprintln!("\x1b[31m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("cli.save_config_failed", &args)
            });
        }
    }
}
