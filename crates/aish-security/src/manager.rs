use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::decision::{RiskLevel, SecurityAnalysis, SecurityDecision};
use crate::fallback::FallbackRuleEngine;
use crate::policy::SecurityPolicy;
use crate::risk::{analyze_without_sandbox, decision_from_risk};
use crate::sandbox::assess::assess_sandbox_result;
use crate::sandbox::degraded::{analyze_sandbox_degraded, SandboxDegradedDetails};
use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::ipc::client::SandboxRunner;
use crate::sandbox::types::SandboxRunRequest;
use crate::secret::SecretScanner;
use crate::SandboxClient;

const DEFAULT_SANDBOX_SOCKET_PATH: &str = "/run/aish/sandbox.sock";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SecurityRequest {
    cwd: Option<PathBuf>,
    repo_root: Option<PathBuf>,
    sandbox_socket_path: Option<PathBuf>,
    sandbox_timeout_s: Option<f64>,
    is_ai_command: bool,
}

impl SecurityRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ai_command() -> Self {
        Self {
            is_ai_command: true,
            ..Self::default()
        }
    }

    pub fn is_ai_command(&self) -> bool {
        self.is_ai_command
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn repo_root(&self) -> Option<&Path> {
        self.repo_root.as_deref()
    }

    pub fn sandbox_socket_path(&self) -> Option<&Path> {
        self.sandbox_socket_path.as_deref()
    }

    pub fn sandbox_timeout_s(&self) -> Option<f64> {
        self.sandbox_timeout_s
    }

    pub fn with_ai_command(mut self, is_ai_command: bool) -> Self {
        self.is_ai_command = is_ai_command;
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_repo_root(mut self, repo_root: impl Into<PathBuf>) -> Self {
        self.repo_root = Some(repo_root.into());
        self
    }

    pub fn with_sandbox_socket_path(mut self, socket_path: impl Into<PathBuf>) -> Self {
        self.sandbox_socket_path = Some(socket_path.into());
        self
    }

    pub fn with_sandbox_timeout_s(mut self, timeout_s: f64) -> Self {
        self.sandbox_timeout_s = Some(timeout_s);
        self
    }
}

#[derive(Clone)]
pub struct SecurityManager {
    policy: SecurityPolicy,
    fallback_engine: FallbackRuleEngine,
    sandbox_runner: Option<Arc<dyn SandboxRunner>>,
    secret_scanner: SecretScanner,
}

impl SecurityManager {
    pub fn new(policy: SecurityPolicy) -> Self {
        let fallback_engine = FallbackRuleEngine::new(policy.clone());
        let sandbox_runner = policy.enable_sandbox.then(|| {
            Arc::new(SandboxClient::new(DEFAULT_SANDBOX_SOCKET_PATH)) as Arc<dyn SandboxRunner>
        });
        let secret_scanner = SecretScanner::new(&policy.secret_patterns);
        Self {
            policy,
            fallback_engine,
            sandbox_runner,
            secret_scanner,
        }
    }

    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }

    pub fn secret_scanner(&self) -> &SecretScanner {
        &self.secret_scanner
    }

    pub fn with_sandbox_client(mut self, client: SandboxClient) -> Self {
        self.sandbox_runner = Some(Arc::new(client));
        self
    }

    /// Replace the active policy and rebuild derived state (fallback engine,
    /// sandbox runner). The secret scanner is intentionally **preserved** —
    /// `/setting` only edits the global fields (`enable_sandbox`,
    /// `default_risk_level`, `sandbox_off_action`,
    /// `sandbox_timeout_seconds`, `input_guard.enabled`), never
    /// `secret_patterns`. Callers that need to swap secret patterns must
    /// construct a fresh `SecurityManager` instead, or the scanner will miss
    /// the new patterns.
    pub fn set_policy(&mut self, policy: SecurityPolicy) {
        let sandbox_runner = policy.enable_sandbox.then(|| {
            Arc::new(SandboxClient::new(DEFAULT_SANDBOX_SOCKET_PATH)) as Arc<dyn SandboxRunner>
        });
        let fallback_engine = FallbackRuleEngine::new(policy.clone());
        self.policy = policy;
        self.fallback_engine = fallback_engine;
        self.sandbox_runner = sandbox_runner;
    }

    #[cfg(test)]
    fn with_sandbox_runner(mut self, runner: impl SandboxRunner + 'static) -> Self {
        self.sandbox_runner = Some(Arc::new(runner));
        self
    }

    pub fn analyze(&self, command: &str, is_ai_command: bool) -> (RiskLevel, SecurityAnalysis) {
        self.analyze_with_request(
            command,
            &SecurityRequest::new().with_ai_command(is_ai_command),
        )
    }

    pub fn analyze_with_request(
        &self,
        command: &str,
        request: &SecurityRequest,
    ) -> (RiskLevel, SecurityAnalysis) {
        if !request.is_ai_command() {
            return (RiskLevel::Low, SecurityAnalysis::default());
        }

        if self.policy.enable_sandbox {
            let analysis = self.analyze_with_sandbox(command, request);
            return (analysis.risk_level, analysis);
        }

        let fallback = self.fallback_engine.assess_disabled_command(command);
        let analysis = analyze_without_sandbox(&self.policy, fallback.as_ref());
        (analysis.risk_level, analysis)
    }

    pub fn decide(&self, command: &str, is_ai_command: bool) -> SecurityDecision {
        self.decide_with_request(
            command,
            &SecurityRequest::new().with_ai_command(is_ai_command),
        )
    }

    pub fn decide_with_request(
        &self,
        command: &str,
        request: &SecurityRequest,
    ) -> SecurityDecision {
        let (level, analysis) = self.analyze_with_request(command, request);
        decision_from_risk(level, analysis)
    }

    /// Check AI input text for sensitive data.
    /// Returns `Confirm` if secrets are detected, `Allow` otherwise.
    pub fn check_ai_input(&self, input: &str) -> SecurityDecision {
        let secrets = self.secret_scanner.scan(input);
        if secrets.is_empty() {
            return SecurityDecision::allow(RiskLevel::Low, SecurityAnalysis::default());
        }

        let analysis = SecurityAnalysis {
            risk_level: RiskLevel::Medium,
            reasons: secrets.iter().map(|s| s.format_reason()).collect(),
            detected_secrets: Some(secrets),
            ..SecurityAnalysis::default()
        };
        SecurityDecision::confirm(RiskLevel::Medium, analysis)
    }

    fn analyze_with_sandbox(&self, command: &str, request: &SecurityRequest) -> SecurityAnalysis {
        let cwd = request
            .cwd()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_cwd);
        let repo_root = request
            .repo_root()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));

        if cwd.strip_prefix(&repo_root).is_err() {
            return analyze_sandbox_degraded(
                &self.policy,
                &self.fallback_engine,
                command,
                SandboxReason::CwdOutsideRepoRoot,
                SandboxDegradedDetails::default()
                    .with_cwd(&cwd)
                    .with_repo_root(&repo_root),
            );
        }

        let sandbox_request = SandboxRunRequest {
            id: next_request_id(),
            command: command.to_string(),
            cwd,
            repo_root,
            client_pid: std::process::id(),
            timeout_s: request
                .sandbox_timeout_s()
                .unwrap_or(self.policy.sandbox_timeout_seconds),
        };

        let sandbox_result = if let Some(socket_path) = request.sandbox_socket_path() {
            SandboxClient::new(socket_path).simulate(&sandbox_request)
        } else if let Some(runner) = &self.sandbox_runner {
            runner.simulate(&sandbox_request)
        } else {
            return analyze_sandbox_degraded(
                &self.policy,
                &self.fallback_engine,
                command,
                SandboxReason::SandboxDisabled,
                SandboxDegradedDetails::default(),
            );
        };

        match sandbox_result {
            Ok(result) => assess_sandbox_result(&self.policy, command, &result),
            Err(error) => analyze_sandbox_degraded(
                &self.policy,
                &self.fallback_engine,
                command,
                error.reason(),
                details_from_error(error),
            ),
        }
    }
}

