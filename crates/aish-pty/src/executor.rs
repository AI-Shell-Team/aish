use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};

use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::openpty;
use nix::sys::signal::{kill, Signal};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{close, dup2, execvp, fork, pipe, ForkResult, Pid};

use aish_core::{AishError, CommandResult, CommandStatus};

use tracing::{debug, warn};

use crate::offload::PtyOutputOffload;
use crate::types::{CancelToken, StreamName};

/// Send a signal to the process *group* led by `pid`.
fn kill_pg(pid: Pid, sig: Signal) -> nix::Result<()> {
    let pgid = -pid.as_raw();
    kill(Pid::from_raw(pgid), sig)
}

// ---------------------------------------------------------------------------
// PtyExecutor
// ---------------------------------------------------------------------------

pub struct PtyExecutor {
    /// How many bytes of tail output to keep in memory.
    pub keep_bytes: usize,
    /// Whether to write output to the real terminal in real-time.
    /// When false, output is only captured in buffers (for AI tool use).
    display_output: bool,
}

impl PtyExecutor {
    pub fn new(keep_bytes: usize) -> Self {
        Self {
            keep_bytes,
            display_output: true,
        }
    }

    /// Create a silent executor that does not write output to the terminal.
    /// Suitable for AI tool execution where output should be captured, not displayed.
    pub fn new_silent(keep_bytes: usize) -> Self {
        Self {
            keep_bytes,
            display_output: false,
        }
    }

    /// Execute a command with full PTY support.
    ///
    /// This is a synchronous, blocking call designed to run inside
    /// `tokio::task::spawn_blocking`.
    pub fn execute_blocking(
        &self,
        command: &str,
        env_vars: HashMap<String, String>,
        cancel_token: &CancelToken,
    ) -> aish_core::Result<CommandResult> {
        self.execute_inner(command, env_vars, cancel_token)
    }

    fn execute_inner(
        &self,
        command: &str,
        env_vars: HashMap<String, String>,
        cancel_token: &CancelToken,
    ) -> aish_core::Result<CommandResult> {
        let session_uuid = uuid::Uuid::new_v4().to_string();
        let base_dir = std::env::temp_dir().to_str().unwrap_or("/tmp").to_string();

        // When display_output is false, skip raw mode and stdin forwarding
        // since we don't need interactive terminal behavior.
        if !self.display_output {
            return self.execute_silent_inner(
                command,
                env_vars,
                cancel_token,
                &session_uuid,
                &base_dir,
            );
        }

        // Create PTY master/slave pair.
        let pty_result =
            openpty(None, None).map_err(|e| AishError::Pty(format!("failed to openpty: {e}")))?;
        let master_fd = pty_result.master;
        let slave_fd = pty_result.slave;

        debug!(
            "openpty: master={}, slave={}",
            master_fd.as_raw_fd(),
            slave_fd.as_raw_fd()
        );

        // Create a pipe for stderr (PTY merges stdout/stderr, but we try to
        // separate them using a stderr pipe).
        let (stderr_pipe_read, stderr_pipe_write) =
            pipe().map_err(|e| AishError::Pty(format!("failed to create stderr pipe: {e}")))?;

        // Set master fd to non-blocking.
        set_nonblocking(&master_fd)?;

        // Save current terminal settings so we can restore them later.
        let stdin_fd: RawFd = libc::STDIN_FILENO;
        // SAFETY: stdin_fd is libc::STDIN_FILENO (0), which is always a valid
        // open file descriptor at process start. We only borrow it temporarily
        // for tcgetattr/tcsetattr calls; we do not close it.
        let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
        let saved_termios = tcgetattr(stdin_borrowed).ok();

        // Set stdin to raw mode.
        if let Some(ref saved) = saved_termios {
            let mut raw = saved.clone();
            cfmakeraw(&mut raw);
            if let Err(e) = tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw) {
                warn!("failed to set stdin to raw mode: {e}");
            }
        }

        // Try to sync window size from stdin to the PTY.
        let _ = sync_window_size(stdin_fd, master_fd.as_raw_fd());

        // Fork the child process.
        // We need raw fds for the child (after fork we can't use OwnedFd).
        let slave_raw = slave_fd.as_raw_fd();
        let stderr_write_raw = stderr_pipe_write.as_raw_fd();

