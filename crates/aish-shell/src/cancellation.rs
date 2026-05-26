//! Reusable stdin cancellation polling for AI operations.
//! Replaces duplicated libc::select/read blocks throughout app.rs.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aish_i18n::t;
use aish_llm::CancellationToken;
use crate::animation::SharedAnimation;
use crate::keyboard::{read_event, ShellEvent};

/// Poll stdin once for Ctrl+C or standalone ESC using the keyboard module.
///
/// Returns `true` if cancellation was requested (Ctrl+C or standalone ESC).
/// Uses crossterm's event parsing so escape sequences (arrow keys etc.) are
/// handled correctly without manual byte-level parsing.
///
/// # Arguments
/// * `cancelled` - Atomic flag to set on cancellation
/// * `token` - Optional cancellation token to cancel
/// * `anim` - Animation to stop on cancellation
pub fn poll_stdin_for_cancel(
    cancelled: &Arc<AtomicBool>,
    token: Option<&Arc<CancellationToken>>,
    anim: &SharedAnimation,
) -> bool {
    // Enter input-raw mode briefly for the poll.  InputRawGuard preserves
    // OPOST so output processing is not disrupted.
    let _guard = crate::keyboard::InputRawGuard::enter().ok();

    match read_event(Some(Duration::from_millis(100))) {
        Ok(Some(ShellEvent::Key(key))) => {
            use crossterm::event::{KeyCode, KeyEventKind};
            if key.kind != KeyEventKind::Press {
                return false;
            }
            match key.code {
                KeyCode::Esc => {
                    trigger_cancel(cancelled, token, anim);
                    return true;
                }
                KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                    trigger_cancel(cancelled, token, anim);
                    return true;
                }
                _ => {}
            }
        }
        Ok(Some(ShellEvent::Mouse(_))) | Ok(Some(ShellEvent::Resize(_, _))) => {}
        Ok(None) | Err(_) => {}
    }

    false
}

fn trigger_cancel(
    cancelled: &Arc<AtomicBool>,
    token: Option<&Arc<CancellationToken>>,
    anim: &SharedAnimation,
) {
    cancelled.store(true, Ordering::SeqCst);
    if let Some(t) = token {
        t.cancel();
    }
    anim.stop();
    print!("\r\n\x1b[33m{}\x1b[0m\r\n", t("shell.command_cancelled"));
    let _ = std::io::stdout().flush();
}
