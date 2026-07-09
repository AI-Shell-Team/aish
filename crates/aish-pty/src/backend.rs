//! PtyBackend trait: abstraction over direct PTY ownership vs daemon-attached mode.
//!
//! This allows `send_command_interactive` and other PTY consumers to work
//! uniformly whether they're talking to a local bash subprocess (OwnedBackend)
//! or a remote daemon-held session (AttachedBackend).

use std::os::fd::RawFd;

use crate::control::BackendControlEvent;
use aish_core::{AishError, Result};

/// Events produced by draining a PtyBackend.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Raw PTY output bytes (stdout/stderr from bash).
    Output(Vec<u8>),
    /// Decoded control event from bash (PromptReady, ShellExiting, etc.).
    Control(BackendControlEvent),
}

/// Abstraction over PTY I/O for both direct ownership and daemon-attached modes.
///
/// Implementations:
/// - [`OwnedBackend`](crate::backend::OwnedBackend) — wraps a `PersistentPty` directly
/// - [`AttachedBackend`](crate::backend::AttachedBackend) — communicates with a PTY daemon via Unix socket
pub trait PtyBackend {
    /// Write user input to the PTY (keystrokes forwarded to bash).
    fn write_input(&mut self, data: &[u8]) -> Result<()>;

    /// FDs to watch in select() for readable output data.
    /// Owned mode: [master_fd, control_fd]
    /// Attached mode: [socket_fd]
    fn readable_fds(&self) -> Vec<RawFd>;

    /// Read available data and return events (output + control, in arrival order).
    /// Called after select() reports data available on one of `readable_fds()`.
    fn drain_events(&mut self) -> Result<Vec<PtyEvent>>;

    /// Resize the PTY window.
    fn resize(&mut self, rows: u16, cols: u16) -> Result<()>;

    /// Whether the bash process is still running.
    fn is_running(&self) -> bool;

    /// Current window size (rows, cols).
    fn window_size(&self) -> (u16, u16);
}

// ---------------------------------------------------------------------------
// OwnedBackend: wraps PersistentPty for direct PTY access
// ---------------------------------------------------------------------------

use crate::control::decode_control_chunk;
use crate::persistent::PersistentPty;

/// Direct PTY ownership backend — wraps an existing PersistentPty.
pub struct OwnedBackend {
    pty: PersistentPty,
    control_buffer: String,
}

impl OwnedBackend {
    pub fn new(pty: PersistentPty) -> Self {
        Self {
            pty,
            control_buffer: String::new(),
        }
    }

    /// Consume and return the inner PersistentPty.
    pub fn into_pty(self) -> PersistentPty {
        self.pty
    }

    /// Borrow the inner PersistentPty.
    pub fn pty(&self) -> &PersistentPty {
        &self.pty
    }

    /// Mutably borrow the inner PersistentPty.
    pub fn pty_mut(&mut self) -> &mut PersistentPty {
        &mut self.pty
    }
}

impl PtyBackend for OwnedBackend {
    fn write_input(&mut self, data: &[u8]) -> Result<()> {
        self.pty.write_master_pub(data)
    }

    fn readable_fds(&self) -> Vec<RawFd> {
        vec![self.pty.master_fd(), self.pty.control_fd()]
    }

    fn drain_events(&mut self) -> Result<Vec<PtyEvent>> {
        let mut events = Vec::new();
        let master_fd = self.pty.master_fd();
        let control_fd = self.pty.control_fd();

        // Read PTY output (non-blocking)
        let mut buf = [0u8; 8192];
        loop {
            match nix::unistd::read(master_fd, &mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    events.push(PtyEvent::Output(buf[..n].to_vec()));
                }
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(e) => {
                    if e == nix::errno::Errno::EIO {
                        break;
                    }
                    return Err(AishError::Pty(format!("read master_fd: {e}")));
                }
            }
        }

