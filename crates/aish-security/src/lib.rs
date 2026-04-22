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
pub mod overlay;
pub mod policy;
pub mod sandbox;
pub mod sandbox_daemon;
pub mod sandbox_ipc;
pub mod sandbox_worker;
pub mod strip_sudo;
pub mod types;

// Core manager types
pub use fallback::FallbackRuleEngine;
pub use manager::{SecurityDecision, SecurityManager};
pub use policy::SecurityPolicy;

// Unified types from types.rs (canonical definitions)
pub use types::{
    AiRiskAssessment, FsChange, IpcRequest, IpcResponse, IpcResult, PolicyRule, SandboxConfig,
    SandboxResult, SandboxSecurityResult,
};

// Sandbox executor (uses unified types)
pub use sandbox::SandboxExecutor;

// Sandbox daemon types
pub use sandbox_daemon::{DaemonConfig, SandboxDaemon};

// IPC client types
pub use sandbox_ipc::{SandboxIpcClient, SandboxSecurityIpc, DEFAULT_SOCKET_PATH};

// Utility functions
pub use strip_sudo::strip_sudo_prefix;
