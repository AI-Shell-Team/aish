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

/// Result of URL generation, indicating whether logs were truncated.
struct IssueUrlResult {
    url: String,
    logs_truncated: bool,
}

/// Build a GitHub issue URL with template, title, labels, and pre-filled body.
/// Truncates logs from the tail to fit within `MAX_URL_LEN` when necessary.
fn generate_issue_url(
    feedback_type: FeedbackType,
    info: &SystemInfo,
    include_logs: bool,
) -> IssueUrlResult {
    let params_prefix = format!(
        "https://github.com/AI-Shell-Team/aish/issues/new?template=feedback_auto.md&title={}{}",
        feedback_type.title_prefix(),
        percent_encode_str("<describe your issue>")
    );
    let label = feedback_type.label();
    let params_suffix = if label.is_empty() {
        String::new()
    } else {
        format!("&labels={}", label)
    };

    // Calculate how many bytes are available for the body parameter
    let overhead = params_prefix.len() + params_suffix.len() + "&body=".len();
    let max_body_encoded = MAX_URL_LEN.saturating_sub(overhead);

    let (body, truncated) = build_body_fitting(feedback_type, info, include_logs, max_body_encoded);

    let url = format!("{}{}&body={}", params_prefix, params_suffix, body);

    IssueUrlResult {
        url,
        logs_truncated: truncated,
    }
}

/// Compose the Markdown body for the GitHub issue with environment info and optional logs.
fn build_body(feedback_type: FeedbackType, info: &SystemInfo, include_logs: bool) -> String {
    let mut body = match feedback_type {
        FeedbackType::Bug => {
            let mut b = format!("{}\n\n", t("shell.feedback.body_summary"));
            b.push_str(&format!(
                "{}\n\n1.\n2.\n3.\n\n",
                t("shell.feedback.body_steps")
            ));
            b.push_str(&format!("{}\n\n", t("shell.feedback.body_expected")));
            b.push_str(&format!("{}\n\n", t("shell.feedback.body_actual")));
            b
        }
        FeedbackType::Feature => {
            let mut b = format!("{}\n\n", t("shell.feedback.body_feature_summary"));
            b.push_str(&format!("{}\n\n", t("shell.feedback.body_use_case")));
            b.push_str(&format!(
                "{}\n\n",
                t("shell.feedback.body_proposed_solution")
            ));
            b.push_str(&format!("{}\n\n", t("shell.feedback.body_alternatives")));
            b
        }
        FeedbackType::Question => {
            format!("{}\n\n", t("shell.feedback.body_question"))
        }
    };

    body.push_str(&format!("{}\n", t("shell.feedback.body_environment")));
    {
        let mut args = std::collections::HashMap::new();
        args.insert("version".to_string(), info.version.clone());
        body.push_str(&format!(
            "{}\n",
            t_with_args("shell.feedback.body_version", &args)
        ));
    }
    {
        let mut args = std::collections::HashMap::new();
        args.insert("os".to_string(), info.os.clone());
        body.push_str(&format!(
            "{}\n",
            t_with_args("shell.feedback.body_os", &args)
        ));
    }
    {
        let mut args = std::collections::HashMap::new();
        args.insert("model".to_string(), info.model.clone());
        args.insert("provider".to_string(), info.provider.clone());
        body.push_str(&format!(
            "{}\n",
            t_with_args("shell.feedback.body_model", &args)
        ));
    }

    if include_logs {
        if let Some(ref logs) = info.logs {
            body.push_str(&format!("\n{}\n```shell\n", t("shell.feedback.body_logs")));
            body.push_str(logs);
            body.push_str("\n```\n");
        }
    }

    body
}

