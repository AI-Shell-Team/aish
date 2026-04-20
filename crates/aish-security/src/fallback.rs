//! Fallback rule engine for when the sandbox is disabled.
//!
//! Parses commands that modify/delete files and checks them against policy rules.

use crate::policy::SecurityPolicy;
use crate::types::PolicyRule;
use aish_core::RiskLevel;

/// Result of assessing a command against fallback rules.
#[derive(Debug, Clone)]
pub struct FallbackAssessment {
    pub level: RiskLevel,
    pub reasons: Vec<String>,
    pub matched_paths: Vec<String>,
}

/// A delete command parsed from user input.
#[derive(Debug, Clone)]
struct ParsedDeleteCommand {
    #[allow(dead_code)]
    command_name: String,
    paths: Vec<String>,
}

/// Fallback engine that checks commands against policy rules when sandbox is off.
#[derive(Default)]
pub struct FallbackRuleEngine {
    /// Delete-related command names to look for.
    delete_commands: Vec<&'static str>,
}

impl FallbackRuleEngine {
    pub fn new() -> Self {
        Self {
            delete_commands: vec!["rm", "rmdir", "unlink", "truncate", "shred", "mv"],
        }
    }

    /// Assess a command against fallback rules.
    /// Returns None if the command is not a recognized destructive operation.
    pub fn assess(&self, command: &str, policy: &SecurityPolicy) -> Option<FallbackAssessment> {
        let parsed = self.parse_delete_command(command)?;

        let mut hits: Vec<(&PolicyRule, String)> = Vec::new();
        for path in &parsed.paths {
            for rule in &policy.rules {
                if self.path_matches_rule(path, rule) {
                    hits.push((rule, path.clone()));
                }
            }
        }

        if hits.is_empty() {
            return None;
        }

        let max_risk = hits
            .iter()
            .map(|(r, _)| r.risk.clone())
            .max_by_key(|r| match r {
                RiskLevel::High => 3,
                RiskLevel::Medium => 2,
                RiskLevel::Low => 1,
            })
            .unwrap_or(RiskLevel::Medium);

        let reasons: Vec<String> = hits
            .iter()
            .map(|(r, p)| {
                format!(
                    "path '{}' matched rule '{}' ({} risk)",
                    p,
                    r.pattern,
                    match r.risk {
                        RiskLevel::High => "high",
                        RiskLevel::Medium => "medium",
                        RiskLevel::Low => "low",
                    }
                )
            })
            .collect();

        let matched_paths: Vec<String> = hits.iter().map(|(_, p)| p.clone()).collect();

        Some(FallbackAssessment {
            level: max_risk,
            reasons,
            matched_paths,
        })
    }

    /// Parse a command to extract the command name and file paths.
    fn parse_delete_command(&self, command: &str) -> Option<ParsedDeleteCommand> {
        let command = command.trim();

        // Strip sudo prefix
        let command = command
            .strip_prefix("sudo ")
            .or_else(|| command.strip_prefix("sudo"))
            .unwrap_or(command)
            .trim();

        // Handle shell wrappers like: bash -c "rm -rf /path"
        if let Some(stripped) = self.extract_wrapper(command) {
            return self.parse_delete_command(&stripped);
        }

        // Split into tokens
        let tokens: Vec<&str> = command.split_whitespace().collect();
        if tokens.is_empty() {
            return None;
        }

        let command_name = tokens[0]
            .rsplit('/')
            .next()
            .unwrap_or(tokens[0])
            .to_string();

        // Check if it's a recognized delete command
        if !self.delete_commands.iter().any(|dc| *dc == command_name) {
            return None;
        }

        // Extract paths: skip flags (tokens starting with -)
        let paths: Vec<String> = tokens[1..]
            .iter()
            .filter(|t| !t.starts_with('-') && !t.is_empty())
            .map(|t| t.to_string())
            .collect();

        if paths.is_empty() {
            return None;
        }

        Some(ParsedDeleteCommand {
            command_name,
            paths,
        })
    }

    /// Extract the inner command from shell wrappers like `bash -c "rm -rf /path"`.
    fn extract_wrapper(&self, command: &str) -> Option<String> {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        if tokens.len() >= 3 && tokens[0] == "bash" && tokens[1] == "-c" {
            let inner = tokens[2..].join(" ");
            let inner = inner.trim_matches('"').trim_matches('\'').to_string();
            if !inner.is_empty() {
                return Some(inner);
            }
        }
        None
    }

