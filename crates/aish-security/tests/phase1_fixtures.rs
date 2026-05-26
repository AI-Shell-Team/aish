use std::path::PathBuf;

use aish_security::{
    load_policy, resolve_security_policy_path, RiskLevel, SandboxOffAction, SecurityManager,
    SecurityRequest,
};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn fixture_policy_parses_legacy_global_fields_and_keeps_default_rule() {
    let path = fixture_path("phase1-legacy-policy.yaml");

    let resolved = resolve_security_policy_path(Some(path.as_path()));
    let policy = load_policy(Some(path.as_path()));

    assert_eq!(resolved.as_deref(), Some(path.as_path()));
    assert!(!policy.enable_sandbox);
    assert_eq!(policy.sandbox_off_action, SandboxOffAction::Confirm);
    assert_eq!(policy.default_risk_level, RiskLevel::Low);
    assert_eq!(policy.sandbox_timeout_seconds, 12.0);
    assert!(policy
        .rules
        .iter()
        .any(|rule| rule.rule_id.as_deref() == Some("H-SEC-001")));
    assert!(policy
        .rules
        .iter()
        .any(|rule| rule.rule_id.as_deref() == Some("H-001")));
    assert!(policy
        .rules
        .iter()
        .any(|rule| rule.rule_id.as_deref() == Some("M-001")));
}

#[test]
fn fixture_policy_records_invalid_rules_for_fallback() {
    let path = fixture_path("phase1-invalid-policy.yaml");

    let policy = load_policy(Some(path.as_path()));

    assert_eq!(policy.validation_issues.len(), 1);
    assert_eq!(
        policy.validation_issues[0].rule_id.as_deref(),
        Some("X-001")
    );
    assert_eq!(policy.invalid_fallback_rules.len(), 1);
    assert_eq!(policy.invalid_fallback_rules[0].rule_id, "X-001");
}

#[test]
fn fixture_manager_blocks_high_risk_delete_via_sudo_wrapper() {
    let policy = load_policy(Some(fixture_path("phase1-legacy-policy.yaml").as_path()));
    let manager = SecurityManager::new(policy);
    let request = SecurityRequest::ai_command().with_cwd("/tmp/repo");

    let decision = manager.decide_with_request("sudo -E -u root bash -lc 'rm -rf /etc'", &request);

    assert_eq!(decision.level, RiskLevel::High);
    assert!(!decision.allow);
    assert!(!decision.require_confirmation);
    assert!(decision.analysis.fallback_rule_matched);
    assert_eq!(
        decision
            .analysis
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.as_deref()),
        Some("H-001")
    );
    assert_eq!(decision.analysis.matched_paths, vec!["/etc"]);
}

#[test]
fn fixture_manager_confirms_medium_write_and_allows_default_low() {
    let policy = load_policy(Some(fixture_path("phase1-legacy-policy.yaml").as_path()));
    let manager = SecurityManager::new(policy);
    let request = SecurityRequest::ai_command().with_cwd("/tmp/repo");

    let confirm = manager.decide_with_request("cp /tmp/a /home/x/a", &request);
    assert_eq!(confirm.level, RiskLevel::Medium);
    assert!(confirm.allow);
    assert!(confirm.require_confirmation);
    assert!(confirm.analysis.fallback_rule_matched);
    assert_eq!(confirm.analysis.matched_paths, vec!["/home/x/a"]);

    let allow = manager.decide_with_request("echo hi", &request);
    assert_eq!(allow.level, RiskLevel::Low);
    assert!(allow.allow);
    assert!(!allow.require_confirmation);
    assert!(!allow.analysis.fallback_rule_matched);
    assert!(allow.analysis.fail_open);
    assert_eq!(
        allow.analysis.sandbox_off_action,
        Some(SandboxOffAction::Confirm)
    );
}
