use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::path::PathBuf;

pub struct SkillsChecker {
    user_skills_dir: PathBuf,
    claude_skills_dir: PathBuf,
}

impl SkillsChecker {
    pub fn new() -> Self {
        Self {
            user_skills_dir: dirs::config_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"))
                .join("aish/skills"),
            claude_skills_dir: dirs::home_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"))
                .join(".claude/skills"),
        }
    }

    fn check_skill_dir(&self, dir: &PathBuf, name: &str) -> Vec<CheckItem> {
        let mut items = Vec::new();

        if !dir.exists() {
            items.push(CheckItem::warn(name, format!("{} not found", dir.display())).fixable());
            return items;
        }

        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let skill_dirs: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .collect();
                if !skill_dirs.is_empty() {
                    items.push(CheckItem::pass(
                        name,
                        format!("{}: {} skills", name, skill_dirs.len()),
                    ));

                    for dir_entry in skill_dirs {
                        let skill_name = dir_entry.file_name().to_string_lossy().to_string();
                        if !dir_entry.path().join("SKILL.md").exists() {
                            items.push(CheckItem::warn(
                                &skill_name,
                                format!("{}: missing SKILL.md", skill_name),
                            ));
                        }
                    }
                } else {
                    items.push(CheckItem::pass(name, format!("{}: empty", name)));
                }
            }
            Err(e) => {
                items.push(CheckItem::fail(
                    name,
                    format!("Cannot read {}: {}", name, e),
                ));
            }
        }

        items
    }
}

impl Checker for SkillsChecker {
    fn name(&self) -> &str {
        "Skills"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();
        items.extend(self.check_skill_dir(&self.user_skills_dir, "User Skills"));
        items.extend(self.check_skill_dir(&self.claude_skills_dir, "Claude Skills"));
        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, item: &CheckItem) -> FixResult {
        let path = if item.name == "User Skills" {
            Some(self.user_skills_dir.clone())
        } else if item.name == "Claude Skills" {
            Some(self.claude_skills_dir.clone())
        } else {
            None
        };

        match path {
            Some(p) => match std::fs::create_dir_all(&p) {
                Ok(_) => FixResult {
                    success: true,
                    message: format!("Created skills directory: {}", p.display()),
                },
                Err(e) => FixResult {
                    success: false,
                    message: format!("Failed to create skills directory: {}", e),
                },
            },
            None => FixResult {
                success: false,
                message: "Cannot auto-fix skill configuration".to_string(),
            },
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
