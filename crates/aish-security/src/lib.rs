// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments
)]

pub mod fallback;
pub mod manager;
pub mod policy;
pub mod sandbox;
pub mod sandbox_daemon;
pub mod sandbox_ipc;
pub mod types;

pub use fallback::FallbackRuleEngine;
pub use manager::{SecurityDecision, SecurityManager};
pub use policy::SecurityPolicy;
pub use sandbox::{
    FsChange as SandboxFsChange, SandboxConfig, SandboxExecutor, SandboxResult as SandboxExecResult,
};
pub use sandbox_daemon::{DaemonConfig, DaemonRequest, DaemonResponse, SandboxDaemon};
pub use sandbox_ipc::{FileChange, SandboxIpc, SandboxRequest, SandboxResponse};
pub use types::{AiRiskAssessment, FsChange, PolicyRule, SandboxResult};
