use std::ffi::CString;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::openpty;
use nix::sys::signal::{kill, Signal};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};
use nix::unistd::{close, dup2, execvp, fork, pipe, ForkResult, Pid};

use aish_core::AishError;
use tracing::debug;

use crate::command_state::CommandState;
use crate::control::{decode_control_chunk, BackendControlEvent};
use crate::types::CommandSource;

/// Bash rc wrapper script embedded at compile time.
const BASH_RC_WRAPPER: &str = include_str!("bash_rc_wrapper.sh");

// Interactive commands where Ctrl-C should be forwarded as character, not SIGINT.
const SESSION_COMMANDS: &[&str] = &["ssh", "telnet", "mosh", "nc", "netcat", "ftp", "sftp"];

// Commands that need a real terminal (PTY) for interactive use.
const INTERACTIVE_COMMANDS: &[&str] = &[
    "vim", "vi", "nano", "emacs", "ssh", "telnet", "mosh", "htop", "top", "btop", "iotop", "less",
    "more", "most", "man", "screen", "tmux", "mc", "ranger",
];

/// Persistent PTY session managing a single long-lived bash process.
pub struct PersistentPty {
    master_fd: RawFd,
    control_fd: RawFd,
    child_pid: Pid,
    command_state: CommandState,
    control_buffer: String,
    #[allow(clippy::type_complexity)]
    output_callback: Option<Arc<dyn Fn(&[u8]) + Send + Sync>>,
    rows: u16,
    cols: u16,
    running: AtomicBool,
    /// Next backend command sequence number (decreasing negatives).
    next_backend_seq: i32,
    /// Shared output buffer for execute_command mode.
    exec_buffer: Arc<Mutex<Vec<u8>>>,
    /// Whether we are in exec mode (buffer output instead of forwarding).
    exec_mode: Arc<AtomicBool>,
}

impl PersistentPty {
    /// Start a new persistent bash session.
    pub fn start(cwd: &str, rows: u16, cols: u16) -> aish_core::Result<Self> {
        // Write rcfile to a temp file (bash --rcfile needs a real file path).
        let rcfile_path = write_rcfile_temp()?;

        // Create control pipe.
        let (control_read, control_write) =
            pipe().map_err(|e| AishError::Pty(format!("failed to create control pipe: {e}")))?;

        // Create PTY.
        let pty_result =
            openpty(None, None).map_err(|e| AishError::Pty(format!("failed to openpty: {e}")))?;
        let master_fd = pty_result.master;
        let slave_fd = pty_result.slave;

        // Set master non-blocking.
        set_nonblocking(&master_fd)?;

        // Set control pipe read end non-blocking.
        set_nonblocking(&control_read)?;

        // Sync terminal size.
        let stdin_fd = libc::STDIN_FILENO;
        let _ = sync_window_size(stdin_fd, master_fd.as_raw_fd());

        // Get raw fds for child.
        let slave_raw = slave_fd.as_raw_fd();
        let control_write_raw = control_write.as_raw_fd();
        let rcfile_path_clone = rcfile_path.to_string_lossy().to_string();

        // Fork.
        let child_pid =
            match unsafe { fork() }.map_err(|e| AishError::Pty(format!("fork failed: {e}")))? {
                ForkResult::Parent { child } => {
                    drop(slave_fd);
                    drop(control_write);
                    child
                }
                ForkResult::Child => {
                    child_main(slave_raw, control_write_raw, &rcfile_path_clone, cwd);
                }
            };

        debug!(pid = %child_pid, "persistent bash started");

        // Convert to raw fds.
        let master_raw = master_fd.into_raw_fd();
        let control_raw = control_read.into_raw_fd();

        // NOTE: Don't delete rcfile here -- there's a race condition where bash
        // may not have opened it yet. Delete after session_ready is received.

        let mut pty = Self {
            master_fd: master_raw,
            control_fd: control_raw,
            child_pid,
            command_state: CommandState::new(),
            control_buffer: String::new(),
            output_callback: None,
            rows,
            cols,
            running: AtomicBool::new(true),
            next_backend_seq: -1,
            exec_buffer: Arc::new(Mutex::new(Vec::new())),
            exec_mode: Arc::new(AtomicBool::new(false)),
        };

        // Wait for session_ready event.
        pty.wait_for_session_ready(Duration::from_secs(5))?;

        // Drain any stale events (e.g., prompt_ready from bash's initial
        // prompt rendering) left in the control pipe after session_ready.
        // Without this, the first user command would match the stale
        // prompt_ready and exit the forwarding loop immediately, producing
        // no output.
        pty.drain_master_to_stdout();
        pty.drain_control_pipe();

        // Now safe to clean up rcfile -- bash has loaded it.
        let _ = std::fs::remove_file(&rcfile_path);

        Ok(pty)
    }

