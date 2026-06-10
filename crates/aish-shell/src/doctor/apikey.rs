use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::env;

pub struct ApiKeyChecker;

impl ApiKeyChecker {
    pub fn new() -> Self {
        Self
    }

    fn mask_key(key: &str) -> String {
        if key.len() > 4 {
            format!("{}...", &key[..4])
        } else if !key.is_empty() {
            "***".to_string()
        } else {
            "(empty)".to_string()
        }
    }
}

impl Checker for ApiKeyChecker {
    fn name(&self) -> &str {
        "API Keys"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();

        let config = aish_config::ConfigLoader::load(None).unwrap_or_default();

        if !config.api_key.is_empty() {
            items.push(CheckItem::pass(
                "config.api_key",
                format!(
                    "Config API key: {} (configured)",
                    Self::mask_key(&config.api_key)
                ),
            ));
        } else {
            items.push(
                CheckItem::warn("config.api_key", "API key not set in config")
                    .hint("Run 'aish setup' to configure"),
            );
        }

        if let Ok(key) = env::var("AISH_API_KEY") {
            items.push(CheckItem::pass(
                "AISH_API_KEY",
                format!(
                    "AISH_API_KEY: {} (env override, active)",
                    Self::mask_key(&key)
                ),
            ));
        }

        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, _item: &CheckItem) -> FixResult {
        FixResult {
            success: false,
            message: "Run 'aish setup' to configure API key".to_string(),
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
