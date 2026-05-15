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

pub mod budget;
pub mod manager;
pub mod types;

pub use budget::{
    calculate_budget_state, calculate_thresholds, context_window_hard_min_tokens,
    context_window_warn_below_tokens, effective_reserved_output_tokens,
    resolve_context_window_tokens, ContextBudgetPolicy, ContextBudgetState,
    ContextBudgetThresholds, ContextPressureLevel, ContextWindowResolution, ContextWindowSource,
    DEFAULT_CONTEXT_WINDOW_TOKENS,
};
pub use manager::{
    ContextCompactReport, ContextManager, ContextStats, FullCompactReport, MicrocompactReport,
};
pub use types::ContextMessage;
