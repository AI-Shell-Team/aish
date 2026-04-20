pub mod ai_handler;
pub mod animation;
pub mod app;
pub mod autosuggest;
pub mod commands;
pub mod environment;
pub mod input;
pub mod prompt;
pub mod readline;
pub mod renderer;
pub mod token_store;
pub mod types;
pub mod tui;
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
