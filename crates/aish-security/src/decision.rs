use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::{FsChange, MatchedRuleSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum RiskLevel {
    #[default]
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum SandboxOffAction {
    #[default]
    Allow,
    Confirm,
    Block,
}

impl SandboxOffAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "ALLOW",
            Self::Confirm => "CONFIRM",
            Self::Block => "BLOCK",
        }
    }
}

impl fmt::Display for SandboxOffAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SandboxStatus {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityAnalysis {
    #[serde(default)]
    pub risk_level: RiskLevel,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub changes: Vec<FsChange>,
    #[serde(default)]
    pub impact_description: String,
    #[serde(default)]
    pub suggested_alternatives: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_message: Option<String>,
    #[serde(default)]
    pub fail_open: bool,
    #[serde(default)]
    pub fallback_rule_matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<MatchedRuleSummary>,
    #[serde(default)]
    pub matched_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_off_action: Option<SandboxOffAction>,
    #[serde(default)]
    pub sandbox: SandboxStatus,
}

impl Default for SecurityAnalysis {
    fn default() -> Self {
        Self {
            risk_level: RiskLevel::Low,
            reasons: Vec::new(),
            changes: Vec::new(),
            impact_description: String::new(),
            suggested_alternatives: Vec::new(),
            confirm_message: None,
            fail_open: false,
            fallback_rule_matched: false,
            matched_rule: None,
            matched_paths: Vec::new(),
            sandbox_off_action: None,
            sandbox: SandboxStatus::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityDecision {
    pub level: RiskLevel,
    pub allow: bool,
    pub require_confirmation: bool,
    pub analysis: SecurityAnalysis,
}

impl SecurityDecision {
    pub fn allow(level: RiskLevel, analysis: SecurityAnalysis) -> Self {
        Self {
            level,
            allow: true,
            require_confirmation: false,
            analysis,
        }
    }

    pub fn confirm(level: RiskLevel, analysis: SecurityAnalysis) -> Self {
        Self {
            level,
            allow: true,
            require_confirmation: true,
            analysis,
        }
    }

    pub fn block(level: RiskLevel, analysis: SecurityAnalysis) -> Self {
        Self {
            level,
            allow: false,
            require_confirmation: false,
            analysis,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RiskLevel, SandboxOffAction, SandboxStatus, SecurityAnalysis, SecurityDecision};

    #[test]
    fn risk_level_display_uses_python_compatible_values() {
        assert_eq!(RiskLevel::Low.to_string(), "LOW");
        assert_eq!(RiskLevel::Medium.to_string(), "MEDIUM");
        assert_eq!(RiskLevel::High.to_string(), "HIGH");
    }

    #[test]
    fn sandbox_off_action_display_uses_python_compatible_values() {
        assert_eq!(SandboxOffAction::Allow.to_string(), "ALLOW");
        assert_eq!(SandboxOffAction::Confirm.to_string(), "CONFIRM");
        assert_eq!(SandboxOffAction::Block.to_string(), "BLOCK");
    }

    #[test]
    fn decision_constructors_set_expected_flags() {
        let analysis = SecurityAnalysis::default();

        let allow = SecurityDecision::allow(RiskLevel::Low, analysis.clone());
        assert!(allow.allow);
        assert!(!allow.require_confirmation);

        let confirm = SecurityDecision::confirm(RiskLevel::Medium, analysis.clone());
        assert!(confirm.allow);
        assert!(confirm.require_confirmation);

        let block = SecurityDecision::block(RiskLevel::High, analysis);
        assert!(!block.allow);
        assert!(!block.require_confirmation);
    }

    #[test]
    fn sandbox_status_defaults_to_disabled_without_details() {
        assert!(!SandboxStatus::default().enabled);
        assert_eq!(SandboxStatus::default().reason, None);
        assert_eq!(SandboxStatus::default().error, None);
        assert_eq!(SandboxStatus::default().exit_code, None);
    }
}
