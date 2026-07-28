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

pub mod builtin;
pub mod hotreload;
pub mod manager;
pub mod migrate_seeded;
pub mod models;
pub mod registry;
pub mod validator;

pub use manager::set_skill_trusted;
pub use manager::SkillManager;
pub use models::UNTRUSTED_MARKER;
pub use models::{Skill, SkillExecutionContext, SkillList, SkillMetadata};
