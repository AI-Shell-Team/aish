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

pub mod executor;
pub mod hooks;
pub mod loader;
pub mod models;
pub mod registry;

pub use executor::{ScriptExecutionResult, ScriptExecutor};
pub use loader::ScriptLoader;
pub use models::{Script, ScriptArgument, ScriptMetadata};
pub use registry::ScriptRegistry;
