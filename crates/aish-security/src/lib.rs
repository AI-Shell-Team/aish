pub mod decision;
pub mod fallback;
pub mod input_guard;
pub mod manager;
pub mod policy;
pub mod risk;
mod sandbox;
pub mod secret;
pub mod sudo;
pub mod types;

pub use decision::{RiskLevel, SandboxOffAction, SecurityAnalysis, SecurityDecision};
pub use fallback::{FallbackRuleAssessment, FallbackRuleEngine};
pub use manager::{SecurityManager, SecurityRequest};
pub use policy::{
    load_policy, resolve_security_policy_path, save_policy_ui_fields, try_load_policy,
    writable_security_policy_path, InvalidFallbackRule, PolicyLoadError, PolicyRule,
    SecurityPolicy, ValidationIssue,
};
pub use sandbox::{run_sandbox_daemon, run_sandbox_worker, SandboxClient};
pub use types::{FsChange, MatchedRuleSummary};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_smoke_test() {
        assert_eq!(2 + 2, 4);
    }
}
