use crossterm::style::Stylize;
use serde_json;

pub mod api;
pub mod apikey;
pub mod checker;
pub mod config;
pub mod dirs;
pub mod memory;
pub mod output;
pub mod session;
pub mod shell;
pub mod skills;
pub mod tools;

pub use api::ApiConnectivityChecker;
pub use apikey::ApiKeyChecker;
pub use checker::{CheckResult, CheckStatus, Checker};
pub use config::ConfigChecker;
pub use dirs::DirsChecker;
pub use memory::MemoryChecker;
pub use output::Output;
pub use session::SessionChecker;
pub use shell::ShellChecker;
pub use skills::SkillsChecker;
pub use tools::ExternalToolsChecker;

pub struct Doctor {
    checkers: Vec<Box<dyn Checker>>,
}

impl Doctor {
    pub fn new() -> Self {
        let checkers: Vec<Box<dyn Checker>> = vec![
            Box::new(ConfigChecker::new()),
            Box::new(ApiKeyChecker::new()),
            Box::new(DirsChecker::new()),
            Box::new(SessionChecker::new()),
            Box::new(ExternalToolsChecker::new()),
            Box::new(ShellChecker::new()),
            Box::new(SkillsChecker::new()),
            Box::new(MemoryChecker::new()),
            Box::new(ApiConnectivityChecker::new()),
        ];
        Self { checkers }
    }
    pub async fn run(&self, fix: bool, json: bool) {
        let handles: Vec<_> = self
            .checkers
            .iter()
            .map(|checker| {
                let checker: Box<dyn Checker> = checker.box_clone();
                tokio::task::spawn_blocking(move || checker.check())
            })
            .collect();

        let mut all_results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(results) => all_results.extend(results),
                Err(e) => {
                    eprintln!("Warning: A checker task failed: {}", e);
                }
            }
        }

        // Machine-readable JSON output: print only the structured results.
        // --fix is incompatible with --json: fixes require interactive
        // confirmation and human-readable output, so silently ignore fix.
        if json {
            if fix {
                eprintln!("Warning: --fix is ignored with --json (fixes need interactive confirmation)");
            }
            match serde_json::to_string_pretty(&all_results) {
                Ok(s) => println!("{}", s),
                Err(e) => eprintln!("Failed to serialize doctor results: {}", e),
            }
            return;
        }

        Output::print_header();

        for result in &all_results {
            Output::print_result(result);
        }

        // Collect issues and count stats
        let mut fail_count = 0usize;
        let mut warn_count = 0usize;
        let mut fixable_count = 0usize;
        let mut issues: Vec<String> = Vec::new();

        for result in &all_results {
            for item in &result.items {
                match item.status {
                    CheckStatus::Fail => {
                        fail_count += 1;
                        let msg = format!("[{}] {}", result.checker, item.message);
                        issues.push(msg);
                        if item.fixable {
                            fixable_count += 1;
                        }
                    }
                    CheckStatus::Warn => {
                        warn_count += 1;
                        if item.fixable {
                            fixable_count += 1;
                            let msg = format!("[{}] {}", result.checker, item.message);
                            issues.push(msg);
                        }
                    }
                    CheckStatus::Pass | CheckStatus::NotApplicable => {}
                }
            }
        }

        Output::print_summary(fail_count, warn_count, fixable_count, &issues);

        if fix && fixable_count > 0 {
            let remaining = Self::run_fixes(&self.checkers, &all_results);
            let fixed = fixable_count - remaining.len();
            Output::print_fix_summary(fixed, &remaining);
        }
    }

    fn run_fixes(checkers: &[Box<dyn Checker>], results: &[CheckResult]) -> Vec<String> {
        println!("\nRunning auto-fix...\n");
        let mut remaining = Vec::new();

        for checker in checkers {
            for result in results {
                if result.checker == checker.name() {
                    for item in &result.items {
                        if item.fixable
                            && (item.status == CheckStatus::Fail
                                || item.status == CheckStatus::Warn)
                        {
                            let fix_result = checker.fix(item);
                            if fix_result.success {
                                println!(
                                    "  {} {}",
                                    crossterm::style::style("✓")
                                        .with(crossterm::style::Color::Green),
                                    fix_result.message
                                );
                            } else {
                                println!(
                                    "  {} {}",
                                    crossterm::style::style("✗").with(crossterm::style::Color::Red),
                                    fix_result.message
                                );
                                remaining.push(format!("[{}] {}", result.checker, item.message));
                            }
                        }
                    }
                }
            }
        }
        remaining
    }
}

impl Default for Doctor {
    fn default() -> Self {
        Self::new()
    }
}
