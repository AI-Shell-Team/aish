//! Bash command read-only analysis for the bash tool.
//!
//! Classifies whether a command string is read-only in the literal shell sense.
//! Enforcement (block vs allow) is decided by callers: bash preflight policy,
//! `/diagnose` verify gate, future sub-agents, etc.

use aish_i18n::{t, t_with_args};
use aish_llm::PreflightResult;
use aish_security::sudo::strip_sudo_prefix;
use std::collections::HashMap;

/// Result of classifying a bash command for read-only semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyVerdict {
    ReadOnly,
    NotReadOnly { reason: String },
    Unparseable,
}

/// Classify whether a bash command string is read-only.
pub fn classify(command: &str) -> ReadOnlyVerdict {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return ReadOnlyVerdict::Unparseable;
    }
    if has_background_operator(trimmed) {
        return ReadOnlyVerdict::NotReadOnly {
            reason: "background job".into(),
        };
    }

    for segment in split_compound_segments(trimmed) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        let (stripped, _sudo_detected, sudo_ok) = strip_sudo_prefix(seg);
        if !sudo_ok {
            return ReadOnlyVerdict::Unparseable;
        }
        let seg = stripped.trim();
        if seg.is_empty() {
            return ReadOnlyVerdict::Unparseable;
        }
        if let Some(reason) = non_readonly_segment_reason(seg) {
            return ReadOnlyVerdict::NotReadOnly { reason };
        }
        if let Some(verdict) = wrapper_or_subshell_verdict(seg) {
            return verdict;
        }
    }

    ReadOnlyVerdict::ReadOnly
}

/// Returns `true` when the command is classified as read-only.
pub fn is_read_only(command: &str) -> bool {
    matches!(classify(command), ReadOnlyVerdict::ReadOnly)
}

/// When `enforce` is true, return a preflight block for non-read-only commands.
pub fn preflight_enforce(command: &str, enforce: bool) -> Option<PreflightResult> {
    if !enforce {
        return None;
    }

    match classify(command) {
        ReadOnlyVerdict::ReadOnly => None,
        ReadOnlyVerdict::NotReadOnly { reason } => Some(PreflightResult::Block {
            message: block_message(&reason),
            security: None,
        }),
        ReadOnlyVerdict::Unparseable => Some(PreflightResult::Block {
            message: t("tools.bash.read_only.unparseable"),
            security: None,
        }),
    }
}

fn block_message(reason: &str) -> String {
    let mut args = HashMap::new();
    args.insert("reason".to_string(), reason.to_string());
    t_with_args("tools.bash.read_only.blocked", &args)
}

fn split_compound_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(ch);
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(ch);
            continue;
        }
        if ch == '\\' && !in_single_quote {
            current.push(ch);
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }

        if !in_single_quote && !in_double_quote {
            if ch == '&' {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    segments.push(current.clone());
                    current.clear();
                    continue;
                }
                segments.push(current.clone());
                current.clear();
                continue;
            }
            if ch == ';' {
                segments.push(current.clone());
                current.clear();
                continue;
            }
            if ch == '|' {
                if chars.peek() == Some(&'|') {
                    chars.next();
                    segments.push(current.clone());
                    current.clear();
                    continue;
                }
                segments.push(current.clone());
                current.clear();
                continue;
            }
        }

        current.push(ch);
    }

    if !current.is_empty() {
        segments.push(current);
    }

    segments
}

fn has_background_operator(command: &str) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if ch == '\\' && !in_single_quote {
            chars.next();
            continue;
        }
        if !in_single_quote && !in_double_quote && ch == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                continue;
            }
            return true;
        }
    }

    false
}

