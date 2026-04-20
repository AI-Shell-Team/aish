use crate::fallback::FallbackRuleEngine;
use crate::policy::SecurityPolicy;
use crate::sandbox_ipc::SandboxIpc;
use crate::types::{AiRiskAssessment, SandboxResult};
use aish_core::{AishError, RiskLevel, SandboxOffAction};
use std::path::Path;

/// Decision returned by the security manager for a given command.
#[derive(Debug, Clone)]
pub enum SecurityDecision {
    Allow,
    Confirm { reason: String },
    Block { reason: String },
}

/// Top-level security manager that owns a [`SecurityPolicy`] and provides
/// convenience methods for checking commands and assessing AI-generated
/// commands.
pub struct SecurityManager {
    policy: SecurityPolicy,
    fallback: FallbackRuleEngine,
    sandbox_ipc: Option<SandboxIpc>,
}

impl SecurityManager {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self {
            policy,
            fallback: FallbackRuleEngine::new(),
            sandbox_ipc: None,
        }
    }

    /// Set the sandbox IPC client for async security checks.
    pub fn with_sandbox_ipc(mut self, ipc: SandboxIpc) -> Self {
        self.sandbox_ipc = Some(ipc);
        self
    }

    /// Attempt to load a policy from the user config directory.
    ///
    /// If `config_dir` is `None`, falls back to `~/.config/aish/`.
    /// If the file does not exist, returns a default policy.
    pub fn from_config(config_dir: Option<&Path>) -> aish_core::Result<Self> {
        let dir = match config_dir {
            Some(d) => d.to_path_buf(),
            None => dirs::config_dir()
                .map(|d: std::path::PathBuf| d.join("aish"))
                .ok_or_else(|| AishError::Security("cannot determine config directory".into()))?,
        };

        let policy_path = dir.join("security_policy.yaml");
        let policy = if policy_path.exists() {
            SecurityPolicy::load(&policy_path)?
        } else {
            SecurityPolicy::default_policy()
        };

        Ok(Self {
            policy,
            fallback: FallbackRuleEngine::new(),
            sandbox_ipc: None,
        })
    }

    /// Check a command asynchronously with sandbox IPC support.
    ///
    /// This method extends the synchronous `check_command` with the ability to
    /// execute commands in the sandbox and assess the actual file system changes
    /// before making a security decision.
    ///
    /// # Decision Flow
    /// 1. Hardcoded block patterns (rm -rf /, fork bomb)
    /// 2. Policy rules (from security_policy.yaml)
    /// 3. If sandbox enabled AND IPC available → try sandbox execution → assess result
    /// 4. Hardcoded confirm patterns
    /// 5. Fallback rule engine (pattern-based assessment)
    /// 6. Apply sandbox_off_action policy when sandbox is disabled
    pub async fn check_command_async(&self, command: &str) -> SecurityDecision {
        let cmd_lower = command.to_lowercase();

        // Step 1: Check policy rules first (from security_policy.yaml)
        for rule in &self.policy.rules {
            if let Some(ref cmds) = rule.command_list {
                let first_word = command.split_whitespace().next().unwrap_or("");
                let basename = first_word.rsplit('/').next().unwrap_or(first_word);
                for cmd_pattern in cmds {
                    if basename == cmd_pattern || cmd_lower.contains(&cmd_pattern.to_lowercase()) {
                        let reason = rule
                            .reason
                            .clone()
                            .or_else(|| rule.description.clone())
                            .unwrap_or_else(|| format!("matched policy rule: {}", rule.pattern));
                        match rule.risk {
                            aish_core::RiskLevel::High => {
                                return SecurityDecision::Block { reason }
                            }
                            aish_core::RiskLevel::Medium => {
                                return SecurityDecision::Confirm { reason }
                            }
                            aish_core::RiskLevel::Low => {} // continue checking
                        }
                    }
                }
            }
        }

        // Step 2: Hard-block patterns that are almost never safe
        let block_patterns = [
            ("rm -rf /", "recursive root deletion"),
            (":(){ :|:& };:", "fork bomb"),
        ];
        for (pat, reason) in &block_patterns {
            if cmd_lower.contains(pat) {
                return SecurityDecision::Block {
                    reason: reason.to_string(),
                };
            }
        }

        // Step 3: Try sandbox execution if available and enabled
        if self.policy.enable_sandbox {
            if let Some(ref ipc) = self.sandbox_ipc {
                if ipc.is_available() {
                    // Try executing in sandbox to see what actually happens
                    match ipc.execute(command, false).await {
                        Ok(response) => {
                            // Assess the sandbox result
                            if response.blocked {
                                return SecurityDecision::Block {
                                    reason: format!(
                                        "command blocked by sandbox: {}",
                                        response.stderr.trim()
                                    ),
                                };
                            }

                            // Check if any changes are to sensitive paths
                            for change in &response.changes {
                                let path = &change.path;
                                for rule in &self.policy.rules {
                                    let pattern = &rule.pattern;
                                    // Simple pattern matching (supports ** wildcard)
                                    let pattern_normalized =
                                        pattern.replace("**", "*").replace('*', "");
                                    if path.starts_with(&pattern_normalized)
                                        || path.contains(&pattern_normalized)
                                    {
                                        match rule.risk {
                                            aish_core::RiskLevel::High => {
                                                return SecurityDecision::Block {
                                                    reason: format!(
                                                        "{}: {} {}",
                                                        change.operation,
                                                        path,
                                                        rule.reason
                                                            .as_ref()
                                                            .unwrap_or(&pattern.clone())
                                                    ),
                                                };
                                            }
                                            aish_core::RiskLevel::Medium => {
                                                return SecurityDecision::Confirm {
                                                    reason: format!(
                                                        "{}: {} {}",
                                                        change.operation,
                                                        path,
                                                        rule.reason
                                                            .as_ref()
                                                            .unwrap_or(&pattern.clone())
                                                    ),
                                                };
                                            }
                                            aish_core::RiskLevel::Low => {} // continue checking
                                        }
                                    }
                                }
                            }

                            // If sandbox execution succeeded with no violations, allow
                            if response.exit_code == 0 {
                                return SecurityDecision::Allow;
                            }
                        }
                        Err(e) => {
                            // Sandbox execution failed - fall through to other checks
                            tracing::warn!("sandbox execution failed: {}", e);
                        }
                    }
                }
            }
        }

        // Step 4: Patterns requiring confirmation
        let confirm_patterns = [
            ("rm -rf", "recursive force delete"),
            ("mkfs.", "filesystem format command"),
            ("dd if=", "raw disk write"),
            (
                "chmod -R 777",
                "recursively making everything world-writable",
            ),
            (":> /etc/", "truncating system file"),
        ];
        for (pat, reason) in &confirm_patterns {
            if cmd_lower.contains(pat) {
                return SecurityDecision::Confirm {
                    reason: reason.to_string(),
                };
            }
        }

        // Step 5: Fallback rule engine: check destructive commands against policy paths
        if let Some(assessment) = self.fallback.assess(command, &self.policy) {
            match assessment.level {
                RiskLevel::High => {
                    return SecurityDecision::Block {
                        reason: assessment.reasons.join("; "),
                    }
                }
                RiskLevel::Medium => {
                    return SecurityDecision::Confirm {
                        reason: assessment.reasons.join("; "),
                    }
                }
                RiskLevel::Low => {} // continue checking
            }
        }

        // Step 6: Use sandbox_off_action when sandbox is disabled
        if !self.policy.enable_sandbox {
            return match &self.policy.sandbox_off_action {
                SandboxOffAction::Allow => SecurityDecision::Allow,
                SandboxOffAction::Confirm => SecurityDecision::Confirm {
                    reason: "sandbox is disabled; confirmation required by policy".to_string(),
                },
                SandboxOffAction::Block => SecurityDecision::Block {
                    reason: "sandbox is disabled; blocked by policy".to_string(),
                },
            };
        }

        SecurityDecision::Allow
    }

    /// Check a command and return a security decision.
    pub fn check_command(&self, command: &str) -> SecurityDecision {
        let cmd_lower = command.to_lowercase();

        // Check policy rules first (from security_policy.yaml)
        for rule in &self.policy.rules {
            if let Some(ref cmds) = rule.command_list {
                let first_word = command.split_whitespace().next().unwrap_or("");
                let basename = first_word.rsplit('/').next().unwrap_or(first_word);
                for cmd_pattern in cmds {
                    if basename == cmd_pattern || cmd_lower.contains(&cmd_pattern.to_lowercase()) {
                        let reason = rule
                            .reason
                            .clone()
                            .or_else(|| rule.description.clone())
                            .unwrap_or_else(|| format!("matched policy rule: {}", rule.pattern));
                        match rule.risk {
                            aish_core::RiskLevel::High => {
                                return SecurityDecision::Block { reason }
                            }
                            aish_core::RiskLevel::Medium => {
                                return SecurityDecision::Confirm { reason }
                            }
                            aish_core::RiskLevel::Low => {} // continue checking
                        }
                    }
                }
            }
        }

        // Hard-block patterns that are almost never safe
        let block_patterns = [
            ("rm -rf /", "recursive root deletion"),
            (":(){ :|:& };:", "fork bomb"),
        ];
        for (pat, reason) in &block_patterns {
            if cmd_lower.contains(pat) {
                return SecurityDecision::Block {
                    reason: reason.to_string(),
                };
            }
        }

        // Patterns requiring confirmation
        let confirm_patterns = [
            ("rm -rf", "recursive force delete"),
            ("mkfs.", "filesystem format command"),
            ("dd if=", "raw disk write"),
            (
                "chmod -R 777",
                "recursively making everything world-writable",
            ),
            (":> /etc/", "truncating system file"),
        ];
        for (pat, reason) in &confirm_patterns {
            if cmd_lower.contains(pat) {
                return SecurityDecision::Confirm {
                    reason: reason.to_string(),
                };
            }
        }

        // Fallback rule engine: check destructive commands against policy paths
        if let Some(assessment) = self.fallback.assess(command, &self.policy) {
            match assessment.level {
                RiskLevel::High => {
                    return SecurityDecision::Block {
                        reason: assessment.reasons.join("; "),
                    }
                }
                RiskLevel::Medium => {
                    return SecurityDecision::Confirm {
                        reason: assessment.reasons.join("; "),
                    }
                }
                RiskLevel::Low => {} // continue checking
            }
        }

        // Use sandbox_off_action when sandbox is disabled
        if !self.policy.enable_sandbox {
            return match &self.policy.sandbox_off_action {
                SandboxOffAction::Allow => SecurityDecision::Allow,
                SandboxOffAction::Confirm => SecurityDecision::Confirm {
                    reason: "sandbox is disabled; confirmation required by policy".to_string(),
                },
                SandboxOffAction::Block => SecurityDecision::Block {
                    reason: "sandbox is disabled; blocked by policy".to_string(),
                },
            };
        }

        SecurityDecision::Allow
    }

    /// Assess risk of an AI-generated command, optionally using sandbox results.
    pub fn assess_ai_command(
        &self,
        command: &str,
        sandbox_result: Option<&SandboxResult>,
    ) -> AiRiskAssessment {
        self.policy.assess_risk(command, sandbox_result)
    }

    /// Access the underlying policy.
    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_command_block() {
        let mgr = SecurityManager::new(SecurityPolicy::default_policy());
        let decision = mgr.check_command("rm -rf /");
        assert!(matches!(decision, SecurityDecision::Block { .. }));
    }

    #[test]
    fn test_check_command_confirm() {
        let mut policy = SecurityPolicy::default_policy();
        policy.enable_sandbox = true;
        let mgr = SecurityManager::new(policy);
        let decision = mgr.check_command("rm -rf old_dir");
        assert!(matches!(decision, SecurityDecision::Confirm { .. }));
    }

    #[test]
    fn test_check_command_allow() {
        let mgr = SecurityManager::new(SecurityPolicy::default_policy());
        let decision = mgr.check_command("ls -la");
        assert!(matches!(decision, SecurityDecision::Allow));
    }

    #[test]
    fn test_check_command_policy_block() {
        let mut policy = SecurityPolicy::default_policy();
        policy.rules.push(crate::types::PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: aish_core::RiskLevel::High,
            description: Some("system files".to_string()),
            command_list: Some(vec!["mkfs".to_string()]),
            reason: Some("filesystem format blocked by policy".to_string()),
            ..Default::default()
        });
        let mgr = SecurityManager::new(policy);
        let decision = mgr.check_command("mkfs.ext4 /dev/sda1");
        assert!(matches!(decision, SecurityDecision::Block { .. }));
    }

    #[test]
    fn test_check_command_policy_confirm() {
        let mut policy = SecurityPolicy::default_policy();
        policy.rules.push(crate::types::PolicyRule {
            pattern: "/data/**".to_string(),
            risk: aish_core::RiskLevel::Medium,
            description: Some("data directory".to_string()),
            command_list: Some(vec!["reboot".to_string()]),
            reason: Some("reboot requires confirmation by policy".to_string()),
            ..Default::default()
        });
        let mgr = SecurityManager::new(policy);
        let decision = mgr.check_command("reboot");
        assert!(matches!(decision, SecurityDecision::Confirm { .. }));
    }

    #[test]
    fn test_check_command_policy_low_continues() {
        // A Low-risk policy rule should not block or confirm; fall through to
        // the hardcoded checks.
        let mut policy = SecurityPolicy::default_policy();
        policy.rules.push(crate::types::PolicyRule {
            pattern: "/tmp/**".to_string(),
            risk: aish_core::RiskLevel::Low,
            command_list: Some(vec!["rm".to_string()]),
            reason: Some("low risk rm".to_string()),
            ..Default::default()
        });
        let mgr = SecurityManager::new(policy);
        // "rm -rf old_dir" should hit the hardcoded confirm pattern
        let decision = mgr.check_command("rm -rf old_dir");
        assert!(matches!(decision, SecurityDecision::Confirm { .. }));
    }

    #[test]
    fn test_check_command_policy_block_overrides_hardcoded() {
        // Policy High-risk rule should trigger before hardcoded confirm patterns
        let mut policy = SecurityPolicy::default_policy();
        policy.rules.push(crate::types::PolicyRule {
            pattern: "/etc/**".to_string(),
            risk: aish_core::RiskLevel::High,
            command_list: Some(vec!["dd".to_string()]),
            reason: Some("dd blocked by policy".to_string()),
            ..Default::default()
        });
        let mgr = SecurityManager::new(policy);
        let decision = mgr.check_command("dd if=/dev/zero of=/dev/sda");
        assert!(matches!(decision, SecurityDecision::Block { .. }));
    }
}
