use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aish_llm::CancellationToken;
use nix::sys::termios::{tcgetattr, tcsetattr, SetArg, Termios};

/// Watches for ESC and Ctrl+C keypresses on stdin during AI operations.
///
/// On `start()`, saves the current terminal settings, switches stdin to raw
/// mode, and spawns a listener thread. On standalone ESC (0x1b with no
/// follow-up bytes within 50 ms) or Ctrl+C (0x03), calls
/// `CancellationToken::cancel()`.
///
/// On `stop()`, signals the thread to exit, joins it, and restores the
/// original terminal settings. If the watcher is dropped without calling
/// `stop()` (e.g. due to a panic), `Drop` restores the terminal as a safety
/// net.
///
/// **Note:** The listener thread reads and discards all stdin bytes that are
/// not standalone ESC. User input during AI streaming is silently consumed.
pub struct EscWatcher {
    thread: Option<std::thread::JoinHandle<()>>,
    saved_termios: Option<Termios>,
    stop_flag: Arc<AtomicBool>,
}

impl EscWatcher {
    /// Start watching for ESC keypresses.
    ///
    /// If the terminal cannot be switched to raw mode (e.g. stdin is not a
    /// tty), returns a no-op watcher that does nothing on `stop()`.
    pub fn start(token: Arc<CancellationToken>) -> Self {
        let stdin_fd = libc::STDIN_FILENO;
        let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_fd) };

        let saved_termios = match tcgetattr(stdin_borrowed) {
            Ok(t) => t,
            Err(_) => {
                return Self {
                    thread: None,
                    saved_termios: None,
                    stop_flag: Arc::new(AtomicBool::new(false)),
                };
            }
        };

        // Switch to raw mode: disable canonical mode, echo, and signal
        // generation so we receive individual bytes including ESC.
        let mut raw = saved_termios.clone();
        use nix::sys::termios::{ControlFlags, InputFlags, LocalFlags};
        raw.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
        raw.input_flags &= !InputFlags::ISTRIP;
        raw.control_flags &= !ControlFlags::CSIZE;
        raw.control_flags |= ControlFlags::CS8;
        raw.control_chars[libc::VMIN] = 1;
        raw.control_chars[libc::VTIME] = 0;
        if tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw).is_err() {
            return Self {
                thread: None,
                saved_termios: None,
                stop_flag: Arc::new(AtomicBool::new(false)),
            };
        }

        let stop_flag = Arc::new(AtomicBool::new(false));

        let stop_clone = stop_flag.clone();
        let token_clone = token.clone();
        let handle = std::thread::Builder::new()
            .name("esc-watcher".into())
            .spawn(move || {
                let mut buf = [0u8; 16];

                loop {
                    if stop_clone.load(Ordering::Relaxed) {
                        return;
                    }

                    // Use select() with 100ms timeout so we can check
                    // stop_flag periodically without blocking indefinitely.
                    let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
                    unsafe {
                        libc::FD_ZERO(&mut read_fds);
                        libc::FD_SET(stdin_fd, &mut read_fds);
                    }
                    let mut tv = libc::timeval {
                        tv_sec: 0,
                        tv_usec: 100_000, // 100 ms
                    };
                    let sel = unsafe {
                        libc::select(
                            stdin_fd + 1,
                            &mut read_fds,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &mut tv,
                        )
                    };

                    if sel <= 0 {
                        // Timeout or error — loop back to check stop_flag.
                        continue;
                    }

                    // Read the byte(s) available on stdin.
                    let n = unsafe {
                        libc::read(stdin_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                    };

                    if n <= 0 {
                        continue;
                    }

                    // Ctrl+C (0x03) — cancel immediately. Raw mode disables
                    // ISIG so the kernel won't generate SIGINT; we handle it.
                    if buf[0] == 0x03 {
                        token_clone.cancel();
                        return;
                    }

                    // Check for ESC (0x1b) as the first byte.
                    if buf[0] != 0x1b {
                        continue;
                    }

                    // If more than one byte was read, this is already a
                    // complete escape sequence (e.g. arrow key), not a
                    // standalone ESC.
                    if n > 1 {
                        continue;
                    }

                    // ESC received — wait 50 ms to see if follow-up bytes
                    // arrive (arrow keys and function keys send ESC followed
                    // by additional bytes, e.g. ESC [ A = up arrow).
                    let mut follow_fds: libc::fd_set = unsafe { std::mem::zeroed() };
                    unsafe {
                        libc::FD_ZERO(&mut follow_fds);
                        libc::FD_SET(stdin_fd, &mut follow_fds);
                    }
                    let mut follow_tv = libc::timeval {
                        tv_sec: 0,
                        tv_usec: 50_000, // 50 ms
                    };
                    let follow_sel = unsafe {
                        libc::select(
                            stdin_fd + 1,
                            &mut follow_fds,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &mut follow_tv,
                        )
                    };

                    if follow_sel == 0 {
                        // No follow-up bytes within 50 ms — standalone ESC.
                        token_clone.cancel();
                        return;
                    }
                    // Follow-up bytes exist (ANSI escape sequence) — ignore.
                }
            })
            .ok();

        Self {
            thread: handle,
            saved_termios: Some(saved_termios),
            stop_flag,
        }
    }

    /// Stop the listener thread and restore terminal settings.
    pub fn stop(&mut self) {
        self.cleanup();
    }

    fn cleanup(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);

        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }

        if let Some(saved) = self.saved_termios.take() {
            let stdin_fd = libc::STDIN_FILENO;
            let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, &saved);
        }
    }
}

impl Drop for EscWatcher {
    fn drop(&mut self) {
        self.cleanup();
    }
}