fn non_readonly_segment_reason(segment: &str) -> Option<String> {
    if scan_outside_quotes(segment, |window| {
        window.starts_with("$(")
            || window.starts_with('`')
            || window.starts_with("<(")
            || window.starts_with(">(")
    }) {
        return Some("command or process substitution".into());
    }
    if scan_outside_quotes(segment, |window| window.starts_with('*')) {
        return Some("unquoted glob".into());
    }
    if scan_outside_quotes(segment, |window| {
        if let Some(rest) = window.strip_prefix(">>") {
            return !rest.trim_start().starts_with("/dev/null");
        }
        if let Some(rest) = window.strip_prefix('>') {
            return !rest.trim_start().starts_with("/dev/null");
        }
        false
    }) {
        return Some("write redirect".into());
    }
    if scan_outside_quotes(segment, |window| {
        window
            .strip_prefix("<<")
            .is_some_and(|rest| !rest.contains("/dev/null"))
    }) {
        return Some("heredoc write".into());
    }

    let tokens = tokenize_shell_words(segment);
    let first = tokens
        .first()
        .map(|token| normalize_shell_word(token))
        .unwrap_or_default();
    let base = first
        .rsplit('/')
        .next()
        .unwrap_or(&first)
        .to_ascii_lowercase();

    const BLOCKED: &[&str] = &[
        "rm", "mv", "cp", "chmod", "chown", "mkdir", "touch", "tee", "truncate", "install", "ln",
        "apt", "yum", "dnf", "pip", "npm", "cargo", "vim", "vi", "nano", "reboot", "shutdown",
        "kill", "pkill", "killall",
    ];
    if BLOCKED.iter().any(|b| base == *b) {
        return Some(format!("blocked command: {base}"));
    }

    if base == "systemctl" || base == "service" {
        for token in segment.split_whitespace().skip(1) {
            let t = token.to_lowercase();
            if matches!(
                t.as_str(),
                "start" | "stop" | "restart" | "reload" | "enable" | "disable"
            ) {
                return Some(format!("service control: {t}"));
            }
        }
    }

    if base == "find"
        && tokens.iter().any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
            )
        })
    {
        return Some("find mutating or command execution".into());
    }

    if base == "curl" && curl_writes_or_mutates(segment) {
        return Some("curl mutating or output write".into());
    }

    let lower = segment.to_lowercase();
    if base == "sed" && lower.contains("-i") {
        return Some("in-place edit".into());
    }

    None
}

fn curl_writes_or_mutates(segment: &str) -> bool {
    let compact = segment
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact.contains("-xpost")
        || compact.contains("-xput")
        || compact.contains("-xdelete")
        || compact.contains("-xpatch")
    {
        return true;
    }
    let lower = segment.to_ascii_lowercase();
    if lower.contains("--request") {
        let rest = lower.split("--request").nth(1).unwrap_or("");
        let method = rest.split_whitespace().next().unwrap_or("");
        if matches!(
            method,
            "post" | "put" | "delete" | "patch" | "connect" | "trace"
        ) {
            return true;
        }
    }
    for token in tokenize_shell_words(segment).iter().skip(1) {
        let t = token.to_ascii_lowercase();
        if t == "-d" || t == "--data" || t == "--data-raw" || t == "--data-binary" {
            return true;
        }
        if t == "-o" || t == "--output" {
            return true;
        }
        if let Some(path) = t.strip_prefix("-o") {
            if !path.is_empty() {
                return true;
            }
        }
        if let Some(path) = t.strip_prefix("--output=") {
            if !path.is_empty() {
                return true;
            }
        }
        if let Some(method) = t.strip_prefix("-x") {
            if matches!(
                method,
                "post" | "put" | "delete" | "patch" | "connect" | "trace"
            ) {
                return true;
            }
        }
    }
    false
}

fn wrapper_or_subshell_verdict(segment: &str) -> Option<ReadOnlyVerdict> {
    if let Some(inner) = extract_subshell_inner(segment) {
        return propagate_inner_verdict(&inner);
    }

    match extract_wrapper_script(segment) {
        None => None,
        Some(WrapperScript::Unparseable) => Some(ReadOnlyVerdict::Unparseable),
        Some(WrapperScript::WithoutScript) => Some(ReadOnlyVerdict::NotReadOnly {
            reason: "shell or interpreter wrapper".into(),
        }),
        Some(WrapperScript::Script(script)) => propagate_inner_verdict(&script),
    }
}

fn propagate_inner_verdict(inner: &str) -> Option<ReadOnlyVerdict> {
    match classify(inner) {
        ReadOnlyVerdict::ReadOnly => None,
        other => Some(other),
    }
}

enum WrapperScript {
    WithoutScript,
    Unparseable,
    Script(String),
}

fn extract_subshell_inner(segment: &str) -> Option<String> {
    let trimmed = segment.trim();
    if !trimmed.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut end_index = None;

    for (index, ch) in trimmed.char_indices() {
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if in_single_quote || in_double_quote {
            continue;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                end_index = Some(index);
                break;
            }
        }
    }

    let end = end_index?;
    if trimmed[end + ')'.len_utf8()..].trim().is_empty() {
        Some(trimmed[1..end].trim().to_string())
    } else {
        None
    }
}

