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

/// Default policy content shipped with aish. Seeded into
/// `~/.config/aish/security_policy.yaml` on first use (when no system-level
/// policy exists at `/etc/aish/security_policy.yaml`).
const DEFAULT_POLICY_TEMPLATE: &str = include_str!("../../../config/security_policy.yaml");

/// Default system-level policy path — takes full precedence over the
/// user-level policy when it exists. Allows administrators to enforce a
/// single security policy for all users on the machine.
const DEFAULT_SYSTEM_POLICY_PATH: &str = "/etc/aish/security_policy.yaml";

/// Resolve the system-level policy path. In tests this can be overridden
/// via the `AISH_SYSTEM_POLICY_PATH` environment variable to avoid touching
/// the real `/etc/aish/`.
fn system_policy_path() -> PathBuf {
    env::var_os("AISH_SYSTEM_POLICY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SYSTEM_POLICY_PATH))
}

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

/// Seed `~/.config/aish/security_policy.yaml` if missing from the compile-time
/// default template. Writes the full template to a temp file first, then
/// installs with `create_new` so concurrent races cannot clobber a user file
/// and a failed seed cannot leave an empty destination.
fn ensure_user_policy_template(path: &Path) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Write the complete template to a temp file first, then publish it with a
    // no-clobber install so concurrent readers never observe a partial dest and
    // an existing user file is never overwritten.
    let tmp_path = path.with_extension("yaml.seed.tmp");
    if fs::write(&tmp_path, DEFAULT_POLICY_TEMPLATE).is_err() {
        let _ = fs::remove_file(&tmp_path);
        return;
    }

    match fs::hard_link(&tmp_path, path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            // Fallback when hard links are unavailable: create_new + full write,
            // removing the destination if that write fails.
            use std::io::Write;
            if let Ok(mut file) = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                if file.write_all(DEFAULT_POLICY_TEMPLATE.as_bytes()).is_err() {
                    drop(file);
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
    let _ = fs::remove_file(&tmp_path);
}

/// Resolve the policy file used for **reads**.
///
/// Priority:
/// 1. Explicit `config_path` (tests / overrides)
/// 2. `/etc/aish/security_policy.yaml` (system-level, read-only)
/// 3. `~/.config/aish/security_policy.yaml` (auto-seeded from the shipped
///    template when missing)
///
/// When the system-level policy exists it takes full precedence — the
/// user-level file is not consulted. This allows administrators to enforce
/// a single security policy across all users. When no system-level policy
/// exists, the user-level file is used (and auto-seeded if missing).
pub fn resolve_security_policy_path(config_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = config_path {
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    // System-level policy takes full precedence when present.
    let system_path = system_policy_path();
    if system_path.exists() {
        return Some(system_path);
    }

    // Fall back to user-level policy (auto-seeded from the shipped template).
    let user_path = user_security_policy_path();
    if !user_path.exists() {
        ensure_user_policy_template(&user_path);
    }
    if user_path.exists() {
        return Some(user_path);
    }

    None
}

/// Path `/setting` (and other UI writers) must use — always
/// `~/.config/aish/security_policy.yaml`. Seeds the shipped template when
/// missing.
pub fn writable_security_policy_path() -> Option<PathBuf> {
    let user_path = user_security_policy_path();
    ensure_user_policy_template(&user_path);
    if user_path.exists() {
        Some(user_path)
    } else {
        None
    }
}

fn mapping_get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_string()))
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

/// Errors from an authoritative security policy load (no silent defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyLoadError {
    /// Policy path could not be resolved / seeded.
    Missing { path: PathBuf },
    /// Policy file exists but could not be read.
    Read { path: PathBuf, message: String },
    /// Policy file is not valid YAML.
    Parse { path: PathBuf, message: String },
    /// YAML parsed, but required sections have the wrong shape.
    Invalid { path: PathBuf, message: String },
}

