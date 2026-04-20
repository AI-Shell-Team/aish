use aish_core::RiskLevel;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    pub pattern: String,
    pub risk: RiskLevel,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub operations: Option<Vec<String>>,
    #[serde(default)]
    pub command_list: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub confirm_message: Option<String>,
    #[serde(default)]
    pub suggestion: Option<String>,
}

impl Default for PolicyRule {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            risk: RiskLevel::Low,
            description: None,
            operations: None,
            command_list: None,
            exclude: None,
            rule_id: None,
            name: None,
            reason: None,
            confirm_message: None,
            suggestion: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsChange {
    pub path: String,
    pub operation: String, // "write", "delete", "create", "modify"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRiskAssessment {
    pub level: RiskLevel,
    pub reasons: Vec<String>,
    pub changes: Vec<FsChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxResult {
    pub success: bool,
    pub changes: Vec<FsChange>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
