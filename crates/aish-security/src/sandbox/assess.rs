use crate::decision::{RiskLevel, SandboxStatus, SecurityAnalysis};
use crate::policy::{PolicyRule, SecurityPolicy};
use crate::sandbox::types::{FsChange as SandboxFsChange, FsChangeKind, SandboxResult};
use crate::types::{FsChange, MatchedRuleSummary};

pub(crate) fn assess_sandbox_result(
    policy: &SecurityPolicy,
    _command: &str,
    sandbox: &SandboxResult,
) -> SecurityAnalysis {
    let changes = convert_changes(&sandbox.changes);

    if sandbox.changes.is_empty() {
        let mut analysis = SecurityAnalysis {
            risk_level: policy.default_risk_level,
            reasons: vec![format!(
                "sandbox observed no filesystem changes; using default risk level {}",
                policy.default_risk_level.as_str()
            )],
            changes,
            sandbox_off_action: Some(policy.sandbox_off_action),
            sandbox: SandboxStatus {
                enabled: true,
                exit_code: Some(sandbox.exit_code),
                ..SandboxStatus::default()
            },
            ..SecurityAnalysis::default()
        };
        apply_truncated_policy(&mut analysis, sandbox.changes_truncated);
        return analysis;
    }

    if sandbox.exit_code != 0 {
        return SecurityAnalysis {
            risk_level: RiskLevel::Medium,
            reasons: vec![format!(
                "sandbox execution returned non-zero exit code {}; require confirmation",
                sandbox.exit_code
            )],
            changes,
            sandbox_off_action: Some(policy.sandbox_off_action),
            sandbox: SandboxStatus {
                enabled: true,
                reason: Some("sandbox_execute_failed".to_string()),
                exit_code: Some(sandbox.exit_code),
                ..SandboxStatus::default()
            },
            ..SecurityAnalysis::default()
        };
    }

    let mut high_hits = Vec::new();
    let mut medium_hits = Vec::new();
    let mut low_hits = Vec::new();
    let mut unmatched = Vec::new();

    for change in &sandbox.changes {
        let path = normalize_path(&change.path);
        let operation = operation_for_change(change.kind);

        match match_rule(policy, &path, operation) {
            Some(rule) if rule.risk == RiskLevel::High => high_hits.push((change, path, rule)),
            Some(rule) if rule.risk == RiskLevel::Medium => medium_hits.push((change, path, rule)),
            Some(rule) => low_hits.push((change, path, rule)),
            None => unmatched.push((change, path)),
        }
    }

    let (risk_level, selected_hits) = if !high_hits.is_empty() {
        (RiskLevel::High, high_hits)
    } else if !medium_hits.is_empty() {
        (RiskLevel::Medium, medium_hits)
    } else if !low_hits.is_empty() {
        (RiskLevel::Low, low_hits)
    } else {
        (policy.default_risk_level, Vec::new())
    };

    let mut reasons = Vec::new();
    if !selected_hits.is_empty() {
        reasons.push(format!(
            "sandbox matched {} {}-risk filesystem change(s)",
            selected_hits.len(),
            risk_level.as_str().to_ascii_lowercase()
        ));
        let preview_paths = selected_hits
            .iter()
            .take(3)
            .map(|(_, path, _)| path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !preview_paths.is_empty() {
            reasons.push(format!("matched paths: {}", preview_paths));
        }
    } else {
        reasons.push(format!(
            "sandbox changes matched no policy rule; using default risk level {}",
            policy.default_risk_level.as_str()
        ));
        if !unmatched.is_empty() {
            let preview_paths = unmatched
                .iter()
                .take(3)
                .map(|(_, path)| path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if !preview_paths.is_empty() {
                reasons.push(format!("unmatched paths: {}", preview_paths));
            }
        }
    }

    let primary_rule = selected_hits.first().map(|(_, _, rule)| (*rule).clone());
    let mut analysis = SecurityAnalysis {
        risk_level,
        reasons,
        changes,
        impact_description: primary_rule
            .as_ref()
            .and_then(|rule| rule.description.clone())
            .unwrap_or_default(),
        suggested_alternatives: primary_rule
            .as_ref()
            .map(|rule| parse_suggestions(rule.suggestion.as_deref()))
            .unwrap_or_default(),
        confirm_message: primary_rule
            .as_ref()
            .and_then(|rule| rule.confirm_message.clone()),
        matched_rule: primary_rule.as_ref().map(rule_summary),
        matched_paths: selected_hits
            .iter()
            .map(|(_, path, _)| path.clone())
            .collect(),
        sandbox_off_action: Some(policy.sandbox_off_action),
        sandbox: SandboxStatus {
            enabled: true,
            exit_code: Some(sandbox.exit_code),
            ..SandboxStatus::default()
        },
        ..SecurityAnalysis::default()
    };

    apply_truncated_policy(&mut analysis, sandbox.changes_truncated);
    analysis
}

fn convert_changes(changes: &[SandboxFsChange]) -> Vec<FsChange> {
    changes
        .iter()
        .map(|change| FsChange {
            path: normalize_path(&change.path),
            kind: change.kind.as_str().to_string(),
            detail: change.detail.clone(),
        })
        .collect()
}

fn apply_truncated_policy(analysis: &mut SecurityAnalysis, changes_truncated: bool) {
    if !changes_truncated {
        return;
    }

    if analysis.risk_level == RiskLevel::Low {
        analysis.risk_level = RiskLevel::Medium;
    }
    analysis
        .reasons
        .push("sandbox change list was truncated; keeping a conservative risk floor".to_string());
}

fn operation_for_change(kind: FsChangeKind) -> &'static str {
    match kind {
        FsChangeKind::Deleted => "DELETE",
        FsChangeKind::Created
        | FsChangeKind::Modified
        | FsChangeKind::Chmod
        | FsChangeKind::Chown
        | FsChangeKind::Unknown => "WRITE",
    }
}

fn match_rule(policy: &SecurityPolicy, path: &str, operation: &str) -> Option<PolicyRule> {
    for rule in &policy.rules {
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
        if rule
            .operations
            .as_ref()
            .is_some_and(|ops| !ops.contains(operation))
        {
            continue;
        }
        return Some(rule.clone());
    }
    None
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

    for row in 1..=pattern_chars.len() {
        if pattern_chars[row - 1] == '*' {
            dp[row][0] = dp[row - 1][0];
        }
    }

    for row in 1..=pattern_chars.len() {
        for col in 1..=text_chars.len() {
            dp[row][col] = match pattern_chars[row - 1] {
                '*' => dp[row - 1][col] || dp[row][col - 1],
                '?' => dp[row - 1][col - 1],
                ch => dp[row - 1][col - 1] && ch == text_chars[col - 1],
            };
        }
    }

    dp[pattern_chars.len()][text_chars.len()]
}

fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path.trim_start_matches('/'))
    }
}