        // Read control events (non-blocking)
        let mut ctrl_buf = [0u8; 4096];
        loop {
            match nix::unistd::read(control_fd, &mut ctrl_buf) {
                Ok(0) => break,
                Ok(n) => {
                    let control_events =
                        decode_control_chunk(&mut self.control_buffer, &ctrl_buf[..n]);
                    for evt in control_events {
                        events.push(PtyEvent::Control(evt));
                    }
                }
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(e) => {
                    return Err(AishError::Pty(format!("read control_fd: {e}")));
                }
            }
        }

        Ok(events)
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.pty.resize(rows, cols);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.pty.is_running()
    }

    fn window_size(&self) -> (u16, u16) {
        (self.pty.rows(), self.pty.cols())
    }
}

// ---------------------------------------------------------------------------
// AttachedBackend: client-side backend that talks to a PTY daemon
// ---------------------------------------------------------------------------

use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::daemon_protocol::{
    encode_frame, AttachRequest, Frame, FrameReader, MAX_FRAME_PAYLOAD, PROTOCOL_VERSION,
    TYPE_ATTACH_ACK, TYPE_CONTROL_EVENT, TYPE_DAEMON_ERROR, TYPE_EXIT_NOTICE, TYPE_PTY_OUTPUT,
    TYPE_SCROLLBACK, TYPE_SCROLLBACK_END, TYPE_SESSION_INFO,
};

/// Default deadline for the whole attach handshake (attach + scrollback replay).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Client-side backend: connects to a PTY daemon via Unix socket.
///
/// Wire-protocol overview (see [`crate::daemon_protocol`] for the frame format):
/// 1. Client sends an `Attach` frame with the desired window size.
/// 2. Daemon replies with `AttachAck`, replays scrollback (`Scrollback` frames,
///    raw bytes), and finishes with `ScrollbackEnd` carrying session metadata
///    (`child_pid`, `running`, ...).
/// 3. Live mode: daemon streams `PtyOutput` / `ControlEvent` frames; client
///    sends `Input` / `Resize` / `Detach` / `KillSession` frames.
///
/// Events collected during the handshake (scrollback replay + any early control
/// events) are buffered internally and yielded by the first
/// [`PtyBackend::drain_events`] call.  Callers should therefore invoke
/// `drain_events()` once immediately after [`AttachedBackend::attach`] to render
/// the scrollback before entering their select/poll loop.
pub struct AttachedBackend {
    stream: UnixStream,
    reader: FrameReader,
    rows: u16,
    cols: u16,
    running: bool,
    session_id: String,
    child_pid: u32,
    /// Events buffered during the attach handshake (scrollback + early control).
    /// Drained first by [`PtyBackend::drain_events`].
    pending_events: Vec<PtyEvent>,
}