        // SAFETY: fork() is called once during PTY setup. The parent and child
        // take completely different code paths immediately after the fork: the
        // parent closes slave fds and proceeds to the I/O loop, while the child
        // calls child_main() which never returns.
        let child_pid =
            match unsafe { fork() }.map_err(|e| AishError::Pty(format!("fork failed: {e}")))? {
                ForkResult::Parent { child } => {
                    // Close slave and write-end of stderr pipe in parent.
                    // They are OwnedFd so they will be dropped automatically, but
                    // let's be explicit.
                    drop(slave_fd);
                    drop(stderr_pipe_write);
                    child
                }
                ForkResult::Child => {
                    // --- Child process ---
                    // child_main returns `!` so it never comes back.
                    child_main(slave_raw, stderr_write_raw, command, env_vars);
                }
            };

        debug!(pid = %child_pid, "child process started");

        // Convert OwnedFds to raw for the I/O loop (we manage closing manually).
        let master_raw = master_fd.into_raw_fd();
        let stderr_read_raw = stderr_pipe_read.into_raw_fd();

        // Run the I/O loop.
        let result = self.run_io_loop(
            master_raw,
            stderr_read_raw,
            child_pid,
            cancel_token,
            &session_uuid,
            &base_dir,
            command,
        );

        // Restore terminal settings.
        if let Some(ref saved) = saved_termios {
            if let Err(e) = tcsetattr(stdin_borrowed, SetArg::TCSANOW, saved) {
                warn!("failed to restore terminal settings: {e}");
            }
        }

        // Close master and stderr pipe read end.
        // SAFETY: master_raw and stderr_read_raw are valid open file descriptors
        // that we own (converted from OwnedFd via into_raw_fd earlier).
        // File::from_raw_fd takes ownership so the File drop will close them.
        let _ = unsafe { File::from_raw_fd(master_raw) }; // drop closes it
        let _ = unsafe { File::from_raw_fd(stderr_read_raw) };

