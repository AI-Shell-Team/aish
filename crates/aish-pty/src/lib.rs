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
pub mod executor;
pub mod offload;
pub mod persistent;
pub mod state_capture;
pub mod types;

pub use command_state::CommandState;
pub use control::{decode_control_chunk, encode_control_event, BackendControlEvent};
pub use executor::{CancelToken, PtyExecutor};
pub use offload::{OffloadResult, OffloadState, PtyOutputOffload};
pub use persistent::{is_interactive_command, PersistentPty};
pub use state_capture::StateChanges;
pub use types::{CommandSource, CommandSubmission, PtyCommandResult, StreamName};
