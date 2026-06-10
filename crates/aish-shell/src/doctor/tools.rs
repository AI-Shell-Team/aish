use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};

struct ToolInfo {
    name: &'static str,
    binary: &'static str,
    install_hint: &'static str,
}

pub struct ExternalToolsChecker;

impl ExternalToolsChecker {
    pub fn new() -> Self {
        Self
    }

    fn tools() -> Vec<ToolInfo> {
        vec![
            ToolInfo {
                name: "git",
                binary: "git",
                install_hint: "Install git for version control support",
            },
            ToolInfo {
                name: "ripgrep (rg)",
                binary: "rg",
                install_hint:
                    "Install ripgrep for faster file search: https://github.com/BurntSushi/ripgrep",
            },
        ]
    }
}

impl Checker for ExternalToolsChecker {
    fn name(&self) -> &str {
        "External Tools"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();

        for tool in Self::tools() {
            if which::which(tool.binary).is_ok() {
                items.push(CheckItem::pass(tool.name, tool.name));
            } else {
                items.push(
                    CheckItem::warn(tool.name, format!("{} not found", tool.name))
                        .hint(tool.install_hint),
                );
            }
        }

        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, _item: &CheckItem) -> FixResult {
        FixResult {
            success: false,
            message: "Install external tools manually".to_string(),
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
