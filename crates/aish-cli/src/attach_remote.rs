//! `aish remote attach`: attach the local terminal to a remote PTY session
//! by tunneling the daemon frame protocol over ssh stdio (#586).
//!
//! The remote side runs `aish --remote-attach <session-id>`, which connects
//! to the session's Unix socket and pipes frames on stdin/stdout. Locally we
//! run a select loop treating the ssh child's stdout as the frame channel.

use std::io::Write;
use std::os::fd::AsRawFd;

use crate::remote_cmd::SshChannel;

/// Attach to a remote session; returns an error message on failure.
pub fn run(host: &str, session_id: &str) -> Result<(), String> {
    let bin = crate::remote_cmd::probe_remote_bin(host)?;
    let args = vec![bin, "--remote-attach".to_string(), session_id.to_string()];
    let mut ch = SshChannel::spawn(host, &args)?;
    let (reader, mut stdin) = ch.split()?;
    // Operator identity: when the target enforces trust, the first stdin
    // bytes are the signed challenge line (terminated by '\n'), consumed by
    // the agent's auth gate before any frame reaches the relay. Harmless in
    // open mode — the line is simply ignored there. A line is sent even when
    // identity creation fails (blank line): the gate then rejects with
    // "malformed identity request" instead of hanging on "no identity
    // presented", keeping the failure message actionable.
    use std::io::Write;
    match crate::remote_identity::identity_request_json("session.attach") {
        Ok(identity) => {
            let _ = writeln!(stdin, "{identity}");
        }
        Err(e) => {
            let _ = writeln!(stdin);
            eprintln!("[identity] {e}");
        }
    }
    let _ = stdin.flush();

    // Switch local terminal to raw mode for passthrough.
    let stdin_fd = std::io::stdin().as_raw_fd();
    // SAFETY: [Category 4 — Uninitialized memory] `termios` is a POD C struct;
    // `zeroed()` produces a valid instance filled by tcgetattr below.
    let mut orig_termios: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: [Category 8 — FFI] tcgetattr/tcsetattr operate on stdin (valid fd).
    if unsafe { libc::tcgetattr(stdin_fd, &mut orig_termios) } != 0 {
        return Err("remote attach needs an interactive terminal for raw keyboard input, but stdin is not a TTY.\nTip: run this command in a real terminal; use 'aish remote session list --host ...' to inspect sessions without attaching".to_string());
    }
    let mut raw = orig_termios;
    // SAFETY: [Category 8 — FFI] cfmakeraw mutates the in-memory struct.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_iflag &= !libc::IXON;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: [Category 8 — FFI] applies raw mode to stdin.
    if unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &raw) } != 0 {
        return Err("failed to set raw mode".to_string());
    }

    let result = remote_attach_loop(reader, stdin, session_id);

    // Restore terminal regardless of outcome.
    // SAFETY: [Category 8 — FFI] restores saved termios on stdin.
    unsafe {
        libc::tcsetattr(stdin_fd, libc::TCSANOW, &orig_termios);
    }
    println!();
    result
}