    /// Send a command to bash (no waiting for completion).
    pub fn send_command(&mut self, command: &str, seq: Option<i32>) -> aish_core::Result<()> {
        let source = if seq.is_some() {
            CommandSource::Backend
        } else {
            CommandSource::User
        };
        self.command_state.register_command(command, source, seq);

        let mut payload = String::new();
        if let Some(s) = seq {
            let quoted = shell_quote_escape(command);
            payload.push_str(&format!(
                " __AISH_ACTIVE_COMMAND_SEQ={s}; __AISH_ACTIVE_COMMAND_TEXT={quoted}; "
            ));
        }
        payload.push_str(command);
        payload.push('\n');

        self.write_master(payload.as_bytes())
    }

    /// Execute a command and wait for completion with timeout.
    /// Returns cleaned output and exit code.
    pub fn execute_command(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> aish_core::Result<(String, i32)> {
        let seq = self.allocate_backend_seq();

        // Enter exec mode: buffer output.
        self.exec_buffer.lock().unwrap().clear();
        self.exec_mode.store(true, Ordering::SeqCst);

        self.send_command(command, Some(seq))?;

        // Poll for prompt_ready event.
        let result = self.wait_for_prompt_ready(seq, timeout);

        // Drain any remaining master_fd output into exec buffer before
        // exiting exec mode, so stale data doesn't leak to the next command.
        self.drain_master_to_exec_buffer();

        // Exit exec mode.
        self.exec_mode.store(false, Ordering::SeqCst);

        // Grab buffered output.
        let raw_output = self
            .exec_buffer
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<u8>>();
        let raw_str = String::from_utf8_lossy(&raw_output).to_string();

        match result {
            Some(pty_result) => {
                let cleaned = clean_pty_output(&raw_str, command);
                Ok((cleaned, pty_result.exit_code))
            }
            None => {
                // Timeout -- send Ctrl-C.
                let _ = self.write_master(b"\x03");
                let cleaned = clean_pty_output(&raw_str, command);
                Ok((cleaned, -1))
            }
        }
    }

    /// Send a user command and enter raw stdin forwarding mode until
    /// prompt_ready is received. Returns (exit_code, cwd, output).
    pub fn send_command_interactive(
        &mut self,
        command: &str,
    ) -> aish_core::Result<(i32, String, String)> {
        let is_session = is_session_command(command);
        self.command_state
            .register_command(command, CommandSource::User, None);

        // Drain any stale PTY output left from the previous command's
        // prompt rendering (PS1 / readline init sequences, etc.).
        self.drain_master_silent();

        // Write command to bash.
        let mut payload = command.to_string();
        payload.push('\n');
        self.write_master(payload.as_bytes())?;

        // Save and set terminal to raw mode.
        let stdin_fd = libc::STDIN_FILENO;
        let stdin_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
        let saved_termios = tcgetattr(stdin_borrowed).ok();
        if let Some(ref saved) = saved_termios {
            let mut raw = saved.clone();
            cfmakeraw(&mut raw);
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw);
        }

        // Forwarding loop.
        let mut write_buf: Vec<u8> = Vec::new();
        let mut result_cwd = String::new();
        let mut result_exit_code: i32 = -1;
        let mut output_buf: Vec<u8> = Vec::new();
        let mut done = false;
        // After receiving PromptReady, keep draining master_fd until a full
        // select timeout passes with no new data.  The control pipe may
        // deliver PromptReady before the kernel has flushed all PTY output
        // to master_fd, causing intermittent missing output for fast
        // commands.
        let mut draining = false;
        // The PTY may emit a bare leading newline from stale prompt
        // rendering.  Only skip a leading CR-LF or LF at the very start
        // of the first chunk -- never consume actual command output.
        let mut skip_leading_newline = true;

        while !done {
            // Build fd sets.
            let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            let mut write_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            unsafe {
                libc::FD_ZERO(&mut read_fds);
                libc::FD_ZERO(&mut write_fds);
                if !draining {
                    libc::FD_SET(stdin_fd, &mut read_fds);
                    libc::FD_SET(self.control_fd, &mut read_fds);
                }
                libc::FD_SET(self.master_fd, &mut read_fds);
                if !write_buf.is_empty() {
                    libc::FD_SET(self.master_fd, &mut write_fds);
                }
            }

            let max_fd = if draining {
                self.master_fd + 1
            } else {
                self.master_fd.max(self.control_fd).max(stdin_fd) + 1
            };
            // Shorter timeout during drain phase (5ms) to avoid noticeable
            // latency after the command has already completed.
            let mut tv = libc::timeval {
                tv_sec: 0,
                tv_usec: if draining { 5_000 } else { 50_000 },
            };

            let sel = unsafe {
                libc::select(
                    max_fd,
                    &mut read_fds,
                    &mut write_fds,
                    std::ptr::null_mut(),
                    &mut tv,
                )
            };

            if sel < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    continue;
                }
                break;
            }

