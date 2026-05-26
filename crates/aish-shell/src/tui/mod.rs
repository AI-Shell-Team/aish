//! Terminal UI functionality for aish-shell.
//!
//! This module provides both inline prompts using `inquire` and
//! alternate-screen TUI panels using `ratatui`.

use std::time::Duration;

// Re-export public API from inline prompts
pub use crate::tui::inline_prompts::{
    show_confirmation_dialog, show_selection_dialog, DialogResult, DialogOption,
    SecretDialogChoice,
};

// Re-export TUI backend and components
pub use backend::TuiBackend;
pub use security_dialog::{DialogAction, SecurityDialog};
pub use secret_dialog::{SecretDialog, SecretDialogChoice as TuiSecretDialogChoice};

mod backend;
mod inline_prompts;
mod security_dialog;
mod secret_dialog;

/// Show a secret detection dialog using the ratatui TUI panel.
///
/// Falls back to the `inquire`-based inline prompt if the TUI backend
/// cannot be initialized (e.g. stdin is not a terminal).
pub fn show_secret_dialog_tui(title: &str, message: &str) -> SecretDialogChoice {
    // Try the TUI path first.
    match TuiBackend::new() {
        Ok(mut backend) => {
        let mut dialog = SecretDialog::new(title, message);
        let choice = loop {
            let _ = backend.draw(|frame| {
                dialog.render(frame, frame.area());
            });
            match crate::keyboard::read_event(Some(Duration::from_millis(50))) {
                Ok(Some(crate::keyboard::ShellEvent::Key(key))) => {
                    if let Some(c) = dialog.handle_key(key) {
                        break convert_tui_choice(c);
                    }
                }
                Ok(Some(_) | None) | Err(_) => {}
            }
        };
        let _ = backend.restore();
            return choice;
        }
        Err(e) => {
            let _ = e; // TUI unavailable, fall back silently
        }
    }
    inline_prompts::show_secret_dialog(title, message)
}

fn convert_tui_choice(c: secret_dialog::SecretDialogChoice) -> SecretDialogChoice {
    match c {
        secret_dialog::SecretDialogChoice::Redact => SecretDialogChoice::Redact,
        secret_dialog::SecretDialogChoice::Allow => SecretDialogChoice::Allow,
        secret_dialog::SecretDialogChoice::Abort => SecretDialogChoice::Abort,
    }
}
