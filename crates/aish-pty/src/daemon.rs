//! PTY daemon: long-lived process that holds a bash PTY session.
//!
//! The daemon survives client (aish) disconnects, keeping the bash process
//! and any running commands alive. When a client reconnects (attaches), the
//! daemon replays buffered scrollback output and resumes live streaming.
//!
//! Architecture:
//! - Single-threaded select() event loop
//! - Watches: PTY master_fd, control_fd, optional client socket, SIGCHLD self-pipe
//! - Non-blocking client socket I/O with FrameReader/FrameWriter
//! - Scrollback ring buffer for detach→reattach replay

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aish_core::{AishError, Result};
use libc::{self, c_int};
use nix::unistd::Pid;

use crate::daemon_protocol::{
    AttachAck, AttachRequest, Frame, FrameReader, ScrollbackEnd, PROTOCOL_VERSION, TYPE_ATTACH,
    TYPE_DETACH, TYPE_INPUT, TYPE_KILL_SESSION, TYPE_RESIZE, TYPE_SCROLLBACK,
};
use crate::scrollback::ScrollbackBuffer;

// Frame type constants re-exported for convenience
use crate::daemon_protocol as proto;

// ---------------------------------------------------------------------------
// Signal handling (SIGCHLD via self-pipe)
// ---------------------------------------------------------------------------

/// Self-pipe for receiving SIGCHLD in the select loop.
struct SigchldPipe {
    read_fd: OwnedFd,
    _write_fd: OwnedFd,
}

// SAFETY: [Category 9 — incorrect Send/Sync + Category 8 — FFI]
// `SIGCHLD_WRITE_FD` is a process-global fd written from an async-signal
// handler. Access is safe because: (1) only one signal handler runs at a time
// per thread (POSIX guarantee), (2) the value is set once during install()
// before the handler is registered and only reset to -1 in Drop after the
// handler is unregistered, (3) `libc::write` is async-signal-safe per
// POSIX.1-2017 §2.4.1.
static mut SIGCHLD_WRITE_FD: c_int = -1;

extern "C" fn sigchld_handler(_sig: c_int) {
    // SAFETY: [Category 8 — FFI] [Category 9 — Send/Sync]
    // `write()` is async-signal-safe (POSIX). The guard `>= 0` check and the
    // single-byte write are non-branching and cannot allocate, satisfying the
    // async-signal-safety constraints. See `SIGCHLD_WRITE_FD` comment above.
    unsafe {
        if SIGCHLD_WRITE_FD >= 0 {
            let buf = [1u8];
            libc::write(SIGCHLD_WRITE_FD, buf.as_ptr() as *const _, 1);
        }
    }
}

impl SigchldPipe {
    fn install() -> Result<Self> {
        let mut fds = [0i32; 2];
        // SAFETY: [Category 8 — FFI] `pipe()` writes two c_int into the
        // 8-byte array `fds`. The pointer is valid and properly aligned.
        let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if ret != 0 {
            return Err(AishError::Pty(format!(
                "pipe() for SIGCHLD failed: {}",
                io::Error::last_os_error()
            )));
        }

        // Set both ends non-blocking
        for fd in &fds {
            set_fd_nonblocking(*fd)?;
        }
        // Set close-on-exec
        for fd in &fds {
            set_fd_cloexec(*fd)?;
        }

        // SAFETY: [Category 13 — unsafe contract] `from_raw_fd` takes
        // ownership of an fd that was just created by `pipe()` and not yet
        // owned by any `OwnedFd`. The fd is valid (pipe succeeded). After
        // this, `fds[0]`/`fds[1]` must not be used directly — the `OwnedFd`
        // drops will close them.
        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        // SAFETY: [Category 9 — Send/Sync] Store the write fd number for the
        // signal handler. This runs before `sigaction` registers the handler,
        // so no concurrent signal can race. The `OwnedFd` `write_fd` is kept
        // alive in the struct to prevent the fd from being reused.
        unsafe {
            SIGCHLD_WRITE_FD = fds[1];
        }

        // Install signal handler
        // SAFETY: [Category 4 — Uninitialized memory] `sigaction` is a POD
        // C struct; `zeroed()` produces a valid zero-initialised instance
        // whose fields we overwrite immediately below.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = sigchld_handler as *const () as usize;
        sa.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
        // SAFETY: [Category 8 — FFI] `sigaction` with SIGCHLD installs our
        // handler. `&sa` is a valid pointer to the initialised struct.
        let ret = unsafe { libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut()) };
        if ret != 0 {
            // SAFETY: [Category 9 — Send/Sync] Disarm the global before
            // returning; the handler is not yet installed (sigaction failed),
            // so no race is possible.
            unsafe {
                SIGCHLD_WRITE_FD = -1;
            }
            return Err(AishError::Pty(format!(
                "sigaction(SIGCHLD) failed: {}",
                io::Error::last_os_error()
            )));
        }