            if sel == 0 {
                // Timeout -- during drain phase this means all output has
                // been delivered.  During normal phase it's just a poll
                // cycle with nothing to do.
                if draining {
                    done = true;
                }
                continue;
            }

            // Write buffered data.
            if unsafe { libc::FD_ISSET(self.master_fd, &write_fds) } && !write_buf.is_empty() {
                match unsafe {
                    libc::write(
                        self.master_fd,
                        write_buf.as_ptr() as *const libc::c_void,
                        write_buf.len(),
                    )
                } {
                    n if n > 0 => {
                        write_buf.drain(..n as usize);
                    }
                    _ => {
                        write_buf.clear();
                    }
                }
            }

            // Read stdin -> master (only during normal phase).
            if !draining && unsafe { libc::FD_ISSET(stdin_fd, &read_fds) } {
                let mut tmp = [0u8; 1024];
                match unsafe {
                    libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        if data.contains(&0x03) && !is_session {
                            let _ = kill_pg(self.child_pid, Signal::SIGINT);
                        }
                        write_buf.extend_from_slice(data);
                    }
                    _ => {}
                }
            }

            // Read master -> stdout.
            if unsafe { libc::FD_ISSET(self.master_fd, &read_fds) } {
                let mut tmp = [0u8; 8192];
                match unsafe {
                    libc::read(
                        self.master_fd,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        let mut data = &tmp[..n as usize];
                        if skip_leading_newline {
                            // Only strip a bare leading CR-LF or LF that
                            // came from stale prompt rendering.  Do NOT
                            // discard actual command output.
                            if data.starts_with(b"\r\n") {
                                data = &data[2..];
                            } else if data.starts_with(b"\n") {
                                data = &data[1..];
                            }
                            skip_leading_newline = false;
                        }
                        if !data.is_empty() {
                            output_buf.extend_from_slice(data);
                            let _ = unsafe {
                                libc::write(
                                    libc::STDOUT_FILENO,
                                    data.as_ptr() as *const libc::c_void,
                                    data.len(),
                                )
                            };
                        }
                    }
                    0 => {
                        // EOF on master_fd means the bash slave closed --
                        // the child process exited.
                        self.running.store(false, Ordering::SeqCst);
                        done = true;
                    }
                    _ => {}
                }
            }

            // Read control pipe for events (only during normal phase).
            if !draining && unsafe { libc::FD_ISSET(self.control_fd, &read_fds) } {
                let mut tmp = [0u8; 4096];
                match unsafe {
                    libc::read(
                        self.control_fd,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        let events =
                            decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]);
                        for event in &events {
                            if let BackendControlEvent::ShellExiting { .. } = event {
                                // Bash is shutting down -- mark as not running so
                                // the caller can restart the PTY before the next
                                // command.
                                self.running.store(false, Ordering::SeqCst);
                            }
                            if let Some(r) = self.command_state.handle_event(event) {
                                result_exit_code = r.exit_code;
                                // Enter drain phase instead of exiting immediately.
                                // The control pipe may deliver PromptReady before
                                // all PTY output has been flushed to master_fd.
                                draining = true;
                            }
                            if let BackendControlEvent::PromptReady { cwd, .. } = event {
                                result_cwd = cwd.clone();
                            }
                        }
                    }
                    0 => {
                        // Control pipe closed -- bash exited.
                        self.running.store(false, Ordering::SeqCst);
                        done = true;
                    }
                    _ => {}
                }
            }
        }

        // Restore terminal.
        if let Some(ref saved) = saved_termios {
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, saved);
        }

        // Decode captured output, stripping ANSI escape sequences for a clean
        // text representation suitable for LLM context.
        let raw_output = String::from_utf8_lossy(&output_buf).to_string();
        let output = strip_ansi_escapes(&raw_output);

        Ok((result_exit_code, result_cwd, output))
    }

    /// Resize the PTY.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        ws.ws_row = rows;
        ws.ws_col = cols;
        unsafe {
            libc::ioctl(self.master_fd, libc::TIOCSWINSZ, &ws);
        }
    }

    /// Stop the bash session.
    pub fn stop(&mut self) {
        if !self.running.load(Ordering::SeqCst) {
            return; // Already stopped
        }
        self.running.store(false, Ordering::SeqCst);
        let _ = kill_pg(self.child_pid, Signal::SIGTERM);
        std::thread::sleep(Duration::from_millis(100));
        let _ = kill_pg(self.child_pid, Signal::SIGKILL);
        // Reap child.
        let _ = nix::sys::wait::waitpid(self.child_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG));
        // Close fds (use raw close to avoid IO Safety issues with from_raw_fd).
        if self.master_fd >= 0 {
            let _ = unsafe { libc::close(self.master_fd) };
            self.master_fd = -1;
        }
        if self.control_fd >= 0 {
            let _ = unsafe { libc::close(self.control_fd) };
            self.control_fd = -1;
        }
        self.command_state.reset();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn last_exit_code(&self) -> i32 {
        self.command_state.last_exit_code()
    }

    pub fn last_command(&self) -> &str {
        self.command_state.last_command()
    }

    pub fn can_correct_error(&self) -> bool {
        self.command_state.can_correct_error()
    }

    pub fn consume_error(&mut self) -> Option<(String, i32)> {
        self.command_state.consume_error()
    }

    pub fn clear_error_correction(&mut self) {
        self.command_state.clear_error_correction();
    }

    // ---- Internal helpers ----

    fn allocate_backend_seq(&mut self) -> i32 {
        let seq = self.next_backend_seq;
        self.next_backend_seq -= 1;
        seq
    }

    /// Drain any remaining data from master_fd and discard it.
    /// Used to clear stale prompt rendering output before sending a
    /// new command, so it does not leak into the forwarding loop.
    fn drain_master_silent(&self) {
        let mut tmp = [0u8; 8192];
        loop {
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => { /* discard */ }
                _ => break,
            }
        }
    }

    /// Drain any remaining data from master_fd to stdout.
    /// Called after the forwarding loop exits to prevent stale output
    /// from appearing at the start of the next command.
    fn drain_master_to_stdout(&self) {
        let mut tmp = [0u8; 8192];
        loop {
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    let _ = unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            tmp[..n as usize].as_ptr() as *const libc::c_void,
                            n as usize,
                        )
                    };
                }
                _ => break, // EAGAIN / EWOULDBLOCK / error -- nothing more to read
            }
        }
    }

    /// Drain remaining master_fd output into the exec buffer.
    /// Used by execute_command() to capture all output before returning.
    fn drain_master_to_exec_buffer(&self) {
        let mut tmp = [0u8; 8192];
        loop {
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    if self.exec_mode.load(Ordering::SeqCst) {
                        self.exec_buffer
                            .lock()
                            .unwrap()
                            .extend_from_slice(&tmp[..n as usize]);
                    }
                }
                _ => break,
            }
        }
    }

    /// Drain all remaining events from the control pipe.
    /// Called after session_ready to consume any stale prompt_ready events
    /// emitted during bash initialization, preventing them from being
    /// misinterpreted as completion of the first user command.
    fn drain_control_pipe(&mut self) {
        let mut tmp = [0u8; 4096];
        loop {
            match unsafe {
                libc::read(
                    self.control_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    let events = decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]);
                    // Process events (e.g., update command_state) but
                    // don't act on them -- they're stale.
                    for event in &events {
                        let _ = self.command_state.handle_event(event);
                    }
                }
                _ => break, // EAGAIN / nothing more to read
            }
        }
    }

    fn write_master(&self, data: &[u8]) -> aish_core::Result<()> {
        let mut written = 0;
        while written < data.len() {
            match unsafe {
                libc::write(
                    self.master_fd,
                    data[written..].as_ptr() as *const libc::c_void,
                    data.len() - written,
                )
            } {
                n if n > 0 => written += n as usize,
                _ => {
                    return Err(AishError::Pty("failed to write to master fd".into()));
                }
            }
        }
        Ok(())
    }

    fn wait_for_session_ready(&mut self, timeout: Duration) -> aish_core::Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            // Drain any initial bash output from master_fd.
            let mut tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    if let Some(ref cb) = self.output_callback {
                        cb(&tmp[..n as usize]);
                    }
                }
                0 => {
                    // EOF on master_fd -- bash exited during init.
                    self.running.store(false, Ordering::SeqCst);
                    return Err(AishError::Pty("bash exited before session_ready".into()));
                }
                _ => {}
            }

            // Read control pipe for session_ready.
            let mut ctrl_tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.control_fd,
                    ctrl_tmp.as_mut_ptr() as *mut libc::c_void,
                    ctrl_tmp.len(),
                )
            } {
                n if n > 0 => {
                    let events =
                        decode_control_chunk(&mut self.control_buffer, &ctrl_tmp[..n as usize]);
                    for event in &events {
                        if matches!(event, BackendControlEvent::SessionReady { .. }) {
                            debug!("received session_ready from bash");
                            return Ok(());
                        }
                    }
                }
                0 => {
                    // Control pipe closed -- bash exited during init.
                    self.running.store(false, Ordering::SeqCst);
                    return Err(AishError::Pty(
                        "control pipe closed before session_ready".into(),
                    ));
                }
                _ => {}
            }

            std::thread::sleep(Duration::from_millis(10));
        }
        Err(AishError::Pty(
            "timeout waiting for session_ready event".into(),
        ))
    }

    fn wait_for_prompt_ready(
        &mut self,
        expected_seq: i32,
        timeout: Duration,
    ) -> Option<crate::types::PtyCommandResult> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            // Drain master output into exec buffer.
            let mut tmp = [0u8; 8192];
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    let data: Vec<u8> = tmp[..n as usize].to_vec();
                    if self.exec_mode.load(Ordering::SeqCst) {
                        self.exec_buffer.lock().unwrap().extend_from_slice(&data);
                    } else if let Some(ref cb) = self.output_callback {
                        cb(&data);
                    }
                }
                0 => {
                    // EOF on master_fd -- bash exited.
                    self.running.store(false, Ordering::SeqCst);
                    return None;
                }
                _ => {}
            }

            // Read control pipe.
            let mut ctrl_tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.control_fd,
                    ctrl_tmp.as_mut_ptr() as *mut libc::c_void,
                    ctrl_tmp.len(),
                )
            } {
                n if n > 0 => {
                    let events =
                        decode_control_chunk(&mut self.control_buffer, &ctrl_tmp[..n as usize]);
                    for event in &events {
                        if let BackendControlEvent::ShellExiting { .. } = event {
                            self.running.store(false, Ordering::SeqCst);
                        }
                        if let Some(result) = self.command_state.handle_event(event) {
                            if result.command_seq == Some(expected_seq) {
                                return Some(result);
                            }
                        }
                    }
                }
                0 => {
                    // Control pipe closed -- bash exited.
                    self.running.store(false, Ordering::SeqCst);
                    return None;
                }
                _ => {}
            }

            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for PersistentPty {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- Child process ----

fn child_main(slave_fd: RawFd, control_write_fd: RawFd, rcfile_path: &str, cwd: &str) -> ! {
    unsafe { libc::setsid() };
    unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) };

    let _ = dup2(slave_fd, libc::STDIN_FILENO);
    let _ = dup2(slave_fd, libc::STDOUT_FILENO);
    let _ = dup2(slave_fd, libc::STDERR_FILENO);

    if slave_fd > 2 {
        let _ = close(slave_fd);
    }

    // Set CWD.
    let _ = std::env::set_current_dir(cwd);

    // Set env.
    std::env::set_var("TERM", "xterm-256color");

    // dup2 control_write_fd to fd 3 if it's not already.
    if control_write_fd != 3 {
        let rc = dup2(control_write_fd, 3);
        if rc.is_err() {
            let msg = b"aish: dup2 control_write_fd to fd 3 failed\n";
            unsafe {
                libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            }
            unsafe {
                libc::_exit(126);
            }
        }
        let _ = close(control_write_fd);
    }
    std::env::set_var("AISH_CONTROL_FD", "3");

    let c_shell = CString::new("/bin/bash").unwrap();
    let c_rcfile = CString::new(rcfile_path).unwrap();
    let c_interactive = CString::new("-i").unwrap();
    let c_rcfile_flag = CString::new("--rcfile").unwrap();

    let args = vec![c_shell.clone(), c_rcfile_flag, c_rcfile, c_interactive];

    let _ = execvp(&c_shell, &args);

    // execvp failed.
    unsafe {
        libc::_exit(127);
    }
}

