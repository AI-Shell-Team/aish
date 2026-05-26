use std::path::Path;

use crate::decision::{RiskLevel, SandboxOffAction, SandboxStatus, SecurityAnalysis};
use crate::fallback::{FallbackRuleAssessment, FallbackRuleEngine};
use crate::policy::SecurityPolicy;
use crate::sandbox::error::SandboxReason;
use crate::types::{FsChange, MatchedRuleSummary};

#[derive(Debug, Clone, Default)]
pub(crate) struct SandboxDegradedDetails {
    pub(crate) error: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) cwd: Option<String>,
    pub(crate) repo_root: Option<String>,
}

impl SandboxDegradedDetails {
    pub(crate) fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub(crate) fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = Some(exit_code);
        self
    }

    pub(crate) fn with_cwd(mut self, cwd: &Path) -> Self {
        self.cwd = Some(cwd.display().to_string());
        self
    }

    pub(crate) fn with_repo_root(mut self, repo_root: &Path) -> Self {
        self.repo_root = Some(repo_root.display().to_string());
        self
    }
}

pub(crate) fn analyze_sandbox_degraded(
    policy: &SecurityPolicy,
    fallback_engine: &FallbackRuleEngine,
    command: &str,
    reason: SandboxReason,
    details: SandboxDegradedDetails,
) -> SecurityAnalysis {
    match reason {
        SandboxReason::SandboxDisabled
        | SandboxReason::SandboxDisabledByPolicy
        | SandboxReason::SandboxIpcUnavailable => {
            analyze_with_optional_fallback(policy, fallback_engine, command, reason, details)
        }

        SandboxReason::BadRequest
        | SandboxReason::RequestTooLarge
        | SandboxReason::SandboxIpcTimeout
        | SandboxReason::SandboxIpcProtocolError
        | SandboxReason::SandboxIpcFailed
        | SandboxReason::SandboxTimeout
        | SandboxReason::SandboxExecuteFailed
        | SandboxReason::SandboxCleanupFailed
        | SandboxReason::SandboxException
        | SandboxReason::SandboxFailed
        | SandboxReason::CommandNotFound => {
            analyze_with_action(SandboxOffAction::Confirm, reason, details)
        }

        SandboxReason::SandboxUnavailable
        | SandboxReason::CwdOutsideRepoRoot
        | SandboxReason::OverlayMountFailed
        | SandboxReason::OverlayPermFailed
        | SandboxReason::BindMountFailed
        | SandboxReason::RemountRoFailed => {
            analyze_with_action(policy.sandbox_off_action, reason, details)
        }
    }
}

fn analyze_with_optional_fallback(
    policy: &SecurityPolicy,
    fallback_engine: &FallbackRuleEngine,
    command: &str,
    reason: SandboxReason,
    details: SandboxDegradedDetails,
) -> SecurityAnalysis {
    match fallback_engine.assess_disabled_command(command) {
        Some(assessment) => analysis_from_fallback(policy, &assessment, reason, details),
        None => analyze_with_action(policy.sandbox_off_action, reason, details),
    }
}

fn analyze_with_action(
    effective_action: SandboxOffAction,
    reason: SandboxReason,
    details: SandboxDegradedDetails,
) -> SecurityAnalysis {
    let risk_level = risk_for_action(effective_action);
    let mut reasons = vec![format!(
        "sandbox degraded because {}; using {}",
        reason.as_str(),
        effective_action.as_str()
    )];
    if let Some(exit_code) = details.exit_code {
        reasons.push(format!("sandbox exit_code: {}", exit_code));
    }
    if let (Some(cwd), Some(repo_root)) = (&details.cwd, &details.repo_root) {
        reasons.push(format!("cwd={}, repo_root={}", cwd, repo_root));
    }

    SecurityAnalysis {
        risk_level,
        reasons,
        fail_open: effective_action == SandboxOffAction::Allow,
        sandbox_off_action: Some(effective_action),
        sandbox: sandbox_status(reason, details),
        ..SecurityAnalysis::default()
    }
}

fn analysis_from_fallback(
    policy: &SecurityPolicy,
    assessment: &FallbackRuleAssessment,
    reason: SandboxReason,
    details: SandboxDegradedDetails,
) -> SecurityAnalysis {
    let primary_rule = &assessment.primary_rule;
    let reasons = if let Some(rule_reason) = primary_rule.reason.as_ref() {
        vec![rule_reason.clone()]
    } else {
        assessment.reasons.iter().take(1).cloned().collect()
    };

    SecurityAnalysis {
        risk_level: assessment.level,
        reasons,
        changes: assessment
            .matched_paths
            .iter()
            .map(|path| FsChange {
                path: path.clone(),
                kind: "fallback_deleted".to_string(),
                detail: None,
            })
            .collect(),
        impact_description: primary_rule.description.clone().unwrap_or_default(),
        suggested_alternatives: parse_suggestions(primary_rule.suggestion.as_deref()),
        confirm_message: primary_rule.confirm_message.clone(),
        fail_open: false,
        fallback_rule_matched: true,
        matched_rule: Some(MatchedRuleSummary {
            id: primary_rule.rule_id.clone(),
            name: primary_rule.name.clone(),
            pattern: primary_rule.pattern.clone(),
        }),
        matched_paths: assessment.matched_paths.clone(),
        sandbox_off_action: Some(policy.sandbox_off_action),
        sandbox: sandbox_status(reason, details),
        detected_secrets: None,
    }
}

