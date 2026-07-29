use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use aish_config::ConfigModel;
use std::path::PathBuf;

pub struct ConfigChecker {
    config_path: PathBuf,
}

impl ConfigChecker {
    pub fn new() -> Self {
        Self {
            config_path: dirs::config_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"))
                .join("aish/config.yaml"),
        }
    }

    fn validate_structure(&self, config: &ConfigModel) -> Vec<CheckItem> {
        let mut items = Vec::new();

        if !config.api_base.is_empty() {
            if config.api_base.starts_with("http://") || config.api_base.starts_with("https://") {
                items.push(CheckItem::pass(
                    "api_base_valid",
                    format!("API base URL: {}", config.api_base),
                ));
            } else {
                items.push(
                    CheckItem::fail(
                        "api_base_valid",
                        format!(
                            "Invalid api_base: {} (must start with http:// or https://)",
                            config.api_base
                        ),
                    )
                    .hint("Fix api_base in config.yaml"),
                );
            }
        }

        if !config.model.is_empty() {
            items.push(CheckItem::pass(
                "model_set",
                format!("Model: {}", config.model),
            ));
        } else {
            items.push(
                CheckItem::warn("model_set", "No model configured")
                    .hint("Run 'aish setup' to select a model"),
            );
        }

        // Sandbox globals live in security_policy.yaml (not config.yaml).
        // Use the fallible loader so a missing/broken policy is visible instead
        // of silently evaluating SecurityPolicy::default().
        match aish_security::try_load_policy(None) {
            Ok(policy) => {
                items.push(CheckItem::pass(
                    "security_policy",
                    "security_policy.yaml loaded",
                ));
                if !policy.enable_sandbox
                    && policy.sandbox_off_action != aish_security::SandboxOffAction::Allow
                {
                    items.push(CheckItem::warn(
                        "sandbox_config",
                        format!(
                            "Sandbox disabled but sandbox_off_action is '{}' (expected 'ALLOW')",
                            policy.sandbox_off_action
                        ),
                    ));
                }
            }
            Err(e) => {
                items.push(CheckItem::fail("security_policy", e.to_string()).hint(
                    "Fix or recreate ~/.config/aish/security_policy.yaml (or delete it to reseed)",
                ));
            }
        }

        items
    }
}

impl Checker for ConfigChecker {
    fn name(&self) -> &str {
        "Configuration"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();

        if self.config_path.exists() {
            items.push(CheckItem::pass(
                "exists",
                format!("Config file: {}", self.config_path.display()),
            ));

            match std::fs::read_to_string(&self.config_path) {
                Ok(content) => match serde_yaml::from_str::<ConfigModel>(&content) {
                    Ok(config) => {
                        items.push(CheckItem::pass("yaml", "YAML format valid"));
                        items.extend(self.validate_structure(&config));
                    }
                    Err(e) => {
                        items.push(
                            CheckItem::fail("yaml", format!("YAML parse error: {}", e)).fixable(),
                        );
                    }
                },
                Err(e) => {
                    items.push(CheckItem::fail(
                        "read",
                        format!("Cannot read config: {}", e),
                    ));
                }
            }
        } else {
            items.push(
                CheckItem::fail("exists", "Config file not found")
                    .fixable()
                    .hint("Run 'aish setup' to create one"),
            );
        }

        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, item: &CheckItem) -> FixResult {
        match item.name.as_str() {
            "exists" => {
                if let Some(parent) = self.config_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let default_config = ConfigModel::default();
                match serde_yaml::to_string(&default_config) {
                    Ok(yaml) => match std::fs::write(&self.config_path, yaml) {
                        Ok(_) => FixResult {
                            success: true,
                            message: format!(
                                "Created default config at {}",
                                self.config_path.display()
                            ),
                        },
                        Err(e) => FixResult {
                            success: false,
                            message: format!("Failed to write config: {}", e),
                        },
                    },
                    Err(e) => FixResult {
                        success: false,
                        message: format!("Failed to serialize default config: {}", e),
                    },
                }
            }
            "yaml" => {
                let backup_path = self.config_path.with_extension("yaml.bak");
                if let Err(e) = std::fs::rename(&self.config_path, &backup_path) {
                    return FixResult {
                        success: false,
                        message: format!("Failed to backup broken config: {}", e),
                    };
                }
                let default_config = ConfigModel::default();
                match serde_yaml::to_string(&default_config) {
                    Ok(yaml) => match std::fs::write(&self.config_path, yaml) {
                        Ok(_) => FixResult {
                            success: true,
                            message: format!(
                                "Reset to default config (backup: {})",
                                backup_path.display()
                            ),
                        },
                        Err(e) => FixResult {
                            success: false,
                            message: format!("Failed to write config: {}", e),
                        },
                    },
                    Err(e) => FixResult {
                        success: false,
                        message: format!("Failed to serialize default config: {}", e),
                    },
                }
            }
            _ => FixResult {
                success: false,
                message: "Cannot fix this item".to_string(),
            },
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