// ---- Helpers ----

fn set_nonblocking(fd: &OwnedFd) -> aish_core::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = fcntl(raw, FcntlArg::F_GETFL)
        .map_err(|e| AishError::Pty(format!("fcntl F_GETFL failed: {e}")))?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(raw, FcntlArg::F_SETFL(flags))
        .map_err(|e| AishError::Pty(format!("fcntl F_SETFL O_NONBLOCK failed: {e}")))?;
    Ok(())
}

fn sync_window_size(src_fd: RawFd, dst_fd: RawFd) -> nix::Result<()> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(src_fd, libc::TIOCGWINSZ, &mut ws) };
    if rc >= 0 {
        unsafe {
            libc::ioctl(dst_fd, libc::TIOCSWINSZ, &ws);
        }
    }
    Ok(())
}

fn kill_pg(pid: Pid, sig: Signal) -> nix::Result<()> {
    kill(Pid::from_raw(-pid.as_raw()), sig)
}

/// Write the rc wrapper script to a temp file and return the path.
fn write_rcfile_temp() -> aish_core::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("aish-rc");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("rc-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, BASH_RC_WRAPPER)
        .map_err(|e| AishError::Pty(format!("failed to write rcfile temp: {e}")))?;
    Ok(path)
}

/// Simple shell quoting for embedding a command in a bash assignment.
pub fn shell_quote_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Check if a command needs a full interactive terminal.
pub fn is_interactive_command(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or("");
    let basename = first.rsplit('/').next().unwrap_or(first);
    if INTERACTIVE_COMMANDS.contains(&basename) {
        return true;
    }
    // sudo/su with interactive flags.
    if basename == "sudo" || basename == "su" {
        let lower = command.to_lowercase();
        if lower.contains("-i") || lower.contains("-s") || lower.contains("bash") {
            return true;
        }
    }
    false
}

