use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::path::PathBuf;

pub struct MemoryChecker {
    memory_path: PathBuf,
    config_dir: PathBuf,
}

impl MemoryChecker {
    pub fn new() -> Self {
        let config_dir =
            dirs::config_dir().unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"));
        Self {
            memory_path: config_dir.join("aish/MEMORY.md"),
            config_dir: config_dir.join("aish"),
        }
    }
}

impl Checker for MemoryChecker {
    fn name(&self) -> &str {
        "Memory"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();

        if !self.config_dir.exists() {
            items.push(CheckItem::warn("config_dir", "Config directory does not exist").fixable());
            return vec![CheckResult::from_items(self.name(), items)];
        }

        items.push(CheckItem::pass(
            "config_dir",
            format!("Config directory: {}", self.config_dir.display()),
        ));

        if self.memory_path.exists() {
            items.push(CheckItem::pass(
                "memory_md",
                format!("MEMORY.md: {}", self.memory_path.display()),
            ));
        } else {
            items.push(CheckItem::warn("memory_md", "MEMORY.md not found").fixable());
        }

        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, item: &CheckItem) -> FixResult {
        match item.name.as_str() {
            "config_dir" => match std::fs::create_dir_all(&self.config_dir) {
                Ok(_) => FixResult {
                    success: true,
                    message: format!("Created config directory: {}", self.config_dir.display()),
                },
                Err(e) => FixResult {
                    success: false,
                    message: format!("Failed to create config directory: {}", e),
                },
            },
            "memory_md" => match std::fs::write(&self.memory_path, "# Memory\n\n") {
                Ok(_) => FixResult {
                    success: true,
                    message: format!("Created MEMORY.md at {}", self.memory_path.display()),
                },
                Err(e) => FixResult {
                    success: false,
                    message: format!("Failed to create MEMORY.md: {}", e),
                },
            },
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
