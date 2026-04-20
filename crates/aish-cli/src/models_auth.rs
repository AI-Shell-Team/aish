//! Provider authentication flows (OAuth, device-code, etc.).

use std::io::{self, Write};

use aish_config::ConfigModel;

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

    println!("\x1b[1;36mModels Auth: {}\x1b[0m\n", provider_id);

    if !force {
        println!("\x1b[2mChecking for existing auth state...\x1b[0m");
    }

    println!("\x1b[33mNote: OAuth flows are not yet implemented in the Rust version.\x1b[0m");
    println!("Please enter your auth token manually.\n");

    if !open_browser {
        println!("(\x1b[2m--no-open-browser: skipping browser\x1b[0m)");
    }

    print!("Auth token: ");
    io::stdout().flush().unwrap();
    let mut token = String::new();
    if io::stdin().read_line(&mut token).is_err() {
        eprintln!("\x1b[31mFailed to read token.\x1b[0m");
        return;
    }
    let token = token.trim();

    if token.is_empty() {
        eprintln!("\x1b[31mToken cannot be empty.\x1b[0m");
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
            println!("\n\x1b[32mAuth configured for {}.\x1b[0m", provider_id);
            if set_default {
                println!("\x1b[32mDefault model set to: {}\x1b[0m", config.model);
            }
        }
        Err(e) => {
            eprintln!("\x1b[31mFailed to save config: {}\x1b[0m", e);
        }
    }
}
