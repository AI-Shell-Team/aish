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

pub mod manager;
pub mod models;
pub mod ttl;

pub use manager::MemoryManager;
pub use models::{MemoryEntry, MemorySource};
pub use ttl::{default_ttl, resolve_ttl, ENVIRONMENT_CAP_SECS, HOST_SCOPE_CAP_SECS};
