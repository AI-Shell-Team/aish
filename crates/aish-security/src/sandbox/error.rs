use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SandboxReason {
    BadRequest,
    RequestTooLarge,
    SandboxDisabled,
    SandboxDisabledByPolicy,
    SandboxIpcUnavailable,
    SandboxIpcTimeout,
    SandboxIpcProtocolError,
    SandboxIpcFailed,
    SandboxTimeout,
    SandboxExecuteFailed,
    SandboxCleanupFailed,
    SandboxUnavailable,
    SandboxException,
    SandboxFailed,
    CwdOutsideRepoRoot,
    OverlayMountFailed,
    OverlayPermFailed,
    BindMountFailed,
    RemountRoFailed,
    CommandNotFound,
}

impl SandboxReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::RequestTooLarge => "request_too_large",
            Self::SandboxDisabled => "sandbox_disabled",
            Self::SandboxDisabledByPolicy => "sandbox_disabled_by_policy",
            Self::SandboxIpcUnavailable => "sandbox_ipc_unavailable",
            Self::SandboxIpcTimeout => "sandbox_ipc_timeout",
            Self::SandboxIpcProtocolError => "sandbox_ipc_protocol_error",
            Self::SandboxIpcFailed => "sandbox_ipc_failed",
            Self::SandboxTimeout => "sandbox_timeout",
            Self::SandboxExecuteFailed => "sandbox_execute_failed",
            Self::SandboxCleanupFailed => "sandbox_cleanup_failed",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::SandboxException => "sandbox_exception",
            Self::SandboxFailed => "sandbox_failed",
            Self::CwdOutsideRepoRoot => "cwd_outside_repo_root",
            Self::OverlayMountFailed => "overlay_mount_failed",
            Self::OverlayPermFailed => "overlay_perm_failed",
            Self::BindMountFailed => "bind_mount_failed",
            Self::RemountRoFailed => "remount_ro_failed",
            Self::CommandNotFound => "command_not_found",
        }
    }

    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        match value {
            "bad_request" => Some(Self::BadRequest),
            "request_too_large" => Some(Self::RequestTooLarge),
            "sandbox_disabled" => Some(Self::SandboxDisabled),
            "sandbox_disabled_by_policy" => Some(Self::SandboxDisabledByPolicy),
            "sandbox_ipc_unavailable" => Some(Self::SandboxIpcUnavailable),
            "sandbox_ipc_timeout" => Some(Self::SandboxIpcTimeout),
            "sandbox_ipc_protocol_error" => Some(Self::SandboxIpcProtocolError),
            "sandbox_ipc_failed" => Some(Self::SandboxIpcFailed),
            "sandbox_timeout" => Some(Self::SandboxTimeout),
            "sandbox_execute_failed" => Some(Self::SandboxExecuteFailed),
            "sandbox_cleanup_failed" => Some(Self::SandboxCleanupFailed),
            "sandbox_unavailable" => Some(Self::SandboxUnavailable),
            "sandbox_exception" => Some(Self::SandboxException),
            "sandbox_failed" => Some(Self::SandboxFailed),
            "cwd_outside_repo_root" => Some(Self::CwdOutsideRepoRoot),
            "overlay_mount_failed" => Some(Self::OverlayMountFailed),
            "overlay_perm_failed" => Some(Self::OverlayPermFailed),
            "bind_mount_failed" => Some(Self::BindMountFailed),
            "remount_ro_failed" => Some(Self::RemountRoFailed),
            "command_not_found" => Some(Self::CommandNotFound),
            _ => None,
        }
    }
}

impl std::fmt::Display for SandboxReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SandboxError {
    #[error("{reason}")]
    Reason { reason: SandboxReason },
    #[error("{reason}: {details}")]
    Detailed {
        reason: SandboxReason,
        details: String,
    },
}

impl SandboxError {
    pub(crate) const fn reason(&self) -> SandboxReason {
        match self {
            Self::Reason { reason } | Self::Detailed { reason, .. } => *reason,
        }
    }

    pub(crate) fn details(&self) -> Option<&str> {
        match self {
            Self::Reason { .. } => None,
            Self::Detailed { details, .. } => Some(details.as_str()),
        }
    }

    pub(crate) const fn from_reason(reason: SandboxReason) -> Self {
        Self::Reason { reason }
    }

    pub(crate) fn with_details(reason: SandboxReason, details: impl Into<String>) -> Self {
        Self::Detailed {
            reason,
            details: details.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SandboxError, SandboxReason};

    #[test]
    fn sandbox_reason_display_uses_snake_case_values() {
        assert_eq!(SandboxReason::SandboxTimeout.to_string(), "sandbox_timeout");
        assert_eq!(
            SandboxReason::CwdOutsideRepoRoot.to_string(),
            "cwd_outside_repo_root"
        );
        assert_eq!(
            SandboxReason::from_wire("overlay_mount_failed"),
            Some(SandboxReason::OverlayMountFailed)
        );
        assert_eq!(
            SandboxReason::from_wire("sandbox_cleanup_failed"),
            Some(SandboxReason::SandboxCleanupFailed)
        );
    }

    #[test]
    fn sandbox_error_exposes_reason_and_optional_details() {
        let plain = SandboxError::from_reason(SandboxReason::SandboxIpcUnavailable);
        assert_eq!(plain.reason(), SandboxReason::SandboxIpcUnavailable);
        assert_eq!(plain.details(), None);

        let detailed =
            SandboxError::with_details(SandboxReason::OverlayMountFailed, "permission denied");
        assert_eq!(detailed.reason(), SandboxReason::OverlayMountFailed);
        assert_eq!(detailed.details(), Some("permission denied"));
    }
}
