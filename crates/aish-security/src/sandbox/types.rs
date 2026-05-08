use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) type SandboxChangeDetail = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FsChangeKind {
    Created,
    Modified,
    Deleted,
    Chmod,
    Chown,
    Unknown,
}

impl FsChangeKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Chmod => "chmod",
            Self::Chown => "chown",
            Self::Unknown => "unknown",
        }
    }
}

impl Default for FsChangeKind {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FsChange {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) kind: FsChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<SandboxChangeDetail>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SandboxResult {
    pub(crate) exit_code: i32,
    #[serde(default)]
    pub(crate) stdout: String,
    #[serde(default)]
    pub(crate) stderr: String,
    #[serde(default)]
    pub(crate) changes: Vec<FsChange>,
    #[serde(default)]
    pub(crate) stdout_truncated: bool,
    #[serde(default)]
    pub(crate) stderr_truncated: bool,
    #[serde(default)]
    pub(crate) changes_truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SandboxSecurityResult {
    #[serde(default)]
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    pub(crate) sandbox: SandboxResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SandboxRunRequest {
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    pub(crate) repo_root: PathBuf,
    pub(crate) client_pid: u32,
    pub(crate) timeout_s: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SandboxLimits {
    pub(crate) request_bytes: usize,
    pub(crate) response_bytes: usize,
    pub(crate) stdout_bytes: usize,
    pub(crate) stderr_bytes: usize,
    pub(crate) changes_max: usize,
    pub(crate) timeout_min_s: u64,
    pub(crate) timeout_max_s: u64,
}

impl SandboxLimits {
    pub(crate) const DEFAULT_REQUEST_BYTES: usize = 1024 * 1024;
    pub(crate) const DEFAULT_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
    pub(crate) const DEFAULT_STDOUT_BYTES: usize = 2 * 1024 * 1024;
    pub(crate) const DEFAULT_STDERR_BYTES: usize = 2 * 1024 * 1024;
    pub(crate) const DEFAULT_CHANGES_MAX: usize = 10_000;
    pub(crate) const DEFAULT_TIMEOUT_MIN_S: u64 = 1;
    pub(crate) const DEFAULT_TIMEOUT_MAX_S: u64 = 300;

    pub(crate) fn clamp_timeout_s(self, timeout_s: f64) -> f64 {
        let timeout_s = if timeout_s.is_finite() {
            timeout_s
        } else {
            0.0
        };
        timeout_s.clamp(self.timeout_min_s as f64, self.timeout_max_s as f64)
    }
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            request_bytes: Self::DEFAULT_REQUEST_BYTES,
            response_bytes: Self::DEFAULT_RESPONSE_BYTES,
            stdout_bytes: Self::DEFAULT_STDOUT_BYTES,
            stderr_bytes: Self::DEFAULT_STDERR_BYTES,
            changes_max: Self::DEFAULT_CHANGES_MAX,
            timeout_min_s: Self::DEFAULT_TIMEOUT_MIN_S,
            timeout_max_s: Self::DEFAULT_TIMEOUT_MAX_S,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RequestIdentity {
    pub(crate) pid: u32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

impl RequestIdentity {
    pub(crate) const fn from_peer_credentials(pid: u32, uid: u32, gid: u32) -> Self {
        Self { pid, uid, gid }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PayloadIdentity {
    User { uid: u32, gid: u32 },
    Root,
}

impl PayloadIdentity {
    pub(crate) const fn from_request_identity(
        identity: RequestIdentity,
        run_as_root: bool,
    ) -> Self {
        if run_as_root {
            Self::Root
        } else {
            Self::User {
                uid: identity.uid,
                gid: identity.gid,
            }
        }
    }

    pub(crate) const fn is_root(self) -> bool {
        matches!(self, Self::Root)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SandboxRunContext {
    pub(crate) request: SandboxRunRequest,
    pub(crate) limits: SandboxLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) socket_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) request_identity: Option<RequestIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) payload_identity: Option<PayloadIdentity>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl SandboxDeadline {
    pub(crate) fn from_timeout_s(timeout_s: f64, limits: SandboxLimits) -> Self {
        let clamped = limits.clamp_timeout_s(timeout_s);
        Self {
            started_at: Instant::now(),
            timeout: Duration::from_secs_f64(clamped),
        }
    }

    pub(crate) fn with_started_at(started_at: Instant, timeout: Duration) -> Self {
        Self {
            started_at,
            timeout,
        }
    }

    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.timeout.checked_sub(self.started_at.elapsed())
    }

    pub(crate) fn is_expired(&self) -> bool {
        self.remaining().is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{
        FsChangeKind, PayloadIdentity, RequestIdentity, SandboxDeadline, SandboxLimits,
        SandboxResult,
    };

    #[test]
    fn fs_change_kind_serializes_to_lowercase_values() {
        assert_eq!(
            serde_json::to_string(&FsChangeKind::Created).unwrap(),
            "\"created\""
        );
        assert_eq!(
            serde_json::to_string(&FsChangeKind::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn sandbox_result_defaults_truncation_flags_when_missing() {
        let value = json!({
            "exit_code": 0,
            "stdout": "",
            "stderr": "",
            "changes": [{"path": "/tmp/x", "kind": "created"}]
        });

        let result: SandboxResult = serde_json::from_value(value).unwrap();
        assert!(!result.stdout_truncated);
        assert!(!result.stderr_truncated);
        assert!(!result.changes_truncated);
        assert_eq!(result.changes[0].kind, FsChangeKind::Created);
    }

    #[test]
    fn sandbox_limits_clamp_timeout_into_supported_range() {
        let limits = SandboxLimits::default();

        assert_eq!(limits.clamp_timeout_s(0.1), 1.0);
        assert_eq!(limits.clamp_timeout_s(999.0), 300.0);
        assert_eq!(limits.clamp_timeout_s(12.5), 12.5);
    }

    #[test]
    fn payload_identity_tracks_root_and_user_execution() {
        let identity = RequestIdentity::from_peer_credentials(42, 1000, 1000);

        let user = PayloadIdentity::from_request_identity(identity, false);
        assert!(!user.is_root());

        let root = PayloadIdentity::from_request_identity(identity, true);
        assert!(root.is_root());
    }

    #[test]
    fn sandbox_deadline_reports_expiry_from_elapsed_time() {
        let deadline = SandboxDeadline::with_started_at(
            Instant::now() - Duration::from_secs(2),
            Duration::from_secs(1),
        );

        assert!(deadline.is_expired());
        assert_eq!(deadline.remaining(), None);
    }
}
