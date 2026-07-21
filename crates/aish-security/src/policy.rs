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
    pub audit_include_commands: bool,
    pub rules: Vec<PolicyRule>,
    #[serde(default)]
    pub invalid_fallback_rules: Vec<InvalidFallbackRule>,
    #[serde(default)]
    pub validation_issues: Vec<ValidationIssue>,
    #[serde(default)]
    pub secret_patterns: Vec<crate::secret::CustomPattern>,
    #[serde(default)]
    pub input_guard: crate::input_guard::config::InputGuardConfig,
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
            audit_include_commands: false,
            rules: default_rules(),
            invalid_fallback_rules: Vec::new(),
            validation_issues: Vec::new(),
            secret_patterns: Vec::new(),
            input_guard: crate::input_guard::config::InputGuardConfig::default(),
        }
    }
}

const EMPTY_POLICY_TEMPLATE: &str = "# AI-Shell Security Policy\n\
\n\
# Lines below ship aish defaults. Edit to tighten or relax; removing a\n\
# key restores its default value. System location: /etc/aish/security_policy.yaml.\n\
# User fallback (~/.config/aish/security_policy.yaml) is auto-seeded from this\n\
# template on first launch.\n\
\n\
global:\n\
  default_risk_level: LOW        # LOW | MEDIUM | HIGH\n\
  enable_sandbox: false\n\
  sandbox_off_action: ALLOW      # ALLOW | CONFIRM | BLOCK\n\
  sandbox_timeout_seconds: 10\n\
\n\
audit:\n\
  enabled: false\n\
  # include_commands: false   # set true to also audit regular shell commands\n\
  # log_path: /var/log/aish/audit.log\n\
\n\
# InputGuard pre-screens shell commands and AI prompts before execution.\n\
# Built-in rules: crates/aish-security/src/input_guard/patterns.rs.\n\
# Custom rules below are merged on top of built-ins; a custom rule with\n\
# the same `name` as a built-in overrides it.\n\
input_guard:\n\
  enabled: true\n\
  # max_analyzable_bytes: 4096    # inputs longer than this force-confirm\n\
  # custom_block_rules:\n\
  #   - name: no_rm_data\n\
  #     pattern: \"rm\\s+-rf\\s+/data\"\n\
  #     message: deleting /data is forbidden\n\
  # custom_confirm_rules:\n\
  #   - name: confirm_docker_push\n\
  #     pattern: \"docker\\s+push\"\n\
  #     message: pushing image — confirm tag\n\
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
    let audit_include_commands = parse_bool_strict(
        audit_cfg.and_then(|m| mapping_get(m, "include_commands")),
        false,
    );

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

    let input_guard: crate::input_guard::config::InputGuardConfig = root
        .and_then(|m| mapping_get(m, "input_guard"))
        .and_then(|v| serde_yaml::from_value(v.clone()).ok())
        .unwrap_or_default();

    SecurityPolicy {
        enable_sandbox,
        sandbox_off_action,
        sandbox_timeout_seconds,
        default_risk_level,
        audit_enabled,
        audit_log_path,
        audit_include_commands,
        rules,
        invalid_fallback_rules,
        validation_issues,
        secret_patterns,
        input_guard,
    }
}

/// Update one or more `global:` fields in a `security_policy.yaml` file
/// **in place**, preserving all comments, blank lines, and other sections.
///
/// Each entry in `updates` is `(field_name, new_value)`. If a field already
/// exists under `global:`, its value is replaced (inline comments kept). If
/// it is absent, it is inserted into the `global:` block. If `global:` itself
/// is missing, a minimal section is appended.
///
/// Used by the `/setting` panel so security toggles are visible in the file
/// users consider authoritative — not just in `config.yaml`.
pub fn save_policy_globals(path: &Path, updates: &[(&str, &str)]) -> Result<(), String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut updated = text;
    for (field, value) in updates {
        updated = update_global_field(&updated, field, value);
    }
    fs::write(path, updated).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(())
}

