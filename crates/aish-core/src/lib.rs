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

pub mod audit;
pub mod error;
pub mod plan;
pub mod types;

/// Default and minimum `max_tokens` for LLM responses.
///
/// Used when the config leaves `max_tokens` unset, and enforced as a floor on
/// any smaller explicit value: the previous 4096 default truncated long
/// answers, so installs that had it written into `config.yaml` are lifted to
/// this value on load.
pub const DEFAULT_MAX_TOKENS: u32 = 40_960;

pub use audit::{AuditEvent, AuditEventType, AuditSink, NullAuditSink};
pub use error::{AishError, Result};
pub use plan::*;
pub use types::*;
