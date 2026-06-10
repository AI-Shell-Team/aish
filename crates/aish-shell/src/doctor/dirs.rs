use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::path::PathBuf;

pub struct DirsChecker {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl DirsChecker {
    pub fn new() -> Self {
        Self {
            config_dir: dirs::config_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"))
                .join("aish"),
            data_dir: dirs::data_local_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"))
                .join("aish"),
        }
    }
}

impl Checker for DirsChecker {
    fn name(&self) -> &str {
        "Directory Structure"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();

        if self.config_dir.exists() {
            items.push(CheckItem::pass(
                "config_dir",
                format!("Config directory: {}", self.config_dir.display()),
            ));

            let skills_dir = self.config_dir.join("skills");
            if skills_dir.exists() {
                items.push(CheckItem::pass(
                    "skills",
                    format!("Skills directory: {}", skills_dir.display()),
                ));
            } else {
                items.push(CheckItem::warn("skills", "Skills directory not found").fixable());
            }

            let logs_dir = self.config_dir.join("logs");
            if logs_dir.exists() {
                items.push(CheckItem::pass(
                    "logs",
                    format!("Logs directory: {}", logs_dir.display()),
                ));
            } else {
                items.push(CheckItem::warn("logs", "Logs directory not found").fixable());
            }
        } else {
            items.push(CheckItem::fail("config_dir", "Config directory not found").fixable());
        }

        if self.data_dir.exists() {
            items.push(CheckItem::pass(
                "data_dir",
                format!("Data directory: {}", self.data_dir.display()),
            ));
        } else {
            items.push(CheckItem::warn("data_dir", "Data directory not found").fixable());
        }

        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, item: &CheckItem) -> FixResult {
        let path = match item.name.as_str() {
            "config_dir" => Some(self.config_dir.clone()),
            "skills" => Some(self.config_dir.join("skills")),
            "logs" => Some(self.config_dir.join("logs")),
            "data_dir" => Some(self.data_dir.clone()),
            _ => None,
        };

        match path {
            Some(p) => match std::fs::create_dir_all(&p) {
                Ok(_) => FixResult {
                    success: true,
                    message: format!("Created directory: {}", p.display()),
                },
                Err(e) => FixResult {
                    success: false,
                    message: format!("Failed to create directory: {}", e),
                },
            },
            None => FixResult {
                success: false,
                message: "Cannot fix this item".to_string(),
            },
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