impl AttachedBackend {
    /// Connect to a PTY daemon at `socket_path` and perform the attach handshake.
    ///
    /// The socket is switched to non-blocking mode after the handshake completes
    /// so that [`PtyBackend::drain_events`] can be driven from a select loop.
    pub fn attach(socket_path: &Path, session_id: &str, rows: u16, cols: u16) -> Result<Self> {
        // 1. Connect. UnixStream::connect blocks until established (fine for handshake).
        // `mut` because Read/Write on UnixStream require &mut self (unlike TcpStream,
        // UnixStream has no impl for &UnixStream).
        let mut stream = UnixStream::connect(socket_path)
            .map_err(|e| AishError::Pty(format!("connect to daemon socket failed: {e}")))?;
        stream.set_nonblocking(true)?;

        // 2. Send the Attach frame.
        let req = AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.to_string(),
            rows,
            cols,
            term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string()),
        };
        let attach_bytes = encode_frame(&Frame::attach(&req));
        write_all_nonblocking(&mut stream, &attach_bytes, Duration::from_secs(5))?;

        // 3. Read frames until ScrollbackEnd (poll-style, matching the daemon's handshake).
        let mut reader = FrameReader::new();
        let mut pending_events: Vec<PtyEvent> = Vec::new();
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

        let mut read_buf = [0u8; 16384];
        loop {
            if Instant::now() > deadline {
                return Err(AishError::Timeout);
            }
            match stream.read(&mut read_buf) {
                Ok(0) => {
                    return Err(AishError::Pty(
                        "daemon closed connection during handshake".into(),
                    ));
                }
                Ok(n) => reader.push(&read_buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // No data yet; back off briefly and retry until deadline.
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }

            let frames = reader.drain_frames()?;
            for frame in frames {
                match frame.frame_type {
                    TYPE_ATTACH_ACK => match frame.as_attach_ack() {
                        Ok(ack) => tracing::debug!(
                            ack.scrollback_size,
                            session_id = %ack.session_id,
                            "attach acknowledged, scrollback replay starting"
                        ),
                        Err(e) => tracing::warn!(error = %e, "malformed AttachAck frame"),
                    },
                    TYPE_SCROLLBACK | TYPE_PTY_OUTPUT => {
                        // Scrollback is historical output. PtyOutput during the
                        // handshake (rare race) is treated identically.
                        if !frame.payload.is_empty() {
                            pending_events.push(PtyEvent::Output(frame.payload));
                        }
                    }
                    TYPE_CONTROL_EVENT => match frame.as_control_event() {
                        Ok(evt) => pending_events.push(PtyEvent::Control(evt)),
                        Err(e) => tracing::warn!(error = %e, "malformed control event frame"),
                    },
                    TYPE_DAEMON_ERROR => {
                        let msg = match frame.as_daemon_error() {
                            Ok(err) => format!("daemon error [{}]: {}", err.code, err.message),
                            Err(_) => "daemon error (malformed payload)".to_string(),
                        };
                        return Err(AishError::Pty(msg));
                    }
                    TYPE_SCROLLBACK_END => {
                        let end = frame.as_scrollback_end().map_err(|e| {
                            AishError::Pty(format!("invalid ScrollbackEnd frame: {e}"))
                        })?;
                        let child_pid = end.child_pid;
                        let running = end.running;
                        tracing::info!(
                            child_pid,
                            running,
                            pending_events = pending_events.len(),
                            "attach handshake complete"
                        );
                        return Ok(Self {
                            stream,
                            reader,
                            rows,
                            cols,
                            running,
                            session_id: session_id.to_string(),
                            child_pid,
                            pending_events,
                        });
                    }
                    TYPE_SESSION_INFO | TYPE_EXIT_NOTICE => {
                        // Should not happen during the handshake; safe to ignore.
                        tracing::debug!(
                            frame_type = frame.frame_type,
                            "unexpected frame during attach handshake"
                        );
                    }
                    _ => {
                        tracing::debug!(
                            frame_type = frame.frame_type,
                            "unknown frame type during attach handshake"
                        );
                    }
                }
            }
        }
    }

    /// Gracefully detach from the daemon.
    ///
    /// Sends a `Detach` frame and shuts the socket down.  The daemon keeps the
    /// session alive for future re-attach.
    pub fn detach(&mut self) -> Result<()> {
        let encoded = encode_frame(&Frame::detach());
        if let Err(e) = write_all_nonblocking(&mut self.stream, &encoded, Duration::from_secs(2)) {
            tracing::warn!(error = %e, "failed to send Detach frame");
        }
        let _ = self.stream.shutdown(Shutdown::Both);
        Ok(())
    }

    /// Ask the daemon to kill the bash session and shut down.
    pub fn kill_session(&mut self) -> Result<()> {
        let encoded = encode_frame(&Frame::kill_session());
        write_all_nonblocking(&mut self.stream, &encoded, Duration::from_secs(2))
    }

    /// Session id this backend is attached to.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Bash child PID, as reported by the daemon at attach time.
    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }
}