/// Check if a command is an interactive session command (ssh/telnet etc.)
fn is_session_command(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or("");
    let basename = first.rsplit('/').next().unwrap_or(first);
    SESSION_COMMANDS.contains(&basename)
}

/// Strip ANSI escape sequences from a string to produce clean text
/// suitable for LLM context.
fn strip_ansi_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // ESC sequence
            match chars.peek() {
                Some('[') => {
                    chars.next(); // consume '['
                                  // CSI sequence: skip until a letter (final byte)
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next(); // consume ']'
                                  // OSC sequence: skip until BEL or ST
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    // Two-character sequence (e.g. ESC c)
                    chars.next();
                }
                None => {}
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Clean PTY output: strip ANSI, command echo, trailing prompt.
fn clean_pty_output(raw: &str, command: &str) -> String {
    // Strip ANSI escape sequences.
    let re = regex_simple();
    let text = re.replace_all(raw, "").to_string();

    // CRLF -> LF.
    let text = text.replace("\r\n", "\n").replace('\r', "");

    // Remove command echo.
    let cmd_trimmed = command.trim();
    if let Some(pos) = text.find(cmd_trimmed) {
        let after = &text[pos + cmd_trimmed.len()..];
        // Skip to next newline after the echo.
        if let Some(nl) = after.find('\n') {
            let cleaned = after[nl + 1..].to_string();
            return cleaned.trim().to_string();
        }
    }

    text.trim().to_string()
}

fn regex_simple() -> regex::Regex {
    regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_interactive_command() {
        assert!(is_interactive_command("vim file.txt"));
        assert!(is_interactive_command("ssh user@host"));
        assert!(is_interactive_command("htop"));
        assert!(!is_interactive_command("ls -la"));
        assert!(!is_interactive_command("echo hello"));
    }

    #[test]
    fn test_is_session_command() {
        assert!(is_session_command("ssh user@host"));
        assert!(is_session_command("telnet example.com"));
        assert!(!is_session_command("vim file.txt"));
        assert!(!is_session_command("ls"));
    }

    #[test]
    fn test_clean_pty_output() {
        let raw = "\x1b[0m\x1b[32mecho hello\x1b[0m\r\nhello world\r\n\x1b[?2004l";
        let cleaned = clean_pty_output(raw, "echo hello");
        assert_eq!(cleaned, "hello world");
    }

    #[test]
    fn test_shell_quote_escape() {
        assert_eq!(shell_quote_escape("ls -la"), "'ls -la'");
        assert_eq!(shell_quote_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_persistent_pty_start_stop() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");
        assert!(pty.is_running());
        pty.stop();
        assert!(!pty.is_running());
    }

    #[test]
    fn test_execute_simple_command() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");

        let (output, exit_code) = pty
            .execute_command("echo hello_world_123", Duration::from_secs(5))
            .expect("execute should succeed");
        assert_eq!(exit_code, 0);
        assert!(output.contains("hello_world_123"), "output was: {}", output);

        pty.stop();
    }

    #[test]
    fn test_execute_multiple_commands() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");

        let (out1, code1) = pty
            .execute_command("echo first", Duration::from_secs(5))
            .expect("cmd1");
        assert_eq!(code1, 0);
        assert!(out1.contains("first"));

        let (out2, code2) = pty
            .execute_command("echo second", Duration::from_secs(5))
            .expect("cmd2");
        assert_eq!(code2, 0);
        assert!(out2.contains("second"));

        pty.stop();
    }
}