/// Replace `field` under the top-level `global:` mapping with `value`, or
/// insert it if absent. Preserves comments, indentation, and all other content.
fn update_global_field(text: &str, field: &str, value: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_global = false;
    let mut global_line_idx: Option<usize> = None;
    let mut last_indent: String = String::from("  ");
    let mut last_global_child_idx: Option<usize> = None;
    let mut done = false;

    for (i, raw) in lines.iter().enumerate() {
        // A top-level key: no leading whitespace, not blank, not a comment,
        // not a list-item dash.
        let is_top_level = !raw.is_empty()
            && !raw.starts_with(' ')
            && !raw.starts_with('\t')
            && !raw.starts_with('#')
            && !raw.starts_with('-');
        if is_top_level {
            in_global = raw.trim_start().starts_with("global:");
            if in_global {
                global_line_idx = Some(i);
            }
        }

        if in_global && !done {
            let stripped = raw.trim_start();
            let indent_len = raw.len() - stripped.len();
            if indent_len > 0 && !stripped.starts_with('#') && !stripped.is_empty() {
                // Track the indentation used inside global: so an insertion
                // (if needed) matches the surrounding style.
                last_indent = raw[..indent_len].to_string();
                last_global_child_idx = Some(i);
            }
            // Match `  field:` exactly (the ':' terminator prevents
            // `enable_sandbox_extra:` from matching `enable_sandbox`).
            let needle = format!("{}:", field);
            if stripped.starts_with(&needle) {
                let after = &stripped[needle.len()..];
                // Preserve a trailing inline comment, e.g. `field: VALUE  # note`.
                let comment = after.find('#').map(|pos| &after[pos..]);
                out.push(format!(
                    "{}{}: {}{}",
                    &raw[..indent_len],
                    field,
                    value,
                    comment.map(|c| format!("  {}", c)).unwrap_or_default()
                ));
                done = true;
                continue;
            }
        }

        out.push(raw.to_string());
    }

    // Field not found under global: — insert it.
    if !done {
        if let Some(gidx) = global_line_idx {
            // Insert after the last existing child of global: (or right
            // after `global:` if the section is empty).
            let insert_at = last_global_child_idx.map(|c| c + 1).unwrap_or(gidx + 1);
            // Find where `out` currently has the corresponding line — since
            // we pushed every line, indices match `lines`.
            out.insert(insert_at, format!("{}{}: {}", last_indent, field, value));
        } else {
            // No global: section at all — append a minimal one.
            if !out.is_empty() && out.last().map(|l| !l.is_empty()).unwrap_or(false) {
                out.push(String::new());
            }
            out.push("global:".to_string());
            out.push(format!("  {}: {}", field, value));
        }
    }

    let mut result = out.join("\n");
    if text.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
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

    #[test]
    fn load_policy_parses_input_guard_config() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            r#"
global:
  enable_sandbox: false

input_guard:
  enabled: true
  custom_block_rules:
    - name: block_dangerous
      pattern: "dangerous_cmd"
      message: "This is dangerous!"
  custom_confirm_rules:
    - name: confirm_risky
      pattern: "risky_tool"
      message: "Please confirm this risky operation"

rules: []
"#,
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        assert!(policy.input_guard.enabled);
        assert_eq!(policy.input_guard.custom_block_rules.len(), 1);
        assert_eq!(policy.input_guard.custom_confirm_rules.len(), 1);

        assert_eq!(
            policy.input_guard.custom_block_rules[0].name,
            "block_dangerous"
        );
        assert_eq!(
            policy.input_guard.custom_block_rules[0].pattern,
            "dangerous_cmd"
        );
        assert_eq!(
            policy.input_guard.custom_block_rules[0].message,
            "This is dangerous!"
        );

        assert_eq!(
            policy.input_guard.custom_confirm_rules[0].name,
            "confirm_risky"
        );
        assert_eq!(
            policy.input_guard.custom_confirm_rules[0].pattern,
            "risky_tool"
        );
        assert_eq!(
            policy.input_guard.custom_confirm_rules[0].message,
            "Please confirm this risky operation"
        );
    }

    #[test]
    fn load_policy_parses_max_analyzable_bytes() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            r#"
input_guard:
  enabled: true
  max_analyzable_bytes: 8192

rules: []
"#,
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        assert_eq!(policy.input_guard.max_analyzable_bytes, Some(8192));
    }

    #[test]
    fn load_policy_defaults_max_analyzable_bytes_to_none() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            r#"