impl PtyBackend for AttachedBackend {
    fn write_input(&mut self, data: &[u8]) -> Result<()> {
        // Defensive chunking: keystrokes are tiny, but large pastes could
        // exceed MAX_FRAME_PAYLOAD.
        for chunk in data.chunks(MAX_FRAME_PAYLOAD) {
            let frame = Frame::input(chunk);
            let encoded = encode_frame(&frame);
            write_all_nonblocking(&mut self.stream, &encoded, Duration::from_secs(5))?;
        }
        Ok(())
    }

    fn readable_fds(&self) -> Vec<RawFd> {
        vec![self.stream.as_raw_fd()]
    }

    fn drain_events(&mut self) -> Result<Vec<PtyEvent>> {
        // First, flush events buffered during the attach handshake (scrollback
        // replay + early control events). Taken once.
        let mut events = if self.pending_events.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut self.pending_events)
        };

        // Drain all bytes currently available on the non-blocking socket.
        let mut read_buf = [0u8; 16384];
        loop {
            match self.stream.read(&mut read_buf) {
                Ok(0) => {
                    // EOF: daemon closed the connection.
                    self.running = false;
                    break;
                }
                Ok(n) => self.reader.push(&read_buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }

        // Decode every complete frame now accumulated in the reader.
        let frames = self.reader.drain_frames()?;
        for frame in frames {
            match frame.frame_type {
                TYPE_PTY_OUTPUT | TYPE_SCROLLBACK => {
                    if !frame.payload.is_empty() {
                        events.push(PtyEvent::Output(frame.payload));
                    }
                }
                TYPE_CONTROL_EVENT => match frame.as_control_event() {
                    Ok(evt) => events.push(PtyEvent::Control(evt)),
                    Err(e) => tracing::warn!(error = %e, "malformed control event frame"),
                },
                TYPE_EXIT_NOTICE => {
                    match frame.as_exit_notice() {
                        Ok(notice) => tracing::info!(
                            exit_code = notice.exit_code,
                            reason = %notice.reason,
                            "daemon reports bash exited"
                        ),
                        Err(e) => tracing::warn!(error = %e, "malformed ExitNotice frame"),
                    }
                    self.running = false;
                }
                TYPE_SESSION_INFO => match frame.as_session_info() {
                    Ok(info) => self.running = info.running,
                    Err(e) => tracing::warn!(error = %e, "malformed SessionInfo frame"),
                },
                TYPE_DAEMON_ERROR => match frame.as_daemon_error() {
                    Ok(err) => tracing::warn!(
                        code = %err.code,
                        message = %err.message,
                        "daemon error frame"
                    ),
                    Err(e) => tracing::warn!(error = %e, "malformed DaemonError frame"),
                },
                TYPE_ATTACH_ACK | TYPE_SCROLLBACK_END => {
                    // Handshake frames must not arrive in live mode.
                    tracing::debug!(
                        frame_type = frame.frame_type,
                        "handshake frame received in live mode, ignoring"
                    );
                }
                _ => {
                    tracing::debug!(
                        frame_type = frame.frame_type,
                        "unexpected frame type in live mode"
                    );
                }
            }
        }

        Ok(events)
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let encoded = encode_frame(&Frame::resize(rows, cols));
        write_all_nonblocking(&mut self.stream, &encoded, Duration::from_secs(2))?;
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running
    }

    fn window_size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }
}

/// Write an encoded frame to a non-blocking [`UnixStream`], retrying on EAGAIN
/// until all bytes are written or `timeout` elapses.
///
/// User input and control frames are small, so under normal conditions this
/// returns immediately; the loop only matters when the kernel send buffer is
/// momentarily full.
fn write_all_nonblocking(stream: &mut UnixStream, data: &[u8], timeout: Duration) -> Result<()> {
    let mut written = 0;
    let deadline = Instant::now() + timeout;
    while written < data.len() {
        match stream.write(&data[written..]) {
            Ok(0) => {
                return Err(AishError::Pty(
                    "socket write returned 0 (connection closed)".into(),
                ));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    return Err(AishError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
