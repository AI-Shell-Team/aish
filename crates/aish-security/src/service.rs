use crate::decision::{RiskLevel, SecurityAnalysis, SecurityDecision};
use crate::fallback::FallbackRuleEngine;
use crate::policy::SecurityPolicy;
use crate::risk::{analyze_without_sandbox, decision_from_risk};

#[derive(Debug, Clone)]
pub struct SecurityManager {
    policy: SecurityPolicy,
    fallback_engine: FallbackRuleEngine,
}

impl SecurityManager {
    pub fn new(policy: SecurityPolicy) -> Self {
        let fallback_engine = FallbackRuleEngine::new(policy.clone());
        Self {
            policy,
            fallback_engine,
        }
    }

    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }

    pub fn analyze(&self, command: &str, is_ai_command: bool) -> (RiskLevel, SecurityAnalysis) {
        if !is_ai_command {
            return (RiskLevel::Low, SecurityAnalysis::default());
        }

        let fallback = self.fallback_engine.assess_disabled_command(command);
        let analysis = analyze_without_sandbox(&self.policy, fallback.as_ref());
        (analysis.risk_level, analysis)
    }

    pub fn decide(&self, command: &str, is_ai_command: bool) -> SecurityDecision {
        let (level, analysis) = self.analyze(command, is_ai_command);
        decision_from_risk(level, analysis)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::SecurityManager;
    use crate::decision::{RiskLevel, SandboxOffAction};
    use crate::policy::{PolicyRule, SecurityPolicy};

    #[test]
    fn non_ai_commands_default_to_low_allow() {
        let manager = SecurityManager::new(SecurityPolicy::default());

        let decision = manager.decide("echo hi", false);

        assert_eq!(decision.level, RiskLevel::Low);
        assert!(decision.allow);
        assert!(!decision.require_confirmation);
    }

    #[test]
    fn default_high_risk_blocks_without_rule_match() {
        let manager = SecurityManager::new(SecurityPolicy {
            default_risk_level: RiskLevel::High,
            ..SecurityPolicy::default()
        });

        let decision = manager.decide("echo hi", true);

        assert_eq!(decision.level, RiskLevel::High);
        assert!(!decision.allow);
        assert!(!decision.require_confirmation);
        assert_eq!(
            decision.analysis.reasons,
            vec!["no fallback rule matched; using default risk level HIGH"]
        );
    }

    #[test]
    fn fallback_medium_risk_requires_confirmation() {
        let manager = SecurityManager::new(SecurityPolicy {
            enable_sandbox: false,
            rules: vec![PolicyRule {
                pattern: "/home/**".to_string(),
                risk: RiskLevel::Medium,
                description: None,
                operations: Some(BTreeSet::from(["WRITE".to_string()])),
                command_list: Some(BTreeSet::from(["cp".to_string()])),
                exclude: None,
                rule_id: Some("M-001".to_string()),
                name: Some("protect home".to_string()),
                reason: Some("home path is protected".to_string()),
                confirm_message: None,
                suggestion: None,
            }],
            sandbox_off_action: SandboxOffAction::Allow,
            ..SecurityPolicy::default()
        });

        let decision = manager.decide("cp /tmp/a.txt /home/lixin/a.txt", true);

        assert_eq!(decision.level, RiskLevel::Medium);
        assert!(decision.allow);
        assert!(decision.require_confirmation);
        assert!(decision.analysis.fallback_rule_matched);
        assert_eq!(decision.analysis.matched_paths, vec!["/home/lixin/a.txt"]);
    }

    #[test]
    fn default_low_risk_marks_fail_open() {
        let manager = SecurityManager::new(SecurityPolicy {
            default_risk_level: RiskLevel::Low,
            sandbox_off_action: SandboxOffAction::Confirm,
            ..SecurityPolicy::default()
        });

        let (_level, analysis) = manager.analyze("echo hi", true);

        assert!(analysis.fail_open);
        assert_eq!(analysis.sandbox_off_action, Some(SandboxOffAction::Confirm));
    }
}