use crate::types::{AiRiskAssessment, FsChange, PolicyRule, SandboxResult};
use aish_core::{AishError, RiskLevel, SandboxOffAction};
use regex::Regex;
use std::path::Path;

/// Serializable form used when loading the YAML config file.
#[derive(Debug, Clone, serde::Deserialize)]
struct PolicyFile {
    #[serde(default)]
    enable_sandbox: bool,
    #[serde(default = "default_sandbox_off_action")]
    sandbox_off_action: String,
    #[serde(default = "default_risk")]
    default_risk_level: String,
    #[serde(default = "default_timeout")]
    sandbox_timeout_seconds: f64,
    #[serde(default)]
    rules: Vec<PolicyRule>,
}

fn default_sandbox_off_action() -> String {
    "allow".to_string()
}
fn default_risk() -> String {
    "low".to_string()
}
fn default_timeout() -> f64 {
    10.0
}

#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub enable_sandbox: bool,
    pub rules: Vec<PolicyRule>,
    pub sandbox_off_action: SandboxOffAction,
    pub sandbox_timeout_seconds: f64,
    pub default_risk_level: RiskLevel,
}

impl SecurityPolicy {
    pub fn default_policy() -> Self {
        Self {
            enable_sandbox: false,
            rules: Vec::new(),
            sandbox_off_action: SandboxOffAction::Allow,
            sandbox_timeout_seconds: 10.0,
            default_risk_level: RiskLevel::Low,
        }
    }

    /// Load security policy from a YAML file.
    pub fn load(path: &Path) -> aish_core::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            AishError::Security(format!("failed to read policy file {:?}: {}", path, e))
        })?;

        let pf: PolicyFile = serde_yaml::from_str(&content)
            .map_err(|e| AishError::Security(format!("failed to parse policy YAML: {}", e)))?;

        let sandbox_off_action = parse_sandbox_off_action(&pf.sandbox_off_action)?;
        let default_risk_level = parse_risk_level(&pf.default_risk_level)?;

        Ok(Self {
            enable_sandbox: pf.enable_sandbox,
            rules: pf.rules,
            sandbox_off_action,
            sandbox_timeout_seconds: pf.sandbox_timeout_seconds,
            default_risk_level,
        })
    }

    /// Match a path against policy rules, returning the first matching rule.
    pub fn match_rule(&self, path: &str, operation: Option<&str>) -> Option<&PolicyRule> {
        for rule in &self.rules {
            if let Ok(re) = glob_to_regex(&rule.pattern) {
                if !re.is_match(path) {
                    continue;
                }
            } else {
                continue;
            }

            // Check operation filter
            if let Some(ops) = &rule.operations {
                if let Some(op) = operation {
                    if !ops.iter().any(|o| o.eq_ignore_ascii_case(op)) {
                        continue;
                    }
                } else {
                    // Rule requires specific operations but none provided
                    continue;
                }
            }

            // Check exclude list
            if let Some(excludes) = &rule.exclude {
                let excluded = excludes.iter().any(|exc| {
                    glob_to_regex(exc)
                        .map(|re| re.is_match(path))
                        .unwrap_or(false)
                });
                if excluded {
                    continue;
                }
            }

            return Some(rule);
        }
        None
    }

    /// Assess the risk of a command based on optional sandbox results.
    pub fn assess_risk(
        &self,
        command: &str,
        sandbox_result: Option<&SandboxResult>,
    ) -> AiRiskAssessment {
        let mut reasons: Vec<String> = Vec::new();
        let mut changes: Vec<FsChange> = Vec::new();
        let mut max_risk = self.default_risk_level.clone();

        // Inspect sandbox results for file changes
        if let Some(result) = sandbox_result {
            for change in &result.changes {
                changes.push(change.clone());

                if let Some(rule) = self.match_rule(&change.path, Some(&change.kind)) {
                    if matches!(rule.risk, RiskLevel::High) {
                        max_risk = RiskLevel::High;
                        reasons.push(format!(
                            "file change to {} is high risk ({})",
                            change.path,
                            rule.description.as_deref().unwrap_or(&rule.pattern)
                        ));
                    } else if matches!(rule.risk, RiskLevel::Medium)
                        && !matches!(max_risk, RiskLevel::High)
                    {
                        max_risk = RiskLevel::Medium;
                        reasons.push(format!(
                            "file change to {} is medium risk ({})",
                            change.path,
                            rule.description.as_deref().unwrap_or(&rule.pattern)
                        ));
                    }
                }
            }

            // Heuristics: many write operations bump risk
            if result.changes.len() > 5 {
                if !matches!(max_risk, RiskLevel::High) {
                    max_risk = RiskLevel::Medium;
                }
                reasons.push(format!(
                    "command modifies {} files, which is significant",
                    result.changes.len()
                ));
            }
        }

        // Inspect command text for dangerous patterns
        let cmd_lower = command.to_lowercase();
        let dangerous_patterns = [
            ("rm -rf /", "recursive root delete"),
            ("mkfs.", "filesystem format"),
            ("dd if=", "raw disk write"),
            (":(){ :|:& };:", "fork bomb"),
            ("> /dev/sd", "direct device write"),
        ];
        for (pat, desc) in &dangerous_patterns {
            if cmd_lower.contains(pat) {
                max_risk = RiskLevel::High;
                reasons.push(format!("command matches dangerous pattern: {}", desc));
            }
        }

        // System paths in command
        let system_paths = ["/etc/", "/boot/", "/usr/lib", "/sbin/"];
        for sp in &system_paths {
            if cmd_lower.contains(sp) {
                if !matches!(max_risk, RiskLevel::High) {
                    max_risk = RiskLevel::Medium;
                }
                reasons.push(format!("command references system path: {}", sp));
            }
        }

        if reasons.is_empty() {
            reasons.push("no elevated risk indicators detected".to_string());
        }

        AiRiskAssessment {
            level: max_risk,
            reasons,
            changes,
        }
    }
}