        Ok(Self {
            read_fd,
            _write_fd: write_fd,
        })
    }

    fn read_fd(&self) -> RawFd {
        self.read_fd.as_raw_fd()
    }

    /// Drain the self-pipe (acknowledge SIGCHLD).
    fn drain(&self) {
        let mut buf = [0u8; 64];
        let _ = nix::unistd::read(self.read_fd.as_raw_fd(), &mut buf);
    }

    /// Reap all zombie children (non-blocking).
    fn reap_children(&self) -> bool {
        let mut any_reaped = false;
        loop {
            match nix::sys::wait::waitpid(
                Pid::from_raw(-1),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ) {
                Ok(nix::sys::wait::WaitStatus::Exited(pid, _code)) => {
                    tracing::debug!("reaped child pid={pid}");
                    any_reaped = true;
                }
                Ok(nix::sys::wait::WaitStatus::Signaled(pid, _sig, _)) => {
                    tracing::debug!("reaped signaled child pid={pid}");
                    any_reaped = true;
                }
                Ok(_) => break,
                Err(nix::errno::Errno::ECHILD) => break,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    tracing::warn!("waitpid error: {e}");
                    break;
                }
            }
        }
        any_reaped
    }
}

impl Drop for SigchldPipe {
    fn drop(&mut self) {
        // SAFETY: [Category 9 — Send/Sync] Disarm the global so the signal
        // handler becomes a no-op. Drop runs after the select loop exits, so
        // no concurrent handler invocation reads the fd while it is being
        // closed by the `OwnedFd` drop that follows.
        unsafe {
            SIGCHLD_WRITE_FD = -1;
        }
        // Restore default SIGCHLD
        // SAFETY: [Category 4 — Uninitialized memory] `sigaction` is a POD
        // C struct; zero-initialise then set SIG_DFL before the call.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = libc::SIG_DFL;
        // SAFETY: [Category 8 — FFI] `sigaction` restores the default
        // SIGCHLD disposition. `&sa` points to the initialised struct.
        unsafe {
            libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// FD utility functions
// ---------------------------------------------------------------------------

fn set_fd_nonblocking(fd: RawFd) -> Result<()> {
    // SAFETY: [Category 8 — FFI] `fcntl(F_GETFL)` reads the fd's status flags.
    // `fd` is a valid open fd passed by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(AishError::Pty(format!(
            "fcntl F_GETFL failed: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: [Category 8 — FFI] `fcntl(F_SETFL, …)` sets the fd's status
    // flags. `fd` is valid and `flags | O_NONBLOCK` is a well-formed bitmask.
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(AishError::Pty(format!(
            "fcntl F_SETFL failed: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn set_fd_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: [Category 8 — FFI] `fcntl(F_GETFD)` reads the fd's descriptor
    // flags. `fd` is a valid open fd passed by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(AishError::Pty(format!(
            "fcntl F_GETFD failed: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: [Category 8 — FFI] `fcntl(F_SETFD, …)` sets the fd's descriptor
    // flags. `fd` is valid and `flags | FD_CLOEXEC` is a well-formed bitmask.
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if ret < 0 {
        return Err(AishError::Pty(format!(
            "fcntl F_SETFD failed: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OSC 5151 stripping for scrollback
// ---------------------------------------------------------------------------

/// The OSC 5151 prefix used by the inner aish shell to signal the outer
/// client (session switch / new / detach).
const OSC_5151_PREFIX: &[u8] = b"\x1b]5151;";

/// Strip OSC 5151 escape sequences from a chunk of PTY output.
///
/// These sequences are **ephemeral control signals** between the inner aish
/// shell and the outer client — they must never be stored in the scrollback
/// buffer. If they were, re-attaching to the session would replay them, and
/// the outer client's `OscScanner` would extract the old command and
/// re-trigger a session switch. With two sessions whose scrollback both
/// contain an OSC 5151, this creates an infinite switch loop.
///
/// This function removes complete `\x1b]5151;…\x07` and `\x1b]5151;…\x1b\\`
/// sequences. Live output is still forwarded to clients unchanged (the outer
/// client needs the raw bytes to detect switches); only the scrollback copy
/// is cleaned.
///
/// Chunk-boundary handling: if the OSC prefix appears without a terminator
/// (the sequence was split across two PTY reads — extremely rare for the
/// ~30-byte atomic `write!` that produces it), the remaining bytes are kept
/// as-is to avoid data loss. The outer client's defence-in-depth flag
/// (`skip_osc_commands`) handles this edge case.
fn strip_osc_5151(data: &[u8]) -> Vec<u8> {
    // Fast path: no OSC 5151 prefix anywhere in this chunk.
    if data.len() < OSC_5151_PREFIX.len()
        || !data
            .windows(OSC_5151_PREFIX.len())
            .any(|w| w == OSC_5151_PREFIX)
    {
        return data.to_vec();
    }

    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + OSC_5151_PREFIX.len() <= data.len()
            && &data[i..i + OSC_5151_PREFIX.len()] == OSC_5151_PREFIX
        {
            // Found OSC 5151 start — scan forward for the terminator.
            let mut j = i + OSC_5151_PREFIX.len();
            let mut end = None;
            while j < data.len() {
                if data[j] == 0x07 {
                    end = Some(j + 1);
                    break;
                }
                if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' {
                    end = Some(j + 2);
                    break;
                }
                j += 1;
            }
            match end {
                Some(e) => i = e, // skip the entire sequence
                None => {
                    // No terminator in this chunk — keep remaining bytes
                    // verbatim to avoid data loss (chunk-boundary edge case).
                    out.extend_from_slice(&data[i..]);
                    break;
                }
            }
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Client connection state
// ---------------------------------------------------------------------------

/// Lifecycle states for a connected client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientState {
    /// Just accepted, waiting for Attach frame from client.
    Handshaking,
    /// Attach received, scrollback replay in progress.
    Replaying,
    /// Fully live — receiving real-time broadcast.
    Live,
}

/// State of a connected client.
struct ClientConnection {
    stream: UnixStream,
    /// Lifecycle state
    state: ClientState,
    /// Accumulated partial reads for frame decoding
    reader: FrameReader,
    /// Pending data to write (when client is slow to consume)
    write_buf: VecDeque<u8>,
    /// Remaining scrollback chunks to send (empty = replay done)
    scrollback_queue: VecDeque<Vec<u8>>,
    /// Session ID (filled during handshake)
    session_id: Option<String>,
    /// Terminal size reported by client
    rows: u16,
    cols: u16,
    /// Deadline for handshake completion
    handshake_deadline: Option<std::time::Instant>,
}

impl ClientConnection {
    fn new(stream: UnixStream) -> Self {
        let _ = stream.set_nonblocking(true);
        Self {
            stream,
            state: ClientState::Handshaking,
            reader: FrameReader::new(),
            write_buf: VecDeque::new(),
            scrollback_queue: VecDeque::new(),
            session_id: None,
            rows: 24,
            cols: 80,
            handshake_deadline: Some(std::time::Instant::now() + Duration::from_secs(5)),
        }
    }

    fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Read available data from socket into frame reader. Returns false on EOF.
    fn do_read(&mut self) -> Result<bool> {
        let mut buf = [0u8; 8192];
        match self.stream.read(&mut buf) {
            Ok(0) => Ok(false), // EOF
            Ok(n) => {
                self.reader.push(&buf[..n]);
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(e) => Err(AishError::Pty(format!("client socket read: {e}"))),
        }
    }

    /// Decode all complete frames currently buffered.
    fn drain_frames(&mut self) -> Result<Vec<Frame>> {
        self.reader.drain_frames().map_err(AishError::from)
    }

    /// Queue raw bytes for writing to the client.
    fn queue_write(&mut self, data: &[u8]) {
        self.write_buf.extend(data.iter().copied());
    }

    /// Queue a frame for writing.
    fn queue_frame(&mut self, frame: &Frame) {
        let encoded = proto::encode_frame(frame);
        self.queue_write(&encoded);
    }

    /// Attempt to flush pending writes. Returns true if all data was written.
    fn do_write(&mut self) -> Result<bool> {
        if self.write_buf.is_empty() {
            return Ok(true);
        }

        let (first, second) = self.write_buf.as_slices();
        let mut written = 0;

        // Write first contiguous slice
        if !first.is_empty() {
            match self.stream.write(first) {
                Ok(0) => return Ok(false),
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(false),
                Err(e) => return Err(AishError::Pty(format!("client socket write: {e}"))),
            }
        }

        // If first slice fully written, try second slice
        if written == first.len() && !second.is_empty() {
            match self.stream.write(second) {
                Ok(0) => {}
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(AishError::Pty(format!("client socket write: {e}"))),
            }
        }

        // Remove written bytes from the front
        for _ in 0..written {
            self.write_buf.pop_front();
        }

        Ok(self.write_buf.is_empty())
    }

    fn has_pending_writes(&self) -> bool {
        !self.write_buf.is_empty()
    }

    /// Start scrollback replay: queue all chunks for incremental sending.
    fn start_scrollback_replay(&mut self, chunks: Vec<Vec<u8>>) {
        self.scrollback_queue = chunks.into_iter().collect();
        self.flush_scrollback_to_write_buf();
    }

    /// Move scrollback chunks from queue into write_buf (up to a limit).
    /// Called repeatedly from the main loop as write_buf drains.
    fn flush_scrollback_to_write_buf(&mut self) {
        const MAX_WRITE_BUF_BYTES: usize = 64 * 1024;
        while !self.scrollback_queue.is_empty() && self.write_buf.len() < MAX_WRITE_BUF_BYTES {
            if let Some(chunk) = self.scrollback_queue.pop_front() {
                let frame = Frame {
                    frame_type: TYPE_SCROLLBACK,
                    payload: chunk,
                };
                self.queue_frame(&frame);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PTY Daemon
// ---------------------------------------------------------------------------

/// Session metadata persisted alongside the PTY.
#[derive(Debug, Clone)]
pub struct DaemonSessionInfo {
    pub session_id: String,
    pub socket_path: PathBuf,
    pub daemon_pid: u32,
    pub child_pid: u32,
    pub started_at: u64,
    pub cwd: String,
    pub model: Option<String>,
    pub api_base: Option<String>,
    /// Optional user-defined name for the session.
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Select wrapper
// ---------------------------------------------------------------------------

struct SelectResult {
    readable: Vec<RawFd>,
    writable: Vec<RawFd>,
}

fn select_multi(
    read_fds: &[RawFd],
    write_fds: &[RawFd],
    timeout: Option<Duration>,
) -> Result<SelectResult> {
    // SAFETY: [Category 4 — Uninitialized memory] `fd_set` is a POD C struct
    // (bitmask of fds). `zeroed()` produces a valid all-zero instance. All
    // `FD_*` macros below operate on these initialised sets in-place.
    let mut read_set: libc::fd_set = unsafe { std::mem::zeroed() };
    let mut write_set: libc::fd_set = unsafe { std::mem::zeroed() };

    // SAFETY: [Category 8 — FFI] `FD_ZERO` clears the fd_set to empty.
    unsafe {
        libc::FD_ZERO(&mut read_set);
        libc::FD_ZERO(&mut write_set);
    }

    let mut max_fd: c_int = -1;
    for &fd in read_fds {
        if fd as usize >= libc::FD_SETSIZE {
            return Err(AishError::Pty(format!("fd {} exceeds FD_SETSIZE", fd)));
        }
        // SAFETY: [Category 8 — FFI] `FD_SET` adds `fd` to the set. `fd` is
        // below FD_SETSIZE (checked above) and `read_set` is initialised.
        unsafe { libc::FD_SET(fd, &mut read_set) };
        if fd > max_fd {
            max_fd = fd;
        }
    }
    for &fd in write_fds {
        if fd as usize >= libc::FD_SETSIZE {
            return Err(AishError::Pty(format!("fd {} exceeds FD_SETSIZE", fd)));
        }
        // SAFETY: [Category 8 — FFI] Same as the read_fds FD_SET above.
        unsafe { libc::FD_SET(fd, &mut write_set) };
        if fd > max_fd {
            max_fd = fd;
        }
    }

    let timeout_val = match timeout {
        Some(d) => libc::timeval {
            tv_sec: d.as_secs() as libc::time_t,
            tv_usec: d.subsec_micros() as libc::suseconds_t,
        },
        None => libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
    };

    let timeout_ptr = if timeout.is_some() {
        &timeout_val as *const libc::timeval
    } else {
        std::ptr::null()
    };

    // SAFETY: [Category 8 — FFI] `select()` polls the fd_sets for readiness.
    // `max_fd+1` is the nfds argument, `read_set`/`write_set` are initialised,
    // `timeout_ptr` is null (infinite) or points to a valid `timeval`.
    let ret = unsafe {
        libc::select(
            max_fd + 1,
            &mut read_set,
            &mut write_set,
            std::ptr::null_mut(),
            timeout_ptr as *mut libc::timeval,
        )
    };

    if ret < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(SelectResult {
                readable: Vec::new(),
                writable: Vec::new(),
            });
        }
        return Err(AishError::Pty(format!("select: {err}")));
    }

    let mut readable = Vec::new();
    let mut writable = Vec::new();

    for &fd in read_fds {
        // SAFETY: [Category 8 — FFI] `FD_ISSET` tests membership of `fd` in
        // the post-select `read_set`. The set was initialised and passed to
        // select; `fd` is below FD_SETSIZE.
        if unsafe { libc::FD_ISSET(fd, &read_set) } {
            readable.push(fd);
        }
    }
    for &fd in write_fds {
        // SAFETY: [Category 8 — FFI] Same as the FD_ISSET above for write_set.
        if unsafe { libc::FD_ISSET(fd, &write_set) } {
            writable.push(fd);
        }
    }

    Ok(SelectResult { readable, writable })
}

/// Get the directory for PTY session files.
pub fn pty_session_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
        .ok_or_else(|| AishError::Pty("cannot determine data directory".into()))?;
    let dir = base.join("aish").join("pty-sessions");
    Ok(dir)
}

/// Get the socket directory (prefers XDG_RUNTIME_DIR).
/// Validates ownership on /tmp fallback to prevent symlink attacks.
pub fn pty_socket_dir() -> Result<PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        // SAFETY: [Category 8 — FFI] `getuid()` never fails (always returns
        // the real UID of the calling process) and has no UB surface.
        .unwrap_or_else(|_| PathBuf::from(format!("/tmp/aish-{}", unsafe { libc::getuid() })));
    let socket_dir = dir.join("aish");

    // For /tmp fallback, verify directory ownership and permissions
    if !std::path::Path::new(&dir).starts_with("/run/user") {
        if let Ok(meta) = std::fs::metadata(&dir) {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: [Category 8 — FFI] `getuid()` — see above, never fails.
            let uid = unsafe { libc::getuid() };
            if meta.uid() != uid {
                return Err(AishError::Pty(format!(
                    "socket dir {:?} not owned by uid {} (owned by {})",
                    dir,
                    uid,
                    meta.uid()
                )));
            }
            if meta.mode() & 0o077 != 0 {
                return Err(AishError::Pty(format!(
                    "socket dir {:?} is group/world accessible (mode {:o})",
                    dir,
                    meta.mode()
                )));
            }
        }
    }

    Ok(socket_dir)
}

/// Write a session metadata file for client discovery.
fn write_session_file(info: &DaemonSessionInfo) {
    let dir = match pty_session_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.json", info.session_id));
    // SAFETY: [Category 8 — FFI] `getuid()` — see `pty_socket_dir` above.
    let uid = unsafe { libc::getuid() };
    let json = serde_json::json!({
        "session_uuid": info.session_id,
        "socket_path": info.socket_path.to_string_lossy(),
        "daemon_pid": info.daemon_pid,
        "child_pid": info.child_pid,
        "started_at": info.started_at,
        "owner_uid": uid,
        "cwd": info.cwd,
        "model": info.model,
        "api_base": info.api_base,
        "name": info.name,
    });
    if std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    )
    .is_ok()
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Remove session file on exit.
fn remove_session_file(session_id: &str) {
    let dir = match pty_session_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let path = dir.join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(&path);
}

/// Set or clear the user-defined name of a live session.
///
/// `name` is an empty string to clear the name, or a non-empty string to set
/// it. The name is persisted into the session JSON file so it survives
/// `aish` restarts and is shown by `live-sessions`, the attach picker, and
/// `kill`.
///
/// Returns `Err` if the session file cannot be found, read, or written.
pub fn rename_session(session_id: &str, name: &str) -> Result<()> {
    let dir = pty_session_dir()?;
    let path = dir.join(format!("{session_id}.json"));
    let content = std::fs::read_to_string(&path)
        .map_err(|e| AishError::Pty(format!("read session file: {e}")))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AishError::Pty(format!("parse session file: {e}")))?;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        if let Some(obj) = json.as_object_mut() {
            obj.remove("name");
        }
    } else if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "name".to_string(),
            serde_json::Value::String(trimmed.to_string()),
        );
    }
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();
    std::fs::write(&path, pretty)
        .map_err(|e| AishError::Pty(format!("write session file: {e}")))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Check if a daemon process is alive by PID (non-perturbing, does not
/// trigger the daemon's accept/handshake logic).
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // kill(pid, 0) returns 0 if the process exists, -1 otherwise.
    // SAFETY: [Category 8 — FFI] `kill(pid, 0)` sends no signal — it only
    // checks process existence. `pid` is non-zero (guarded above). ESRCH
    // (process gone) is reported via return value, not UB.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Check if a daemon is alive by testing its socket. Note: this connects
/// to the socket and triggers the daemon's accept logic. Prefer
/// `is_pid_alive` for discovery probes.
pub fn check_daemon_alive(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).is_ok()
}

/// Discover all active PTY sessions.
pub fn discover_sessions() -> Vec<DaemonSessionInfo> {
    let dir = match pty_session_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    let socket_path = json
                        .get("socket_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let socket_path = PathBuf::from(socket_path);

                    let daemon_pid =
                        json.get("daemon_pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                    // Check if daemon is still alive via PID (non-perturbing)
                    if is_pid_alive(daemon_pid) {
                        sessions.push(DaemonSessionInfo {
                            session_id: json
                                .get("session_uuid")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            socket_path,
                            daemon_pid,
                            child_pid: json.get("child_pid").and_then(|v| v.as_u64()).unwrap_or(0)
                                as u32,
                            started_at: json
                                .get("started_at")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            cwd: json
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            model: json.get("model").and_then(|v| v.as_str()).map(String::from),
                            api_base: json
                                .get("api_base")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            name: json.get("name").and_then(|v| v.as_str()).map(String::from),
                        });
                    } else {
                        // Stale session file, clean up
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
    sessions
}

/// Kill a daemon session by sending KillSession to its socket.
pub fn kill_session(socket_path: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| AishError::Pty(format!("connect to daemon: {e}")))?;

    let frame = Frame::kill_session();
    let encoded = proto::encode_frame(&frame);
    stream
        .write_all(&encoded)
        .map_err(|e| AishError::Pty(format!("send kill: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Shell-mode daemon: runs aish inside a PTY (full UI preserved)
// ---------------------------------------------------------------------------

/// Run a PTY daemon that executes `shell_exe` (typically the aish binary)
/// inside a PTY. The daemon holds the PTY master fd, serves clients via
/// Unix socket, and replays scrollback on reattach.
///
/// This mode runs the FULL aish process inside the PTY — preserving all UI
/// features (prompt, AI mode, slash commands, completions, status bar).
pub fn run_pty_daemon_shell(
    cwd: &str,
    rows: u16,
    cols: u16,
    socket_path: &Path,
    session_id: &str,
    shell_exe: &str,
) -> Result<()> {
    use nix::unistd::ForkResult;

    // Ignore SIGHUP so daemon survives terminal disconnects.
    // SAFETY: [Category 4 — Uninitialized memory] [Category 8 — FFI]
    // `sigaction` is zeroed then set to SIG_IGN. `sigaction(SIGHUP, …)`
    // installs the ignore disposition. Runs before any threads exist (the
    // daemon is single-threaded), so no race.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_IGN;
        libc::sigaction(libc::SIGHUP, &sa, std::ptr::null_mut());
    }

    let sigchld = SigchldPipe::install()?;

    // Create PTY pair
    // SAFETY: [Category 4 — Uninitialized memory] `winsize` is a POD C struct;
    // `zeroed()` produces a valid instance. We set `ws_row`/`ws_col` before use.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    ws.ws_row = rows;
    ws.ws_col = cols;
    let mut master_pty: libc::c_int = -1;
    let mut slave_pty: libc::c_int = -1;
    // SAFETY: [Category 8 — FFI] `openpty()` allocates a PTY pair and writes
    // the master/slave fds into `master_pty`/`slave_pty`. The pointers are
    // valid (stack locals), name buffer is null (we don't need the tty name),
    // and `&ws` points to the initialised winsize. Returns 0 on success.
    let ret = unsafe {
        libc::openpty(
            &raw mut master_pty,
            &raw mut slave_pty,
            std::ptr::null_mut(),
            std::ptr::null(),
            &raw const ws,
        )
    };
    if ret != 0 {
        return Err(AishError::Pty(format!(
            "openpty failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    set_fd_nonblocking(master_pty)?;
    set_fd_cloexec(master_pty)?;

    // Fork
    // SAFETY: [Category 8 — FFI] `fork()` duplicates the process. In the
    // child, only async-signal-safe calls are used (setsid, ioctl, dup2,
    // close, setenv, execvp) before `_exit`. The parent continues with the
    // master fd. `nix::unistd::fork` is a thin safe wrapper that returns
    // the `ForkResult` enum.
    match unsafe { nix::unistd::fork() } {
        Ok(ForkResult::Child) => {
            // Child: set up slave as terminal and exec aish
            // SAFETY: [Category 8 — FFI] All calls here are async-signal-safe
            // post-fork: `setsid()` creates a new session, `ioctl(TIOCSCTTY)`
            // makes the slave the controlling terminal, `dup2` duplicates it
            // onto stdin/stdout/stderr, `close` drops the originals, `setenv`
            // sets environment variables. `slave_pty` and `master_pty` are
            // valid fds from `openpty` above. After `execvp`, only `_exit`
            // runs if exec failed.
            unsafe {
                libc::setsid();
                libc::ioctl(slave_pty, libc::TIOCSCTTY, 0);
                libc::dup2(slave_pty, 0);
                libc::dup2(slave_pty, 1);
                libc::dup2(slave_pty, 2);
                if slave_pty > 2 {
                    libc::close(slave_pty);
                }
                libc::close(master_pty);
                std::env::set_var("TERM", "xterm-256color");
                std::env::set_var("AISH_PTY_DAEMON", "0");
                std::env::set_var("AISH_SESSION_ID", session_id);
                std::env::set_var("AISH_DAEMON_MODE", "1");
                let _ = std::env::set_current_dir(cwd);
            }
            {
                let exe_cstr = std::ffi::CString::new(shell_exe).unwrap_or_default();
                let arg_cstr = std::ffi::CString::new(shell_exe).unwrap_or_default();
                let err = nix::unistd::execvp(&exe_cstr, &[&arg_cstr]);
                eprintln!("exec failed: {:?}", err);
            }
            // SAFETY: [Category 8 — FFI] `_exit(127)` terminates the child
            // immediately after exec failure. Must not run destructors (no
            // `std::process::exit`) because the forked address space may be
            // in an inconsistent state.
            unsafe {
                libc::_exit(127);
            }
        }
        Ok(ForkResult::Parent { child }) => {
            // SAFETY: [Category 8 — FFI] `close(slave_pty)` — the parent does
            // not need the slave end. `slave_pty` is a valid fd from openpty
            // and is not owned by any `OwnedFd`.
            unsafe {
                libc::close(slave_pty);
            }
            let child_pid = child.as_raw() as u32;

            // Bind listener
            if let Some(parent) = socket_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(socket_path);
            let listener = UnixListener::bind(socket_path)
                .map_err(|e| AishError::Pty(format!("bind socket: {e}")))?;
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
            listener
                .set_nonblocking(true)
                .map_err(|e| AishError::Pty(format!("listener nonblocking: {e}")))?;

            let session_info = DaemonSessionInfo {
                session_id: session_id.to_string(),
                socket_path: socket_path.to_path_buf(),
                daemon_pid: std::process::id(),
                child_pid,
                started_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                cwd: cwd.to_string(),
                model: None,
                api_base: None,
                name: None,
            };
            write_session_file(&session_info);
            tracing::info!(session_id, child_pid, "shell daemon started");

            let mut scrollback = ScrollbackBuffer::with_default_size();
            let mut clients: Vec<ClientConnection> = Vec::new();
            let mut child_exited = false;

            loop {
                let sigchld_fd = sigchld.read_fd();
                let listener_fd = listener.as_raw_fd();

                let mut read_fds = vec![master_pty, sigchld_fd, listener_fd];
                let mut write_fds = Vec::new();
                for client in &clients {
                    read_fds.push(client.fd());
                    if client.has_pending_writes() {
                        write_fds.push(client.fd());
                    }
                }

                let timeout = Duration::from_millis(50);
                match select_multi(&read_fds, &write_fds, Some(timeout)) {
                    Ok(SelectResult {
                        readable, writable, ..
                    }) => {
                        if readable.contains(&sigchld_fd) {
                            sigchld.drain();
                            if sigchld.reap_children() {
                                child_exited = true;
                            }
                        }

                        // PTY output → scrollback + broadcast
                        if readable.contains(&master_pty) {
                            let mut buf = [0u8; 8192];
                            loop {
                                match nix::unistd::read(master_pty, &mut buf) {
                                    Ok(0) => {
                                        child_exited = true;
                                        break;
                                    }
                                    Ok(n) => {
                                        let data = &buf[..n];
                                        // Forward raw bytes to live clients (the
                                        // outer client needs OSC 5151 to detect
                                        // session switches).
                                        for client in &mut clients {
                                            if client.state == ClientState::Live {
                                                for chunk in data.chunks(
                                                    crate::daemon_protocol::MAX_FRAME_PAYLOAD,
                                                ) {
                                                    client.queue_frame(&Frame::pty_output(chunk));
                                                }
                                            }
                                        }
                                        // Store OSC-stripped bytes in scrollback
                                        // so re-attach does not replay stale OSC
                                        // commands (which would cause an infinite
                                        // switch loop).
                                        let clean = strip_osc_5151(data);
                                        if !clean.is_empty() {
                                            scrollback.push(&clean);
                                        }
                                    }
                                    Err(nix::errno::Errno::EAGAIN) => break,
                                    Err(nix::errno::Errno::EINTR) => continue,
                                    Err(nix::errno::Errno::EIO) => {
                                        child_exited = true;
                                        break;
                                    }
                                    Err(_) => break,
                                }
                            }
                        }

                        // Client I/O
                        let mut to_remove = Vec::new();
                        for i in (0..clients.len()).rev() {
                            let fd = clients[i].fd();
                            let mut remove = false;

                            if readable.contains(&fd) {
                                let alive = match clients[i].do_read() {
                                    Ok(true) => true,
                                    Ok(false) => false,
                                    Err(_) => false,
                                };
                                if !alive {
                                    remove = true;
                                } else {
                                    let frames = clients[i].drain_frames().unwrap_or_default();
                                    for frame in frames {
                                        // KillSession allowed from any state
                                        if frame.frame_type == TYPE_KILL_SESSION {
                                            let _ = nix::sys::signal::kill(
                                                child,
                                                Some(nix::sys::signal::Signal::SIGTERM),
                                            );
                                            std::thread::sleep(Duration::from_millis(200));
                                            let _ = nix::sys::signal::kill(
                                                child,
                                                Some(nix::sys::signal::Signal::SIGKILL),
                                            );
                                            child_exited = true;
                                            remove = true;
                                            break;
                                        }
                                        if clients[i].state == ClientState::Handshaking {
                                            if frame.frame_type == TYPE_ATTACH {
                                                shell_daemon_handshake(
                                                    &mut clients[i],
                                                    &scrollback,
                                                    &req_from_frame(&frame),
                                                    master_pty,
                                                    child_pid,
                                                    session_id,
                                                    cwd,
                                                    child_exited,
                                                );
                                            }
                                            continue;
                                        }
                                        match frame.frame_type {
                                            TYPE_INPUT => {
                                                // Write to PTY master, retry on EAGAIN
                                                let data = &frame.payload;
                                                let mut written = 0;
                                                while written < data.len() {
                                                    // SAFETY: [Category 8 — FFI]
                                                    // `write(master_pty, …)` forwards
                                                    // user input to the PTY. `master_pty`
                                                    // is a valid open fd. The slice
                                                    // `data[written..]` is valid (written
                                                    // <= data.len, checked by the while
                                                    // condition). On EAGAIN we retry
                                                    // after a brief sleep; on real error
                                                    // we log and drop the remaining data.
                                                    let n = unsafe {
                                                        libc::write(
                                                            master_pty,
                                                            data[written..].as_ptr() as *const _,
                                                            data.len() - written,
                                                        )
                                                    };
                                                    if n < 0 {
                                                        let err = std::io::Error::last_os_error();
                                                        if err.kind()
                                                            == std::io::ErrorKind::WouldBlock
                                                        {
                                                            // PTY buffer full, wait briefly and retry
                                                            std::thread::sleep(
                                                                Duration::from_millis(1),
                                                            );
                                                            continue;
                                                        }
                                                        if err.kind()
                                                            == std::io::ErrorKind::Interrupted
                                                        {
                                                            continue;
                                                        }
                                                        // Real error, drop remaining data
                                                        tracing::warn!("PTY write error: {}", err);
                                                        break;
                                                    }
                                                    written += n as usize;
                                                }
                                            }
                                            TYPE_RESIZE => {
                                                if let Ok(r) = frame.as_resize_request() {
                                                    clients[i].rows = r.rows;
                                                    clients[i].cols = r.cols;
                                                    do_resize(master_pty, r.rows, r.cols);
                                                }
                                            }
                                            TYPE_DETACH => remove = true,
                                            TYPE_KILL_SESSION => {
                                                let _ = nix::sys::signal::kill(
                                                    child,
                                                    Some(nix::sys::signal::Signal::SIGTERM),
                                                );
                                                std::thread::sleep(Duration::from_millis(200));
                                                let _ = nix::sys::signal::kill(
                                                    child,
                                                    Some(nix::sys::signal::Signal::SIGKILL),
                                                );
                                                child_exited = true;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            if writable.contains(&fd) && !remove {
                                let _ = clients[i].do_write();
                            }
                            if remove {
                                to_remove.push(i);
                            }
                        }
                        for i in to_remove {
                            let r = clients.swap_remove(i);
                            tracing::info!(
                                fd = r.fd(),
                                remaining = clients.len(),
                                "client removed"
                            );
                        }

                        // Timeout handshakes
                        let now = std::time::Instant::now();
                        for i in (0..clients.len()).rev() {
                            if clients[i].state == ClientState::Handshaking {
                                if let Some(dl) = clients[i].handshake_deadline {
                                    if now > dl {
                                        clients.swap_remove(i);
                                    }
                                }
                            }
                        }

                        // Accept
                        if readable.contains(&listener_fd) {
                            if let Ok((stream, _)) = listener.accept() {
                                if clients.len() < 8 {
                                    clients.push(ClientConnection::new(stream));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("select error: {e}");
                    }
                }

                // Replay continuation
                for client in &mut clients {
                    if client.state == ClientState::Replaying {
                        if !client.scrollback_queue.is_empty() {
                            client.flush_scrollback_to_write_buf();
                        }
                        if client.scrollback_queue.is_empty() {
                            client.state = ClientState::Live;
                        }
                    }
                }

                if child_exited {
                    for client in &mut clients {
                        client.queue_frame(&Frame::exit_notice(0, "process exited"));
                        let _ = client.do_write();
                    }
                    break;
                }
            }

            let _ = std::fs::remove_file(socket_path);
            remove_session_file(session_id);
            // SAFETY: [Category 8 — FFI] `close(master_pty)` — the daemon is
            // exiting; `master_pty` is a valid fd from openpty not owned by
            // any `OwnedFd`.
            unsafe {
                libc::close(master_pty);
            }
            Ok(())
        }
        Err(e) => {
            // SAFETY: [Category 8 — FFI] `close()` both PTY fds on fork
            // failure. Both are valid fds from openpty, not owned by any
            // `OwnedFd`.
            unsafe {
                libc::close(master_pty);
                libc::close(slave_pty);
            }
            Err(AishError::Pty(format!("fork failed: {e}")))
        }
    }
}

/// Helper: parse AttachRequest from a frame.
fn req_from_frame(frame: &Frame) -> AttachRequest {
    frame.as_attach_request().unwrap_or(AttachRequest {
        protocol_version: PROTOCOL_VERSION,
        session_id: String::new(),
        rows: 24,
        cols: 80,
        term: "xterm-256color".to_string(),
    })
}

/// Helper: complete handshake for shell daemon clients.
fn shell_daemon_handshake(
    client: &mut ClientConnection,
    scrollback: &ScrollbackBuffer,
    req: &AttachRequest,
    master_pty: RawFd,
    child_pid: u32,
    session_id: &str,
    cwd: &str,
    running: bool,
) {
    client.rows = req.rows;
    client.cols = req.cols;
    client.session_id = Some(req.session_id.clone());
    do_resize(master_pty, req.rows, req.cols);

    let ack = AttachAck {
        protocol_version: PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        scrollback_size: scrollback.len(),
    };
    client.queue_frame(&Frame::attach_ack(&ack));

    let chunks = scrollback.replay_chunks(crate::scrollback::SCROLLBACK_CHUNK_SIZE);
    client.start_scrollback_replay(chunks);

    let end_info = ScrollbackEnd {
        child_pid,
        cwd: cwd.to_string(),
        running: !running,
        model: None,
        api_base: None,
    };
    client.queue_frame(&Frame::scrollback_end(&end_info));
    client.state = ClientState::Replaying;
}

/// Helper: resize PTY via ioctl.
fn do_resize(master_fd: RawFd, rows: u16, cols: u16) {
    // SAFETY: [Category 4 — Uninitialized memory] `winsize` is a POD C struct;
    // `zeroed()` produces a valid instance. We set ws_row/ws_col before use.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    ws.ws_row = rows;
    ws.ws_col = cols;
    // SAFETY: [Category 8 — FFI] `ioctl(TIOCSWINSZ)` sets the PTY window size.
    // `master_fd` is a valid open fd (the PTY master). `&mut ws` points to the
    // initialised struct. A failure is non-fatal (the PTY keeps its old size).
    unsafe {
        libc::ioctl(master_fd, libc::TIOCSWINSZ, &mut ws);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_osc_5151_no_sequence_returns_unchanged() {
        let data = b"hello world\n";
        assert_eq!(strip_osc_5151(data), data.to_vec());
    }

    #[test]
    fn strip_osc_5151_empty_input() {
        assert_eq!(strip_osc_5151(b""), Vec::<u8>::new());
    }

    #[test]
    fn strip_osc_5151_strips_bell_terminated() {
        let data = b"before\x1b]5151;switch:abc123\x07after";
        let result = strip_osc_5151(data);
        assert_eq!(result, b"beforeafter");
    }

    #[test]
    fn strip_osc_5151_strips_st_terminated() {
        let data = b"before\x1b]5151;new\x1b\\after";
        let result = strip_osc_5151(data);
        assert_eq!(result, b"beforeafter");
    }

    #[test]
    fn strip_osc_5151_strips_multiple_sequences() {
        let data = b"a\x1b]5151;switch:x\x07b\x1b]5151;new\x07c";
        let result = strip_osc_5151(data);
        assert_eq!(result, b"abc");
    }

    #[test]
    fn strip_osc_5151_preserves_other_osc_sequences() {
        // OSC 0 (set title) should NOT be stripped.
        let data = b"\x1b]0;my title\x07";
        let result = strip_osc_5151(data);
        assert_eq!(result, data.to_vec());
    }

    #[test]
    fn strip_osc_5151_incomplete_keeps_remaining() {
        // OSC prefix without terminator — bytes kept verbatim (chunk boundary).
        let data = b"text\x1b]5151;swi";
        let result = strip_osc_5151(data);
        assert_eq!(result, data.to_vec());
    }

    #[test]
    fn strip_osc_5151_standalone_sequence() {
        let data = b"\x1b]5151;switch:deadbeef\x07";
        let result = strip_osc_5151(data);
        assert_eq!(result, b"");
    }

    #[test]
    fn strip_osc_5151_real_world_switch_command() {
        // Exactly what emit_osc produces in aish-shell/src/app.rs.
        let data = format!("prompt$ \x1b]5151;switch:{}\x07", "abc12345").into_bytes();
        let result = strip_osc_5151(&data);
        assert_eq!(result, b"prompt$ ");
    }
}
