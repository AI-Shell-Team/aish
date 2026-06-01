use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::decision::{RiskLevel, SandboxOffAction};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidFallbackRule {
    pub rule_id: String,
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub pattern: String,
    pub risk: RiskLevel,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operations: Option<BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_list: Option<BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityPolicy {
    pub enable_sandbox: bool,
    pub sandbox_off_action: SandboxOffAction,
    pub sandbox_timeout_seconds: f64,
    pub default_risk_level: RiskLevel,
    pub audit_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_log_path: Option<String>,
    pub rules: Vec<PolicyRule>,
    #[serde(default)]
    pub invalid_fallback_rules: Vec<InvalidFallbackRule>,
    #[serde(default)]
    pub validation_issues: Vec<ValidationIssue>,
    #[serde(default)]
    pub secret_patterns: Vec<crate::secret::CustomPattern>,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            enable_sandbox: false,
            sandbox_off_action: SandboxOffAction::Allow,
            sandbox_timeout_seconds: 10.0,
            default_risk_level: RiskLevel::Low,
            audit_enabled: false,
            audit_log_path: None,
            rules: default_rules(),
            invalid_fallback_rules: Vec::new(),
            validation_issues: Vec::new(),
            secret_patterns: Vec::new(),
        }
    }
}

const EMPTY_POLICY_TEMPLATE: &str = "# AI-Shell Security Policy\n\
\n\
global:\n\
  default_risk_level: LOW\n\
  enable_sandbox: false\n\
  sandbox_off_action: ALLOW\n\
  sandbox_timeout_seconds: 10\n\
\n\
rules: []\n";

fn user_security_policy_path() -> PathBuf {
    let base_dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
            home.push(".config");
            home
        });

    base_dir.join("aish").join("security_policy.yaml")
}

fn ensure_user_policy_template(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|_| fs::write(path, EMPTY_POLICY_TEMPLATE));
}

pub fn resolve_security_policy_path(config_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = config_path {
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    let system_path = PathBuf::from("/etc/aish/security_policy.yaml");
    if system_path.exists() {
        return Some(system_path);
    }

    let user_path = user_security_policy_path();
    if !user_path.exists() {
        ensure_user_policy_template(&user_path);
    }
    if user_path.exists() {
        return Some(user_path);
    }

    None
}

fn mapping_get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_string()))
}

fn as_mapping(value: Option<&Value>) -> Option<&Mapping> {
    value.and_then(Value::as_mapping)
}

fn ensure_list(value: Option<&Value>) -> Vec<&Value> {
    match value {
        None => Vec::new(),
        Some(Value::Sequence(items)) => items.iter().collect(),
        Some(other) => vec![other],
    }
}

fn parse_risk(value: Option<&Value>, default: RiskLevel) -> RiskLevel {
    let Some(value) = value else {
        return default;
    };

    let Some(text) = value.as_str() else {
        return default;
    };

    match text.trim().to_ascii_uppercase().as_str() {
        "LOW" => RiskLevel::Low,
        "MEDIUM" => RiskLevel::Medium,
        "HIGH" => RiskLevel::High,
        _ => default,
    }
}

fn parse_sandbox_off_action(value: Option<&Value>) -> Option<SandboxOffAction> {
    let text = value?.as_str()?.trim().to_ascii_uppercase();
    match text.as_str() {
        "ALLOW" => Some(SandboxOffAction::Allow),
        "CONFIRM" => Some(SandboxOffAction::Confirm),
        "BLOCK" => Some(SandboxOffAction::Block),
        _ => None,
    }
}

fn parse_bool_strict(value: Option<&Value>, default: bool) -> bool {
    match value {
        None => default,
        Some(Value::Bool(flag)) => *flag,
        _ => default,
    }
}

fn parse_float_gt_zero(value: Option<&Value>, default: f64) -> f64 {
    let Some(value) = value else {
        return default;
    };

    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    };

    match parsed {
        Some(number) if number > 0.0 => number,
        _ => default,
    }
}