fn extract_wrapper_script(segment: &str) -> Option<WrapperScript> {
    let tokens = tokenize_shell_words(segment);
    if tokens.is_empty() {
        return None;
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_ascii_lowercase();

    const SHELL_WRAPPERS: &[&str] = &["bash", "sh", "dash", "zsh", "fish", "ksh", "ash"];
    const INTERPRETERS: &[&str] = &["python", "python3", "perl", "node", "ruby", "php"];

    if base == "env" || base == "command" {
        let inner = tokens[1..].join(" ");
        return Some(if inner.is_empty() {
            WrapperScript::WithoutScript
        } else {
            WrapperScript::Script(inner)
        });
    }

    if SHELL_WRAPPERS.contains(&base.as_str()) {
        return Some(parse_script_flag(&tokens[1..]));
    }

    if INTERPRETERS.contains(&base.as_str()) {
        if let Some(script) = parse_script_flag_value(&tokens[1..]) {
            return Some(WrapperScript::Script(script));
        }
        return Some(WrapperScript::WithoutScript);
    }

    None
}

fn parse_script_flag(tokens: &[String]) -> WrapperScript {
    if let Some(script) = parse_script_flag_value(tokens) {
        WrapperScript::Script(script)
    } else if tokens.iter().any(|t| is_script_flag_token(t)) {
        WrapperScript::Unparseable
    } else {
        WrapperScript::WithoutScript
    }
}

fn is_script_flag_token(token: &str) -> bool {
    let t = token.to_ascii_lowercase();
    t == "-c" || t == "-e" || t == "--command" || t.starts_with("-c") || t.starts_with("-e")
}

fn parse_script_flag_value(tokens: &[String]) -> Option<String> {
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let lower = token.to_ascii_lowercase();
        if lower == "-c" || lower == "-e" || lower == "--command" {
            if let Some(next) = tokens.get(index + 1) {
                return Some(next.clone());
            }
            return None;
        }
        if let Some(rest) = lower.strip_prefix("-c") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
        if let Some(rest) = lower.strip_prefix("-e") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
        index += 1;
    }
    None
}

