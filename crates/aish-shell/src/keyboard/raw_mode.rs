#![cfg(unix)]

use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::warn;

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
    active: AtomicBool,
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

        nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSANOW, &raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotConnected, e))?;

        Ok(Self {
            saved: Some(saved),
            active: AtomicBool::new(true),
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
                    nix::sys::termios::SetArg::TCSADRAIN,
                    &saved,
                );
            }
        }
    }
}

/// Discard terminal query responses that leaked into the stdin input queue
/// while the shell sat in cooked (canonical + ECHO) mode between readlines.
///
/// The real terminal emits these responses when it sees a device-query
/// escape sequence — either aish's own `cursor::position()` (`ESC[6n`) or a
/// query forwarded verbatim from the PTY subprocess (`ESC[>c`, `ESC[?c`,
/// ...). Left in the queue, rustyline reads them as key events, which shows
/// up as garbled text and corrupts the next prompt's input (e.g.
/// `0;115;0cRRR` spliced into the typed line).
///
/// Only complete CSI report sequences (`ESC[...R`, `ESC[?...c`,
/// `ESC[>...c`, `ESC[=...c`, DSR `ESC[...n`, text-area `ESC[...t`) are
/// dropped. Any remaining bytes — genuine user type-ahead — are re-injected
/// with `TIOCSTI` so the editor still receives them. When nothing is
/// pending this performs a single non-blocking poll and returns
/// immediately (no prompt latency).
pub fn drain_terminal_responses() {
    let fd = libc::STDIN_FILENO;

    // Enter raw mode BEFORE the readiness probe. In canonical (cooked) mode
    // the line discipline releases input only as complete lines, and a
    // terminal report (e.g. `ESC[2;2R`) carries no line delimiter — so
    // poll() would never see it and the report would stay queued until
    // rustyline enters raw mode and reads it as key events. Raw mode makes
    // pending reports visible to a zero-timeout poll.
    let guard = match InputRawGuard::enter() {
        Ok(g) => g,
        Err(_) => return, // stdin is not a tty
    };
    let saved_flags = set_stdin_nonblocking(fd);

    // Cheap probe: if nothing is pending, do no further work.
    if !stdin_readable_now(fd) {
        restore_stdin_flags(fd, saved_flags);
        return; // `guard` restores termios on drop.
    }

    let data = read_pending(fd);

    // Drop the leading run of complete report sequences; keep the rest.
    let keep_from = report_prefix_len(&data);
    let tail = &data[keep_from..];
    let mut reinjected = 0;
    for &byte in tail {
        if !reinject_byte(fd, byte) {
            break;
        }
        reinjected += 1;
    }
    if reinjected < tail.len() {
        // TIOCSTI rejected by the kernel (dev.tty.legacy_tiocsti=0). The
        // remaining non-report bytes — genuine user type-ahead — cannot be
        // put back and are lost. Log so this rare loss is diagnosable.
        warn!(
            tail_len = tail.len(),
            reinjected, "TIOCSTI unavailable; non-report type-ahead left in stdin was lost"
        );
    }

    restore_stdin_flags(fd, saved_flags);
    drop(guard);
}

/// True if at least one byte is readable from `fd` right now.
fn stdin_readable_now(fd: libc::c_int) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll() on a single valid fd with a zero timeout never blocks.
    unsafe { libc::poll(&mut pfd, 1, 0) > 0 && (pfd.revents & libc::POLLIN) != 0 }
}

/// Make `fd` non-blocking. Returns the prior flags (for restoration).
fn set_stdin_nonblocking(fd: libc::c_int) -> libc::c_int {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 && (flags & libc::O_NONBLOCK) == 0 {
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    flags
}

/// Restore `fd` to blocking if it was blocking before the drain.
fn restore_stdin_flags(fd: libc::c_int, saved: libc::c_int) {
    if saved >= 0 && (saved & libc::O_NONBLOCK) == 0 {
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, saved);
        }
    }
}

/// Read every byte available right now. Never blocks and never waits for
/// stragglers — the drain must finish in microseconds so it cannot capture
/// keystrokes the user types as the prompt appears.
fn read_pending(fd: libc::c_int) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        // SAFETY: reading into a valid buffer from a valid (non-blocking) fd.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}