impl std::fmt::Debug for SecurityManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecurityManager")
            .field("policy", &self.policy)
            .field("fallback_engine", &self.fallback_engine)
            .field("sandbox_runner", &self.sandbox_runner.is_some())
            .field("secret_scanner", &"<SecretScanner>")
            .finish()
    }
}

fn default_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

fn next_request_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("sandbox-{}-{timestamp}", std::process::id())
}

fn details_from_error(error: SandboxError) -> SandboxDegradedDetails {
    let fallback_detail = error.reason().as_str().to_string();
    SandboxDegradedDetails::default().with_error(error.details().unwrap_or(&fallback_detail))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{SecurityManager, SecurityRequest};
    use crate::decision::{RiskLevel, SandboxOffAction};
    use crate::policy::{PolicyRule, SecurityPolicy};
    use crate::sandbox::error::{SandboxError, SandboxReason};
    use crate::sandbox::ipc::client::SandboxRunner;
    use crate::sandbox::types::{FsChange, FsChangeKind, SandboxResult, SandboxRunRequest};

    #[derive(Clone)]
    struct FakeSandboxRunner {
        calls: Arc<Mutex<Vec<SandboxRunRequest>>>,
        result: Result<SandboxResult, SandboxError>,
    }

    impl FakeSandboxRunner {
        fn success(result: SandboxResult) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                result: Ok(result),
            }
        }

        fn failure(error: SandboxError) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                result: Err(error),
            }
        }

        fn calls(&self) -> Vec<SandboxRunRequest> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SandboxRunner for FakeSandboxRunner {
        fn simulate(&self, request: &SandboxRunRequest) -> Result<SandboxResult, SandboxError> {
            self.calls.lock().unwrap().push(request.clone());
            self.result.clone()
        }
    }

    fn sandbox_result_with_change(path: &str, kind: FsChangeKind) -> SandboxResult {
        SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            changes: vec![FsChange {
                path: path.to_string(),
                kind,
                detail: None,
            }],
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        }
    }

    #[test]
    fn security_request_tracks_ai_flag_and_cwd() {
        let request = SecurityRequest::ai_command()
            .with_cwd("/tmp/worktree")
            .with_repo_root("/tmp")
            .with_sandbox_socket_path("/tmp/aish.sock")
            .with_sandbox_timeout_s(12.5);

        assert!(request.is_ai_command());
        assert_eq!(request.cwd(), Some(Path::new("/tmp/worktree")));
        assert_eq!(request.repo_root(), Some(Path::new("/tmp")));
        assert_eq!(
            request.sandbox_socket_path(),
            Some(Path::new("/tmp/aish.sock"))
        );
        assert_eq!(request.sandbox_timeout_s(), Some(12.5));
    }

    #[test]
    fn non_ai_commands_default_to_low_allow() {
        let manager = SecurityManager::new(SecurityPolicy::default());

        let decision = manager.decide("echo hi", false);

        assert_eq!(decision.level, RiskLevel::Low);
        assert!(decision.allow);
        assert!(!decision.require_confirmation);
    }

    /// `set_policy` must rebuild the sandbox runner when `enable_sandbox`
    /// flips on, so a live `/setting` toggle takes effect immediately.
    #[test]
    fn set_policy_rebuilds_sandbox_runner_when_enabled() {
        let mut manager = SecurityManager::new(SecurityPolicy::default());
        // Starts disabled (no sandbox runner).
        assert!(!manager.policy().enable_sandbox);
        assert!(manager.sandbox_runner.is_none());

        manager.set_policy(SecurityPolicy {
            enable_sandbox: true,
            default_risk_level: RiskLevel::Medium,
            ..SecurityPolicy::default()
        });

        assert!(manager.policy().enable_sandbox);
        assert_eq!(manager.policy().default_risk_level, RiskLevel::Medium);
        assert!(
            manager.sandbox_runner.is_some(),
            "enabling sandbox must construct a runner"
        );

        // Flipping back off drops the runner.
        manager.set_policy(SecurityPolicy::default());
        assert!(!manager.policy().enable_sandbox);
        assert!(manager.sandbox_runner.is_none());
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

    #[test]
    fn request_based_api_matches_existing_decision_flow() {
        let manager = SecurityManager::new(SecurityPolicy {
            enable_sandbox: false,
            rules: vec![PolicyRule {
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
            }],
            ..SecurityPolicy::default()
        });
        let request = SecurityRequest::ai_command().with_cwd("/tmp/repo");

        let decision = manager.decide_with_request("rm -rf /etc", &request);

        assert_eq!(decision.level, RiskLevel::High);
        assert!(!decision.allow);
        assert!(!decision.require_confirmation);
        assert!(decision.analysis.fallback_rule_matched);
    }

    #[test]
    fn sandbox_success_drives_assess_and_blocks_high_risk_change() {
        let runner = FakeSandboxRunner::success(sandbox_result_with_change(
            "/etc/aish/config.yaml",
            FsChangeKind::Deleted,
        ));
        let manager = SecurityManager::new(SecurityPolicy {
            enable_sandbox: true,
            rules: vec![PolicyRule {
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
            }],
            ..SecurityPolicy::default()
        })
        .with_sandbox_runner(runner.clone());
        let request = SecurityRequest::ai_command()
            .with_cwd("/repo")
            .with_repo_root("/repo")
            .with_sandbox_timeout_s(9.0);

        let decision = manager.decide_with_request("rm -rf /etc/aish", &request);

        assert_eq!(decision.level, RiskLevel::High);
        assert!(!decision.allow);
        assert!(decision.analysis.sandbox.enabled);
        assert_eq!(
            decision.analysis.matched_paths,
            vec!["/etc/aish/config.yaml"]
        );
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].command, "rm -rf /etc/aish");
        assert_eq!(calls[0].cwd, std::path::PathBuf::from("/repo"));
        assert_eq!(calls[0].repo_root, std::path::PathBuf::from("/repo"));
        assert_eq!(calls[0].timeout_s, 9.0);
    }

    #[test]
    fn sandbox_failure_enters_degraded_semantics() {
        let runner = FakeSandboxRunner::failure(SandboxError::with_details(
            SandboxReason::SandboxIpcTimeout,
            "read timed out",
        ));
        let manager = SecurityManager::new(SecurityPolicy {
            enable_sandbox: true,
            sandbox_off_action: SandboxOffAction::Allow,
            ..SecurityPolicy::default()
        })
        .with_sandbox_runner(runner);
        let request = SecurityRequest::ai_command()
            .with_cwd("/repo")
            .with_repo_root("/repo");

        let decision = manager.decide_with_request("echo hi", &request);

        assert_eq!(decision.level, RiskLevel::Medium);
        assert!(decision.allow);
        assert!(decision.require_confirmation);
        assert_eq!(
            decision.analysis.sandbox.reason.as_deref(),
            Some("sandbox_ipc_timeout")
        );
        assert_eq!(
            decision.analysis.sandbox.error.as_deref(),
            Some("read timed out")
        );
        assert_eq!(
            decision.analysis.sandbox_off_action,
            Some(SandboxOffAction::Confirm)
        );
    }

    #[test]
    fn sandbox_non_zero_exit_with_no_changes_uses_assessed_result() {
        let runner = FakeSandboxRunner::success(crate::sandbox::types::SandboxResult {
            exit_code: 2,
            stdout: String::new(),
            stderr: "ls: cannot access '/tmp/missing': No such file or directory".to_string(),
            changes: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        });
        let manager = SecurityManager::new(SecurityPolicy {
            enable_sandbox: true,
            default_risk_level: RiskLevel::Low,
            ..SecurityPolicy::default()
        })
        .with_sandbox_runner(runner);
        let request = SecurityRequest::ai_command()
            .with_cwd("/repo")
            .with_repo_root("/repo");

        let decision = manager.decide_with_request("ls /tmp/missing", &request);

        assert_eq!(decision.level, RiskLevel::Low);
        assert!(decision.allow);
        assert!(!decision.require_confirmation);
        assert!(decision.analysis.sandbox.enabled);
        assert_eq!(decision.analysis.sandbox.exit_code, Some(2));
        assert_eq!(
            decision.analysis.reasons,
            vec!["sandbox observed no filesystem changes; using default risk level LOW"]
        );
    }

    #[test]
    fn cwd_outside_repo_root_degrades_without_calling_runner() {
        let runner = FakeSandboxRunner::success(sandbox_result_with_change(
            "/tmp/file.txt",
            FsChangeKind::Created,
        ));
        let manager = SecurityManager::new(SecurityPolicy {
            enable_sandbox: true,
            sandbox_off_action: SandboxOffAction::Block,
            ..SecurityPolicy::default()
        })
        .with_sandbox_runner(runner.clone());
        let request = SecurityRequest::ai_command()
            .with_cwd("/tmp/outside")
            .with_repo_root("/repo");

        let decision = manager.decide_with_request("echo hi", &request);

        assert_eq!(decision.level, RiskLevel::High);
        assert_eq!(runner.calls().len(), 0);
        assert_eq!(
            decision.analysis.sandbox.reason.as_deref(),
            Some("cwd_outside_repo_root")
        );
        assert_eq!(
            decision.analysis.sandbox.cwd.as_deref(),
            Some("/tmp/outside")
        );
        assert_eq!(
            decision.analysis.sandbox.repo_root.as_deref(),
            Some("/repo")
        );
    }

    #[test]
    fn check_ai_input_allows_clean_text() {
        let manager = SecurityManager::new(SecurityPolicy::default());
        let decision = manager.check_ai_input("list files in current directory");
        assert!(decision.allow);
        assert!(!decision.require_confirmation);
    }

    #[test]
    fn check_ai_input_confirms_on_secret() {
        let manager = SecurityManager::new(SecurityPolicy::default());
        let decision = manager.check_ai_input(
            "use key sk-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ123456",
        );
        assert!(decision.allow);
        assert!(decision.require_confirmation);
        let secrets = decision.analysis.detected_secrets.unwrap();
        assert!(!secrets.is_empty());
        assert_eq!(secrets[0].secret_type, crate::secret::SecretType::ApiKey);
    }

    #[test]
    fn check_ai_input_with_custom_pattern() {
        let policy = SecurityPolicy {
            secret_patterns: vec![crate::secret::CustomPattern {
                name: "MyToken".to_string(),
                pattern: r"mytoken_[a-z]{8}".to_string(),
                secret_type: crate::secret::SecretType::Token,
            }],
            ..Default::default()
        };
        let manager = SecurityManager::new(policy);
        let decision = manager.check_ai_input("here is mytoken_abcdefgh ok");
        assert!(decision.require_confirmation);
        let secrets = decision.analysis.detected_secrets.unwrap();
        assert_eq!(secrets[0].pattern_name, "MyToken");
    }
}