/// Build the issue body, truncating logs line-by-line from the top to fit
/// within `max_encoded_len` bytes after percent-encoding.
/// Returns `(encoded_body, was_truncated)`.
fn build_body_fitting(
    feedback_type: FeedbackType,
    info: &SystemInfo,
    include_logs: bool,
    max_encoded_len: usize,
) -> (String, bool) {
    let body_without_logs = build_body(feedback_type, info, false);
    let encoded_without = percent_encode_str(&body_without_logs);

    if !include_logs || info.logs.is_none() {
        return (encoded_without, false);
    }

    let logs = info.logs.as_ref().unwrap();
    let log_lines: Vec<&str> = logs.lines().collect();

    if log_lines.is_empty() {
        return (encoded_without, false);
    }

    // Binary search (exclusive upper bound) for the maximum number of
    // tail log lines that fit within max_encoded_len.
    let mut lo = 0usize;
    let mut hi = log_lines.len();
    let mut best = 0usize;

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let start = log_lines.len().saturating_sub(mid);
        let truncated_logs: String = if mid == 0 {
            String::new()
        } else {
            log_lines[start..].join("\n")
        };
        let header = if mid < log_lines.len() {
            format!(
                "... (truncated, showing last {} of {} lines)\n",
                mid,
                log_lines.len()
            )
        } else {
            String::new()
        };
        let full = format!(
            "{}\n{}\n```shell\n{}{}\n```\n",
            body_without_logs,
            t("shell.feedback.body_logs"),
            header,
            truncated_logs
        );
        if percent_encode_str(&full).len() <= max_encoded_len {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    // Nothing fits — return body without logs
    if best == 0 {
        return (encoded_without, true);
    }

    let truncated = best < log_lines.len();
    let start = log_lines.len().saturating_sub(best);
    let kept_logs = log_lines[start..].join("\n");
    let header = if truncated {
        format!(
            "... (truncated, showing last {} of {} lines)\n",
            best,
            log_lines.len()
        )
    } else {
        String::new()
    };
    let full = format!(
        "{}\n{}\n```shell\n{}{}\n```\n",
        body_without_logs,
        t("shell.feedback.body_logs"),
        header,
        kept_logs
    );
    (percent_encode_str(&full), truncated)
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

    let result = generate_issue_url(feedback_type, &info, include_logs);
    if result.logs_truncated {
        eprintln!("{}", t("shell.feedback.logs_truncated"));
    }
    println!("{}", t("shell.feedback.opening"));
    if let Err(e) = open::that(&result.url) {
        let mut args = std::collections::HashMap::new();
        args.insert("error".to_string(), e.to_string());
        eprintln!("{}", t_with_args("shell.feedback.failed", &args));
        let mut visit_args = std::collections::HashMap::new();
        visit_args.insert("url".to_string(), result.url);
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

        let result = generate_issue_url(FeedbackType::Bug, &info, false);
        assert!(result
            .url
            .starts_with("https://github.com/AI-Shell-Team/aish/issues/new?"));
        assert!(result.url.contains("template=feedback_auto.md"));
        assert!(result.url.contains("labels=bug"));
        assert!(result.url.contains("title="));
        assert!(result.url.contains("body="));
        assert!(result.url.contains(&percent_encode_str(&info.version)));
        assert!(!result.logs_truncated);

        let result = generate_issue_url(FeedbackType::Feature, &info, false);
        assert!(result.url.contains("labels=enhancement"));

        let result = generate_issue_url(FeedbackType::Question, &info, false);
        assert!(!result.url.contains("labels="));
    }

    #[test]
    fn test_url_length_limit_truncates_logs() {
        // Single huge line — nothing fits, logs are dropped entirely
        let big_logs = "x".repeat(10000);
        let info = SystemInfo {
            version: "0.3.3".to_string(),
            os: "Linux".to_string(),
            model: "test".to_string(),
            provider: "Test".to_string(),
            logs: Some(big_logs),
        };

        let result = generate_issue_url(FeedbackType::Bug, &info, true);
        assert!(result.url.len() <= MAX_URL_LEN);
        assert!(result.logs_truncated);
        // Logs section should not appear since nothing fits
        let logs_header = percent_encode_str(&t("shell.feedback.body_logs"));
        assert!(!result.url.contains(&logs_header));
    }

    #[test]
    fn test_url_truncation_keeps_tail_lines() {
        // Many lines with enough content to exceed URL limit but short
        // enough that some tail lines can still fit
        let lines: Vec<String> = (0..200)
            .map(|i| {
                format!(
                    "2026-06-05 10:00:{:02} [INFO] this is a log message with enough content to be meaningful entry number {}",
                    i % 60, i
                )
            })
            .collect();
        let logs = lines.join("\n");
        let info = SystemInfo {
            version: "0.3.3".to_string(),
            os: "Linux".to_string(),
            model: "test".to_string(),
            provider: "Test".to_string(),
            logs: Some(logs),
        };

        let result = generate_issue_url(FeedbackType::Bug, &info, true);
        assert!(result.url.len() <= MAX_URL_LEN);
        assert!(result.logs_truncated);
        // Body should contain the localized logs section header
        let logs_header = format!("{}\n```shell", t("shell.feedback.body_logs"));
        assert!(result.url.contains(&percent_encode_str(&logs_header)));
        assert!(result.url.contains("truncated"));
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

        let body = build_body(FeedbackType::Bug, &info, true);
        assert!(body.contains(&t("shell.feedback.body_summary")));
        assert!(body.contains(&t("shell.feedback.body_steps")));
        assert!(body.contains(&t("shell.feedback.body_expected")));
        assert!(body.contains(&t("shell.feedback.body_actual")));
        assert!(body.contains(&info.version));
        assert!(body.contains(&info.os));
        assert!(body.contains(&info.model));
        assert!(body.contains(&info.provider));
        assert!(body.contains(&t("shell.feedback.body_logs")));
        assert!(body.contains("line1\nline2"));
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

        let body = build_body(FeedbackType::Bug, &info, false);
        assert!(!body.contains(&t("shell.feedback.body_logs")));
    }
}