fn tokenize_shell_words(segment: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = segment.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(ch);
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(ch);
            continue;
        }
        if ch == '\\' && !in_single_quote {
            current.push(ch);
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        if ch.is_whitespace() && !in_single_quote && !in_double_quote {
            if !current.is_empty() {
                tokens.push(unquote_shell_word(&current));
                current.clear();
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(unquote_shell_word(&current));
    }
    tokens
}

fn unquote_shell_word(word: &str) -> String {
    if word.len() >= 2
        && ((word.starts_with('\'') && word.ends_with('\''))
            || (word.starts_with('"') && word.ends_with('"')))
    {
        word[1..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

fn normalize_shell_word(word: &str) -> String {
    let mut out = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = word.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if ch == '\\' && !in_single_quote {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        out.push(ch);
    }

    out
}

fn scan_outside_quotes(segment: &str, mut predicate: impl FnMut(&str) -> bool) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut index = 0;

    while index < segment.len() {
        let ch = segment[index..].chars().next().expect("valid utf8");
        if ch == '\\' && !in_single_quote {
            index += ch.len_utf8();
            if index < segment.len() {
                let next = segment[index..].chars().next().expect("valid utf8");
                index += next.len_utf8();
            }
            continue;
        }
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            index += ch.len_utf8();
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            index += ch.len_utf8();
            continue;
        }
        if !in_single_quote && !in_double_quote && predicate(&segment[index..]) {
            return true;
        }
        index += ch.len_utf8();
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_read_only(command: &str) {
        assert_eq!(classify(command), ReadOnlyVerdict::ReadOnly, "{command}");
        assert!(is_read_only(command));
    }

    fn assert_not_read_only(command: &str) {
        assert!(matches!(
            classify(command),
            ReadOnlyVerdict::NotReadOnly { .. }
        ));
        assert!(!is_read_only(command));
    }

    #[test]
    fn empty_command_is_unparseable() {
        assert_eq!(classify("   "), ReadOnlyVerdict::Unparseable);
    }

    #[test]
    fn read_only_status_commands() {
        assert_read_only("systemctl status nginx");
        assert_read_only("journalctl -u foo --no-pager");
        assert_read_only("ss -tlnp");
        assert_read_only("ps aux");
        assert_read_only("which foo");
    }

    #[test]
    fn read_only_file_inspection() {
        assert_read_only("cat /etc/hosts");
        assert_read_only("head -n 5 file.txt");
        assert_read_only("tail -f /var/log/syslog");
        assert_read_only("rg pattern src/");
        assert_read_only("grep -R foo .");
        assert_read_only("find . -name '*.rs'");
    }

    #[test]
    fn read_only_resource_commands() {
        assert_read_only("df -h");
        assert_read_only("free -m");
        assert_read_only("curl -s https://example.com");
    }

    #[test]
    fn blocks_write_redirect() {
        assert_not_read_only("echo x > /tmp/foo");
        assert_not_read_only("echo x >> /tmp/foo");
    }

    #[test]
    fn allows_redirect_to_dev_null() {
        assert_read_only("cmd 2>/dev/null");
        assert_read_only("cmd >>/dev/null");
    }

    #[test]
    fn quote_redirect_is_not_write() {
        assert_read_only(r#"echo 'foo > bar'"#);
        assert_read_only(r#"echo "foo > bar""#);
    }

    #[test]
    fn blocks_destructive_commands() {
        assert_not_read_only("rm -rf /");
        assert_not_read_only("mv a b");
        assert_not_read_only("cp a b");
        assert_not_read_only("touch /tmp/x");
        assert_not_read_only("chmod +x script.sh");
    }

    #[test]
    fn blocks_service_control() {
        assert_not_read_only("systemctl restart nginx");
        assert_not_read_only("service nginx restart");
    }

    #[test]
    fn blocks_compound_if_any_segment_writes() {
        assert_not_read_only("systemctl status nginx && rm x");
        assert_not_read_only("true; echo x > /tmp/a");
        assert_not_read_only("cat file || systemctl restart nginx");
        assert_not_read_only("ls | tee out.txt");
    }

    #[test]
    fn compound_all_read_only() {
        assert_read_only("systemctl status nginx && journalctl -u nginx --no-pager");
        assert_read_only("ps aux | grep nginx");
    }

    #[test]
    fn sudo_stripped_before_analysis() {
        assert_read_only("sudo systemctl status nginx");
        assert_not_read_only("sudo rm -rf /");
    }

    #[test]
    fn sudo_in_later_compound_segment_is_stripped() {
        assert_not_read_only("ls && sudo rm -rf /tmp/x");
        assert_not_read_only("true; sudo touch /tmp/x");
    }

    #[test]
    fn escaped_quotes_do_not_hide_writes_or_separators() {
        assert_not_read_only(r"echo \' > /tmp/a");
        assert_not_read_only(r"echo \' ; rm -rf /tmp/a");
    }

    #[test]
    fn blocks_command_substitution() {
        assert_not_read_only("echo $(rm -rf /)");
        assert_not_read_only("echo `rm -rf /`");
    }

    #[test]
    fn blocks_unquoted_glob() {
        assert_not_read_only("ls *.txt");
    }

    #[test]
    fn blocks_package_managers() {
        assert_not_read_only("apt update");
        assert_not_read_only("npm install foo");
        assert_not_read_only("cargo build");
    }

    #[test]
    fn blocks_shell_wrapper_commands() {
        assert_not_read_only("bash -c 'rm -rf /'");
        assert_not_read_only("sh -c 'rm -rf /'");
        assert_read_only("bash -c 'systemctl status nginx'");
        assert_not_read_only("env rm -rf /tmp/x");
        assert_not_read_only("python3 -c 'rm -rf /tmp/x'");
    }

    #[test]
    fn blocks_background_job_segments() {
        assert_not_read_only("sleep 1 & rm -rf /tmp/x");
    }

    #[test]
    fn blocks_trailing_background_operator() {
        assert_not_read_only("sleep 9999 &");
    }

    #[test]
    fn blocks_find_exec_and_ok() {
        assert_not_read_only("find . -exec rm {} \\;");
        assert_not_read_only("find . -ok rm {} \\;");
    }

    #[test]
    fn blocks_escaped_command_names() {
        assert_not_read_only(r"r\m -rf /tmp/x");
        assert_not_read_only(r"'r'm -rf /tmp/x");
    }

    #[test]
    fn blocks_process_substitution() {
        assert_not_read_only("cat <(rm -rf /tmp/x)");
    }

    #[test]
    fn blocks_subshell_commands() {
        assert_not_read_only("( rm -rf /tmp/x )");
    }

    #[test]
    fn allows_read_only_sed_without_inplace_edit() {
        assert_read_only("sed -n '1,20p' file");
    }

    #[test]
    fn blocks_curl_compact_mutators_and_output() {
        assert_not_read_only("curl -XPOST https://example.com");
        assert_not_read_only("curl -o/tmp/x https://example.com");
        assert_not_read_only("curl --request POST https://example.com");
    }

    #[test]
    fn blocks_sed_inplace() {
        assert_not_read_only("sed -i 's/a/b/' file");
    }

    #[test]
    fn blocks_find_delete() {
        assert_not_read_only("find . -name '*.tmp' -delete");
    }

    #[test]
    fn blocks_curl_write() {
        assert_not_read_only("curl -o /tmp/x https://example.com");
        assert_not_read_only("curl -X POST https://example.com");
    }

    #[test]
    fn split_respects_quotes_for_separators() {
        let segments = split_compound_segments(r#"echo 'a;b' && ls"#);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].contains("'a;b'"));
    }

    #[test]
    fn preflight_skips_when_not_enforced() {
        assert!(preflight_enforce("rm -rf /", false).is_none());
    }

    #[test]
    fn preflight_blocks_when_enforced() {
        assert!(matches!(
            preflight_enforce("rm -rf /", true),
            Some(PreflightResult::Block { .. })
        ));
    }

    #[test]
    fn preflight_allows_read_only_when_enforced() {
        assert!(preflight_enforce("systemctl status nginx", true).is_none());
    }
}