fn normalize_string_set(values: Vec<&Value>, uppercase: bool) -> Option<BTreeSet<String>> {
    let mut result = BTreeSet::new();

    for value in values {
        let Some(text) = value.as_str() else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let normalized = if uppercase {
            text.to_ascii_uppercase()
        } else {
            text.to_string()
        };
        result.insert(normalized);
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn normalize_string_list(values: Vec<&Value>) -> Option<Vec<String>> {
    let mut result = Vec::new();

    for value in values {
        let Some(text) = value.as_str() else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        result.push(text.to_string());
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn parse_v2_rules(raw_rules: &[&Mapping]) -> Vec<PolicyRule> {
    let mut rules = Vec::with_capacity(raw_rules.len());

    for item in raw_rules {
        let patterns = normalize_string_list(ensure_list(mapping_get(item, "path")));
        let Some(patterns) = patterns else {
            continue;
        };

        let risk = parse_risk(mapping_get(item, "risk"), RiskLevel::Low);
        let operations = normalize_string_set(ensure_list(mapping_get(item, "operations")), true);
        let command_list =
            normalize_string_set(ensure_list(mapping_get(item, "command_list")), false);
        let exclude = normalize_string_list(ensure_list(mapping_get(item, "exclude")));
        let description = mapping_get(item, "description")
            .and_then(Value::as_str)
            .map(str::to_string);
        let rule_id = mapping_get(item, "id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let name = mapping_get(item, "name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let reason = mapping_get(item, "reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let confirm_message = mapping_get(item, "confirm_message")
            .and_then(Value::as_str)
            .map(str::to_string);
        let suggestion = mapping_get(item, "suggestion")
            .and_then(Value::as_str)
            .map(str::to_string);

        for pattern in patterns {
            rules.push(PolicyRule {
                pattern,
                risk,
                description: description.clone(),
                operations: operations.clone(),
                command_list: command_list.clone(),
                exclude: exclude.clone(),
                rule_id: rule_id.clone(),
                name: name.clone(),
                reason: reason.clone(),
                confirm_message: confirm_message.clone(),
                suggestion: suggestion.clone(),
            });
        }
    }

    rules
}

fn parse_invalid_fallback_rules(
    raw_rules: &[&Mapping],
) -> (Vec<InvalidFallbackRule>, Vec<ValidationIssue>) {
    // Pre-allocate: estimate up to half of rules may be invalid
    let mut invalid_rules = Vec::with_capacity(raw_rules.len() / 2);
    let mut issues = Vec::with_capacity(raw_rules.len() / 2);

    for item in raw_rules {
        let patterns = normalize_string_list(ensure_list(mapping_get(item, "path")));
        let Some(patterns) = patterns else {
            continue;
        };

        let risk_value = mapping_get(item, "risk");
        let is_valid = matches!(
            parse_risk(risk_value, RiskLevel::Low),
            RiskLevel::Low | RiskLevel::Medium | RiskLevel::High
        ) && risk_value.and_then(Value::as_str).is_some_and(|text| {
            matches!(
                text.trim().to_ascii_uppercase().as_str(),
                "LOW" | "MEDIUM" | "HIGH"
            )
        });

        if is_valid {
            continue;
        }

        let rule_id = mapping_get(item, "id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let risk_text = risk_value.and_then(Value::as_str).map(str::to_string);
        let exclude = normalize_string_list(ensure_list(mapping_get(item, "exclude")));

        issues.push(ValidationIssue {
            rule_id: rule_id.clone(),
            field: "risk".to_string(),
            value: risk_text,
            message: Some("invalid rule ignored".to_string()),
        });

        if let Some(rule_id) = rule_id {
            for pattern in patterns {
                invalid_rules.push(InvalidFallbackRule {
                    rule_id: rule_id.clone(),
                    pattern,
                    exclude: exclude.clone(),
                });
            }
        }
    }

    (invalid_rules, issues)
}

pub fn load_policy(config_path: Option<&Path>) -> SecurityPolicy {
    let Some(effective_path) = resolve_security_policy_path(config_path) else {
        return SecurityPolicy::default();
    };

    let Ok(text) = fs::read_to_string(&effective_path) else {
        return SecurityPolicy::default();
    };
    let Ok(data) = serde_yaml::from_str::<Value>(&text) else {
        return SecurityPolicy::default();
    };

    let root = data.as_mapping();
    let global_cfg = root.and_then(|m| as_mapping(mapping_get(m, "global")));
    let audit_cfg = root.and_then(|m| as_mapping(mapping_get(m, "audit")));

    let default_risk_level = parse_risk(
        global_cfg.and_then(|m| mapping_get(m, "default_risk_level")),
        RiskLevel::Low,
    );

    let enable_sandbox = parse_bool_strict(
        global_cfg.and_then(|m| mapping_get(m, "enable_sandbox")),
        false,
    );

    let mut sandbox_off_action =
        parse_sandbox_off_action(global_cfg.and_then(|m| mapping_get(m, "sandbox_off_action")))
            .unwrap_or(SandboxOffAction::Allow);

    let sandbox_timeout_seconds = parse_float_gt_zero(
        global_cfg.and_then(|m| mapping_get(m, "sandbox_timeout_seconds")),
        10.0,
    );

    if global_cfg
        .and_then(|m| mapping_get(m, "sandbox_off_action"))
        .is_none()
    {
        if let Some(legacy) = global_cfg.and_then(|m| mapping_get(m, "sandbox_fallback_risk")) {
            sandbox_off_action = match parse_risk(Some(legacy), RiskLevel::Medium) {
                RiskLevel::High => SandboxOffAction::Block,
                RiskLevel::Medium => SandboxOffAction::Confirm,
                RiskLevel::Low => SandboxOffAction::Allow,
            };
        }
    }

    let audit_enabled = parse_bool_strict(audit_cfg.and_then(|m| mapping_get(m, "enabled")), false);
    let audit_log_path = audit_cfg
        .and_then(|m| mapping_get(m, "log_path"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let rule_mappings: Vec<&Mapping> = root
        .and_then(|m| mapping_get(m, "rules"))
        .and_then(Value::as_sequence)
        .map(|seq| seq.iter().filter_map(Value::as_mapping).collect())
        .unwrap_or_default();

    let v2_items: Vec<&Mapping> = rule_mappings
        .into_iter()
        .filter(|item| mapping_get(item, "path").is_some())
        .collect();

    let (invalid_fallback_rules, mut validation_issues) = parse_invalid_fallback_rules(&v2_items);

    let valid_items: Vec<&Mapping> = v2_items
        .iter()
        .copied()
        .filter(|item| {
            mapping_get(item, "risk")
                .and_then(Value::as_str)
                .is_some_and(|text| {
                    matches!(
                        text.trim().to_ascii_uppercase().as_str(),
                        "LOW" | "MEDIUM" | "HIGH"
                    )
                })
        })
        .collect();

    let mut rules = default_rules();
    rules.extend(parse_v2_rules(&valid_items));

    let secret_patterns: Vec<crate::secret::CustomPattern> = root
        .and_then(|m| mapping_get(m, "secret_patterns"))
        .and_then(Value::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(|v| {
                    let yaml_str = serde_yaml::to_string(v).unwrap_or_default();
                    match serde_yaml::from_value(v.clone()) {
                        Ok(pattern) => Some(pattern),
                        Err(e) => {
                            validation_issues.push(ValidationIssue {
                                rule_id: None,
                                field: "secret_patterns".to_string(),
                                value: Some(yaml_str.trim().to_string()),
                                message: Some(format!("Invalid custom pattern: {e}")),
                            });
                            None
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    SecurityPolicy {
        enable_sandbox,
        sandbox_off_action,
        sandbox_timeout_seconds,
        default_risk_level,
        audit_enabled,
        audit_log_path,
        rules,
        invalid_fallback_rules,
        validation_issues,
        secret_patterns,
    }
}

pub fn default_rules() -> Vec<PolicyRule> {
    vec![PolicyRule {
        pattern: "/**/security_policy.yaml".to_string(),
        risk: RiskLevel::High,
        description: Some("Security policy file is protected".to_string()),
        operations: Some(BTreeSet::from(["WRITE".to_string(), "DELETE".to_string()])),
        command_list: None,
        exclude: None,
        rule_id: Some("H-SEC-001".to_string()),
        name: Some("Protect security policy".to_string()),
        reason: Some("Security policy file should not be modified by AI commands".to_string()),
        confirm_message: Some(
            "Security policy file is protected and cannot be modified by AI commands.".to_string(),
        ),
        suggestion: Some("Edit the security policy file manually if needed.".to_string()),
    }]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{load_policy, resolve_security_policy_path, SecurityPolicy};
    use crate::decision::{RiskLevel, SandboxOffAction};

    #[test]
    fn security_policy_default_matches_phase_one_expectations() {
        let policy = SecurityPolicy::default();

        assert!(!policy.enable_sandbox);
        assert_eq!(policy.sandbox_off_action, SandboxOffAction::Allow);
        assert_eq!(policy.sandbox_timeout_seconds, 10.0);
        assert_eq!(policy.default_risk_level, RiskLevel::Low);
        assert_eq!(policy.rules.len(), 1);
    }

    #[test]
    fn resolve_security_policy_path_prefers_explicit_path() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\nrules: []\n",
        )
        .unwrap();

        let resolved = resolve_security_policy_path(Some(&policy_path));

        assert_eq!(resolved.as_deref(), Some(policy_path.as_path()));
    }

    #[test]
    fn load_policy_parses_global_and_rule_fields() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\n  sandbox_off_action: BLOCK\n  sandbox_timeout_seconds: 15\n  default_risk_level: MEDIUM\nrules:\n  - id: H-001\n    name: Protect etc\n    command_list: [rm]\n    path: [/etc/**]\n    operations: [WRITE, DELETE]\n    risk: HIGH\n    reason: do not touch etc\n",
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        assert!(!policy.enable_sandbox);
        assert_eq!(policy.sandbox_off_action, SandboxOffAction::Block);
        assert_eq!(policy.sandbox_timeout_seconds, 15.0);
        assert_eq!(policy.default_risk_level, RiskLevel::Medium);
        assert!(policy
            .rules
            .iter()
            .any(|rule| rule.rule_id.as_deref() == Some("H-001")));
    }

    #[test]
    fn load_policy_maps_legacy_sandbox_fallback_risk() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  sandbox_fallback_risk: HIGH\nrules: []\n",
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        assert_eq!(policy.sandbox_off_action, SandboxOffAction::Block);
    }

    #[test]
    fn load_policy_records_invalid_risk_rule_for_fallback() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\nrules:\n  - id: H-001\n    path: ['/etc/**']\n    risk: MIDUEM\n",
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        assert_eq!(policy.validation_issues.len(), 1);
        assert_eq!(
            policy.validation_issues[0].rule_id.as_deref(),
            Some("H-001")
        );
        assert_eq!(policy.invalid_fallback_rules.len(), 1);
        assert_eq!(policy.invalid_fallback_rules[0].rule_id, "H-001");
    }
}