/// Core select loop: local stdin → INPUT frames → ssh stdin; ssh stdout →
/// decoded frames → PTY bytes to local stdout. Ctrl+Q detaches.
fn remote_attach_loop(
    mut reader: std::io::BufReader<std::process::ChildStdout>,
    mut stdin: std::process::ChildStdin,
    session_id: &str,
) -> Result<(), String> {
    use std::io::Read;

    let stdin_fd = std::io::stdin().as_raw_fd();
    let stdout_fd = std::io::stdout().as_raw_fd();

    // Send attach request with the local window size.
    let (rows, cols) = crate::get_terminal_size();
    let req = aish_pty::AttachRequest {
        protocol_version: aish_pty::PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        rows,
        cols,
        term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string()),
    };
    let attach_frame = aish_pty::daemon_protocol::encode_frame(&aish_pty::Frame::attach(&req));
    stdin
        .write_all(&attach_frame)
        .map_err(|e| format!("cannot send attach frame: {e}"))?;
    stdin.flush().ok();

    let mut input_buf = [0u8; 4096];
    let mut net_buf: Vec<u8> = Vec::with_capacity(16384);
    let mut chunk = [0u8; 16384];
    let mut detached = false;
    let sock_fd = reader.get_ref().as_raw_fd();

    loop {
        let mut fds = unsafe { std::mem::zeroed::<libc::fd_set>() };
        // SAFETY: [Category 8 — FFI] FD_ZERO/FD_SET on a stack fd_set with
        // small fds (stdin, pipe).
        unsafe {
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(stdin_fd, &mut fds);
            libc::FD_SET(sock_fd, &mut fds);
        }
        let ret = unsafe {
            libc::select(
                stdin_fd.max(sock_fd) + 1,
                &mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(), // block; ssh death unblocks via EOF
            )
        };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("select failed: {err}"));
        }

        // Local keystrokes → INPUT frames.
        // SAFETY: [Category 8 — FFI] FD_ISSET on initialized fd_set.
        if unsafe { libc::FD_ISSET(stdin_fd, &fds) } {
            let n = unsafe { libc::read(stdin_fd, input_buf.as_mut_ptr().cast(), input_buf.len()) };
            if n > 0 {
                let bytes = &input_buf[..n as usize];
                // Ctrl+Q (0x11) detaches cleanly.
                if bytes.contains(&0x11) {
                    detached = true;
                    break;
                }
                for chunk in bytes.chunks(aish_pty::MAX_FRAME_PAYLOAD) {
                    let frame = aish_pty::Frame::input(chunk);
                    let encoded = aish_pty::daemon_protocol::encode_frame(&frame);
                    stdin
                        .write_all(&encoded)
                        .map_err(|_| "remote connection closed".to_string())?;
                }
                stdin.flush().ok();
            } else if n == 0 {
                break; // local stdin closed
            }
        }

        // ssh stdout → frames → PTY output.
        // SAFETY: [Category 8 — FFI] FD_ISSET on initialized fd_set.
        if unsafe { libc::FD_ISSET(sock_fd, &fds) } {
            let n = reader.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                // ssh channel closed: remote daemon exited or network died.
                break;
            }
            net_buf.extend_from_slice(&chunk[..n]);
            while let Some((frame, consumed)) =
                aish_pty::try_decode_frame(&net_buf).map_err(|e| e.to_string())?
            {
                net_buf.drain(..consumed);
                match frame.frame_type {
                    aish_pty::daemon_protocol::TYPE_PTY_OUTPUT
                    | aish_pty::daemon_protocol::TYPE_SCROLLBACK
                        if !frame.payload.is_empty() =>
                    {
                        // SAFETY: [Category 8 — FFI] write() to stdout (valid fd).
                        unsafe {
                            libc::write(
                                stdout_fd,
                                frame.payload.as_ptr() as *const _,
                                frame.payload.len(),
                            );
                        }
                    }
                    aish_pty::daemon_protocol::TYPE_EXIT_NOTICE => {
                        // Inner shell exited; finish after flushing output.
                        break;
                    }
                    aish_pty::daemon_protocol::TYPE_ROLE_NOTIFY => {
                        if let Ok(notify) = frame.as_role_notify() {
                            if notify.role == aish_pty::daemon_protocol::ClientRole::Observer {
                                // Raw mode: write the hint straight to stdout.
                                let msg = format!(
                                    "\r\n[aish] observer mode: input is read-only ({}). \
Use Ctrl+Q to detach, or claim input from the controller.\r\n",
                                    notify.message
                                );
                                // SAFETY: [Category 8 — FFI] write() to stdout (valid fd).
                                unsafe {
                                    libc::write(stdout_fd, msg.as_ptr() as *const _, msg.len());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Agent auth rejection: when the gate denies, it prints
    // {"auth_failed":true,...} on stdout and exits. That line is not a valid
    // frame, so it stays in net_buf until EOF. Surface it as an error
    // instead of returning Ok(()) with no explanation.
    let leftover: String = String::from_utf8_lossy(&net_buf).trim().to_string();
    if leftover.starts_with("{\"auth_failed\"") {
        return Err(format!("remote attach rejected: {leftover}"));
    }

    if detached {
        // Best-effort detach frame so the remote daemon keeps the session.
        let enc = aish_pty::daemon_protocol::encode_frame(&aish_pty::Frame::detach());
        stdin.write_all(&enc).ok();
        stdin.flush().ok();
        println!("[aish] detached from remote session {session_id}");
    }
    Ok(())
}