fn parse_suggestions(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn rule_summary(rule: &PolicyRule) -> MatchedRuleSummary {
    MatchedRuleSummary {
        id: rule.rule_id.clone(),
        name: rule.name.clone(),
        pattern: rule.pattern.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::assess_sandbox_result;
    use crate::decision::RiskLevel;
    use crate::policy::{PolicyRule, SecurityPolicy};
    use crate::sandbox::types::{FsChange, FsChangeKind, SandboxResult};

    fn policy_with_rules(rules: Vec<PolicyRule>) -> SecurityPolicy {
        SecurityPolicy {
            enable_sandbox: true,
            default_risk_level: RiskLevel::Low,
            rules,
            ..SecurityPolicy::default()
        }
    }

    fn change(path: &str, kind: FsChangeKind) -> FsChange {
        FsChange {
            path: path.to_string(),
            kind,
            detail: None,
        }
    }

    #[test]
    fn assess_sandbox_result_blocks_high_risk_delete() {
        let policy = policy_with_rules(vec![PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            description: Some("system config directory".to_string()),
            operations: Some(BTreeSet::from(["DELETE".to_string()])),
            command_list: Some(BTreeSet::from(["rm".to_string()])),
            exclude: None,
            rule_id: Some("H-001".to_string()),
            name: Some("protect etc".to_string()),
            reason: Some("system config is protected".to_string()),
            confirm_message: None,
            suggestion: Some("edit manually\nopen a ticket".to_string()),
        }]);
        let sandbox = SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            changes: vec![change("/etc/aish/config.yaml", FsChangeKind::Deleted)],
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        };

        let analysis = assess_sandbox_result(&policy, "echo hi", &sandbox);

        assert_eq!(analysis.risk_level, RiskLevel::High);
        assert_eq!(analysis.matched_paths, vec!["/etc/aish/config.yaml"]);
        assert_eq!(
            analysis
                .matched_rule
                .as_ref()
                .and_then(|rule| rule.id.as_deref()),
            Some("H-001")
        );
        assert_eq!(analysis.impact_description, "system config directory");
        assert_eq!(
            analysis.suggested_alternatives,
            vec!["edit manually", "open a ticket"]
        );
        assert_eq!(analysis.changes[0].kind, "deleted");
        assert!(analysis.sandbox.enabled);
        assert_eq!(analysis.sandbox.exit_code, Some(0));
    }

    #[test]
    fn assess_sandbox_result_confirms_medium_write() {
        let policy = policy_with_rules(vec![PolicyRule {
            pattern: "/home/**".to_string(),
            risk: RiskLevel::Medium,
            description: None,
            operations: Some(BTreeSet::from(["WRITE".to_string()])),
            command_list: Some(BTreeSet::from(["cp".to_string()])),
            exclude: None,
            rule_id: Some("M-001".to_string()),
            name: Some("protect home".to_string()),
            reason: Some("home path is protected".to_string()),
            confirm_message: Some("confirm home write".to_string()),
            suggestion: None,
        }]);
        let sandbox = SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            changes: vec![change("/home/lixin/a.txt", FsChangeKind::Created)],
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        };

        let analysis = assess_sandbox_result(&policy, "echo hi", &sandbox);

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert_eq!(
            analysis.confirm_message.as_deref(),
            Some("confirm home write")
        );
        assert_eq!(analysis.matched_paths, vec!["/home/lixin/a.txt"]);
    }

    #[test]
    fn assess_sandbox_result_uses_default_risk_for_no_changes() {
        let policy = SecurityPolicy {
            enable_sandbox: true,
            default_risk_level: RiskLevel::Medium,
            ..SecurityPolicy::default()
        };
        let sandbox = SandboxResult::default();

        let analysis = assess_sandbox_result(&policy, "echo hi", &sandbox);

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert_eq!(analysis.changes, Vec::new());
        assert_eq!(
            analysis.reasons,
            vec!["sandbox observed no filesystem changes; using default risk level MEDIUM"]
        );
    }

    #[test]
    fn assess_sandbox_result_elevates_truncated_low_results() {
        let policy = policy_with_rules(vec![PolicyRule {
            pattern: "/tmp/**".to_string(),
            risk: RiskLevel::Low,
            description: None,
            operations: Some(BTreeSet::from(["WRITE".to_string()])),
            command_list: None,
            exclude: None,
            rule_id: None,
            name: None,
            reason: None,
            confirm_message: None,
            suggestion: None,
        }]);
        let sandbox = SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            changes: vec![change("/tmp/note.txt", FsChangeKind::Modified)],
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: true,
        };

        let analysis = assess_sandbox_result(&policy, "echo hi", &sandbox);

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert!(analysis
            .reasons
            .iter()
            .any(|reason| reason.contains("truncated")));
    }

    #[test]
    fn assess_sandbox_result_non_zero_exit_with_no_changes_uses_default_risk() {
        let policy = SecurityPolicy {
            enable_sandbox: true,
            default_risk_level: RiskLevel::Low,
            ..SecurityPolicy::default()
        };
        let sandbox = SandboxResult {
            exit_code: 2,
            stdout: String::new(),
            stderr: "missing file".to_string(),
            changes: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        };

        let analysis = assess_sandbox_result(&policy, "ls /tmp/missing", &sandbox);

        assert_eq!(analysis.risk_level, RiskLevel::Low);
        assert_eq!(analysis.sandbox.exit_code, Some(2));
        assert_eq!(
            analysis.reasons,
            vec!["sandbox observed no filesystem changes; using default risk level LOW"]
        );
    }

    #[test]
    fn assess_sandbox_result_non_zero_exit_still_uses_recorded_changes() {
        let policy = policy_with_rules(vec![PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            description: Some("system config directory".to_string()),
            operations: Some(BTreeSet::from(["DELETE".to_string()])),
            command_list: Some(BTreeSet::from(["rm".to_string()])),
            exclude: None,
            rule_id: Some("H-001".to_string()),
            name: Some("protect etc".to_string()),
            reason: Some("system config is protected".to_string()),
            confirm_message: None,
            suggestion: None,
        }]);
        let sandbox = SandboxResult {
            exit_code: 7,
            stdout: String::new(),
            stderr: "boom".to_string(),
            changes: vec![change("/etc/aish/config.yaml", FsChangeKind::Deleted)],
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        };

        let analysis = assess_sandbox_result(&policy, "rm -rf /etc/aish", &sandbox);

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert_eq!(analysis.sandbox.exit_code, Some(7));
        assert_eq!(analysis.changes.len(), 1);
        assert_eq!(
            analysis.reasons,
            vec!["sandbox execution returned non-zero exit code 7; require confirmation"]
        );
    }
}
