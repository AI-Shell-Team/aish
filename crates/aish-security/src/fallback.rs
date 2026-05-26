use std::collections::BTreeSet;
use std::path::{Component, Path};

use crate::decision::RiskLevel;
use crate::policy::{PolicyRule, SecurityPolicy};
use crate::sudo::strip_sudo_prefix;
use crate::types::MatchedRuleSummary;

const SHELL_WRAPPERS: &[&str] = &["bash", "sh"];
const WRAPPER_OPTIONS: &[&str] = &["-c", "-lc", "-cl"];
const CONTROL_TOKENS: &[&str] = &[";", "&&", "||", "|", "&", "(", ")", "<", ">", ">>", "<<"];
const META_CHARS: &[char] = &['$', '~', '`'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackRuleAssessment {
    pub level: RiskLevel,
    pub reasons: Vec<String>,
    pub matched_paths: Vec<String>,
    pub matched_rule: MatchedRuleSummary,
    pub primary_rule: PolicyRule,
}

#[derive(Debug, Clone)]
pub struct FallbackRuleEngine {
    policy: SecurityPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCommand {
    command_name: String,
    paths: Vec<String>,
}

impl FallbackRuleEngine {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }

    pub fn assess_disabled_command(&self, command: &str) -> Option<FallbackRuleAssessment> {
        let parsed = self.parse_policy_command(command)?;

        let mut hits = Vec::new();
        for path in &parsed.paths {
            if let Some(rule) = self.match_rule(&parsed.command_name, path) {
                hits.push((rule, path.clone()));
            }
        }

        if hits.is_empty() {
            return None;
        }

        let level = highest_risk(hits.iter().map(|(rule, _)| rule.risk))?;
        let selected_hits: Vec<(PolicyRule, String)> = hits
            .into_iter()
            .filter(|(rule, _)| rule.risk == level)
            .collect();

        let primary_rule = selected_hits.first()?.0.clone();
        let preview_paths = selected_hits
            .iter()
            .take(3)
            .map(|(_, path)| path.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let mut reasons = vec![format!(
            "policy fallback rule matched for command '{}' with {} {} risk path(s)",
            parsed.command_name,
            selected_hits.len(),
            level.as_str().to_ascii_lowercase()
        )];
        if !preview_paths.is_empty() {
            reasons.push(format!("matched paths: {}", preview_paths));
        }

        Some(FallbackRuleAssessment {
            level,
            reasons,
            matched_paths: selected_hits.iter().map(|(_, path)| path.clone()).collect(),
            matched_rule: MatchedRuleSummary {
                id: primary_rule.rule_id.clone(),
                name: primary_rule.name.clone(),
                pattern: primary_rule.pattern.clone(),
            },
            primary_rule,
        })
    }

    fn parse_policy_command(&self, command: &str) -> Option<ParsedCommand> {
        let (stripped_command, _sudo_detected, ok) = strip_sudo_prefix(command);
        if !ok {
            return None;
        }

        let mut argv = split_shell_like(&stripped_command)?;
        if argv.is_empty() {
            return None;
        }

        if let Some(wrapper_script) = extract_wrapper_script(&argv) {
            argv = split_simple_script(&wrapper_script)?;
            if argv.is_empty() {
                return None;
            }
        }

        let command_name = normalize_command_name(argv.first()?)?;
        let supported_commands = self.policy_command_list();
        if supported_commands.is_empty() || !supported_commands.contains(&command_name) {
            return None;
        }

        let paths = extract_paths(&argv);
        if paths.is_empty() {
            return None;
        }

        Some(ParsedCommand {
            command_name,
            paths,
        })
    }

    fn policy_command_list(&self) -> BTreeSet<String> {
        let mut commands = BTreeSet::new();

        for rule in &self.policy.rules {
            let Some(command_list) = &rule.command_list else {
                continue;
            };
            for command in command_list {
                if let Some(name) = normalize_command_name(command) {
                    commands.insert(name);
                }
            }
        }

        commands
    }

    fn match_rule(&self, command_name: &str, path: &str) -> Option<PolicyRule> {
        for rule in &self.policy.rules {
            if !command_in_rule(command_name, rule.command_list.as_ref()) {
                continue;
            }
            if !path_matches(path, &rule.pattern) {
                continue;
            }
            if rule
                .exclude
                .as_ref()
                .is_some_and(|patterns| patterns.iter().any(|pattern| path_matches(path, pattern)))
            {
                continue;
            }
            return Some(rule.clone());
        }

        None
    }
}

pub(crate) fn extract_policy_command_name(command: &str) -> Option<String> {
    let (stripped_command, _sudo_detected, ok) = strip_sudo_prefix(command);
    if !ok {
        return None;
    }

    let mut argv = split_shell_like(&stripped_command)?;
    if argv.is_empty() {
        return None;
    }

    if let Some(wrapper_script) = extract_wrapper_script(&argv) {
        argv = split_simple_script(&wrapper_script)?;
        if argv.is_empty() {
            return None;
        }
    }

    normalize_command_name(argv.first()?)
}

pub(crate) fn rule_accepts_command(
    command_name: Option<&str>,
    command_list: Option<&BTreeSet<String>>,
) -> bool {
    let Some(command_list) = command_list else {
        return true;
    };
    if command_list.is_empty() {
        return true;
    }
    let Some(command_name) = command_name else {
        return false;
    };

    command_list.iter().any(|command| {
        normalize_command_name(command)
            .as_ref()
            .is_some_and(|normalized| normalized == command_name)
    })
}

fn highest_risk(levels: impl IntoIterator<Item = RiskLevel>) -> Option<RiskLevel> {
    levels.into_iter().max_by_key(|level| match level {
        RiskLevel::Low => 1,
        RiskLevel::Medium => 2,
        RiskLevel::High => 3,
    })
}

fn extract_wrapper_script(argv: &[String]) -> Option<String> {
    let command_name = normalize_command_name(argv.first()?)?;
    if !SHELL_WRAPPERS.contains(&command_name.as_str()) {
        return None;
    }

    let mut index = 1;
    while index < argv.len() {
        let arg = &argv[index];
        if WRAPPER_OPTIONS.contains(&arg.as_str()) {
            return argv.get(index + 1).cloned();
        }
        index += 1;
    }

    None
}

fn split_shell_like(command: &str) -> Option<Vec<String>> {
    tokenize_shell(command, false)
}

fn split_simple_script(script: &str) -> Option<Vec<String>> {
    let tokens = tokenize_shell(script, true)?;
    if tokens.is_empty()
        || tokens
            .iter()
            .any(|token| CONTROL_TOKENS.contains(&token.as_str()))
    {
        return None;
    }
    Some(tokens)
}

fn tokenize_shell(input: &str, punctuation_as_tokens: bool) -> Option<Vec<String>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while index < chars.len() {
        let ch = chars[index];

        if !in_single_quote && !in_double_quote {
            if ch.is_whitespace() {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                index += 1;
                continue;
            }

            if punctuation_as_tokens {
                if let Some(operator) = read_operator(&chars, index) {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                    index += operator.chars().count();
                    tokens.push(operator);
                    continue;
                }
            }
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            index += 1;
            continue;
        }

        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            index += 1;
            continue;
        }

        if ch == '\\' && !in_single_quote {
            index += 1;
            let next = chars.get(index)?;
            current.push(*next);
            index += 1;
            continue;
        }

        current.push(ch);
        index += 1;
    }

    if in_single_quote || in_double_quote {
        return None;
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    Some(tokens)
}

fn read_operator(chars: &[char], index: usize) -> Option<String> {
    let ch = *chars.get(index)?;
    match ch {
        '&' | '|' | '>' | '<' => {
            let next = chars.get(index + 1).copied();
            if matches!(
                (ch, next),
                ('&', Some('&')) | ('|', Some('|')) | ('>', Some('>')) | ('<', Some('<'))
            ) {
                return Some([ch, next.expect("checked above")].iter().collect());
            }
            Some(ch.to_string())
        }
        ';' | '(' | ')' => Some(ch.to_string()),
        _ => None,
    }
}

fn extract_paths(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }

    let mut paths = Vec::new();
    let mut options_ended = false;

    for token in argv.iter().skip(1) {
        if !options_ended && token == "--" {
            options_ended = true;
            continue;
        }

        if !options_ended && token.starts_with('-') {
            continue;
        }

        if is_explicit_absolute_path(token) {
            paths.push(normalize_path(token));
        }
    }

    paths
}