/// Length of the leading run of complete terminal-report CSI sequences.
///
/// Recognized reports: `ESC[` + optional `?`/`>`/`=` + digits/`;` + final
/// byte in `{R, c, n, t}`. Parsing stops at the first byte that is not part
/// of such a sequence (e.g. user input or an unrecognized escape), so the
/// returned offset is safe to discard.
fn report_prefix_len(data: &[u8]) -> usize {
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0x1b || i + 1 >= data.len() || data[i + 1] != b'[' {
            break;
        }
        let mut j = i + 2;
        if j < data.len() && matches!(data[j], b'?' | b'>' | b'=') {
            j += 1;
        }
        while j < data.len() && (data[j].is_ascii_digit() || data[j] == b';') {
            j += 1;
        }
        if j >= data.len() || !matches!(data[j], b'R' | b'c' | b'n' | b't') {
            break;
        }
        i = j + 1;
    }
    i
}

/// Push `byte` back into the tty input queue as if the user typed it, so
/// genuine type-ahead is preserved across the drain. Returns false when the
/// kernel rejects `TIOCSTI` (restricted via `dev.tty.legacy_tiocsti=0`).
fn reinject_byte(fd: libc::c_int, byte: u8) -> bool {
    let c = byte as libc::c_char;
    let ptr: *const libc::c_char = &c;
    // SAFETY: TIOCSTI reads one char from the supplied pointer. `ptr` is a
    // valid pointer to `c` for the duration of the call.
    unsafe { libc::ioctl(fd, libc::TIOCSTI, ptr) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_raw_guard_active_state() {
        let guard = InputRawGuard {
            saved: None,
            active: AtomicBool::new(true),
        };
        assert!(guard.is_active());
    }

    #[test]
    fn test_input_raw_guard_inactive_state() {
        let guard = InputRawGuard {
            saved: None,
            active: AtomicBool::new(false),
        };
        assert!(!guard.is_active());
    }

    #[test]
    fn report_prefix_drops_cpr() {
        // ESC[2;2R cursor-position report -> fully consumed.
        assert_eq!(report_prefix_len(b"\x1b[2;2R"), 6);
    }

    #[test]
    fn report_prefix_drops_secondary_da() {
        // ESC[>0;115;0c secondary device attributes -> fully consumed.
        assert_eq!(report_prefix_len(b"\x1b[>0;115;0c"), 11);
    }

    #[test]
    fn report_prefix_drops_primary_da() {
        // ESC[?64;1c primary device attributes -> fully consumed.
        assert_eq!(report_prefix_len(b"\x1b[?64;1c"), 8);
    }

    #[test]
    fn report_prefix_drops_burst_then_keeps_user_input() {
        // A burst of reports followed by a user-typed command: only the
        // leading report run is consumed, the command is preserved.
        let data = b"\x1b[2;2R\x1b[3;4R\x1b[>0;115;0cls -la\r";
        let n = report_prefix_len(data);
        assert_eq!(&data[..n], b"\x1b[2;2R\x1b[3;4R\x1b[>0;115;0c");
        assert_eq!(&data[n..], b"ls -la\r");
    }

    #[test]
    fn report_prefix_keeps_unrecognized_escape() {
        // A bare ESC (user pressed Escape) is not a complete report: keep it.
        assert_eq!(report_prefix_len(b"\x1b"), 0);
        // ESC followed by something other than '[' is also kept.
        assert_eq!(report_prefix_len(b"\x1blls"), 0);
    }

    #[test]
    fn report_prefix_keeps_incomplete_sequence() {
        // Truncated report (no final byte): keep everything.
        assert_eq!(report_prefix_len(b"\x1b[2;2"), 0);
    }

    #[test]
    fn report_prefix_keeps_non_report_final() {
        // ESC[2;2H is a cursor move (not a report): keep it.
        assert_eq!(report_prefix_len(b"\x1b[2;2H"), 0);
    }

    #[test]
    fn report_prefix_empty() {
        assert_eq!(report_prefix_len(b""), 0);
    }
}
