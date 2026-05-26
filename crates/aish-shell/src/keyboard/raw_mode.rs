use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Guard that enters input-raw mode on the terminal.
///
/// Unlike crossterm's `enable_raw_mode()` which calls `cfmakeraw()` and
/// disables OPOST (breaking `\n` → `\r\n` output processing), this guard
/// only disables canonical mode, echo, and signal generation on the input
/// side.  Output processing (OPOST + ONLCR) is preserved so that
/// `println!()` continues to work correctly during AI streaming.
///
/// The guard automatically restores the original terminal settings when dropped.
pub struct InputRawGuard {
    saved: Option<nix::sys::termios::Termios>,
    active: Arc<AtomicBool>,
}

impl InputRawGuard {
    /// Enter input-raw mode, preserving output processing.
    ///
    /// Returns an error if stdin is not a terminal.
    pub fn enter() -> Result<Self, std::io::Error> {
        let fd = libc::STDIN_FILENO;
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };

        let saved = nix::sys::termios::tcgetattr(borrowed)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotConnected, e))?;

        let mut raw = saved.clone();
        use nix::sys::termios::{ControlFlags, InputFlags, LocalFlags};
        raw.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
        raw.input_flags &= !(InputFlags::ICRNL | InputFlags::IXON);
        raw.control_flags &= !ControlFlags::CSIZE;
        raw.control_flags |= ControlFlags::CS8;
        raw.control_chars[libc::VMIN] = 1;
        raw.control_chars[libc::VTIME] = 0;

        nix::sys::termios::tcsetattr(
            borrowed,
            nix::sys::termios::SetArg::TCSANOW,
            &raw,
        )
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotConnected, e))?;

        Ok(Self {
            saved: Some(saved),
            active: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Returns `true` if this guard is still active (not yet dropped).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

impl Drop for InputRawGuard {
    fn drop(&mut self) {
        if self.active.swap(false, Ordering::AcqRel) {
            if let Some(saved) = self.saved.take() {
                let fd = libc::STDIN_FILENO;
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
                let _ = nix::sys::termios::tcsetattr(
                    borrowed,
                    nix::sys::termios::SetArg::TCSANOW,
                    &saved,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_raw_guard_active_state() {
        let guard = InputRawGuard {
            saved: None,
            active: Arc::new(AtomicBool::new(true)),
        };
        assert!(guard.is_active());
    }

    #[test]
    fn test_input_raw_guard_inactive_state() {
        let guard = InputRawGuard {
            saved: None,
            active: Arc::new(AtomicBool::new(false)),
        };
        assert!(!guard.is_active());
    }
}