    /// Check if a file path matches a policy rule pattern.
    fn path_matches_rule(&self, path: &str, rule: &PolicyRule) -> bool {
        let pattern = &rule.pattern;

        // Exact match
        if path == pattern {
            return true;
        }

        // Glob-style matching: /etc/** matches any path under /etc/
        if let Some(prefix) = pattern.strip_suffix("/**") {
            if path.starts_with(prefix) || path == prefix.trim_end_matches('/') {
                return true;
            }
        }

        // Glob-style matching: /etc/* matches direct children
        if let Some(prefix) = pattern.strip_suffix("/*") {
            if path.starts_with(prefix) && !path[prefix.len()..].contains('/') {
                return true;
            }
        }

        // Prefix match
        if path.starts_with(pattern.trim_end_matches('/')) {
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PolicyRule;
    use aish_core::RiskLevel;

    fn make_policy(rules: Vec<PolicyRule>) -> SecurityPolicy {
        let mut policy = SecurityPolicy::default_policy();
        policy.rules = rules;
        policy
    }

    #[test]
    fn test_parse_rm_command() {
        let engine = FallbackRuleEngine::new();
        let parsed = engine.parse_delete_command("rm -rf /etc/passwd").unwrap();
        assert_eq!(parsed.command_name, "rm");
        assert!(parsed.paths.contains(&"/etc/passwd".to_string()));
    }

    #[test]
    fn test_parse_rm_multiple_paths() {
        let engine = FallbackRuleEngine::new();
        let parsed = engine
            .parse_delete_command("rm -f /etc/passwd /etc/shadow")
            .unwrap();
        assert_eq!(parsed.command_name, "rm");
        assert_eq!(parsed.paths.len(), 2);
    }

    #[test]
    fn test_parse_non_delete_command() {
        let engine = FallbackRuleEngine::new();
        assert!(engine.parse_delete_command("ls -la").is_none());
        assert!(engine.parse_delete_command("cat /etc/passwd").is_none());
    }

    #[test]
    fn test_parse_sudo_rm() {
        let engine = FallbackRuleEngine::new();
        let parsed = engine
            .parse_delete_command("sudo rm -rf /etc/important")
            .unwrap();
        assert_eq!(parsed.command_name, "rm");
        assert!(parsed.paths.contains(&"/etc/important".to_string()));
    }

    #[test]
    fn test_parse_bash_c_wrapper() {
        let engine = FallbackRuleEngine::new();
        let parsed = engine
            .parse_delete_command("bash -c \"rm -rf /data/backup\"")
            .unwrap();
        assert_eq!(parsed.command_name, "rm");
        assert!(parsed.paths.contains(&"/data/backup".to_string()));
    }

    #[test]
    fn test_assess_blocked_path() {
        let engine = FallbackRuleEngine::new();
        let policy = make_policy(vec![PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            description: Some("system files".to_string()),
            reason: Some("system configuration files".to_string()),
            ..Default::default()
        }]);
        let result = engine.assess("rm -f /etc/passwd", &policy).unwrap();
        assert_eq!(result.level, RiskLevel::High);
        assert!(!result.matched_paths.is_empty());
    }

    #[test]
    fn test_assess_allowed_path() {
        let engine = FallbackRuleEngine::new();
        let policy = make_policy(vec![PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            description: Some("system files".to_string()),
            ..Default::default()
        }]);
        let result = engine.assess("rm -f /tmp/junk", &policy);
        assert!(result.is_none());
    }

    #[test]
    fn test_assess_sudo_rm_blocked() {
        let engine = FallbackRuleEngine::new();
        let policy = make_policy(vec![PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            description: Some("system files".to_string()),
            ..Default::default()
        }]);
        let result = engine.assess("sudo rm -rf /etc/config", &policy).unwrap();
        assert_eq!(result.level, RiskLevel::High);
    }

    #[test]
    fn test_path_matches_glob() {
        let engine = FallbackRuleEngine::new();
        let rule = PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            ..Default::default()
        };
        assert!(engine.path_matches_rule("/etc/passwd", &rule));
        assert!(engine.path_matches_rule("/etc/nginx/conf.d/default.conf", &rule));
        assert!(!engine.path_matches_rule("/tmp/file", &rule));
    }
}
