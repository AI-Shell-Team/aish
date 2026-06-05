//! Interactive feedback command: collect system info and open a pre-filled GitHub issue.

use std::path::PathBuf;

use aish_i18n::{t, t_with_args};
use aish_llm::provider::detect_provider_from_model;
use inquire::Select;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Feedback category selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedbackType {
    Bug,
    Feature,
    Question,
}

impl FeedbackType {
    fn title_prefix(&self) -> &'static str {
        match self {
            Self::Bug => "[Bug]: ",
            Self::Feature => "[Feature]: ",
            Self::Question => "[Question]: ",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Feature => "enhancement",
            Self::Question => "",
        }
    }
}

/// Collected system environment information for the GitHub issue body.
struct SystemInfo {
    version: String,
    os: String,
    model: String,
    provider: String,
    logs: Option<String>,
}

// ---------------------------------------------------------------------------
// System info collection
// ---------------------------------------------------------------------------

/// Gather aish version, OS info, AI model/provider, and recent log lines.
fn collect_system_info(model: &str, api_base: &str) -> SystemInfo {
    let version = env!("CARGO_PKG_VERSION").to_string();

    let os = {
        use sysinfo::System;
        let name = System::name().unwrap_or_else(|| "Unknown".into());
        let kernel = System::kernel_version().unwrap_or_else(|| "unknown".into());
        format!("{} {}", name, kernel)
    };

    let provider_info = detect_provider_from_model(model);
    let provider = if api_base.is_empty() {
        provider_info.display_name
    } else {
        aish_llm::provider::detect_provider(model, api_base).display_name
    };

    let logs = read_recent_logs(30);

    SystemInfo {
        version,
        os,
        model: model.to_string(),
        provider,
        logs,
    }
}

