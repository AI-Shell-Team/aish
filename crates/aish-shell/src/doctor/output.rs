use crate::doctor::checker::{CheckItem, CheckResult, CheckStatus};
use crate::theme;

pub struct Output;

impl Output {
    pub fn print_header() {
        println!();
        println!(
            "{}",
            theme::accent(&theme::bold("       AISH Basic Config Diagnostics"))
        );
        println!();
    }

    pub fn print_result(result: &CheckResult) {
        let icon = match result.status {
            CheckStatus::Pass => theme::success(theme::ICON_SUCCESS),
            CheckStatus::Warn => theme::warning(theme::ICON_WARNING),
            CheckStatus::Fail => theme::error(theme::ICON_ERROR),
        };
        println!("[{}] {}", icon, result.checker);
        for item in &result.items {
            Self::print_item(item);
        }
    }

    fn print_item(item: &CheckItem) {
        let icon = match item.status {
            CheckStatus::Pass => theme::success(theme::ICON_SUCCESS),
            CheckStatus::Warn => theme::warning(theme::ICON_WARNING),
            CheckStatus::Fail => theme::error(theme::ICON_ERROR),
        };
        let indent = "    ";
        println!("{}{} {}", indent, icon, item.message);
        if let Some(hint) = &item.hint {
            println!("{}{} {}", indent, theme::accent("→"), hint);
        }
    }

    pub fn print_summary(
        fail_count: usize,
        warn_count: usize,
        fixable_count: usize,
        issues: &[String],
    ) {
        println!();
        println!("{}", theme::dim(&"─".repeat(50)));
        if fail_count == 0 && warn_count == 0 {
            println!("{}", theme::success("  All checks passed!"));
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
                    theme::dim(&format!(
                        "Tip: run 'aish doctor --fix' to auto-fix ({} fixable)",
                        fixable_count
                    ))
                );
            }
        }
        println!("{}", theme::dim(&"─".repeat(50)));
    }

    pub fn print_fix_summary(fixed_count: usize, remaining: &[String]) {
        println!();
        println!("{}", theme::dim(&"─".repeat(50)));
        if fixed_count > 0 {
            println!(
                "  {}",
                theme::success(&format!("Fixed {} issue(s).", fixed_count))
            );
            if !remaining.is_empty() {
                println!(
                    "  {}",
                    theme::warning(&format!(
                        "{} issue(s) require manual intervention:",
                        remaining.len()
                    ))
                );
                println!();
                for (i, issue) in remaining.iter().enumerate() {
                    println!("  {}. {}", i + 1, issue);
                }
            }
        }
        println!("{}", theme::dim(&"─".repeat(50)));
        println!();
    }
}