impl std::fmt::Display for PolicyLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { path } => write!(
                f,
                "security policy missing or could not be seeded: {}",
                path.display()
            ),
            Self::Read { path, message } => {
                write!(
                    f,
                    "cannot read security policy {}: {message}",
                    path.display()
                )
            }
            Self::Parse { path, message } => {
                write!(
                    f,
                    "invalid security policy YAML {}: {message}",
                    path.display()
                )
            }
            Self::Invalid { path, message } => {
                write!(
                    f,
                    "invalid security policy structure {}: {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for PolicyLoadError {}

/// Load the security policy, reporting missing/unreadable/invalid files.
///
/// Unlike [`load_policy`], this does **not** fall back to
/// [`SecurityPolicy::default`] on I/O or parse failure — callers that need to
/// surface diagnostics (e.g. `aish doctor`) should use this entry point.
pub fn try_load_policy(config_path: Option<&Path>) -> Result<SecurityPolicy, PolicyLoadError> {
    let effective_path = match resolve_security_policy_path(config_path) {
        Some(path) => path,
        None => {
            return Err(PolicyLoadError::Missing {
                path: config_path
                    .map(Path::to_path_buf)
                    .unwrap_or_else(user_security_policy_path),
            });
        }
    };

    let text = fs::read_to_string(&effective_path).map_err(|e| PolicyLoadError::Read {
        path: effective_path.clone(),
        message: e.to_string(),
    })?;
    let data = serde_yaml::from_str::<Value>(&text).map_err(|e| PolicyLoadError::Parse {
        path: effective_path.clone(),
        message: e.to_string(),
    })?;
    policy_from_value(data).map_err(|message| PolicyLoadError::Invalid {
        path: effective_path,
        message,
    })
}

/// Load security policy for runtime use. On missing/unreadable/invalid files,
/// falls back to [`SecurityPolicy::default`] (same historical behavior).
pub fn load_policy(config_path: Option<&Path>) -> SecurityPolicy {
    try_load_policy(config_path).unwrap_or_else(|_| SecurityPolicy::default())
}

fn optional_section_mapping<'a>(
    root: &'a Mapping,
    key: &str,
) -> Result<Option<&'a Mapping>, String> {
    match mapping_get(root, key) {
        None => Ok(None),
        Some(value) => value
            .as_mapping()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a mapping")),
    }
}

fn policy_from_value(data: Value) -> Result<SecurityPolicy, String> {
    let root = data
        .as_mapping()
        .ok_or_else(|| "root document must be a mapping".to_string())?;
    let global_cfg = optional_section_mapping(root, "global")?;
    let audit_cfg = optional_section_mapping(root, "audit")?;

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

    let rule_mappings: Vec<&Mapping> = match mapping_get(root, "rules") {
        None => Vec::new(),
        Some(value) => {
            let seq = value
                .as_sequence()
                .ok_or_else(|| "`rules` must be a sequence".to_string())?;
            seq.iter().filter_map(Value::as_mapping).collect()
        }
    };

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

    let secret_patterns: Vec<crate::secret::CustomPattern> =
        match mapping_get(root, "secret_patterns") {
            None => Vec::new(),
            Some(value) => {
                let seq = value
                    .as_sequence()
                    .ok_or_else(|| "`secret_patterns` must be a sequence".to_string())?;
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
            }
        };

    let input_guard: crate::input_guard::config::InputGuardConfig =
        match mapping_get(root, "input_guard") {
            None => crate::input_guard::config::InputGuardConfig::default(),
            Some(value) => serde_yaml::from_value(value.clone())
                .map_err(|e| format!("invalid `input_guard`: {e}"))?,
        };

    Ok(SecurityPolicy {
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
    })
}