fn sandbox_status(reason: SandboxReason, details: SandboxDegradedDetails) -> SandboxStatus {
    SandboxStatus {
        enabled: false,
        reason: Some(reason.as_str().to_string()),
        error: details.error,
        exit_code: details.exit_code,
        cwd: details.cwd,
        repo_root: details.repo_root,
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

fn risk_for_action(action: SandboxOffAction) -> RiskLevel {
    match action {
        SandboxOffAction::Allow => RiskLevel::Low,
        SandboxOffAction::Confirm => RiskLevel::Medium,
        SandboxOffAction::Block => RiskLevel::High,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::{analyze_sandbox_degraded, SandboxDegradedDetails};
    use crate::decision::{RiskLevel, SandboxOffAction};
    use crate::fallback::FallbackRuleEngine;
    use crate::policy::{PolicyRule, SecurityPolicy};
    use crate::sandbox::error::SandboxReason;

    fn policy_with_rule_and_action(
        rule: Option<PolicyRule>,
        action: SandboxOffAction,
    ) -> SecurityPolicy {
        SecurityPolicy {
            enable_sandbox: true,
            sandbox_off_action: action,
            rules: rule.into_iter().collect(),
            ..SecurityPolicy::default()
        }
    }

    #[test]
    fn disabled_reason_uses_fallback_rule_when_available() {
        let policy = policy_with_rule_and_action(
            Some(PolicyRule {
                pattern: "/etc/**".to_string(),
                risk: RiskLevel::High,
                description: Some("etc protected".to_string()),
                operations: Some(BTreeSet::from(["DELETE".to_string()])),
                command_list: Some(BTreeSet::from(["rm".to_string()])),
                exclude: None,
                rule_id: Some("H-001".to_string()),
                name: Some("protect etc".to_string()),
                reason: Some("system config is protected".to_string()),
                confirm_message: None,
                suggestion: Some("edit manually".to_string()),
            }),
            SandboxOffAction::Allow,
        );
        let engine = FallbackRuleEngine::new(policy.clone());

        let analysis = analyze_sandbox_degraded(
            &policy,
            &engine,
            "rm -rf /etc/aish",
            SandboxReason::SandboxDisabled,
            SandboxDegradedDetails::default(),
        );

        assert_eq!(analysis.risk_level, RiskLevel::High);
        assert!(analysis.fallback_rule_matched);
        assert_eq!(analysis.matched_paths, vec!["/etc/aish"]);
        assert_eq!(analysis.sandbox.reason.as_deref(), Some("sandbox_disabled"));
        assert_eq!(analysis.suggested_alternatives, vec!["edit manually"]);
    }

    #[test]
    fn ipc_timeout_forces_confirm_without_fallback() {
        let policy = policy_with_rule_and_action(None, SandboxOffAction::Allow);
        let engine = FallbackRuleEngine::new(policy.clone());

        let analysis = analyze_sandbox_degraded(
            &policy,
            &engine,
            "echo hi",
            SandboxReason::SandboxIpcTimeout,
            SandboxDegradedDetails::default().with_error("timed out"),
        );

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert!(!analysis.fallback_rule_matched);
        assert!(!analysis.fail_open);
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Confirm));
        assert_eq!(analysis.sandbox.error.as_deref(), Some("timed out"));
    }

    #[test]
    fn cwd_outside_repo_root_uses_policy_action_without_fallback() {
        let policy = policy_with_rule_and_action(None, SandboxOffAction::Block);
        let engine = FallbackRuleEngine::new(policy.clone());

        let analysis = analyze_sandbox_degraded(
            &policy,
            &engine,
            "echo hi",
            SandboxReason::CwdOutsideRepoRoot,
            SandboxDegradedDetails::default()
                .with_cwd(Path::new("/tmp/outside"))
                .with_repo_root(Path::new("/repo")),
        );

        assert_eq!(analysis.risk_level, RiskLevel::High);
        assert!(!analysis.fallback_rule_matched);
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Block));
        assert_eq!(analysis.sandbox.cwd.as_deref(), Some("/tmp/outside"));
        assert_eq!(analysis.sandbox.repo_root.as_deref(), Some("/repo"));
    }

    #[test]
    fn ipc_unavailable_without_fallback_uses_policy_allow_and_fail_open() {
        let policy = policy_with_rule_and_action(None, SandboxOffAction::Allow);
        let engine = FallbackRuleEngine::new(policy.clone());

        let analysis = analyze_sandbox_degraded(
            &policy,
            &engine,
            "echo hi",
            SandboxReason::SandboxIpcUnavailable,
            SandboxDegradedDetails::default(),
        );

        assert_eq!(analysis.risk_level, RiskLevel::Low);
        assert!(analysis.fail_open);
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Allow));
    }

    #[test]
    fn execute_failed_with_exit_code_records_reason_and_confirms() {
        let policy = policy_with_rule_and_action(None, SandboxOffAction::Block);
        let engine = FallbackRuleEngine::new(policy.clone());

        let analysis = analyze_sandbox_degraded(
            &policy,
            &engine,
            "rm -rf /etc/aish",
            SandboxReason::SandboxExecuteFailed,
            SandboxDegradedDetails::default().with_exit_code(7),
        );

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert_eq!(
            analysis.sandbox.reason.as_deref(),
            Some("sandbox_execute_failed")
        );
        assert_eq!(analysis.sandbox.exit_code, Some(7));
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Confirm));
        assert!(analysis
            .reasons
            .iter()
            .any(|reason| reason.contains("exit_code: 7")));
    }
}