        result
    }

    /// Silent execution: no raw mode, no stdin forwarding, no terminal output.
    /// Only captures stdout/stderr into buffers for structured return.
    #[allow(clippy::too_many_arguments)]
    fn execute_silent_inner(
        &self,
        command: &str,
        env_vars: HashMap<String, String>,
        cancel_token: &CancelToken,
        session_uuid: &str,
        base_dir: &str,
    ) -> aish_core::Result<CommandResult> {
        // Create PTY master/slave pair.
        let pty_result =
            openpty(None, None).map_err(|e| AishError::Pty(format!("failed to openpty: {e}")))?;
        let master_fd = pty_result.master;
        let slave_fd = pty_result.slave;

        // Create stderr pipe.
        let (stderr_pipe_read, stderr_pipe_write) =
            pipe().map_err(|e| AishError::Pty(format!("failed to create stderr pipe: {e}")))?;

        // Set master fd to non-blocking.
        set_nonblocking(&master_fd)?;

        // Sync window size from real terminal.
        let stdin_fd = libc::STDIN_FILENO;
        let _ = sync_window_size(stdin_fd, master_fd.as_raw_fd());

        let slave_raw = slave_fd.as_raw_fd();
        let stderr_write_raw = stderr_pipe_write.as_raw_fd();

        let child_pid =
            match unsafe { fork() }.map_err(|e| AishError::Pty(format!("fork failed: {e}")))? {
                ForkResult::Parent { child } => {
                    drop(slave_fd);
                    drop(stderr_pipe_write);
                    child
                }
                ForkResult::Child => {
                    child_main(slave_raw, stderr_write_raw, command, env_vars);
                }
            };

        debug!(pid = %child_pid, "silent child process started");

        let master_raw = master_fd.into_raw_fd();
        let stderr_read_raw = stderr_pipe_read.into_raw_fd();

        let result = self.run_silent_io_loop(
            master_raw,
            stderr_read_raw,
            child_pid,
            cancel_token,
            session_uuid,
            base_dir,
            command,
        );

        // Close fds.
        let _ = unsafe { File::from_raw_fd(master_raw) };
        let _ = unsafe { File::from_raw_fd(stderr_read_raw) };

        result
    }

    /// Silent I/O loop: only capture output, no terminal display or stdin forwarding.
    #[allow(clippy::too_many_arguments)]
    fn run_silent_io_loop(
        &self,
        master_fd: RawFd,
        stderr_read: RawFd,
        child_pid: Pid,
        cancel_token: &CancelToken,
        session_uuid: &str,
        base_dir: &str,
        command: &str,
    ) -> aish_core::Result<CommandResult> {
        let mut offload =
            PtyOutputOffload::new(command, session_uuid, "", self.keep_bytes, base_dir);

        let mut stdout_buf: Vec<u8> = Vec::new();
        let mut stderr_buf: Vec<u8> = Vec::new();

        let mut child_exited = false;
        let mut exit_code: i32 = -1;

        let tmp_buf_size: usize = 8192;

        while !child_exited {
            if cancel_token.is_cancelled() {
                let _ = kill_pg(child_pid, Signal::SIGTERM);
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = kill_pg(child_pid, Signal::SIGKILL);
            }

            let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            unsafe {
                libc::FD_ZERO(&mut read_fds);
                libc::FD_SET(master_fd, &mut read_fds);
                libc::FD_SET(stderr_read, &mut read_fds);
            }

            let max_fd = master_fd.max(stderr_read) + 1;

            let mut timeout = libc::timeval {
                tv_sec: 0,
                tv_usec: 100_000, // 100ms
            };

            let select_result = unsafe {
                libc::select(
                    max_fd,
                    &mut read_fds,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut timeout,
                )
            };

            if select_result < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    child_exited = check_child_exit(child_pid, &mut exit_code);
                    continue;
                }
                return Err(AishError::Pty(format!("select error: errno {errno}")));
            }

            if select_result == 0 {
                if !child_exited {
                    child_exited = check_child_exit_nonblocking(child_pid, &mut exit_code);
                }
                continue;
            }

            // Read from PTY master (stdout) — capture only, no display.
            if unsafe { libc::FD_ISSET(master_fd, &read_fds) } {
                let mut tmp = vec![0u8; tmp_buf_size];
                match unsafe {
                    libc::read(master_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        stdout_buf.extend_from_slice(data);
                        offload.append_overflow(StreamName::Stdout, data);
                    }
                    0 => {
                        debug!("master fd closed");
                    }
                    _ => {}
                }
            }

            // Read from stderr pipe — capture only, no display.
            if unsafe { libc::FD_ISSET(stderr_read, &read_fds) } {
                let mut tmp = vec![0u8; tmp_buf_size];
                match unsafe {
                    libc::read(
                        stderr_read,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        stderr_buf.extend_from_slice(data);
                        offload.append_overflow(StreamName::Stderr, data);
                    }
                    _ => {}
                }
            }

            if !child_exited {
                child_exited = check_child_exit_nonblocking(child_pid, &mut exit_code);
            }
        }

        // Final wait to reap the child.
        let (_status, final_exit_code) = reap_child(child_pid, exit_code);
        exit_code = final_exit_code;

        let stdout_tail = tail_bytes(&stdout_buf, self.keep_bytes);
        let stderr_tail = tail_bytes(&stderr_buf, self.keep_bytes);

        let offload_result = offload.finalize(&stdout_tail, &stderr_tail, exit_code);

        let stdout_str = String::from_utf8_lossy(&stdout_tail).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr_tail).to_string();

        let command_status = if cancel_token.is_cancelled() {
            CommandStatus::Cancelled
        } else if exit_code == 0 {
            CommandStatus::Success
        } else {
            CommandStatus::Error
        };

        let offload_value =
            if offload_result.stdout.path.is_some() || offload_result.stderr.path.is_some() {
                Some(serde_json::to_value(&offload_result).unwrap_or(serde_json::Value::Null))
            } else {
                None
            };

        Ok(CommandResult {
            status: command_status,
            exit_code,
            stdout: stdout_str,
            stderr: stderr_str,
            offload: offload_value,
        })
    }

    /// Main I/O loop: relay bytes between stdin/stdout and the PTY master.
    #[allow(clippy::too_many_arguments)]
    fn run_io_loop(
        &self,
        master_fd: RawFd,
        stderr_read: RawFd,
        child_pid: Pid,
        cancel_token: &CancelToken,
        session_uuid: &str,
        base_dir: &str,
        command: &str,
    ) -> aish_core::Result<CommandResult> {
        let mut offload =
            PtyOutputOffload::new(command, session_uuid, "", self.keep_bytes, base_dir);

        let mut stdout_buf: Vec<u8> = Vec::new();
        let mut stderr_buf: Vec<u8> = Vec::new();
        let mut write_buf: Vec<u8> = Vec::new(); // back-pressure buffer for stdin -> master

        let mut child_exited = false;
        let mut exit_code: i32 = -1;

        let tmp_buf_size: usize = 8192;
        let stdin_fd = libc::STDIN_FILENO;

        while !child_exited {
            if cancel_token.is_cancelled() {
                // Send SIGTERM to the child process group.
                let _ = kill_pg(child_pid, Signal::SIGTERM);
                // Give it a moment, then SIGKILL.
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = kill_pg(child_pid, Signal::SIGKILL);
            }

            // Build fd_set using libc directly (nix 0.29 FdSet API changed).
            // SAFETY: fd_set is a C struct that is valid when zero-initialized.
            // Linux's FD_ZERO macro also sets all bytes to zero.
            let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            let mut write_fds: libc::fd_set = unsafe { std::mem::zeroed() };

            // SAFETY: FD_ZERO/FD_SET operate on properly zeroed fd_set structs.
            // All fds (stdin_fd, master_fd, stderr_read) are valid open file
            // descriptors checked above. stdin_fd is STDIN_FILENO, master_fd is
            // from openpty, and stderr_read is from pipe().
            unsafe {
                libc::FD_ZERO(&mut read_fds);
                libc::FD_ZERO(&mut write_fds);
                libc::FD_SET(stdin_fd, &mut read_fds);
                libc::FD_SET(master_fd, &mut read_fds);
                libc::FD_SET(stderr_read, &mut read_fds);
                if !write_buf.is_empty() {
                    libc::FD_SET(master_fd, &mut write_fds);
                }
            }

            let max_fd = master_fd.max(stderr_read).max(stdin_fd) + 1;

            // Select with a short timeout so we can check cancellation.
            let mut timeout = libc::timeval {
                tv_sec: 0,
                tv_usec: 100_000, // 100ms
            };

            // SAFETY: nfds is > 0 (at least STDIN_FILENO+1), read_fds and
            // write_fds are properly initialized via FD_ZERO then FD_SET, and
            // timeout is a valid pointer to a timeval on the stack.
            let select_result = unsafe {
                libc::select(
                    max_fd,
                    &mut read_fds,
                    &mut write_fds,
                    std::ptr::null_mut(),
                    &mut timeout,
                )
            };

            if select_result < 0 {
                // SAFETY: __errno_location returns a pointer to thread-local
                // errno; reading it is safe.
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    child_exited = check_child_exit(child_pid, &mut exit_code);
                    continue;
                }
                return Err(AishError::Pty(format!("select error: errno {errno}")));
            }

            if select_result == 0 {
                // Timeout - check if child has exited.
                if !child_exited {
                    child_exited = check_child_exit_nonblocking(child_pid, &mut exit_code);
                }
                continue;
            }

            // --- Read from stdin -> write to master ---
            // SAFETY: FD_ISSET reads from a valid fd_set with a valid fd.
            if unsafe { libc::FD_ISSET(stdin_fd, &read_fds) } {
                let mut tmp = vec![0u8; tmp_buf_size];
                // SAFETY: stdin_fd is a valid open fd, tmp points to a valid
                // writable buffer of tmp_buf_size bytes.
                match unsafe {
                    libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        // Check for Ctrl-C (0x03).
                        if data.contains(&0x03) {
                            let _ = kill_pg(child_pid, Signal::SIGINT);
                        }
                        write_buf.extend_from_slice(data);
                    }
                    0 => {
                        debug!("EOF on stdin");
                    }
                    _ => {}
                }
            }

            // --- Write pending data to master ---
            // SAFETY: FD_ISSET reads from a valid fd_set with a valid fd.
            if unsafe { libc::FD_ISSET(master_fd, &write_fds) } && !write_buf.is_empty() {
                // SAFETY: master_fd is a valid open PTY master fd, write_buf
                // points to valid readable data.
                match unsafe {
                    libc::write(
                        master_fd,
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

            // --- Read from master (combined stdout+stderr) ---
            // SAFETY: FD_ISSET reads from a valid fd_set with a valid fd.
            if unsafe { libc::FD_ISSET(master_fd, &read_fds) } {
                let mut tmp = vec![0u8; tmp_buf_size];
                // SAFETY: master_fd is a valid open PTY master fd, tmp points to
                // a valid writable buffer of tmp_buf_size bytes.
                match unsafe {
                    libc::read(master_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        // Write to real stdout.
                        // SAFETY: STDOUT_FILENO is a valid open fd, data points
                        // to valid readable bytes from the read above.
                        let _ = unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                data.as_ptr() as *const libc::c_void,
                                data.len(),
                            )
                        };
                        stdout_buf.extend_from_slice(data);
                        offload.append_overflow(StreamName::Stdout, data);
                    }
                    0 => {
                        debug!("master fd closed");
                    }
                    _ => {}
                }
            }

            // --- Read from stderr pipe ---
            // SAFETY: FD_ISSET reads from a valid fd_set with a valid fd.
            if unsafe { libc::FD_ISSET(stderr_read, &read_fds) } {
                let mut tmp = vec![0u8; tmp_buf_size];
                // SAFETY: stderr_read is a valid open pipe read fd, tmp points
                // to a valid writable buffer of tmp_buf_size bytes.
                match unsafe {
                    libc::read(
                        stderr_read,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        // SAFETY: STDOUT_FILENO is a valid open fd, data points
                        // to valid readable bytes from the stderr pipe read.
                        let _ = unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                data.as_ptr() as *const libc::c_void,
                                data.len(),
                            )
                        };
                        stderr_buf.extend_from_slice(data);
                        offload.append_overflow(StreamName::Stderr, data);
                    }
                    _ => {}
                }
            }

            // Check if child has exited.
            if !child_exited {
                child_exited = check_child_exit_nonblocking(child_pid, &mut exit_code);
            }
        }

        // Final wait to reap the child.
        let (_status, final_exit_code) = reap_child(child_pid, exit_code);
        exit_code = final_exit_code;

        // Trim buffers to keep_bytes tail.
        let stdout_tail = tail_bytes(&stdout_buf, self.keep_bytes);
        let stderr_tail = tail_bytes(&stderr_buf, self.keep_bytes);

        let offload_result = offload.finalize(&stdout_tail, &stderr_tail, exit_code);

        let stdout_str = String::from_utf8_lossy(&stdout_tail).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr_tail).to_string();

        let command_status = if cancel_token.is_cancelled() {
            CommandStatus::Cancelled
        } else if exit_code == 0 {
            CommandStatus::Success
        } else {
            CommandStatus::Error
        };

        // Build the offload value for CommandResult.
        let offload_value =
            if offload_result.stdout.path.is_some() || offload_result.stderr.path.is_some() {
                Some(serde_json::to_value(&offload_result).unwrap_or(serde_json::Value::Null))
            } else {
                None
            };

        Ok(CommandResult {
            status: command_status,
            exit_code,
            stdout: stdout_str,
            stderr: stderr_str,
            offload: offload_value,
        })
    }
}

