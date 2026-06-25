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

pub mod command_state;
pub mod control;
pub mod ctrl_o;
pub mod executor;
pub mod exit_code;
pub mod nl_detect;
pub mod offload;
pub mod output_buffer;
pub mod persistent;
pub mod readline_tab;
pub mod session_interceptor;
pub mod types;

/// Result returned by the SSH secret-check closure.
pub struct SshSecretCheckResult {
    /// Formatted warning message (title + detected secrets).
    pub warning: String,
    /// Detected secret matches for vault redaction.
    pub detected_secrets: Vec<aish_security::secret::SecretMatch>,
}

pub use command_state::CommandState;
pub use control::{
    decode_control_chunk, encode_control_event, BackendControlEvent, CompletionCandidate,
    CompletionResponse,
};
pub use executor::PtyExecutor;
pub use offload::{
    truncate_utf8_safe, BashOffloadResult, BashOffloadSettings, BashOutputOffload, OffloadResult,
    OffloadState, PtyOutputOffload,
};
pub use output_buffer::OutputBuffer;
pub use persistent::{is_interactive_command, shell_quote_escape, PersistentPty};
pub use readline_tab::ReadlineTabResult;
pub use session_interceptor::{
    pop_last_utf8_char, AiCallback, AiEvent, AiQuery, AiResponse, AskUserAnswer, AskUserChannel,
    AskUserOption, AskUserRequest, BashExecResult, FollowupCallback, InterceptorState,
    RemoteExecFn, SessionInterceptor, StatusCallback, StdinAction,
};
pub use types::CancelToken;
pub use types::{CommandSource, CommandSubmission, PtyCommandResult, StreamName};
