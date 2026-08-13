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
    clippy::too_many_arguments,
    clippy::useless_conversion
)]

pub mod ai_handler;
pub mod animation;
pub mod app;
pub mod autosuggest;
pub mod commands;
pub mod completion;
pub mod completion_list;
pub mod doctor;
pub mod environment;
pub mod esc_watcher;
pub mod expand_history;
pub mod feedback;
pub mod image;
pub mod inline_completion;
pub mod input;
pub mod keyboard;
pub mod llm_event_ui;
pub mod md_render;
pub mod nl_detect;
pub mod prompt;
pub mod readline;
pub mod recorder;
pub mod renderer;
pub mod resume_selector;
pub mod security_panel;
pub mod settings_panel;
pub mod status;
pub mod theme;
pub mod token_store;
pub mod tui;
pub mod types;
pub mod update_check;
pub mod wizard;

pub use app::AishShell;
pub use types::{InputIntent, ShellState};

/// Check whether the configuration requires an interactive setup wizard.
///
/// Returns `true` when either the model or API key is missing from the
/// configuration (and no CLI override was provided for the missing field).
pub fn needs_interactive_setup(config: &aish_config::ConfigModel) -> bool {
    if config.model.trim().is_empty() {
        return true;
    }
    if config.api_key.trim().is_empty() {
        return true;
    }
    false
}