// ---------------------------------------------------------------------------
// Child process
// ---------------------------------------------------------------------------

/// Code that runs in the child process after fork.
fn child_main(
    slave_fd: RawFd,
    stderr_write_fd: RawFd,
    command: &str,
    env_vars: HashMap<String, String>,
) -> ! {
    // Create a new session.
    // SAFETY: setsid() creates a new session and process group. This is safe to
    // call in the child after fork; it always succeeds unless the child is
    // already a process group leader (which it isn't immediately after fork).
    unsafe {
        libc::setsid();
    }

    // Set the slave as the controlling terminal.
    // SAFETY: slave_fd is a valid open PTY slave fd from openpty. TIOCSCTTY is
    // a safe ioctl that only sets the controlling terminal; the arg 0 means
    // "steal" is not requested.
    unsafe {
        libc::ioctl(slave_fd, libc::TIOCSCTTY, 0);
    }

    // Redirect stdin/stdout to the slave PTY.
    let _ = dup2(slave_fd, libc::STDIN_FILENO);
    let _ = dup2(slave_fd, libc::STDOUT_FILENO);
    // Redirect stderr to our pipe instead of the PTY.
    let _ = dup2(stderr_write_fd, libc::STDERR_FILENO);

    // Close fds we don't need.
    let _ = close(slave_fd);
    let _ = close(stderr_write_fd);

    // Set environment variables.
    for (k, v) in &env_vars {
        std::env::set_var(k, v);
    }

    // Build argv for: /bin/bash -c "<command>"
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let c_shell = std::ffi::CString::new(shell)
        .unwrap_or_else(|_| std::ffi::CString::new("/bin/bash").unwrap());
    let c_arg1 = std::ffi::CString::new("-c").unwrap();
    let c_cmd =
        std::ffi::CString::new(command).unwrap_or_else(|_| std::ffi::CString::new("true").unwrap());

    let args = vec![c_shell.clone(), c_arg1, c_cmd];

    let _ = execvp(&c_shell, &args);

    // If execvp returns, it failed.
    let msg = b"aish-pty: execvp failed\n";
    // SAFETY: STDERR_FILENO is a valid open fd. msg is a static byte slice so
    // its pointer is valid for the write. _exit(127) terminates the child
    // process immediately without running destructors, which is correct in a
    // post-fork child (to avoid flushing parent's buffers).
    unsafe {
        libc::write(
            libc::STDERR_FILENO,
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
        );
        libc::_exit(127);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn set_nonblocking(fd: &OwnedFd) -> aish_core::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = fcntl(raw, FcntlArg::F_GETFL)
        .map_err(|e| AishError::Pty(format!("fcntl F_GETFL failed: {e}")))?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(raw, FcntlArg::F_SETFL(flags))
        .map_err(|e| AishError::Pty(format!("fcntl F_SETFL O_NONBLOCK failed: {e}")))?;
    Ok(())
}

/// Try to sync the terminal window size from `src_fd` to `dst_fd`.
fn sync_window_size(src_fd: RawFd, dst_fd: RawFd) -> nix::Result<()> {
    // SAFETY: winsize is a plain C struct with no padding invariants; it is
    // valid when zero-initialized.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: src_fd is a valid open terminal fd (STDIN_FILENO). TIOCGWINSZ is
    // a safe ioctl that only reads window size into the provided struct.
    let rc = unsafe { libc::ioctl(src_fd, libc::TIOCGWINSZ, &mut ws) };
    if rc >= 0 {
        // SAFETY: dst_fd is a valid PTY master fd. TIOCSWINSZ is a safe ioctl
        // that only sets window size from the provided struct.
        unsafe {
            libc::ioctl(dst_fd, libc::TIOCSWINSZ, &ws);
        }
    }
    Ok(())
}

/// Check if a child has exited (non-blocking wait with WNOHANG).
fn check_child_exit_nonblocking(pid: Pid, exit_code: &mut i32) -> bool {
    match waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(_, code)) => {
            *exit_code = code;
            true
        }
        Ok(WaitStatus::Signaled(_, sig, _)) => {
            *exit_code = 128 + sig as i32;
            true
        }
        Ok(_) => false,
        Err(_) => false,
    }
}

/// Variant for when we know SIGCHLD fired.
fn check_child_exit(pid: Pid, exit_code: &mut i32) -> bool {
    check_child_exit_nonblocking(pid, exit_code)
}

/// Reap the child process, ensuring we get a final exit code.
fn reap_child(pid: Pid, fallback_code: i32) -> (CommandStatus, i32) {
    match waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(_, code)) => (CommandStatus::Success, code),
        Ok(WaitStatus::Signaled(_, sig, _)) => (CommandStatus::Error, 128 + sig as i32),
        Ok(_) => {
            // Still running?  Do a blocking wait.
            match waitpid(pid, None) {
                Ok(WaitStatus::Exited(_, code)) => (CommandStatus::Success, code),
                Ok(WaitStatus::Signaled(_, sig, _)) => (CommandStatus::Error, 128 + sig as i32),
                _ => (CommandStatus::Error, fallback_code),
            }
        }
        Err(_) => (CommandStatus::Error, fallback_code),
    }
}

/// Return the last `max_len` bytes of `buf`.
fn tail_bytes(buf: &[u8], max_len: usize) -> Vec<u8> {
    if buf.len() <= max_len {
        buf.to_vec()
    } else {
        buf[buf.len() - max_len..].to_vec()
    }
}
