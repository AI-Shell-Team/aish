use std::ffi::CString;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::openpty;
use nix::sys::signal::{kill, Signal};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, OutputFlags, SetArg};
use nix::unistd::{close, dup2, execvp, fork, pipe, ForkResult, Pid};

use aish_core::AishError;
use tracing::debug;

use crate::command_state::CommandState;
use crate::control::{decode_control_chunk, BackendControlEvent};
use crate::types::{CancelToken, CommandSource};

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

        // Wait for session_ready event.  Also returns whether the
        // initial PromptReady was seen in the same control-pipe read
        // (common case: both arrive together).
        let saw_prompt = pty.wait_for_session_ready(Duration::from_secs(5))?;

        // Consume the initial PromptReady if it wasn't already seen
        // during session_ready.  Use a short timeout — bash emits it
        // very quickly after SessionReady.
        if !saw_prompt {
            pty.wait_for_initial_prompt_ready(Duration::from_millis(500));
        }
        pty.drain_master_to_stdout();

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

        // Prepend Ctrl-U (NAK) to clear stale input in the PTY line
        // discipline canonical buffer.  Keystrokes forwarded from the
        // interactive forwarding loop may linger there and corrupt the
        // next command.
        let mut payload = b"\x15".to_vec();
        if let Some(s) = seq {
            let quoted = shell_quote_escape(command);
            payload.extend_from_slice(
                format!(" __AISH_ACTIVE_COMMAND_SEQ={s}; __AISH_ACTIVE_COMMAND_TEXT={quoted}; ")
                    .as_bytes(),
            );
        }
        payload.extend_from_slice(command.as_bytes());
        payload.push(b'\n');

        self.write_master(&payload)
    }

    /// Execute a command and wait for completion with timeout.
    /// Returns cleaned output and exit code.
    /// When `cancel_token` is provided, the caller can request
    /// cancellation; on cancel the method sends SIGINT and returns
    /// exit code -1.
    pub fn execute_command(
        &mut self,
        command: &str,
        timeout: Duration,
        cancel_token: Option<&CancelToken>,
    ) -> aish_core::Result<(String, i32)> {
        let seq = self.allocate_backend_seq();

        // Enter exec mode: buffer output.
        self.exec_buffer.lock().unwrap().clear();
        self.exec_mode.store(true, Ordering::SeqCst);

        self.send_command(command, Some(seq))?;

        // Save and set terminal to non-canonical mode so we can read
        // individual bytes (Ctrl+Z = 0x1a, Ctrl+C = 0x03) without the
        // terminal driver intercepting them.
        let stdin_fd = libc::STDIN_FILENO;
        let stdin_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
        let saved_termios = tcgetattr(stdin_borrowed).ok();
        if let Some(ref saved) = saved_termios {
            let mut raw = saved.clone();
            use nix::sys::termios::{ControlFlags, InputFlags, LocalFlags};
            raw.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
            raw.input_flags &= !InputFlags::ISTRIP;
            raw.control_flags &= !ControlFlags::CSIZE;
            raw.control_flags |= ControlFlags::CS8;
            raw.control_chars[libc::VMIN] = 1;
            raw.control_chars[libc::VTIME] = 0;
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw);
        }

        let deadline = std::time::Instant::now() + timeout;
        let mut result_exit_code: i32 = -1;
        let mut cancelled = false;

        // Select-based I/O loop.
        'select_loop: while std::time::Instant::now() < deadline {
            // Check external cancellation.
            if let Some(ref ct) = cancel_token {
                if ct.is_cancelled() {
                    let _ = self.write_master(b"\x03");
                    cancelled = true;
                    break;
                }
            }

            let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            unsafe {
                libc::FD_ZERO(&mut read_fds);
                libc::FD_SET(stdin_fd, &mut read_fds);
                libc::FD_SET(self.master_fd, &mut read_fds);
                libc::FD_SET(self.control_fd, &mut read_fds);
            }
            let max_fd = self.master_fd.max(self.control_fd).max(stdin_fd) + 1;

            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let mut tv = libc::timeval {
                tv_sec: remaining.as_secs().min(1) as libc::c_long,
                tv_usec: (remaining.subsec_micros() % 1_000_000) as libc::c_long,
            };

            let sel = unsafe {
                libc::select(
                    max_fd,
                    &mut read_fds,
                    std::ptr::null_mut(),
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
                continue;
            }

            // Read stdin -> forward to master.
            if unsafe { libc::FD_ISSET(stdin_fd, &read_fds) } {
                let mut tmp = [0u8; 64];
                match unsafe {
                    libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        if data.contains(&0x03) {
                            // Ctrl+C: cancel and send SIGINT to bash.
                            let _ = kill_pg(self.child_pid, Signal::SIGINT);
                            if let Some(ref ct) = cancel_token {
                                ct.cancel();
                            }
                            cancelled = true;
                        } else {
                            // Forward everything else (including Ctrl+Z = 0x1a)
                            // to the PTY so bash handles it natively.
                            let _ = self.write_master(data);
                        }
                    }
                    _ => {}
                }
            }

            // Read master -> exec buffer.
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
                        if self.exec_mode.load(Ordering::SeqCst) {
                            self.exec_buffer
                                .lock()
                                .unwrap()
                                .extend_from_slice(&tmp[..n as usize]);
                        }
                    }
                    0 => {
                        self.running.store(false, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }

            // Read control pipe for events.
            if unsafe { libc::FD_ISSET(self.control_fd, &read_fds) } {
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
                                self.running.store(false, Ordering::SeqCst);
                            }
                            if let Some(r) = self.command_state.handle_event(event) {
                                if r.command_seq == Some(seq) {
                                    result_exit_code = r.exit_code;
                                    break 'select_loop;
                                }
                            }
                        }
                    }
                    0 => {
                        self.running.store(false, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Drain remaining output.
        self.drain_master_to_exec_buffer();
        self.exec_mode.store(false, Ordering::SeqCst);

        // Flush stale input so escape sequences don't confuse the next prompt.
        unsafe {
            libc::tcflush(stdin_fd, libc::TCIFLUSH);
        }

        // Restore terminal settings.
        if let Some(ref saved) = saved_termios {
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSADRAIN, saved);
        }

        let raw_output = self
            .exec_buffer
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<u8>>();
        let raw_str = String::from_utf8_lossy(&raw_output).to_string();

        if cancelled {
            let cleaned = clean_pty_output(&raw_str, command);
            Ok((cleaned, -1))
        } else {
            let cleaned = clean_pty_output(&raw_str, command);
            Ok((cleaned, result_exit_code))
        }
    }

    /// Send a user command and enter raw stdin forwarding mode until
    /// prompt_ready is received. Returns (exit_code, cwd, output).
    pub fn send_command_interactive(
        &mut self,
        command: &str,
        ai_callback: Option<Box<crate::AiCallback>>,
    ) -> aish_core::Result<(i32, String, String)> {
        let is_session = is_session_command(command);
        let mut interceptor = if is_session {
            crate::SessionInterceptor::new(ai_callback)
        } else {
            crate::SessionInterceptor::new(None)
        };

        // Drain stale data from both the PTY master fd and the control
        // pipe BEFORE registering the new command.  A stale PromptReady
        // left in the control pipe (e.g. from bash's initial prompt or
        // from a previous command whose event arrived late) would be
        // matched with the new command's submission, producing a wrong
        // exit code (the classic off-by-one shift).
        self.drain_master_silent();
        self.drain_control_pipe_raw();

        self.command_state
            .register_command(command, CommandSource::User, None);

        // Write command to bash.  Prepend Ctrl-U (NAK) to clear any
        // stale input in the PTY line discipline canonical buffer so
        // that leftover keystrokes from a previous interactive session
        // are not prepended to the actual command.
        let mut payload = vec![0x15];
        payload.extend_from_slice(command.as_bytes());
        payload.push(b'\n');
        self.write_master(&payload)?;

        // Save and set terminal to raw mode.
        let stdin_fd = libc::STDIN_FILENO;
        let stdin_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
        let saved_termios = tcgetattr(stdin_borrowed).ok();
        if let Some(ref saved) = saved_termios {
            let mut raw = saved.clone();
            cfmakeraw(&mut raw);
            // Re-enable output processing so that \n in PTY output is
            // converted to \r\n by the terminal driver.  Without this,
            // interactive sessions (ssh, telnet) display prompts
            // concatenated on the same line because the terminal emulator
            // only moves the cursor down for bare \n without returning to
            // column 0.
            raw.output_flags |= OutputFlags::OPOST | OutputFlags::ONLCR;
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
        // When a command is injected, the remote shell echoes it back.
        // Store the command here so the echo can be stripped from output.
        let mut skip_echo_cmd: Option<String> = None;
        // Followup callback state: after AI injects a command, capture its
        // output and call the followup when the shell goes idle.
        let mut pending_followup: Option<Box<crate::FollowupCallback>> = None;
        let mut followup_captured: Vec<u8> = Vec::new();
        let mut followup_capturing = false;
        // Pending AI response — shared between TriggerAi handler and
        // followup handler for multi-round tool chaining.
        let mut pending_response: Option<crate::AiResponse> = None;
        // Consecutive idle poll count — require N empty polls before treating
        // the shell as truly idle (prevents premature followup triggers over
        // SSH where brief network gaps can exceed 50ms).
        let mut idle_poll_count: u32 = 0;
        const IDLE_THRESHOLD: u32 = 3;

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
                // been delivered.  During normal phase increment the idle
                // counter to require consecutive empty polls before acting.
                idle_poll_count += 1;
                if draining {
                    done = true;
                }
                // Only treat the shell as idle after N consecutive timeouts
                // to avoid false positives from brief SSH network gaps.
                if idle_poll_count >= IDLE_THRESHOLD {
                    // No data for N * 50ms — the remote shell is idle and
                    // sitting at a prompt waiting for input.
                    if is_session {
                        interceptor.mark_prompt_ready();
                    }
                    // If we were capturing output for followup analysis, the
                    // command has finished — invoke the followup callback.
                    if followup_capturing {
                        // Detect stuck state: shell is showing a PS2 continuation
                        // prompt (e.g. unclosed heredoc/quote). Send Ctrl+C to
                        // cancel and skip the followup.
                        if looks_like_continuation_prompt(&followup_captured) {
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\x03".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            followup_capturing = false;
                            pending_followup = None;
                            followup_captured.clear();
                        } else {
                            followup_capturing = false;
                            if let Some(followup) = pending_followup.take() {
                                let output =
                                    String::from_utf8_lossy(&followup_captured).to_string();
                                let clean = strip_ansi_and_prompt(&output);
                                let next_response = followup(&clean);
                                if let Some(resp) = next_response {
                                    pending_response = Some(resp);
                                } else {
                                    unsafe {
                                        libc::write(
                                            self.master_fd,
                                            b"\r".as_ptr() as *const libc::c_void,
                                            1,
                                        );
                                    }
                                    skip_leading_newline = true;
                                }
                            }
                            followup_captured.clear();
                        }
                    }
                }
                // Process pending AI response (multi-round chaining).
                // Must happen here — the `continue` below skips the
                // normal pending_response block after the master-fd read.
                if let Some(response) = pending_response.take() {
                    // Handle ask_user first — it may produce a new pending_response
                    if let Some((request, channel)) = response.ask_user {
                        handle_ask_user_interaction(
                            request,
                            channel,
                            stdin_fd,
                            self.master_fd,
                            &mut pending_response,
                        );
                        // If ask_user produced a final response, fall through
                        // to process it on the next iteration.
                        continue;
                    }
                    if let Some(ref cmd) = response.command {
                        let tool_text = aish_i18n::t_with_args("shell.session.tool_bash", &{
                            let mut m = std::collections::HashMap::new();
                            m.insert("command".to_string(), cmd.clone());
                            m
                        });
                        let tool_line = format!("\x1b[36m{}\x1b[0m\r\n", tool_text);
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                tool_line.as_ptr() as *const libc::c_void,
                                tool_line.len(),
                            );
                        }
                        let confirm = format!(
                            "\x1b[33m{}\x1b[0m ",
                            aish_i18n::t("shell.session.confirm_execute")
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                confirm.as_ptr() as *const libc::c_void,
                                confirm.len(),
                            );
                        }
                        let mut ans = [0u8; 1];
                        let approved = match unsafe {
                            libc::read(stdin_fd, ans.as_mut_ptr() as *mut libc::c_void, 1)
                        } {
                            1 => {
                                let echo = if ans[0] == b'y'
                                    || ans[0] == b'Y'
                                    || ans[0] == b'\r'
                                    || ans[0] == b'\n'
                                {
                                    b"y\r\n"
                                } else {
                                    b"n\r\n"
                                };
                                unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        echo.as_ptr() as *const libc::c_void,
                                        echo.len(),
                                    );
                                }
                                // Drain trailing newline/CR so it doesn't leak
                                // into the next read cycle.
                                drain_stdin_trailing(stdin_fd);
                                ans[0] == b'y'
                                    || ans[0] == b'Y'
                                    || ans[0] == b'\r'
                                    || ans[0] == b'\n'
                            }
                            _ => false,
                        };
                        if approved {
                            let safe_cmd = close_unclosed_heredoc(cmd);
                            skip_echo_cmd = Some(safe_cmd.clone());
                            let mut inject = safe_cmd.as_bytes().to_vec();
                            inject.push(b'\r');
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    inject.as_ptr() as *const libc::c_void,
                                    inject.len(),
                                );
                            }
                            if response.followup.is_some() {
                                followup_captured.clear();
                                followup_capturing = true;
                                pending_followup = response.followup;
                            }
                        } else {
                            let cancel_msg = format!(
                                "\x1b[33m{}\x1b[0m\r\n",
                                aish_i18n::t("shell.command_cancelled")
                            );
                            unsafe {
                                libc::write(
                                    libc::STDOUT_FILENO,
                                    cancel_msg.as_ptr() as *const libc::c_void,
                                    cancel_msg.len(),
                                );
                                libc::write(
                                    self.master_fd,
                                    b"\r".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            // Call the followup with a cancellation message so
                            // the LLM thread receives output instead of
                            // "Channel closed" when the sender is dropped.
                            if let Some(followup) = response.followup {
                                let next_response = followup("Command cancelled by user");
                                if let Some(resp) = next_response {
                                    pending_response = Some(resp);
                                } else {
                                    skip_leading_newline = true;
                                }
                            } else {
                                skip_leading_newline = true;
                            }
                        }
                    } else {
                        unsafe {
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                    }
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

            // Read stdin -> interceptor or master (only during normal phase).
            if !draining && unsafe { libc::FD_ISSET(stdin_fd, &read_fds) } {
                let mut tmp = [0u8; 1024];
                match unsafe {
                    libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        idle_poll_count = 0;

                        // Non-session: original passthrough behavior
                        if !is_session {
                            if data.contains(&0x03) {
                                let _ = kill_pg(self.child_pid, Signal::SIGINT);
                            }
                            write_buf.extend_from_slice(data);
                            continue;
                        }

                        // Session command: route through interceptor
                        for &byte in data {
                            match interceptor.feed_stdin(byte) {
                                crate::StdinAction::Forward => {
                                    write_buf.push(byte);
                                }
                                crate::StdinAction::EchoLocally => unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        &byte as *const u8 as *const libc::c_void,
                                        1,
                                    );
                                },
                                crate::StdinAction::TriggerAi(question) => {
                                    // When triggered from line-level detection, the
                                    // PTY has already echoed the input line.  Send
                                    // Ctrl+C to cancel it on the remote side.
                                    if interceptor.take_cancel_pty_line() {
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                b"\x03".as_ptr() as *const libc::c_void,
                                                1,
                                            );
                                        }
                                        // Drain PTY output from Ctrl+C (^C + new prompt).
                                        // Must consume it NOW before calling the blocking
                                        // AI callback, otherwise it appears after the AI
                                        // response and confirmation prompt.
                                        let mut drain_buf = [0u8; 4096];
                                        loop {
                                            let mut rfds: libc::fd_set =
                                                unsafe { std::mem::zeroed() };
                                            unsafe {
                                                libc::FD_ZERO(&mut rfds);
                                                libc::FD_SET(self.master_fd, &mut rfds);
                                            }
                                            let mut tv = libc::timeval {
                                                tv_sec: 0,
                                                tv_usec: 100_000, // 100ms
                                            };
                                            let sel = unsafe {
                                                libc::select(
                                                    self.master_fd + 1,
                                                    &mut rfds,
                                                    std::ptr::null_mut(),
                                                    std::ptr::null_mut(),
                                                    &mut tv,
                                                )
                                            };
                                            if sel > 0
                                                && unsafe {
                                                    libc::FD_ISSET(self.master_fd, &mut rfds)
                                                }
                                            {
                                                let n = unsafe {
                                                    libc::read(
                                                        self.master_fd,
                                                        drain_buf.as_mut_ptr() as *mut libc::c_void,
                                                        drain_buf.len(),
                                                    )
                                                };
                                                if n <= 0 {
                                                    break;
                                                }
                                                let data = &drain_buf[..n as usize];
                                                interceptor.feed_pty_output(data);
                                                continue;
                                            }
                                            break;
                                        }
                                    }

                                    // Move to a new line (preserve user's input line)
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            b"\r\n".as_ptr() as *const libc::c_void,
                                            2,
                                        );
                                    }

                                    // Call AI callback — it handles ALL display
                                    // and returns an optional command to inject.
                                    let resp = interceptor.call_ai(question);
                                    interceptor.finish_ai();
                                    skip_leading_newline = true;
                                    if let Some(response) = resp {
                                        pending_response = Some(response);
                                    } else {
                                        // AI returned None — restore prompt
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                b"\r".as_ptr() as *const libc::c_void,
                                                1,
                                            );
                                        }
                                    }
                                }
                            }
                        }
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
                        idle_poll_count = 0;
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
                        // Strip the remote shell's echo of an injected command.
                        if let Some(ref echo_cmd) = skip_echo_cmd {
                            let pattern = format!("{}\r\n", echo_cmd).into_bytes();
                            if data.starts_with(&pattern) {
                                data = &data[pattern.len()..];
                            } else {
                                let pattern_cr = format!("{}\r", echo_cmd).into_bytes();
                                if data.starts_with(&pattern_cr) {
                                    data = &data[pattern_cr.len()..];
                                }
                            }
                            skip_echo_cmd = None;
                        }
                        if !data.is_empty() {
                            output_buf.extend_from_slice(data);
                            // Feed interceptor for line-start tracking and output buffering
                            if is_session {
                                interceptor.feed_pty_output(data);
                            }
                            // Capture output for followup analysis
                            if followup_capturing {
                                followup_captured.extend_from_slice(data);
                            }
                            // Display unless AI is processing
                            if !interceptor.is_ai_processing() {
                                let _ = unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        data.as_ptr() as *const libc::c_void,
                                        data.len(),
                                    )
                                };
                            }
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

            // Process pending AI response (from TriggerAi or followup chain).
            if let Some(response) = pending_response.take() {
                // Handle ask_user first
                if let Some((request, channel)) = response.ask_user {
                    handle_ask_user_interaction(
                        request,
                        channel,
                        stdin_fd,
                        self.master_fd,
                        &mut pending_response,
                    );
                    // pending_response may now contain the final AI response
                    // which will be processed on the next loop iteration.
                } else if let Some(ref cmd) = response.command {
                    // Show tool indicator matching local aish style
                    let tool_text = aish_i18n::t_with_args("shell.session.tool_bash", &{
                        let mut m = std::collections::HashMap::new();
                        m.insert("command".to_string(), cmd.clone());
                        m
                    });
                    let tool_line = format!("\x1b[36m{}\x1b[0m\r\n", tool_text);
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            tool_line.as_ptr() as *const libc::c_void,
                            tool_line.len(),
                        );
                    }

                    // Confirmation prompt before execution
                    let confirm = format!(
                        "\x1b[33m{}\x1b[0m ",
                        aish_i18n::t("shell.session.confirm_execute")
                    );
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            confirm.as_ptr() as *const libc::c_void,
                            confirm.len(),
                        );
                    }

                    // Read one byte for confirmation (raw mode)
                    let mut ans = [0u8; 1];
                    let approved = match unsafe {
                        libc::read(stdin_fd, ans.as_mut_ptr() as *mut libc::c_void, 1)
                    } {
                        1 => {
                            let echo = if ans[0] == b'y'
                                || ans[0] == b'Y'
                                || ans[0] == b'\r'
                                || ans[0] == b'\n'
                            {
                                b"y\r\n"
                            } else {
                                b"n\r\n"
                            };
                            unsafe {
                                libc::write(
                                    libc::STDOUT_FILENO,
                                    echo.as_ptr() as *const libc::c_void,
                                    echo.len(),
                                );
                            }
                            drain_stdin_trailing(stdin_fd);
                            ans[0] == b'y' || ans[0] == b'Y' || ans[0] == b'\r' || ans[0] == b'\n'
                        }
                        _ => false,
                    };

                    if approved {
                        let safe_cmd = close_unclosed_heredoc(cmd);
                        skip_echo_cmd = Some(safe_cmd.clone());
                        let mut inject = safe_cmd.as_bytes().to_vec();
                        inject.push(b'\r');
                        unsafe {
                            libc::write(
                                self.master_fd,
                                inject.as_ptr() as *const libc::c_void,
                                inject.len(),
                            );
                        }
                        if response.followup.is_some() {
                            followup_captured.clear();
                            followup_capturing = true;
                            pending_followup = response.followup;
                        }
                    } else {
                        let cancel_msg = format!(
                            "\x1b[33m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.command_cancelled")
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                cancel_msg.as_ptr() as *const libc::c_void,
                                cancel_msg.len(),
                            );
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                        // Call the followup with a cancellation message so
                        // the LLM thread receives output instead of
                        // "Channel closed" when the sender is dropped.
                        if let Some(followup) = response.followup {
                            let next_response = followup("Command cancelled by user");
                            if let Some(resp) = next_response {
                                pending_response = Some(resp);
                            } else {
                                skip_leading_newline = true;
                            }
                        } else {
                            skip_leading_newline = true;
                        }
                    }
                } else {
                    // AI returned explanation only (no command)
                    unsafe {
                        libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                    }
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
                                // Discard any stdin bytes captured in the same
                                // poll cycle.  Without this, a buffered newline
                                // could execute a stale line before the next
                                // command's Ctrl-U gets a chance to clear it.
                                write_buf.clear();
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

    /// Drain stale data from the control pipe and discard it without
    /// processing events through the command state machine.  Called
    /// before registering a new command to prevent a stale PromptReady
    /// (e.g. from bash's initial prompt or a late-arriving event) from
    /// being matched with the wrong command submission.
    fn drain_control_pipe_raw(&mut self) {
        self.control_buffer.clear();
        let mut tmp = [0u8; 4096];
        loop {
            match unsafe {
                libc::read(
                    self.control_fd,
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

    /// Wait for the first PromptReady event from bash (the initial prompt
    /// displayed after startup).  Best-effort — a timeout is not fatal
    /// because `send_command_interactive` also drains stale events.
    fn wait_for_initial_prompt_ready(&mut self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let mut tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.control_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    let events = decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]);
                    for event in &events {
                        if matches!(event, BackendControlEvent::PromptReady { .. }) {
                            debug!("consumed initial prompt_ready from bash");
                            return;
                        }
                    }
                }
                0 => {
                    // Control pipe closed.
                    return;
                }
                _ => {}
            }
            // Drain master_fd to the output callback so initial bash output
            // (MOTD, etc.) is not lost.
            let mut mtmp = [0u8; 8192];
            match unsafe {
                libc::read(
                    self.master_fd,
                    mtmp.as_mut_ptr() as *mut libc::c_void,
                    mtmp.len(),
                )
            } {
                n if n > 0 => {
                    if let Some(ref cb) = self.output_callback {
                        cb(&mtmp[..n as usize]);
                    }
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        debug!("timed out waiting for initial prompt_ready (non-fatal)");
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

    /// Returns Ok(true) if PromptReady was also seen in the same batch.
    fn wait_for_session_ready(&mut self, timeout: Duration) -> aish_core::Result<bool> {
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
                    let mut found_session = false;
                    let mut found_prompt = false;
                    for event in &events {
                        match event {
                            BackendControlEvent::SessionReady { .. } => found_session = true,
                            BackendControlEvent::PromptReady { .. } => found_prompt = true,
                            _ => {}
                        }
                    }
                    if found_session {
                        debug!("received session_ready from bash");
                        return Ok(found_prompt);
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
}

// ---- ask_user helpers for SSH sessions ----

/// Drain trailing bytes (e.g. `\n` or `\r`) from stdin after a single-byte
/// confirmation read so they don't leak into the next input cycle.
fn drain_stdin_trailing(stdin_fd: libc::c_int) {
    let mut discard = [0u8; 1];
    loop {
        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rfds);
            libc::FD_SET(stdin_fd, &mut rfds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 10_000, // 10ms
        };
        let sel = unsafe {
            libc::select(
                stdin_fd + 1,
                &mut rfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel <= 0 {
            break;
        }
        match unsafe { libc::read(stdin_fd, discard.as_mut_ptr() as *mut libc::c_void, 1) } {
            1 => {
                if discard[0] == b'\n' || discard[0] == b'\r' {
                    break;
                }
                // Non-newline byte — stop draining
                break;
            }
            _ => break,
        }
    }
}

/// Truncate a string to `max` bytes, respecting UTF-8 boundaries.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Debug helper: describe the answer kind without exposing the value.
fn answer_kind(answer: &crate::AskUserAnswer) -> &'static str {
    match answer {
        crate::AskUserAnswer::Response(_) => "Response",
        crate::AskUserAnswer::Cancelled => "Cancelled",
    }
}

/// How many lines the ask_user display occupies (so we can erase/redraw).
fn count_display_lines(request: &crate::AskUserRequest) -> usize {
    let mut lines = 1; // Header
    if request.kind == "choice_or_text" {
        if let Some(ref options) = request.options {
            lines += options.len() + 1; // options + custom input
        }
    }
    if request.default.is_some() {
        lines += 1;
    }
    lines += 1; // Help line
                // Prompt line "> " only for text_input mode
    if request.kind != "choice_or_text" {
        lines += 1;
    }
    lines
}

/// Erase the current ask_user display and redraw with the cursor at
/// `cursor` (only meaningful for choice_or_text).
fn redraw_ask_user(request: &crate::AskUserRequest, prev_lines: usize, cursor: usize) {
    let mut out = Vec::new();

    // Move up and clear
    if prev_lines > 0 {
        out.extend_from_slice(format!("\x1b[{}A", prev_lines).as_bytes());
    }
    out.extend_from_slice(b"\x1b[J"); // Clear from cursor to end of screen

    // Header — match local aish's inquire style
    out.extend_from_slice(b"\x1b[36m? \x1b[1m");
    if let Some(ref title) = request.title {
        out.extend_from_slice(title.as_bytes());
        out.extend_from_slice(b": ");
    }
    out.extend_from_slice(request.prompt.as_bytes());
    out.extend_from_slice(b"\x1b[0m\r\n");

    // Options with cursor highlight for choice_or_text
    if request.kind == "choice_or_text" {
        if let Some(ref options) = request.options {
            for (i, opt) in options.iter().enumerate() {
                // Use inquire-style cursor: ">" for selected, " " for others
                if i == cursor {
                    out.extend_from_slice(b"\x1b[36m> \x1b[1m");
                } else {
                    out.extend_from_slice(b"  ");
                }
                out.extend_from_slice(opt.label.as_bytes());
                if let Some(ref desc) = opt.description {
                    out.extend_from_slice(format!(" - {}", desc).as_bytes());
                }
                out.extend_from_slice(b"\x1b[0m\r\n");
            }
            // Custom input entry at the bottom — same label as local aish
            let custom_label = aish_i18n::t("shell.session.ask_user.custom_input_label");
            if cursor == options.len() {
                out.extend_from_slice(b"\x1b[36m> \x1b[1m");
            } else {
                out.extend_from_slice(b"  ");
            }
            out.extend_from_slice(format!("({})", custom_label).as_bytes());
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
    }

    // Default hint — match local aish's [default: xxx] format
    if let Some(ref default) = request.default {
        let default_hint = aish_i18n::t_with_args("shell.session.ask_user.default_hint", &{
            let mut m = std::collections::HashMap::new();
            m.insert("default".to_string(), default.clone());
            m
        });
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(default_hint.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    }

    // Help message — match local aish's style
    if request.allow_cancel {
        let help = aish_i18n::t("shell.session.ask_user.help_with_cancel");
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(help.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    } else {
        let help = aish_i18n::t("shell.session.ask_user.help_no_cancel");
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(help.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    }

    // Prompt (only for text_input mode)
    if request.kind != "choice_or_text" {
        out.extend_from_slice(b"\x1b[33m> \x1b[0m");
    }

    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            out.as_ptr() as *const libc::c_void,
            out.len(),
        );
    }
}

/// Initial display — ensure we start on a fresh line.
fn display_ask_user(request: &crate::AskUserRequest) {
    // Move to a new line to avoid garbling with previous AI output
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            b"\r\n".as_ptr() as *const libc::c_void,
            2,
        );
    }
    redraw_ask_user(request, 0, 0);
}

/// Read one raw byte from stdin with EINTR retry.
/// Returns the byte on success, or None on EOF/error.
fn read_byte(stdin_fd: libc::c_int) -> Option<u8> {
    loop {
        let mut byte = [0u8; 1];
        let n = unsafe { libc::read(stdin_fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
        match n {
            1 => return Some(byte[0]),
            -1 => {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    continue;
                }
                debug!("read_byte: error, errno={}", errno);
                return None;
            }
            0 => {
                debug!("read_byte: EOF");
                return None;
            }
            _ => {
                debug!("read_byte: unexpected return {}", n);
                continue;
            }
        }
    }
}

/// Check whether stdin has data available within the given timeout.
fn stdin_poll(stdin_fd: libc::c_int, timeout: Duration) -> bool {
    let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::FD_ZERO(&mut rfds);
        libc::FD_SET(stdin_fd, &mut rfds);
    }
    let mut tv = libc::timeval {
        tv_sec: timeout.as_secs() as _,
        tv_usec: timeout.subsec_micros() as _,
    };
    let sel = unsafe {
        libc::select(
            stdin_fd + 1,
            &mut rfds,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut tv,
        )
    };
    sel > 0
}

/// Consume a CSI escape sequence (already read `\x1b[`).
/// CSI format: parameters (0x30-0x3F)* intermediate (0x20-0x2F)* final (0x40-0x7E)
/// Returns the final byte (e.g. 'A' for up arrow) or None on error.
fn consume_csi(stdin_fd: libc::c_int) -> Option<u8> {
    loop {
        match read_byte(stdin_fd) {
            Some(b) if b >= 0x40 && b <= 0x7E => return Some(b),
            Some(_) => continue, // parameter or intermediate byte
            None => return None,
        }
    }
}

/// Read a line of user input in raw mode with escape-sequence handling.
/// For choice_or_text: up/down arrows navigate options (including custom
/// input slot at the bottom), Enter selects. Typing text switches to
/// custom input mode.
/// For text_input: normal text editing, Enter submits.
/// Ctrl+C always cancels. Esc cancels only if allow_cancel is true.
fn read_line_from_stdin_raw(
    stdin_fd: libc::c_int,
    request: &crate::AskUserRequest,
) -> crate::AskUserAnswer {
    let is_choice = request.kind == "choice_or_text";
    let num_options = request.options.as_ref().map_or(0, |o| o.len());
    let has_options = is_choice && num_options > 0;
    // Total selectable slots: options + 1 custom-input slot
    let total_slots = if has_options { num_options + 1 } else { 0 };

    // For choice mode, track cursor position
    let mut cursor: usize = 0;
    let mut text_buf: Vec<u8> = Vec::new();

    loop {
        match read_byte(stdin_fd) {
            Some(byte) => match byte {
                // Ctrl+C → always cancel
                0x03 => {
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, b"^C\r\n".as_ptr() as *const _, 5);
                    }
                    return crate::AskUserAnswer::Cancelled;
                }
                // Enter → submit
                b'\r' | b'\n' => {
                    // After printing \r\n the cursor is one line below the
                    // display.  prev_lines must account for the full display
                    // height so redraw can move back to the header line.
                    let prev = count_display_lines(request) + 1; // +1 for the \r\n
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, b"\r\n".as_ptr() as *const _, 2);
                    }
                    // If user typed text, treat as custom input
                    if !text_buf.is_empty() {
                        let text = String::from_utf8_lossy(&text_buf).to_string();
                        let trimmed = text.trim().to_string();
                        if trimmed.is_empty() {
                            // Empty after trim — treat like empty
                            if let Some(ref default) = request.default {
                                return crate::AskUserAnswer::Response(default.clone());
                            }
                            if request.allow_cancel {
                                return crate::AskUserAnswer::Cancelled;
                            }
                            // Required — redisplay and loop
                            redraw_ask_user(request, prev, cursor);
                            text_buf.clear();
                            continue;
                        }
                        if trimmed.len() < request.min_length {
                            let min_len_msg = aish_i18n::t_with_args(
                                "shell.session.ask_user.min_length_error",
                                &{
                                    let mut m = std::collections::HashMap::new();
                                    m.insert("min".to_string(), request.min_length.to_string());
                                    m
                                },
                            );
                            let msg = format!("\x1b[31m{}\x1b[0m\r\n", min_len_msg);
                            unsafe {
                                libc::write(
                                    libc::STDOUT_FILENO,
                                    msg.as_ptr() as *const libc::c_void,
                                    msg.len(),
                                );
                            }
                            redraw_ask_user(request, prev, cursor);
                            text_buf.clear();
                            continue;
                        }
                        return crate::AskUserAnswer::Response(trimmed);
                    }
                    // No text typed — select by cursor position
                    if has_options {
                        if cursor < num_options {
                            // Regular option selected
                            let value = request.options.as_ref().unwrap()[cursor].value.clone();
                            return crate::AskUserAnswer::Response(value);
                        } else {
                            // Custom-input slot selected with no text —
                            // stay in input mode (same as local AskUserTool
                            // which goes back to select on empty input)
                            redraw_ask_user(request, prev, cursor);
                            continue;
                        }
                    }
                    // text_input mode with empty input
                    if let Some(ref default) = request.default {
                        return crate::AskUserAnswer::Response(default.clone());
                    }
                    if request.allow_cancel {
                        return crate::AskUserAnswer::Cancelled;
                    }
                    // Required — redisplay and loop
                    redraw_ask_user(request, prev, cursor);
                    continue;
                }
                // Backspace / Delete
                0x7F | 0x08 => {
                    if !text_buf.is_empty() {
                        // Pop trailing UTF-8 continuation bytes, then leader
                        while text_buf.last().map_or(false, |b| b & 0xC0 == 0x80) {
                            text_buf.pop();
                        }
                        let leader = text_buf.pop().unwrap();
                        // Display width: ASCII=1, 2-byte=1, 3-byte(CJK)=2, 4-byte=2
                        let width = if leader < 0x80 {
                            1
                        } else if leader & 0xE0 == 0xC0 {
                            1
                        } else if leader & 0xF0 == 0xE0 {
                            2
                        } else {
                            2
                        };
                        let erase = format!(
                            "{}{}{}",
                            "\x08".repeat(width),
                            " ".repeat(width),
                            "\x08".repeat(width),
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                erase.as_ptr() as *const _,
                                erase.len(),
                            );
                        }
                    }
                }
                // Escape — could be standalone Esc or start of escape sequence
                0x1B => {
                    // Use 100ms timeout: long enough to cover SSH network
                    // latency (direction keys arrive as ESC [ A in separate
                    // packets) while still allowing standalone ESC to cancel.
                    if stdin_poll(stdin_fd, Duration::from_micros(100_000)) {
                        // Escape sequence — read next byte
                        match read_byte(stdin_fd) {
                            Some(b'[') => {
                                // CSI sequence
                                match consume_csi(stdin_fd) {
                                    Some(b'A') | Some(b'k') => {
                                        // Up arrow — navigate in choice mode
                                        if total_slots > 0 {
                                            if cursor > 0 {
                                                cursor -= 1;
                                            } else {
                                                cursor = total_slots - 1;
                                            }
                                            // Clear text buffer when navigating
                                            if !text_buf.is_empty() {
                                                let erase = "\x08".repeat(text_buf.len())
                                                    + &" ".repeat(text_buf.len())
                                                    + &"\x08".repeat(text_buf.len());
                                                unsafe {
                                                    libc::write(
                                                        libc::STDOUT_FILENO,
                                                        erase.as_ptr() as *const _,
                                                        erase.len(),
                                                    );
                                                }
                                                text_buf.clear();
                                            }
                                            let prev = if request.kind == "choice_or_text" {
                                                count_display_lines(request)
                                            } else {
                                                count_display_lines(request).saturating_sub(1)
                                            };
                                            redraw_ask_user(request, prev, cursor);
                                        }
                                    }
                                    Some(b'B') | Some(b'j') => {
                                        // Down arrow — navigate in choice mode
                                        if total_slots > 0 {
                                            if cursor + 1 < total_slots {
                                                cursor += 1;
                                            } else {
                                                cursor = 0;
                                            }
                                            if !text_buf.is_empty() {
                                                let erase = "\x08".repeat(text_buf.len())
                                                    + &" ".repeat(text_buf.len())
                                                    + &"\x08".repeat(text_buf.len());
                                                unsafe {
                                                    libc::write(
                                                        libc::STDOUT_FILENO,
                                                        erase.as_ptr() as *const _,
                                                        erase.len(),
                                                    );
                                                }
                                                text_buf.clear();
                                            }
                                            let prev = if request.kind == "choice_or_text" {
                                                count_display_lines(request)
                                            } else {
                                                count_display_lines(request).saturating_sub(1)
                                            };
                                            redraw_ask_user(request, prev, cursor);
                                        }
                                    }
                                    _ => {
                                        // Other CSI sequences (Home, End, PgUp, etc.) — ignore
                                    }
                                }
                            }
                            Some(b'O') => {
                                // SS3 sequence (F-keys, etc.) — consume final byte and ignore
                                let _ = read_byte(stdin_fd);
                            }
                            Some(_) => {
                                // Other escape sequences — ignore
                            }
                            None => {
                                // Incomplete sequence — treat as Esc
                                if request.allow_cancel {
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            b"\r\n".as_ptr() as *const _,
                                            2,
                                        );
                                    }
                                    return crate::AskUserAnswer::Cancelled;
                                }
                                // Not allowed to cancel — ignore
                            }
                        }
                    } else {
                        // Standalone Escape
                        if request.allow_cancel {
                            unsafe {
                                libc::write(libc::STDOUT_FILENO, b"\r\n".as_ptr() as *const _, 2);
                            }
                            return crate::AskUserAnswer::Cancelled;
                        }
                        // Not allowed to cancel — ignore
                    }
                }
                // Normal byte — typing text automatically switches to custom input
                _ => {
                    text_buf.push(byte);
                    // Echo
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            &byte as *const u8 as *const libc::c_void,
                            1,
                        );
                    }
                }
            },
            None => {
                // EOF or error
                return crate::AskUserAnswer::Cancelled;
            }
        }
    }
}

