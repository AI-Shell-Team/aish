//! TUI dialogs: inline panels via `aish_ui::PanelRuntime` and
//! stdin fallback.

// Re-export public API from inline prompts
pub use crate::tui::ask_user::run_ask_user_request;
pub use crate::tui::inline_prompts::{
    show_selection_dialog, DialogOption, DialogResult, CUSTOM_DIALOG_VALUE,
};

mod ask_user;
mod inline_prompts;

/// Choice returned by the secret detection dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretDialogChoice {
    /// Replace secrets with environment variable placeholders.
    Redact,
    /// Allow sending original plaintext to AI.
    Allow,
    /// Abort the operation.
    Abort,
}

/// Show a secret detection dialog using inline panel (same UI as ask_user).
pub fn show_secret_dialog_tui(title: &str, message: &str) -> SecretDialogChoice {
    inline_prompts::show_secret_dialog(title, message)
}