/// Convert a simple glob pattern to a Regex.
///
/// Rules:
/// - `**` -> `.*`  (match anything including `/`)
/// - `*`  -> `[^/]*` (match anything except `/`)
/// - `?`  -> `[^/]`  (single char except `/`)
/// - Other regex meta-characters are escaped.
fn glob_to_regex(pattern: &str) -> Result<Regex, regex::Error> {
    let mut regex_str = String::with_capacity(pattern.len() * 2);
    regex_str.push('^');

    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                // `**` matches anything including path separators
                regex_str.push_str(".*");
                i += 2;
                // Skip optional trailing `/` after `**`
                if i < chars.len() && chars[i] == '/' {
                    regex_str.push_str("/?");
                    i += 1;
                }
            }
            '*' => {
                regex_str.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                regex_str.push_str("[^/]");
                i += 1;
            }
            c => {
                regex_str.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }

    regex_str.push('$');
    Regex::new(&regex_str)
}

fn parse_sandbox_off_action(s: &str) -> aish_core::Result<SandboxOffAction> {
    match s.to_lowercase().as_str() {
        "allow" => Ok(SandboxOffAction::Allow),
        "confirm" => Ok(SandboxOffAction::Confirm),
        "block" => Ok(SandboxOffAction::Block),
        other => Err(AishError::Security(format!(
            "invalid sandbox_off_action: {}",
            other
        ))),
    }
}

fn parse_risk_level(s: &str) -> aish_core::Result<RiskLevel> {
    match s.to_lowercase().as_str() {
        "low" => Ok(RiskLevel::Low),
        "medium" => Ok(RiskLevel::Medium),
        "high" => Ok(RiskLevel::High),
        other => Err(AishError::Security(format!(
            "invalid risk level: {}",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_to_regex() {
        let re = glob_to_regex("/etc/**").unwrap();
        assert!(re.is_match("/etc/passwd"));
        assert!(re.is_match("/etc/nginx/nginx.conf"));
        assert!(!re.is_match("/home/user/file"));

        let re = glob_to_regex("/home/*").unwrap();
        assert!(re.is_match("/home/user"));
        assert!(!re.is_match("/home/user/file"));

        let re = glob_to_regex("/tmp/**").unwrap();
        assert!(re.is_match("/tmp/test.txt"));
        assert!(re.is_match("/tmp/a/b/c"));
    }

    #[test]
    fn test_match_rule() {
        let policy = SecurityPolicy::default_policy();
        assert!(policy.match_rule("/any/path", None).is_none());
    }

    #[test]
    fn test_default_policy() {
        let p = SecurityPolicy::default_policy();
        assert!(!p.enable_sandbox);
        assert!(p.rules.is_empty());
        assert_eq!(p.sandbox_timeout_seconds, 10.0);
    }
}