/// Read the last `max_lines` lines from the log file without loading the
/// entire file into memory. Uses seek-to-end + backward byte scan.
/// Read the last `max_lines` lines from the aish log file, with sensitive data redacted.
fn read_recent_logs(max_lines: usize) -> Option<String> {
    let log_path = log_file_path()?;
    let content = std::fs::read_to_string(&log_path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(max_lines);
    let mut selected: Vec<String> = lines[start..].iter().map(|s| s.to_string()).collect();
    redact_sensitive_lines_owned(&mut selected);
    Some(selected.join("\n"))
}

/// Resolve the aish log file path under the user's config directory.
fn log_file_path() -> Option<PathBuf> {
    let config_dir = dirs::config_dir()?;
    let path = config_dir.join("aish").join("logs").join("aish.log");
    if path.exists() {
        return Some(path);
    }
    None
}

/// Replace any line containing sensitive keywords with `[REDACTED]`.
fn redact_sensitive_lines_owned(lines: &mut [String]) {
    let sensitive_patterns = [
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "authorization",
        "bearer",
    ];
    for line in lines.iter_mut() {
        let lower = line.to_lowercase();
        for pattern in &sensitive_patterns {
            if lower.contains(pattern) {
                *line = "[REDACTED]".to_string();
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// URL generation
// ---------------------------------------------------------------------------

/// Maximum URL length to stay within browser/GitHub limits.
const MAX_URL_LEN: usize = 7168;

/// Build a GitHub issue URL with template, title, labels, and pre-filled body.
/// Automatically strips logs if the URL exceeds `MAX_URL_LEN`.
fn generate_issue_url(
    feedback_type: FeedbackType,
    info: &SystemInfo,
    include_logs: bool,
) -> String {
    let mut params = Vec::new();

    params.push("template=feedback_auto.md".to_string());
    params.push(format!(
        "title={}{}",
        feedback_type.title_prefix(),
        percent_encode_str("<describe your issue>")
    ));

    let label = feedback_type.label();
    if !label.is_empty() {
        params.push(format!("labels={}", label));
    }

    // Try with logs first, strip if URL too long
    let body = build_body(info, include_logs);
    params.push(format!("body={}", percent_encode_str(&body)));

    let url = format!(
        "https://github.com/AI-Shell-Team/aish/issues/new?{}",
        params.join("&")
    );

    if url.len() > MAX_URL_LEN && include_logs {
        return generate_issue_url(feedback_type, info, false);
    }

    url
}

/// Compose the Markdown body for the GitHub issue with environment info and optional logs.
fn build_body(info: &SystemInfo, include_logs: bool) -> String {
    let mut body = String::from("## Summary\n\n");
    body.push_str("## Environment\n");
    body.push_str(&format!("- AISH version: {}\n", info.version));
    body.push_str(&format!("- Operating system: {}\n", info.os));
    body.push_str(&format!(
        "- AI Model / Provider: {} / {}\n",
        info.model, info.provider
    ));

    if include_logs {
        if let Some(ref logs) = info.logs {
            body.push_str("\n## Logs\n```shell\n");
            body.push_str(logs);
            body.push_str("\n```\n");
        }
    }

    body
}

/// Percent-encode a string for use in a URL query parameter.
fn percent_encode_str(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push('+'),
            _ => {
                result.push('%');
                result.push_str(&format!("{:02X}", byte));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run the interactive feedback wizard.
pub fn run_feedback(model: &str, api_base: &str) {
    let feedback_type = match select_feedback_type() {
        Some(ft) => ft,
        None => return,
    };

    let info = collect_system_info(model, api_base);

    let include_logs = match confirm_and_review(&info) {
        ConfirmAction::Confirm => true,
        ConfirmAction::WithoutLogs => false,
        ConfirmAction::Cancel => return,
    };

    let url = generate_issue_url(feedback_type, &info, include_logs);
    println!("{}", t("shell.feedback.opening"));
    if let Err(e) = open::that(&url) {
        let mut args = std::collections::HashMap::new();
        args.insert("error".to_string(), e.to_string());
        eprintln!("{}", t_with_args("shell.feedback.failed", &args));
        let mut visit_args = std::collections::HashMap::new();
        visit_args.insert("url".to_string(), url);
        println!("{}", t_with_args("shell.feedback.visit", &visit_args));
    }
}

/// User's choice after reviewing collected system info.
enum ConfirmAction {
    Confirm,
    WithoutLogs,
    Cancel,
}

/// Prompt the user to select a feedback type (Bug / Feature / Question).
fn select_feedback_type() -> Option<FeedbackType> {
    let bug = t("shell.feedback.type_bug");
    let feature = t("shell.feedback.type_feature");
    let question = t("shell.feedback.type_question");

    let items = vec![bug.clone(), feature.clone(), question.clone()];

    let result = Select::new(&t("shell.feedback.select_type"), items).prompt_skippable();

    match result {
        Ok(Some(sel)) => {
            if sel == bug {
                Some(FeedbackType::Bug)
            } else if sel == feature {
                Some(FeedbackType::Feature)
            } else if sel == question {
                Some(FeedbackType::Question)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Show collected system info and ask the user to confirm, exclude logs, or cancel.
fn confirm_and_review(info: &SystemInfo) -> ConfirmAction {
    let confirm_label = t("shell.feedback.confirm");
    let without_logs_label = t("shell.feedback.without_logs");
    let cancel_label = t("shell.feedback.cancel");

    let logs_status = if info.logs.is_some() {
        t("shell.feedback.logs_included")
    } else {
        t("shell.feedback.log_unavailable")
    };

    let review = format!(
        "aish version: {}\nOS: {}\nAI Model: {}\nProvider: {}\nLogs: {}",
        info.version, info.os, info.model, info.provider, logs_status
    );

    let prompt_text = format!("{}\n{}", t("shell.feedback.confirm_question"), review);

    let items = vec![
        confirm_label.clone(),
        without_logs_label.clone(),
        cancel_label,
    ];

    let result = Select::new(&prompt_text, items).prompt_skippable();

    match result {
        Ok(Some(sel)) => {
            if sel == confirm_label {
                ConfirmAction::Confirm
            } else if sel == without_logs_label {
                ConfirmAction::WithoutLogs
            } else {
                ConfirmAction::Cancel
            }
        }
        _ => ConfirmAction::Cancel,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percent_encode_basic() {
        assert_eq!(percent_encode_str("hello"), "hello");
        assert_eq!(percent_encode_str("hello world"), "hello+world");
        assert_eq!(percent_encode_str("a=b&c=d"), "a%3Db%26c%3Dd");
    }

    #[test]
    fn test_percent_encode_special_chars() {
        assert_eq!(percent_encode_str("!@#$%"), "%21%40%23%24%25");
        assert_eq!(percent_encode_str("newline\n"), "newline%0A");
    }

    #[test]
    fn test_redact_sensitive_lines() {
        let mut lines = vec![
            "normal log line".to_string(),
            "setting api_key=sk-abc123".to_string(),
            "another normal line".to_string(),
            "Authorization: Bearer token123".to_string(),
            "my secret value".to_string(),
        ];
        redact_sensitive_lines_owned(&mut lines);
        assert_eq!(lines[0], "normal log line");
        assert_eq!(lines[1], "[REDACTED]");
        assert_eq!(lines[2], "another normal line");
        assert_eq!(lines[3], "[REDACTED]");
        assert_eq!(lines[4], "[REDACTED]");
    }

    #[test]
    fn test_generate_issue_url_structure() {
        let info = SystemInfo {
            version: "0.3.3".to_string(),
            os: "Linux 6.12".to_string(),
            model: "test-model".to_string(),
            provider: "TestProvider".to_string(),
            logs: None,
        };

        let url = generate_issue_url(FeedbackType::Bug, &info, false);
        assert!(url.starts_with("https://github.com/AI-Shell-Team/aish/issues/new?"));
        assert!(url.contains("template=feedback_auto.md"));
        assert!(url.contains("labels=bug"));
        assert!(url.contains("title="));
        assert!(url.contains("body="));
        assert!(url.contains("AISH+version%3A+0.3.3"));

        let url = generate_issue_url(FeedbackType::Feature, &info, false);
        assert!(url.contains("labels=enhancement"));

        let url = generate_issue_url(FeedbackType::Question, &info, false);
        assert!(!url.contains("labels="));
    }

    #[test]
    fn test_url_length_limit_strips_logs() {
        let big_logs = "x".repeat(10000);
        let info = SystemInfo {
            version: "0.3.3".to_string(),
            os: "Linux".to_string(),
            model: "test".to_string(),
            provider: "Test".to_string(),
            logs: Some(big_logs),
        };

        let url = generate_issue_url(FeedbackType::Bug, &info, true);
        assert!(url.len() <= MAX_URL_LEN);
        assert!(!url.contains(&"x".repeat(100)));
    }

    #[test]
    fn test_build_body_contains_env_info() {
        let info = SystemInfo {
            version: "0.3.3".to_string(),
            os: "Linux 6.12".to_string(),
            model: "claude-sonnet".to_string(),
            provider: "Anthropic".to_string(),
            logs: Some("line1\nline2".to_string()),
        };

        let body = build_body(&info, true);
        assert!(body.contains("AISH version: 0.3.3"));
        assert!(body.contains("Operating system: Linux 6.12"));
        assert!(body.contains("AI Model / Provider: claude-sonnet / Anthropic"));
        assert!(body.contains("## Logs"));
        assert!(body.contains("line1\nline2"));
        // No excessive blank lines
        assert!(!body.contains("\n\n\n"));
    }

    #[test]
    fn test_build_body_without_logs() {
        let info = SystemInfo {
            version: "0.3.3".to_string(),
            os: "Linux".to_string(),
            model: "test".to_string(),
            provider: "Test".to_string(),
            logs: Some("secret stuff".to_string()),
        };

        let body = build_body(&info, false);
        assert!(!body.contains("## Logs"));
    }
}
