use crate::decision::{RiskLevel, SecurityAnalysis, SecurityDecision};
use crate::fallback::FallbackRuleAssessment;
use crate::policy::SecurityPolicy;

pub fn normalize_risk(level: RiskLevel) -> RiskLevel {
    level
}

pub fn decision_from_risk(level: RiskLevel, analysis: SecurityAnalysis) -> SecurityDecision {
    match level {
        RiskLevel::High => SecurityDecision::block(level, analysis),
        RiskLevel::Medium => SecurityDecision::confirm(level, analysis),
        RiskLevel::Low => SecurityDecision::allow(level, analysis),
    }
}

pub fn analyze_without_sandbox(
    policy: &SecurityPolicy,
    fallback: Option<&FallbackRuleAssessment>,
) -> SecurityAnalysis {
    match fallback {
        Some(assessment) => analysis_from_fallback(policy, assessment),
        None => analysis_from_default_risk(policy),
    }
}

fn analysis_from_fallback(
    policy: &SecurityPolicy,
    assessment: &FallbackRuleAssessment,
) -> SecurityAnalysis {
    let reasons = if let Some(reason) = assessment.primary_rule.reason.as_ref() {
        vec![reason.clone()]
    } else {
        assessment.reasons.clone()
    };

    SecurityAnalysis {
        risk_level: normalize_risk(assessment.level),
        reasons,
        fail_open: false,
        fallback_rule_matched: true,
        matched_rule: Some(assessment.matched_rule.clone()),
        matched_paths: assessment.matched_paths.clone(),
        sandbox_off_action: Some(policy.sandbox_off_action),
        ..SecurityAnalysis::default()
    }
}

fn analysis_from_default_risk(policy: &SecurityPolicy) -> SecurityAnalysis {
    let risk_level = normalize_risk(policy.default_risk_level);

    SecurityAnalysis {
        risk_level,
        reasons: vec![format!(
            "no fallback rule matched; using default risk level {}",
            risk_level.as_str()
        )],
        fail_open: risk_level == RiskLevel::Low,
        sandbox_off_action: Some(policy.sandbox_off_action),
        ..SecurityAnalysis::default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{analyze_without_sandbox, decision_from_risk};
    use crate::decision::{RiskLevel, SandboxOffAction, SecurityDecision};
    use crate::fallback::FallbackRuleAssessment;
    use crate::policy::{PolicyRule, SecurityPolicy};
    use crate::types::MatchedRuleSummary;

    #[test]
    fn decision_from_risk_maps_to_phase_one_actions() {
        let low = decision_from_risk(RiskLevel::Low, Default::default());
        assert_eq!(
            low,
            SecurityDecision::allow(RiskLevel::Low, Default::default())
        );

        let medium = decision_from_risk(RiskLevel::Medium, Default::default());
        assert_eq!(
            medium,
            SecurityDecision::confirm(RiskLevel::Medium, Default::default())
        );

        let high = decision_from_risk(RiskLevel::High, Default::default());
        assert_eq!(
            high,
            SecurityDecision::block(RiskLevel::High, Default::default())
        );
    }

    #[test]
    fn analyze_without_sandbox_uses_fallback_assessment_when_available() {
        let policy = SecurityPolicy {
            sandbox_off_action: SandboxOffAction::Allow,
            ..SecurityPolicy::default()
        };
        let rule = PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: RiskLevel::High,
            description: None,
            operations: Some(BTreeSet::from(["DELETE".to_string()])),
            command_list: Some(BTreeSet::from(["rm".to_string()])),
            exclude: None,
            rule_id: Some("H-001".to_string()),
            name: Some("protect etc".to_string()),
            reason: Some("system config is protected".to_string()),
            confirm_message: None,
            suggestion: None,
        };
        let assessment = FallbackRuleAssessment {
            level: RiskLevel::High,
            reasons: vec!["fallback reason".to_string()],
            matched_paths: vec!["/etc".to_string()],
            matched_rule: MatchedRuleSummary {
                id: rule.rule_id.clone(),
                name: rule.name.clone(),
                pattern: rule.pattern.clone(),
            },
            primary_rule: rule,
        };

        let analysis = analyze_without_sandbox(&policy, Some(&assessment));

        assert_eq!(analysis.risk_level, RiskLevel::High);
        assert_eq!(analysis.reasons, vec!["system config is protected"]);
        assert!(analysis.fallback_rule_matched);
        assert_eq!(analysis.matched_paths, vec!["/etc"]);
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Allow));
        assert!(!analysis.fail_open);
    }

    #[test]
    fn analyze_without_sandbox_falls_back_to_default_risk() {
        let policy = SecurityPolicy {
            default_risk_level: RiskLevel::Medium,
            sandbox_off_action: SandboxOffAction::Block,
            ..SecurityPolicy::default()
        };

        let analysis = analyze_without_sandbox(&policy, None);

        assert_eq!(analysis.risk_level, RiskLevel::Medium);
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Block));
        assert_eq!(
            analysis.reasons,
            vec!["no fallback rule matched; using default risk level MEDIUM"]
        );
        assert!(!analysis.fallback_rule_matched);
        assert!(!analysis.fail_open);
    }
}