fn command_in_rule(command_name: &str, command_list: Option<&BTreeSet<String>>) -> bool {
    rule_accepts_command(Some(command_name), command_list)
}

fn normalize_command_name(value: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }

    let name = Path::new(text)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(text);

    Some(name.to_ascii_lowercase())
}

fn normalize_path(value: &str) -> String {
    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(segment) => parts.push(segment.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
        }
    }

    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn is_explicit_absolute_path(token: &str) -> bool {
    token.starts_with('/') && !token.chars().any(|ch| META_CHARS.contains(&ch))
}

fn path_matches(path: &str, pattern: &str) -> bool {
    if wildcard_match(pattern, path) {
        return true;
    }

    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{}/", prefix));
    }

    false
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    let mut dp = vec![vec![false; text_chars.len() + 1]; pattern_chars.len() + 1];
    dp[0][0] = true;

    for i in 1..=pattern_chars.len() {
        if pattern_chars[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }

    for i in 1..=pattern_chars.len() {
        for j in 1..=text_chars.len() {
            dp[i][j] = match pattern_chars[i - 1] {
                '*' => dp[i - 1][j] || dp[i][j - 1],
                '?' => dp[i - 1][j - 1],
                ch => dp[i - 1][j - 1] && ch == text_chars[j - 1],
            };
        }
    }

    dp[pattern_chars.len()][text_chars.len()]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::FallbackRuleEngine;
    use crate::decision::RiskLevel;
    use crate::policy::{PolicyRule, SecurityPolicy};

    #[test]
    fn matches_simple_delete_command_against_policy_rule() {
        let engine = FallbackRuleEngine::new(SecurityPolicy {
            rules: vec![PolicyRule {
                pattern: "/etc/**".to_string(),
                risk: RiskLevel::High,
                operations: Some(BTreeSet::from(["DELETE".to_string()])),
                command_list: Some(BTreeSet::from(["rm".to_string()])),
                exclude: None,
                rule_id: Some("H-001".to_string()),
                name: Some("protect etc".to_string()),
                description: None,
                reason: Some("system config is protected".to_string()),
                confirm_message: None,
                suggestion: None,
            }],
            ..SecurityPolicy::default()
        });

        let assessment = engine
            .assess_disabled_command("rm -rf /etc")
            .expect("should match");

        assert_eq!(assessment.level, RiskLevel::High);
        assert_eq!(assessment.matched_rule.id.as_deref(), Some("H-001"));
        assert_eq!(assessment.matched_paths, vec!["/etc"]);
    }

    #[test]
    fn matches_shell_wrapper_simple_script() {
        let engine = test_engine();

        let assessment = engine
            .assess_disabled_command("bash -lc 'rm -rf /etc'")
            .expect("should match wrapped script");

        assert_eq!(assessment.level, RiskLevel::High);
        assert_eq!(assessment.matched_paths, vec!["/etc"]);
    }

    #[test]
    fn rejects_complex_shell_control_flow() {
        let engine = test_engine();

        let assessment = engine.assess_disabled_command("bash -lc 'echo ok; rm -rf /etc'");

        assert!(assessment.is_none());
    }

    #[test]
    fn supports_sudo_prefixed_wrapper_commands() {
        let engine = test_engine();

        let assessment = engine
            .assess_disabled_command("sudo -E -u root bash -lc 'rm -rf /etc'")
            .expect("should match sudo wrapper command");

        assert_eq!(assessment.level, RiskLevel::High);
        assert_eq!(assessment.matched_paths, vec!["/etc"]);
    }

    #[test]
    fn matches_policy_command_list_without_hardcoding_rm() {
        let engine = FallbackRuleEngine::new(SecurityPolicy {
            rules: vec![PolicyRule {
                pattern: "/home/**".to_string(),
                risk: RiskLevel::Medium,
                operations: Some(BTreeSet::from(["WRITE".to_string()])),
                command_list: Some(BTreeSet::from(["cp".to_string()])),
                exclude: None,
                rule_id: Some("M-001".to_string()),
                name: Some("protect home".to_string()),
                description: None,
                reason: None,
                confirm_message: None,
                suggestion: None,
            }],
            ..SecurityPolicy::default()
        });

        let assessment = engine
            .assess_disabled_command("cp /tmp/a.txt /home/lixin/a.txt")
            .expect("should match cp target path");

        assert_eq!(assessment.level, RiskLevel::Medium);
        assert_eq!(assessment.matched_paths, vec!["/home/lixin/a.txt"]);
    }

    #[test]
    fn preserves_wildcard_absolute_paths() {
        let engine = FallbackRuleEngine::new(SecurityPolicy {
            rules: vec![PolicyRule {
                pattern: "/home/**".to_string(),
                risk: RiskLevel::Medium,
                operations: Some(BTreeSet::from(["DELETE".to_string()])),
                command_list: Some(BTreeSet::from(["rm".to_string()])),
                exclude: None,
                rule_id: Some("M-002".to_string()),
                name: None,
                description: None,
                reason: None,
                confirm_message: None,
                suggestion: None,
            }],
            ..SecurityPolicy::default()
        });

        let assessment = engine
            .assess_disabled_command("rm -rf /home/lixin/testdir/*")
            .expect("should match wildcard path");

        assert_eq!(assessment.level, RiskLevel::Medium);
        assert_eq!(assessment.matched_paths, vec!["/home/lixin/testdir/*"]);
    }

    #[test]
    fn respects_exclude_patterns() {
        let engine = FallbackRuleEngine::new(SecurityPolicy {
            rules: vec![PolicyRule {
                pattern: "/etc/**".to_string(),
                risk: RiskLevel::High,
                operations: Some(BTreeSet::from(["DELETE".to_string()])),
                command_list: Some(BTreeSet::from(["rm".to_string()])),
                exclude: Some(vec!["/etc/ssl/**".to_string()]),
                rule_id: Some("H-002".to_string()),
                name: None,
                description: None,
                reason: None,
                confirm_message: None,
                suggestion: None,
            }],
            ..SecurityPolicy::default()
        });

        let assessment = engine.assess_disabled_command("rm -rf /etc/ssl/private/key.pem");

        assert!(assessment.is_none());
    }

    fn test_engine() -> FallbackRuleEngine {
        FallbackRuleEngine::new(SecurityPolicy {
            rules: vec![PolicyRule {
                pattern: "/etc/**".to_string(),
                risk: RiskLevel::High,
                operations: Some(BTreeSet::from(["DELETE".to_string()])),
                command_list: Some(BTreeSet::from(["rm".to_string()])),
                exclude: None,
                rule_id: Some("H-001".to_string()),
                name: Some("protect etc".to_string()),
                description: None,
                reason: None,
                confirm_message: None,
                suggestion: None,
            }],
            ..SecurityPolicy::default()
        })
    }
}
