use crate::doctor::checker::{CheckItem, CheckResult, CheckStatus};
use crossterm::style::{Color, Stylize};

pub struct Output;

impl Output {
    pub fn print_header() {
        println!();
        println!("{}", Self::header("       AISH Basic Config Diagnostics"));
        println!();
    }

    pub fn print_result(result: &CheckResult) {
        let icon = match result.status {
            CheckStatus::Pass => Self::colored("✓", Color::Green),
            CheckStatus::Warn => Self::colored("⚠", Color::Yellow),
            CheckStatus::Fail => Self::colored("✗", Color::Red),
        };
        println!("[{}] {}", icon, result.checker);
        for item in &result.items {
            Self::print_item(item);
        }
    }

    fn print_item(item: &CheckItem) {
        let icon = match item.status {
            CheckStatus::Pass => Self::colored("✓", Color::Green),
            CheckStatus::Warn => Self::colored("⚠", Color::Yellow),
            CheckStatus::Fail => Self::colored("✗", Color::Red),
        };
        let indent = "    ";
        println!("{}{} {}", indent, icon, item.message);
        if let Some(hint) = &item.hint {
            println!("{}{} {}", indent, Self::colored("→", Color::Cyan), hint);
        }
    }

    pub fn print_summary(
        fail_count: usize,
        warn_count: usize,
        fixable_count: usize,
        issues: &[String],
    ) {
        println!();
        println!("{}", Self::separator());
        if fail_count == 0 && warn_count == 0 {
            println!("{}", Self::colored("  All checks passed!", Color::Green));
        } else {
            println!("  Found {} issue(s)", fail_count + warn_count);
            if !issues.is_empty() {
                for (i, issue) in issues.iter().enumerate() {
                    println!("  {}. {}", i + 1, issue);
                }
            }
            if fixable_count > 0 {
                println!();
                println!(
                    "  {}",
                    Self::colored(
                        &format!(
                            "Tip: run 'aish doctor --fix' to auto-fix ({} fixable)",
                            fixable_count
                        ),
                        Color::DarkGrey
                    )
                );
            }
        }
        println!("{}", Self::separator());
    }

    pub fn print_fix_summary(fixed_count: usize, remaining: &[String]) {
        println!();
        println!("{}", Self::separator());
        if fixed_count > 0 {
            println!(
                "  {}",
                Self::colored(&format!("Fixed {} issue(s).", fixed_count), Color::Green)
            );
            if !remaining.is_empty() {
                println!(
                    "  {}",
                    Self::colored(
                        &format!("{} issue(s) require manual intervention:", remaining.len()),
                        Color::Yellow
                    )
                );
                println!();
                for (i, issue) in remaining.iter().enumerate() {
                    println!("  {}. {}", i + 1, issue);
                }
            }
        }
        println!("{}", Self::separator());
        println!();
    }

    fn header(s: &str) -> String {
        Self::colored(s, Color::Cyan).to_string()
    }

    fn separator() -> String {
        Self::colored(&"─".repeat(50), Color::DarkGrey).to_string()
    }

    fn colored(s: &str, color: Color) -> String {
        format!("{}", crossterm::style::style(s).with(color))
    }
}