rules: []
"#,
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        assert_eq!(policy.input_guard.max_analyzable_bytes, None);
    }

    #[test]
    fn load_policy_uses_default_input_guard_when_missing() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\nrules: []\n",
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));

        // Should use default values
        assert!(policy.input_guard.enabled);
        assert!(policy.input_guard.custom_block_rules.is_empty());
        assert!(policy.input_guard.custom_confirm_rules.is_empty());
    }

    // ---- save_policy_globals / update_global_field ----

    #[test]
    fn update_global_field_replaces_existing_value() {
        let yaml = "global:\n  enable_sandbox: true\n  default_risk_level: LOW\n";
        let out = super::update_global_field(yaml, "enable_sandbox", "false");
        assert!(out.contains("enable_sandbox: false"));
        assert!(out.contains("default_risk_level: LOW"));
        assert!(!out.contains("enable_sandbox: true"));
    }

    #[test]
    fn update_global_field_preserves_inline_comment() {
        let yaml = "global:\n  enable_sandbox: false  # toggle me\n";
        let out = super::update_global_field(yaml, "enable_sandbox", "true");
        assert!(out.contains("enable_sandbox: true  # toggle me"));
    }

    #[test]
    fn update_global_field_preserves_block_comments_and_other_sections() {
        let yaml = "# header comment\n\
                    global:\n\
                    # enable sandbox?\n\
                    enable_sandbox: false\n\
                    default_risk_level: LOW\n\
                    \n\
                    audit:\n\
                    enabled: true\n";
        let out = super::update_global_field(yaml, "enable_sandbox", "true");
        assert!(out.contains("# header comment"));
        assert!(out.contains("# enable sandbox?"));
        assert!(out.contains("enable_sandbox: true"));
        assert!(out.contains("default_risk_level: LOW"));
        assert!(out.contains("audit:"));
        assert!(out.contains("enabled: true"));
    }

    #[test]
    fn update_global_field_does_not_match_prefix() {
        // `enable_sandbox` must NOT match `enable_sandbox_extra`.
        let yaml = "global:\n  enable_sandbox: true\n  enable_sandbox_extra: keep\n";
        let out = super::update_global_field(yaml, "enable_sandbox", "false");
        assert!(out.contains("enable_sandbox: false"));
        assert!(out.contains("enable_sandbox_extra: keep"));
    }

    #[test]
    fn update_global_field_inserts_missing_field() {
        let yaml = "global:\n  default_risk_level: LOW\n";
        let out = super::update_global_field(yaml, "enable_sandbox", "true");
        assert!(out.contains("enable_sandbox: true"));
        assert!(out.contains("default_risk_level: LOW"));
        // Inserted with the same indent as the existing child.
        assert!(out.contains("  enable_sandbox: true"));
    }

    #[test]
    fn update_global_field_creates_global_section_if_absent() {
        let yaml = "audit:\n  enabled: true\n";
        let out = super::update_global_field(yaml, "enable_sandbox", "true");
        assert!(out.contains("global:"));
        assert!(out.contains("enable_sandbox: true"));
        assert!(out.contains("audit:"));
    }

    #[test]
    fn save_policy_globals_round_trips_through_load_policy() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "# comment\nglobal:\n  enable_sandbox: true\n  default_risk_level: LOW\nrules: []\n",
        )
        .unwrap();
        super::save_policy_globals(
            &policy_path,
            &[
                ("enable_sandbox", "false"),
                ("default_risk_level", "MEDIUM"),
            ],
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));
        assert!(!policy.enable_sandbox);
        assert_eq!(policy.default_risk_level, RiskLevel::Medium);

        // Comment preserved.
        let on_disk = fs::read_to_string(&policy_path).unwrap();
        assert!(on_disk.contains("# comment"));
    }

    /// Verifies the real-world `/etc/aish/security_policy.yaml` layout
    /// (block comments above each field, inline comments on some lines,
    /// multi-language content) survives a `/setting` sync intact.
    #[test]
    fn save_policy_globals_preserves_production_template_comments() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        // Mirrors the installed template with CJK comments and inline notes.
        let template = concat!(
            "# AI-Shell Security Policy\n",
            "# Installed by `make install`.\n",
            "\n",
            "# 全局配置\n",
            "global:\n",
            "  # 未命中任何规则时的默认风险\n",
            "  default_risk_level: LOW\n",
            "\n",
            "  # 是否开启沙箱预跑\n",
            "  enable_sandbox: true\n",
            "\n",
            "  # 沙箱关闭时的处理动作\n",
            "  sandbox_off_action: ALLOW      # ALLOW | CONFIRM | BLOCK\n",
            "  sandbox_timeout_seconds: 10\n",
            "\n",
            "audit:\n",
            "  enabled: false\n",
            "\n",
            "rules: []\n",
        );
        fs::write(&policy_path, template).unwrap();

        super::save_policy_globals(
            &policy_path,
            &[
                ("enable_sandbox", "false"),
                ("default_risk_level", "MEDIUM"),
                ("sandbox_off_action", "BLOCK"),
                ("sandbox_timeout_seconds", "15"),
            ],
        )
        .unwrap();

        let result = fs::read_to_string(&policy_path).unwrap();

        // Block comments preserved.
        assert!(result.contains("# AI-Shell Security Policy"));
        assert!(result.contains("# 全局配置"));
        assert!(result.contains("# 未命中任何规则时的默认风险"));
        assert!(result.contains("# 是否开启沙箱预跑"));
        assert!(result.contains("# 沙箱关闭时的处理动作"));

        // Values updated.
        assert!(result.contains("enable_sandbox: false"));
        assert!(!result.contains("enable_sandbox: true"));
        assert!(result.contains("default_risk_level: MEDIUM"));
        assert!(result.contains("sandbox_off_action: BLOCK"));
        assert!(result.contains("sandbox_timeout_seconds: 15"));

        // Inline comment on sandbox_off_action line preserved.
        assert!(result.contains("# ALLOW | CONFIRM | BLOCK"));

        // Other sections untouched.
        assert!(result.contains("audit:"));
        assert!(result.contains("rules: []"));
    }
}