/// Handle an ask_user interaction: display question, read answer, wait for
/// next LLM event.  Sets `pending_response` with the final AI response (or
/// None if the LLM finished without further action).
fn handle_ask_user_interaction(
    request: crate::AskUserRequest,
    channel: crate::AskUserChannel,
    stdin_fd: libc::c_int,
    master_fd: libc::c_int,
    pending_response: &mut Option<crate::AiResponse>,
) {
    debug!(
        "handle_ask_user: kind={}, prompt={}",
        request.kind, request.prompt
    );

    // Show tool indicator matching local aish style
    let args_preview = match request.kind.as_str() {
        "choice_or_text" => {
            let n = request.options.as_ref().map_or(0, |o| o.len());
            let mut m = std::collections::HashMap::new();
            m.insert(
                "prompt".to_string(),
                truncate_str(&request.prompt, 60).to_string(),
            );
            m.insert("count".to_string(), n.to_string());
            aish_i18n::t_with_args("shell.session.ask_user.choice_preview", &m)
        }
        _ => {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "prompt".to_string(),
                truncate_str(&request.prompt, 80).to_string(),
            );
            aish_i18n::t_with_args("shell.session.ask_user.text_preview", &m)
        }
    };
    let mut tool_args = std::collections::HashMap::new();
    tool_args.insert("preview".to_string(), args_preview);
    let tool_line = format!(
        "\x1b[36m{}\x1b[0m\r\n",
        aish_i18n::t_with_args("shell.session.ask_user.tool_banner", &tool_args)
    );
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            tool_line.as_ptr() as *const libc::c_void,
            tool_line.len(),
        );
    }

    display_ask_user(&request);
    let answer = read_line_from_stdin_raw(stdin_fd, &request);
    debug!("handle_ask_user: got answer {:?}", answer_kind(&answer));

    if channel.answer_sender.send(answer).is_err() {
        debug!("handle_ask_user: answer channel closed");
        return;
    }

    // Wait for next event from LLM, forwarding PTY output meanwhile.
    loop {
        match channel.event_receiver.try_recv() {
            Ok(crate::AiEvent::Done(resp)) => {
                debug!("handle_ask_user: LLM done, has_command={}", resp.is_some());
                *pending_response = resp;
                break;
            }
            Ok(crate::AiEvent::AskUser(next_req)) => {
                debug!(
                    "handle_ask_user: follow-up ask_user, prompt={}",
                    next_req.prompt
                );
                display_ask_user(&next_req);
                let answer = read_line_from_stdin_raw(stdin_fd, &next_req);
                debug!(
                    "handle_ask_user: follow-up answer {:?}",
                    answer_kind(&answer)
                );
                if channel.answer_sender.send(answer).is_err() {
                    break;
                }
                continue;
            }
            Ok(crate::AiEvent::BashExec {
                command,
                output_sender,
            }) => {
                debug!("handle_ask_user: follow-up bash_exec, cmd={}", command);
                // Show tool indicator and confirmation, then execute inline.
                let tool_text = aish_i18n::t_with_args("shell.session.tool_bash", &{
                    let mut m = std::collections::HashMap::new();
                    m.insert("command".to_string(), command.clone());
                    m
                });
                let tool_line = format!("\x1b[36m{}\x1b[0m\r\n", tool_text);
                unsafe {
                    libc::write(
                        libc::STDOUT_FILENO,
                        tool_line.as_ptr() as *const libc::c_void,
                        tool_line.len(),
                    );
                }
                let confirm = format!(
                    "\x1b[33m{}\x1b[0m ",
                    aish_i18n::t("shell.session.confirm_execute")
                );
                unsafe {
                    libc::write(
                        libc::STDOUT_FILENO,
                        confirm.as_ptr() as *const libc::c_void,
                        confirm.len(),
                    );
                }
                let mut ans = [0u8; 1];
                let approved =
                    match unsafe { libc::read(stdin_fd, ans.as_mut_ptr() as *mut libc::c_void, 1) }
                    {
                        1 => {
                            let echo = if ans[0] == b'y'
                                || ans[0] == b'Y'
                                || ans[0] == b'\r'
                                || ans[0] == b'\n'
                            {
                                b"y\r\n"
                            } else {
                                b"n\r\n"
                            };
                            unsafe {
                                libc::write(
                                    libc::STDOUT_FILENO,
                                    echo.as_ptr() as *const libc::c_void,
                                    echo.len(),
                                );
                            }
                            drain_stdin_trailing(stdin_fd);
                            ans[0] == b'y' || ans[0] == b'Y' || ans[0] == b'\r' || ans[0] == b'\n'
                        }
                        _ => false,
                    };
                if approved {
                    let safe_cmd = close_unclosed_heredoc(&command);
                    let mut inject = safe_cmd.as_bytes().to_vec();
                    inject.push(b'\r');
                    unsafe {
                        libc::write(
                            master_fd,
                            inject.as_ptr() as *const libc::c_void,
                            inject.len(),
                        );
                    }
                    // Wait for command output until the shell goes idle.
                    let mut captured = Vec::new();
                    let mut idle_count: u32 = 0;
                    loop {
                        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
                        unsafe {
                            libc::FD_ZERO(&mut rfds);
                            libc::FD_SET(master_fd, &mut rfds);
                        }
                        let mut tv = libc::timeval {
                            tv_sec: 0,
                            tv_usec: 50_000,
                        };
                        let sel = unsafe {
                            libc::select(
                                master_fd + 1,
                                &mut rfds,
                                std::ptr::null_mut(),
                                std::ptr::null_mut(),
                                &mut tv,
                            )
                        };
                        if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &mut rfds) } {
                            let mut tmp = [0u8; 4096];
                            match unsafe {
                                libc::read(
                                    master_fd,
                                    tmp.as_mut_ptr() as *mut libc::c_void,
                                    tmp.len(),
                                )
                            } {
                                n if n > 0 => {
                                    let data = &tmp[..n as usize];
                                    captured.extend_from_slice(data);
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            data.as_ptr() as *const libc::c_void,
                                            data.len(),
                                        );
                                    }
                                    idle_count = 0;
                                }
                                _ => break,
                            }
                        } else {
                            idle_count += 1;
                            if idle_count >= 3 {
                                break;
                            }
                        }
                    }
                    let output = String::from_utf8_lossy(&captured).to_string();
                    let clean = strip_ansi_and_prompt(&output);
                    let _ = output_sender.send(clean);
                } else {
                    let cancel_msg = format!(
                        "\x1b[33m{}\x1b[0m\r\n",
                        aish_i18n::t("shell.command_cancelled")
                    );
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            cancel_msg.as_ptr() as *const libc::c_void,
                            cancel_msg.len(),
                        );
                    }
                    let _ = output_sender.send(format!("(cancelled: {})", command));
                }
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Forward PTY output while waiting for LLM
                let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
                unsafe {
                    libc::FD_ZERO(&mut rfds);
                    libc::FD_SET(master_fd, &mut rfds);
                }
                let mut tv = libc::timeval {
                    tv_sec: 0,
                    tv_usec: 50_000, // 50ms
                };
                let sel = unsafe {
                    libc::select(
                        master_fd + 1,
                        &mut rfds,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut tv,
                    )
                };
                if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &mut rfds) } {
                    let mut tmp = [0u8; 4096];
                    match unsafe {
                        libc::read(master_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                    } {
                        n if n > 0 => unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                tmp.as_ptr() as *const libc::c_void,
                                n as usize,
                            );
                        },
                        _ => {}
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
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

/// Strip ANSI escapes and trim trailing shell prompt from captured output.
/// Removes the last non-empty line (typically a prompt like `user@host:~$ `).
fn strip_ansi_and_prompt(raw: &str) -> String {
    let clean = strip_ansi_escapes(raw);
    let mut lines: Vec<&str> = clean.lines().collect();
    // Remove trailing empty lines
    while lines.last().map_or(false, |l| l.trim().is_empty()) {
        lines.pop();
    }
    // Remove last non-empty line (shell prompt)
    if !lines.is_empty() {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
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

/// Detect unclosed heredoc in a shell command and close it.
/// Returns the command with missing heredoc closing delimiters appended.
/// e.g. "cat > f << 'EOF'" → "cat > f << 'EOF'\nEOF"
fn close_unclosed_heredoc(cmd: &str) -> String {
    let bytes = cmd.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    let mut result = cmd.to_string();
    let mut appended = false;

    while i + 1 < len {
        if bytes[i] == b'<' && bytes[i + 1] == b'<' {
            // Skip << and optional -
            let mut j = i + 2;
            if j < len && bytes[j] == b'-' {
                j += 1;
            }
            // Skip whitespace
            while j < len && bytes[j] == b' ' {
                j += 1;
            }
            // Skip optional quote
            if j < len && (bytes[j] == b'\'' || bytes[j] == b'"') {
                j += 1;
            }
            // Extract delimiter word
            let delim_start = j;
            while j < len
                && ![b' ', b'\n', b'\r', b';', b'&', b'|', b'<', b'>', b'#'].contains(&bytes[j])
                && bytes[j] != b'\''
                && bytes[j] != b'"'
            {
                j += 1;
            }
            let delimiter = &cmd[delim_start..j];

            if !delimiter.is_empty() {
                // Check if delimiter appears as a standalone line after the <<
                let search_start = if appended { 0 } else { j.min(len) };
                let rest = &result[search_start..];
                let closed = rest.lines().any(|line| line.trim() == delimiter);
                if !closed {
                    result.push('\n');
                    result.push_str(delimiter);
                    appended = true;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    result
}

/// Detect if PTY output looks like a continuation prompt (PS2: `> `).
/// Used to detect stuck heredoc/quote states after command injection.
fn looks_like_continuation_prompt(output: &[u8]) -> bool {
    if output.is_empty() {
        return false;
    }
    let stripped = strip_ansi_escapes(&String::from_utf8_lossy(output));
    let lines: Vec<&str> = stripped.lines().collect();
    if let Some(last_line) = lines.last() {
        let trimmed_line = last_line.trim();
        return trimmed_line == ">" || trimmed_line.ends_with("> ");
    }
    false
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
            .execute_command("echo hello_world_123", Duration::from_secs(5), None)
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
            .execute_command("echo first", Duration::from_secs(5), None)
            .expect("cmd1");
        assert_eq!(code1, 0);
        assert!(out1.contains("first"));

        let (out2, code2) = pty
            .execute_command("echo second", Duration::from_secs(5), None)
            .expect("cmd2");
        assert_eq!(code2, 0);
        assert!(out2.contains("second"));

        pty.stop();
    }
}
