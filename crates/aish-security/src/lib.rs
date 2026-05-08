pub mod decision;
pub mod fallback;
pub mod manager;
pub mod policy;
pub mod risk;
mod sandbox;
pub mod sudo;
pub mod types;

pub use decision::{RiskLevel, SandboxOffAction, SecurityAnalysis, SecurityDecision};
pub use fallback::{FallbackRuleAssessment, FallbackRuleEngine};
pub use manager::{SecurityManager, SecurityRequest};
pub use policy::{
    load_policy, resolve_security_policy_path, InvalidFallbackRule, PolicyRule, SecurityPolicy,
    ValidationIssue,
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