/// Persist `/setting` Security fields in one read/modify/write: `global:`
/// updates plus optional `input_guard.enabled`. Preserves comments.
pub fn save_policy_ui_fields(
    path: &Path,
    global_updates: &[(&str, &str)],
    input_guard_enabled: Option<bool>,
) -> Result<(), String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut updated = text;
    for (field, value) in global_updates {
        updated = update_section_field(&updated, "global", field, value);
    }
    if let Some(enabled) = input_guard_enabled {
        updated = update_section_field(
            &updated,
            "input_guard",
            "enabled",
            if enabled { "true" } else { "false" },
        );
    }
    // Prefer atomic write (temp + rename in the same directory) to avoid
    // truncation on crash/ENOSPC. Fall back to direct fs::write when the
    // directory isn't writable — uncommon for ~/.config, but kept for
    // callers that still pass an explicit path.
    let tmp_path = path.with_extension("yaml.tmp");
    let atomic_ok = fs::write(&tmp_path, &updated).is_ok() && fs::rename(&tmp_path, path).is_ok();
    if !atomic_ok {
        let _ = fs::remove_file(&tmp_path);
        fs::write(path, &updated)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Replace `field` under a top-level YAML `section:` mapping with `value`, or
/// insert it if absent. Preserves comments, indentation, and all other content.
fn update_section_field(text: &str, section: &str, field: &str, value: &str) -> String {
    let section_key = format!("{}:", section);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_section = false;
    let mut section_line_idx: Option<usize> = None;
    // Indentation of direct children under `section:` (min indent seen).
    // Nested list/map lines must not overwrite this, or a missing-field
    // insert lands inside the nested block.
    let mut child_indent: String = String::from("  ");
    let mut saw_child_indent = false;
    let mut last_section_child_idx: Option<usize> = None;
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
            in_section = raw.trim_start().starts_with(&section_key);
            if in_section {
                section_line_idx = Some(i);
            }
        }

        if in_section && !done {
            let stripped = raw.trim_start();
            let indent_len = raw.len() - stripped.len();
            if indent_len > 0 && !stripped.starts_with('#') && !stripped.is_empty() {
                // Always advance the insert cursor past every descendant line,
                // but only learn direct-child indent from the shallowest level.
                last_section_child_idx = Some(i);
                if !saw_child_indent || indent_len < child_indent.len() {
                    child_indent = raw[..indent_len].to_string();
                    saw_child_indent = true;
                }
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

    // Field not found under section: — insert it.
    if !done {
        if let Some(sidx) = section_line_idx {
            // Insert after the last existing child of the section (or right
            // after `section:` if the section is empty).
            let insert_at = last_section_child_idx.map(|c| c + 1).unwrap_or(sidx + 1);
            // Find where `out` currently has the corresponding line — since
            // we pushed every line, indices match `lines`.
            out.insert(insert_at, format!("{}{}: {}", child_indent, field, value));
        } else {
            // No section at all — append a minimal one.
            if !out.is_empty() && out.last().map(|l| !l.is_empty()).unwrap_or(false) {
                out.push(String::new());
            }
            out.push(format!("{}:", section));
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

    /// Serialize tests that mutate process-global env vars (cargo runs them
    /// in parallel threads of one process).
    fn xdg_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// RAII env var restore for path-resolution tests.
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let prev = std::env::var_os(key);
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

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
    fn resolve_security_policy_path_uses_user_config() {
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(xdg.join("aish")).unwrap();
        let user_path = xdg.join("aish").join("security_policy.yaml");
        fs::write(&user_path, "global:\n  enable_sandbox: true\nrules: []\n").unwrap();

        let _lock = xdg_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap()));
        // Point system policy at a non-existent path so the user-level file
        // is the effective fallback.
        let _sys_guard = EnvGuard::set(
            "AISH_SYSTEM_POLICY_PATH",
            Some(dir.path().join("no-system-policy.yaml").to_str().unwrap()),
        );
        let resolved = resolve_security_policy_path(None);
        assert_eq!(resolved.as_deref(), Some(user_path.as_path()));
        let policy = load_policy(None);
        assert!(policy.enable_sandbox);
    }

    #[test]
    fn resolve_security_policy_path_seeds_user_when_no_system_policy() {
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(&xdg).unwrap();
        let _lock = xdg_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap()));
        // Point system policy at a non-existent path so the user-level file
        // is seeded and used.
        let _sys_guard = EnvGuard::set(
            "AISH_SYSTEM_POLICY_PATH",
            Some(dir.path().join("no-system-policy.yaml").to_str().unwrap()),
        );

        let resolved = resolve_security_policy_path(None).expect("user policy");
        assert!(resolved.starts_with(&xdg));
        assert!(resolved.exists());
    }

    #[test]
    fn resolve_security_policy_path_prefers_system_over_user() {
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(xdg.join("aish")).unwrap();
        let user_path = xdg.join("aish").join("security_policy.yaml");
        // User-level policy with sandbox enabled — should NOT be used.
        fs::write(&user_path, "global:\n  enable_sandbox: true\nrules: []\n").unwrap();

        // System-level policy with sandbox disabled — should be used.
        let system_path = dir.path().join("system-policy.yaml");
        fs::write(
            &system_path,
            "global:\n  enable_sandbox: false\nrules: []\n",
        )
        .unwrap();

        let _lock = xdg_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap()));
        let _sys_guard = EnvGuard::set(
            "AISH_SYSTEM_POLICY_PATH",
            Some(system_path.to_str().unwrap()),
        );

        let resolved = resolve_security_policy_path(None);
        assert_eq!(resolved.as_deref(), Some(system_path.as_path()));

        let policy = load_policy(None);
        assert!(
            !policy.enable_sandbox,
            "system policy should override user policy"
        );
    }

    #[test]
    fn writable_security_policy_path_seeds_user_file() {
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(&xdg).unwrap();
        let _lock = xdg_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap()));

        let path = super::writable_security_policy_path().expect("user path");
        assert!(path.exists());
        assert!(path.starts_with(&xdg));
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("security_policy.yaml")
        );

        let seeded = fs::read_to_string(&path).unwrap();
        assert!(seeded.contains("# AI-Shell Security Policy"));
        assert!(seeded.contains("Protect /etc"));
        assert!(seeded.contains("id: H-001"));
        assert!(seeded.contains("id: M-001"));
        assert!(seeded.contains("id: L-001"));
        // Shipped seed is English-only (no CJK comments).
        let has_cjk = seeded
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        assert!(!has_cjk, "seeded policy must not contain CJK comments");
    }

    #[test]
    fn try_load_policy_reports_parse_errors() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(&policy_path, "global: [\n").unwrap();
        let err = super::try_load_policy(Some(&policy_path)).expect_err("parse");
        match err {
            super::PolicyLoadError::Parse { path, message } => {
                assert_eq!(path, policy_path);
                assert!(!message.is_empty());
            }
            other => panic!("expected Parse, got {other:?}"),
        }
        // Runtime loader still falls back instead of panicking.
        let _ = load_policy(Some(&policy_path));
    }

    #[test]
    fn try_load_policy_rejects_invalid_global_shape() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        // Valid YAML, invalid policy structure: global must be a mapping.
        fs::write(&policy_path, "global: []\nrules: []\n").unwrap();
        let err = super::try_load_policy(Some(&policy_path)).expect_err("invalid");
        match err {
            super::PolicyLoadError::Invalid { path, message } => {
                assert_eq!(path, policy_path);
                assert!(message.contains("`global`"), "{message}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        let _ = load_policy(Some(&policy_path));
    }

    #[test]
    fn try_load_policy_rejects_invalid_input_guard_shape() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(&policy_path, "global: {}\ninput_guard: []\nrules: []\n").unwrap();
        let err = super::try_load_policy(Some(&policy_path)).expect_err("invalid");
        match err {
            super::PolicyLoadError::Invalid { path, message } => {
                assert_eq!(path, policy_path);
                assert!(message.contains("`input_guard`"), "{message}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
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

    // ---- save_policy_ui_fields / update_section_field ----

    #[test]
    fn update_section_field_global_replaces_existing_value() {
        let yaml = "global:\n  enable_sandbox: true\n  default_risk_level: LOW\n";
        let out = super::update_section_field(yaml, "global", "enable_sandbox", "false");
        assert!(out.contains("enable_sandbox: false"));
        assert!(out.contains("default_risk_level: LOW"));
        assert!(!out.contains("enable_sandbox: true"));
    }

    #[test]
    fn update_section_field_global_preserves_inline_comment() {
        let yaml = "global:\n  enable_sandbox: false  # toggle me\n";
        let out = super::update_section_field(yaml, "global", "enable_sandbox", "true");
        assert!(out.contains("enable_sandbox: true  # toggle me"));
    }

    #[test]
    fn update_section_field_global_preserves_block_comments_and_other_sections() {
        let yaml = concat!(
            "# header comment\n",
            "global:\n",
            "  # enable sandbox?\n",
            "  enable_sandbox: false\n",
            "  default_risk_level: LOW\n",
            "\n",
            "audit:\n",
            "  enabled: true\n",
        );
        let out = super::update_section_field(yaml, "global", "enable_sandbox", "true");
        assert!(out.contains("# header comment"));
        assert!(out.contains("# enable sandbox?"));
        assert!(out.contains("enable_sandbox: true"));
        assert!(
            !out.contains("enable_sandbox: false"),
            "old value must be replaced, not duplicated"
        );
        assert!(out.contains("default_risk_level: LOW"));
        assert!(out.contains("audit:"));
        assert!(out.contains("enabled: true"));
    }

    #[test]
    fn update_section_field_global_does_not_match_prefix() {
        // `enable_sandbox` must NOT match `enable_sandbox_extra`.
        let yaml = "global:\n  enable_sandbox: true\n  enable_sandbox_extra: keep\n";
        let out = super::update_section_field(yaml, "global", "enable_sandbox", "false");
        assert!(out.contains("enable_sandbox: false"));
        assert!(out.contains("enable_sandbox_extra: keep"));
    }

    #[test]
    fn update_section_field_global_inserts_missing_field() {
        let yaml = "global:\n  default_risk_level: LOW\n";
        let out = super::update_section_field(yaml, "global", "enable_sandbox", "true");
        assert!(out.contains("enable_sandbox: true"));
        assert!(out.contains("default_risk_level: LOW"));
        // Inserted with the same indent as the existing child.
        assert!(out.contains("  enable_sandbox: true"));
    }

    #[test]
    fn update_section_field_inserts_at_direct_child_indent_with_nested() {
        // Nested custom rules must not steal the indent used for a missing
        // top-level field under the same section.
        let yaml = concat!(
            "input_guard:\n",
            "  custom_block_rules:\n",
            "    - name: no_rm_data\n",
            "      pattern: \"rm\"\n",
            "      message: nope\n",
        );
        let out = super::update_section_field(yaml, "input_guard", "enabled", "false");
        let enabled_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("enabled:"))
            .expect("enabled field missing");
        assert_eq!(
            enabled_line, "  enabled: false",
            "enabled must be a direct child of input_guard; got:\n{out}"
        );
        assert!(out.contains("custom_block_rules:"));
    }

    #[test]
    fn update_section_field_global_creates_global_section_if_absent() {
        let yaml = "audit:\n  enabled: true\n";
        let out = super::update_section_field(yaml, "global", "enable_sandbox", "true");
        assert!(out.contains("global:"));
        assert!(out.contains("enable_sandbox: true"));
        assert!(out.contains("audit:"));
    }

    #[test]
    fn save_policy_ui_fields_round_trips_through_load_policy() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "# comment\nglobal:\n  enable_sandbox: true\n  default_risk_level: LOW\nrules: []\n",
        )
        .unwrap();
        super::save_policy_ui_fields(
            &policy_path,
            &[
                ("enable_sandbox", "false"),
                ("default_risk_level", "MEDIUM"),
            ],
            None,
        )
        .unwrap();

        let policy = load_policy(Some(&policy_path));
        assert!(!policy.enable_sandbox);
        assert_eq!(policy.default_risk_level, RiskLevel::Medium);

        // Comment preserved.
        let on_disk = fs::read_to_string(&policy_path).unwrap();
        assert!(on_disk.contains("# comment"));
    }

    /// Verifies the shipped `config/security_policy.yaml` layout
    /// (block comments above each field, inline comments on some lines,
    /// multi-language content) survives a `/setting` sync intact.
    #[test]
    fn save_policy_ui_fields_preserves_production_template_comments() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        // Mirrors the shipped English template shape with block + inline comments.
        let template = concat!(
            "# AI-Shell Security Policy\n",
            "# Seeded to ~/.config/aish/security_policy.yaml on first use.\n",
            "\n",
            "# Global defaults\n",
            "global:\n",
            "  # Default risk when no rules match\n",
            "  default_risk_level: LOW\n",
            "\n",
            "  # Sandbox pre-run toggle\n",
            "  enable_sandbox: true\n",
            "\n",
            "  # Action when sandbox is off/unavailable\n",
            "  sandbox_off_action: ALLOW      # ALLOW | CONFIRM | BLOCK\n",
            "  sandbox_timeout_seconds: 10\n",
            "\n",
            "audit:\n",
            "  enabled: false\n",
            "\n",
            "rules: []\n",
        );
        fs::write(&policy_path, template).unwrap();

        super::save_policy_ui_fields(
            &policy_path,
            &[
                ("enable_sandbox", "false"),
                ("default_risk_level", "MEDIUM"),
                ("sandbox_off_action", "BLOCK"),
                ("sandbox_timeout_seconds", "15"),
            ],
            None,
        )
        .unwrap();

        let result = fs::read_to_string(&policy_path).unwrap();

        // Block comments preserved.
        assert!(result.contains("# AI-Shell Security Policy"));
        assert!(result.contains("# Global defaults"));
        assert!(result.contains("# Default risk when no rules match"));
        assert!(result.contains("# Sandbox pre-run toggle"));
        assert!(result.contains("# Action when sandbox is off/unavailable"));

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

    #[test]
    fn save_policy_ui_fields_updates_input_guard_enabled() {
        let dir = tempdir().unwrap();
        let policy_path = dir.path().join("security_policy.yaml");
        fs::write(
            &policy_path,
            "global:\n  enable_sandbox: false\n\ninput_guard:\n  enabled: true\nrules: []\n",
        )
        .unwrap();

        super::save_policy_ui_fields(&policy_path, &[("enable_sandbox", "true")], Some(false))
            .unwrap();

        let policy = load_policy(Some(&policy_path));
        assert!(policy.enable_sandbox);
        assert!(!policy.input_guard.enabled);

        let on_disk = fs::read_to_string(&policy_path).unwrap();
        assert!(on_disk.contains("enable_sandbox: true"));
        assert!(on_disk.contains("enabled: false"));
    }
}
