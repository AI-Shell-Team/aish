//! stdio transport: newline-delimited JSON-RPC over a child process.
//!
//! Framing is one JSON object per line (`\n`), not `Content-Length`: that is
//! what the MCP stdio spec mandates and what every server implementation
//! speaks.
//!
//! Three tasks share one transport handle:
//!
//! - a **read loop** that splits stdout into frames and correlates them,
//! - a **writer loop** that drains a queue of encoded frames into stdin, and
//! - a **stderr loop** that logs the server's diagnostics and discards them.
//!
//! Neither loop ever blocks a caller: a request registers its pending entry,
//! queues its frame and then waits on the response, its deadline or the
//! caller's cancellation token. The writer loop owns the child's stdin, so a
//! subprocess that stops draining the pipe cannot park a request past its own
//! deadline; the loop is aborted (which drops the handle and closes the pipe)
//! on teardown.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::cancel::CancelListener;
use crate::config::ServerSettings;
use crate::jsonrpc::{self, RequestIdAllocator};
use crate::types::{
    classify_frames, IncomingFrame, JsonRpcError, JsonRpcId, JsonRpcResponse, McpError,
    McpFailureClass, McpFailureStage, McpTransport, TransportEvent, TransportKind,
};

/// Cooperative shutdown window after SIGTERM before escalating to SIGKILL.
const TERM_GRACE: Duration = Duration::from_millis(1000);
/// Window to observe SIGKILL before `close()` gives up and returns.
const KILL_GRACE: Duration = Duration::from_millis(500);
/// Poll interval while waiting for the child to be reaped.
const EXIT_POLL: Duration = Duration::from_millis(10);
/// Extra time the read loop gives the kernel to reap an exited child, so the
/// EOF failure can name the exit status.
const EXIT_STATUS_GRACE: Duration = Duration::from_millis(100);

/// Longest stderr line written to the debug log.
const MAX_STDERR_LOG_BYTES: usize = 500;

/// Hard cap on one inbound frame. A server that writes a line without ever
/// sending a newline must fail the connection instead of growing the read
/// buffer until memory runs out.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Hard cap on in-flight requests. Every entry is removed when its request
/// settles or is dropped, so the map only grows with genuinely concurrent
/// callers; the cap keeps a runaway one from exhausting memory.
const MAX_PENDING_REQUESTS: usize = 1024;

/// Create and start a stdio transport.
///
/// Spawns `settings.command` with `settings.args`, `settings.child_env()` and
/// `settings.cwd` (the current directory when unset), then starts the read,
/// writer and stderr loops. Must be called from within a tokio runtime.
pub fn create_stdio_transport(
    settings: &ServerSettings,
    events: mpsc::UnboundedSender<TransportEvent>,
) -> Result<Arc<dyn McpTransport>, McpError> {
    let command = settings
        .command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| {
            McpError::new(
                &settings.name,
                TransportKind::Stdio,
                McpFailureStage::Connect,
                McpFailureClass::Connect,
                true,
                "stdio server has no `command` configured",
            )
        })?;

    let mut process = Command::new(command);
    process
        .args(&settings.args)
        // The child gets only the inherited allowlist plus the configured
        // overlay, never the full parent environment (see `child_env`).
        .env_clear()
        .envs(settings.child_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = settings.cwd.as_deref() {
        process.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        // SAFETY: `setsid` and `prctl` are async-signal-safe and run in the
        // forked child before `exec`. A fresh fork is never a session leader,
        // so `setsid` cannot fail; should it ever do so, the closure returns
        // the OS error and the spawn aborts instead of leaving the server in
        // aish's session, where a terminal job-control signal
        // (SIGTSTP/SIGTTIN) would stop it and park the read loop until the
        // user continues it.
        unsafe {
            process.pre_exec(|| {
                if nix::unistd::setsid().is_err() {
                    return Err(io::Error::last_os_error());
                }
                // Ask the kernel to terminate the server when aish dies, even
                // on signals aish cannot handle (SIGKILL, a closed terminal
                // sending SIGHUP). Without this an abruptly killed shell
                // orphans the whole server tree; an MCP server with no client
                // is dead weight holding resources. The ordinary exit path
                // still sends SIGTERM then SIGKILL through `close()`.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = process.spawn().map_err(|error| {
        McpError::new(
            &settings.name,
            TransportKind::Stdio,
            McpFailureStage::Connect,
            McpFailureClass::Connect,
            true,
            format!("failed to spawn `{command}`: {error}"),
        )
    })?;

    let pid = child.id().map(|pid| pid as i32).unwrap_or(0);
    // The pipes move into their loops; the child handle stays here for exit
    // status polling and signalling.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (writes, frames) = mpsc::unbounded_channel::<Vec<u8>>();
    let transport = Arc::new(StdioTransport {
        settings: settings.clone(),
        pid,
        detached: cfg!(unix),
        child: Mutex::new(Some(child)),
        pending: Mutex::new(HashMap::new()),
        ids: RequestIdAllocator::new(),
        events,
        writes,
        write_failure: Mutex::new(None),
        writer_task: Mutex::new(None),
        connected: AtomicBool::new(true),
    });

    if let Some(stdin) = stdin {
        let writer = tokio::spawn(writer_loop(stdin, frames, Arc::downgrade(&transport)));
        *transport.writer_task.lock() = Some(writer);
    }
    if let Some(stdout) = stdout {
        tokio::spawn(read_loop(Arc::downgrade(&transport), stdout));
    }
    if let Some(stderr) = stderr {
        tokio::spawn(stderr_loop(Arc::downgrade(&transport), stderr));
    }

    Ok(transport as Arc<dyn McpTransport>)
}

struct StdioTransport {
    settings: ServerSettings,
    /// Child process id, `0` when the platform did not report one.
    pid: i32,
    /// This transport put the child into its own session, so signals go to its
    /// process group.
    detached: bool,
    child: Mutex<Option<Child>>,
    pending: Mutex<HashMap<JsonRpcId, oneshot::Sender<Result<Value, McpError>>>>,
    ids: RequestIdAllocator,
    events: mpsc::UnboundedSender<TransportEvent>,
    writes: mpsc::UnboundedSender<Vec<u8>>,
    /// First stdin write failure, reported to later senders.
    write_failure: Mutex<Option<McpError>>,
    writer_task: Mutex<Option<JoinHandle<()>>>,
    connected: AtomicBool,
}

impl StdioTransport {
    /// Reject every pending request and emit the single `Closed` event.
    ///
    /// Idempotent: only the first caller - the read loop on a fatal failure or
    /// `close()` - wins, so a torn-down connection never emits `Closed` twice.
    fn shutdown(&self, error: Option<McpError>) {
        if !self.connected.swap(false, Ordering::SeqCst) {
            return;
        }
        let pending: Vec<(JsonRpcId, oneshot::Sender<Result<Value, McpError>>)> =
            self.pending.lock().drain().collect();
        let fallback = McpError::closed(&self.settings.name, TransportKind::Stdio);
        for (_, settle) in pending {
            let failure = error.clone().unwrap_or_else(|| fallback.clone());
            let _ = settle.send(Err(failure));
        }
        let _ = self.events.send(TransportEvent::Closed { error });
    }

    /// Tear the child down: close its stdin, SIGTERM its session (POSIX) or the
    /// process itself, wait up to [`TERM_GRACE`], escalate to SIGKILL and wait
    /// up to [`KILL_GRACE`]. Bounded and idempotent.
    async fn terminate(&self) {
        // Aborting the writer drops the child's stdin handle, which lets a
        // well-behaved server exit on EOF before any signal is sent. Dropping
        // the task does not wait for the write it interrupts.
        if let Some(writer) = self.writer_task.lock().take() {
            writer.abort();
        }
        if self.pid <= 0 {
            return;
        }
        if self.try_exit().is_some() {
            // The leader is gone, but its session group may not be: a launcher
            // (`npx`, `uvx`, a shell wrapper) can exit while the server it
            // started keeps running, so sweep the group even here.
            if self.detached {
                self.signal_kill();
            }
            return;
        }
        self.signal_term();
        if self.wait_for_exit(TERM_GRACE).await {
            // A detached session's leader exiting is no proof that its group is
            // empty: a grandchild can outlive it and ignore SIGTERM, so sweep
            // the group with SIGKILL even after a clean leader exit.
            if self.detached {
                self.signal_kill();
            }
            return;
        }
        self.signal_kill();
        self.wait_for_exit(KILL_GRACE).await;
    }

    /// Signal SIGTERM to the transport's process group / process.
    #[cfg(unix)]
    fn signal_term(&self) {
        self.signal(nix::sys::signal::Signal::SIGTERM);
    }

    /// Signal SIGKILL to the transport's process group / process.
    #[cfg(unix)]
    fn signal_kill(&self) {
        self.signal(nix::sys::signal::Signal::SIGKILL);
    }

    /// SIGNAL the child's process group (it leads its own session) and fall
    /// back to the child alone when the group is out of reach.
    #[cfg(unix)]
    fn signal(&self, signal: nix::sys::signal::Signal) {
        if self.detached {
            match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(self.pid), signal) {
                Ok(()) => return,
                // The whole group is already gone: nothing left to signal.
                Err(nix::errno::Errno::ESRCH) => return,
                // e.g. EPERM: fall through and signal the child directly.
                Err(_) => {}
            }
        }
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid), signal);
    }

    /// SIGTERM the child directly.
    #[cfg(not(unix))]
    fn signal_term(&self) {
        self.start_kill();
    }

    /// SIGKILL the child directly.
    #[cfg(not(unix))]
    fn signal_kill(&self) {
        self.start_kill();
    }

    #[cfg(not(unix))]
    fn start_kill(&self) {
        if let Some(child) = self.child.lock().as_mut() {
            let _ = child.start_kill();
        }
    }

    /// Poll the child once: `Some(description)` once it exited, else `None`.
    fn try_exit(&self) -> Option<String> {
        let mut guard = self.child.lock();
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(describe_exit(status)),
            Ok(None) | Err(_) => None,
        }
    }

    /// Wait up to `budget` for the child to be reaped.
    async fn wait_for_exit(&self, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            if self.try_exit().is_some() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(EXIT_POLL).await;
        }
    }

    /// Exit status description for an EOF failure, waiting briefly for a child
    /// that is still shutting down.
    async fn exit_description(&self) -> Option<String> {
        if let Some(description) = self.try_exit() {
            return Some(description);
        }
        if self.wait_for_exit(EXIT_STATUS_GRACE).await {
            return self.try_exit();
        }
        None
    }

    /// The failure reported when stdout closed.
    async fn stream_end_error(&self) -> McpError {
        let message = match self.exit_description().await {
            Some(description) => {
                format!("MCP subprocess {description} before responding on stdio")
            }
            None => "MCP subprocess closed stdout before responding".to_string(),
        };
        McpError::eof(&self.settings.name, TransportKind::Stdio, message)
    }

    /// Route one decoded frame.
    fn dispatch(&self, value: Value) {
        for frame in classify_frames(&value) {
            match frame {
                IncomingFrame::Response(response) => self.complete(response),
                IncomingFrame::Notification { method, params } => {
                    let _ = self
                        .events
                        .send(TransportEvent::Notification { method, params });
                }
                IncomingFrame::Request { id, method, params } => {
                    let _ = self
                        .events
                        .send(TransportEvent::Request { id, method, params });
                }
            }
        }
    }

    /// Settle the pending request an inbound response belongs to. A response
    /// for an unknown id is a late answer to a timed-out or cancelled request
    /// and is dropped.
    fn complete(&self, response: JsonRpcResponse) {
        let Some(settle) = self.pending.lock().remove(&response.id) else {
            return;
        };
        let outcome = match response.error {
            Some(error) => {
                let mut failure = McpError::json_rpc(
                    &self.settings.name,
                    TransportKind::Stdio,
                    error.code,
                    error.message,
                );
                if let Some(data) = error.data {
                    failure = failure.with_data(data);
                }
                Err(failure)
            }
            None => Ok(response.result.unwrap_or(Value::Null)),
        };
        let _ = settle.send(outcome);
    }

    /// Decode one newline-delimited frame. `Ok(None)` for a blank line; an
    /// `Err` is a fatal decode failure for the whole connection.
    fn decode_frame(&self, line: &[u8]) -> Result<Option<Value>, McpError> {
        let trimmed = trim_frame(line);
        if trimmed.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice::<Value>(trimmed)
            .map(Some)
            .map_err(|error| {
                McpError::new(
                    &self.settings.name,
                    TransportKind::Stdio,
                    McpFailureStage::Decode,
                    McpFailureClass::MalformedResponse,
                    false,
                    format!("invalid JSON-RPC frame: {error}"),
                )
            })
    }

    /// Queue one encoded frame for the writer loop.
    fn submit_frame(&self, frame: &Value) -> Result<(), McpError> {
        if !self.is_connected() {
            return Err(McpError::closed(&self.settings.name, TransportKind::Stdio));
        }
        let mut bytes = serde_json::to_vec(frame).map_err(|error| {
            McpError::new(
                &self.settings.name,
                TransportKind::Stdio,
                McpFailureStage::Send,
                McpFailureClass::Unknown,
                false,
                format!("failed to encode a JSON-RPC frame: {error}"),
            )
        })?;
        bytes.push(b'\n');
        self.writes.send(bytes).map_err(|_| self.send_failure())
    }

    /// Best-effort `notifications/cancelled` for a request this client gave up
    /// on, so the server stops working on it instead of running to completion.
    ///
    /// Queueing is a non-blocking push into the writer's queue, so the cancel
    /// path never waits on the server; a dead pipe is ignored.
    fn notify_cancelled(&self, id: &JsonRpcId, reason: &str) {
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": id, "reason": reason },
        });
        let _ = self.submit_frame(&frame);
    }

    /// Failure handed to a caller whose frame could not be queued.
    fn send_failure(&self) -> McpError {
        self.write_failure.lock().clone().unwrap_or_else(|| {
            McpError::new(
                &self.settings.name,
                TransportKind::Stdio,
                McpFailureStage::Send,
                McpFailureClass::Eof,
                true,
                "MCP subprocess stdin is closed",
            )
        })
    }

    /// Tear the transport down after a fatal I/O failure.
    async fn fail(&self, error: McpError) {
        self.shutdown(Some(error));
        self.terminate().await;
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // A transport dropped without `close()` must not leak the child tree:
        // `close()` is asynchronous and cannot run here, so the group is swept
        // synchronously. `connected` is still true only when no teardown ran,
        // which is exactly the case this path exists for - and a launcher can
        // have exited while a grandchild kept the group alive, so the leader's
        // own exit says nothing about the group. SIGTERM first gives a
        // well-behaved server the chance to exit on its own; the SIGKILL right
        // behind it is the guarantee, because `Drop` has no budget to wait out
        // the grace window `terminate()` would allow.
        if self.pid > 0 && self.connected.load(Ordering::SeqCst) {
            self.signal_term();
            self.signal_kill();
        }
        // Collect a child that already exited, so it is not left a zombie; a
        // freshly killed one is reaped by the runtime's process driver.
        let _ = self.try_exit();
    }
}

impl McpTransport for StdioTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Stdio
    }

    fn endpoint_label(&self) -> String {
        self.settings.endpoint_label()
    }

    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Option<Value>,
        cancel: Option<Arc<CancelListener>>,
    ) -> BoxFuture<'a, Result<Value, McpError>> {
        Box::pin(async move {
            if !self.is_connected() {
                return Err(McpError::closed(&self.settings.name, TransportKind::Stdio));
            }
            if let Some(cancel) = &cancel {
                if cancel.is_cancelled() {
                    return Err(McpError::cancelled(
                        &self.settings.name,
                        TransportKind::Stdio,
                    ));
                }
            }

            let id = self.ids.next();
            let frame = jsonrpc::encode_request(&id, method, params.as_ref());
            let (settle, mut response) = oneshot::channel();
            // Register before writing: a server that answers immediately must
            // find its pending entry.
            {
                let mut pending = self.pending.lock();
                if pending.len() >= MAX_PENDING_REQUESTS {
                    return Err(McpError::new(
                        &self.settings.name,
                        TransportKind::Stdio,
                        McpFailureStage::Send,
                        McpFailureClass::Protocol,
                        false,
                        format!("more than {MAX_PENDING_REQUESTS} requests are already in flight"),
                    ));
                }
                pending.insert(id.clone(), settle);
            }
            // The guard removes the entry even when this future is dropped
            // before it settles, which a plain removal after the select below
            // cannot cover.
            let _pending = PendingGuard {
                transport: self,
                id: id.clone(),
            };
            self.submit_frame(&frame)?;

            let timeout = self.settings.effective_timeout();
            let outcome = match &cancel {
                Some(cancel) => tokio::select! {
                    settled = &mut response => RequestOutcome::Settled(settled),
                    () = deadline(timeout) => RequestOutcome::Timeout,
                    () = cancel.cancelled() => RequestOutcome::Cancelled,
                },
                None => tokio::select! {
                    settled = &mut response => RequestOutcome::Settled(settled),
                    () = deadline(timeout) => RequestOutcome::Timeout,
                },
            };

            match outcome {
                RequestOutcome::Settled(Ok(Ok(value))) => Ok(value),
                RequestOutcome::Settled(Ok(Err(error))) => Err(error),
                // The transport dropped the settling end without answering:
                // report the loss instead of hanging.
                RequestOutcome::Settled(Err(_)) => {
                    Err(McpError::closed(&self.settings.name, TransportKind::Stdio))
                }
                RequestOutcome::Timeout => {
                    self.notify_cancelled(&id, "the request deadline expired");
                    Err(McpError::timeout(
                        &self.settings.name,
                        TransportKind::Stdio,
                        timeout.unwrap_or_default(),
                    ))
                }
                RequestOutcome::Cancelled => {
                    self.notify_cancelled(&id, "the caller cancelled the request");
                    Err(McpError::cancelled(
                        &self.settings.name,
                        TransportKind::Stdio,
                    ))
                }
            }
        })
    }

    fn notify<'a>(
        &'a self,
        method: &'a str,
        params: Option<Value>,
    ) -> BoxFuture<'a, Result<(), McpError>> {
        Box::pin(async move {
            let frame = jsonrpc::encode_notification(method, params.as_ref());
            self.submit_frame(&frame)
        })
    }

    fn respond<'a>(
        &'a self,
        id: JsonRpcId,
        result: Result<Value, JsonRpcError>,
    ) -> BoxFuture<'a, Result<(), McpError>> {
        Box::pin(async move {
            let frame = jsonrpc::encode_response(&id, result.as_ref());
            self.submit_frame(&frame)
        })
    }

    fn set_protocol_version(&self, _version: String) {
        // JSONL framing carries no protocol header; the version only matters
        // for the handshake payload the client layer builds.
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.shutdown(None);
            self.terminate().await;
        })
    }
}

/// Removes one pending request entry when its future is dropped.
///
/// The tool layer cancels a call by dropping its future, so the removal cannot
/// be left to the code after the request select: a dropped future would leak
/// the entry (and its `oneshot` sender) for the life of the transport.
struct PendingGuard<'a> {
    transport: &'a StdioTransport,
    id: JsonRpcId,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.transport.pending.lock().remove(&self.id);
    }
}

/// Race outcome of one request: the response (or its failure), its deadline or
/// the caller's cancellation.
enum RequestOutcome {
    Settled(Result<Result<Value, McpError>, oneshot::error::RecvError>),
    Timeout,
    Cancelled,
}

/// Request deadline. `None` means the config disabled the client-side deadline,
/// so this future never resolves.
async fn deadline(timeout: Option<Duration>) {
    match timeout {
        Some(timeout) => tokio::time::sleep(timeout).await,
        None => std::future::pending().await,
    }
}

/// Strip the frame terminator. Servers using `\r\n` are tolerated.
fn trim_frame(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &line[..end]
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exited with code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("was terminated by signal {signal}");
        }
    }
    "exited without a status".to_string()
}

/// Run the bounded process teardown on its own task.
///
/// `terminate()` aborts the writer task, so a failure detected by that writer
/// must hand the teardown to a task that outlives it.
fn spawn_teardown(transport: &Arc<StdioTransport>) {
    let transport = transport.clone();
    tokio::spawn(async move { transport.terminate().await });
}

/// Drain the frame queue into the child's stdin.
///
/// The queue preserves frame order, and the awaiting half of the write happens
/// here rather than in `request`: a subprocess that stops reading would
/// otherwise park the caller inside a write that never completes. A write
/// failure is fatal - the pipe is gone - so the transport tears down.
async fn writer_loop(
    mut stdin: ChildStdin,
    mut frames: mpsc::UnboundedReceiver<Vec<u8>>,
    transport: Weak<StdioTransport>,
) {
    while let Some(frame) = frames.recv().await {
        if let Err(error) = stdin.write_all(&frame).await {
            let Some(transport) = transport.upgrade() else {
                break;
            };
            *transport.write_failure.lock() = Some(McpError::transport_error(
                &transport.settings.name,
                TransportKind::Stdio,
                McpFailureStage::Send,
                &error,
            ));
            let failure = McpError::eof(
                &transport.settings.name,
                TransportKind::Stdio,
                format!("failed to write to MCP subprocess stdin: {error}"),
            );
            transport.shutdown(Some(failure));
            // The teardown runs on its own task: it aborts this writer, so it
            // must not share the writer's lifecycle or the SIGKILL escalation
            // would never be reached.
            spawn_teardown(&transport);
            break;
        }
    }
    // Dropping the handle closes the child's stdin pipe.
}

/// Split the child's stdout into frames and route them.
///
/// A parse failure or a frame past [`MAX_FRAME_BYTES`] is fatal: the stream can
/// no longer be trusted to stay frame-aligned, so every pending request is
/// rejected and the child is reaped.
async fn read_loop(transport: Weak<StdioTransport>, stdout: ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut buffer: Vec<u8> = Vec::new();
    let error = loop {
        buffer.clear();
        // The transport is only upgraded after the read: holding a strong
        // handle across the (potentially long) read would keep the child alive
        // for as long as the loop waits on it.
        let frame = read_frame(&mut reader, &mut buffer).await;
        let Some(transport) = transport.upgrade() else {
            return;
        };
        match frame {
            Ok(FrameRead::Eof) => break transport.stream_end_error().await,
            Ok(FrameRead::Frame) => match transport.decode_frame(&buffer) {
                Ok(None) => continue,
                Ok(Some(value)) => transport.dispatch(value),
                Err(error) => break error,
            },
            Ok(FrameRead::Oversized) => {
                break McpError::new(
                    &transport.settings.name,
                    TransportKind::Stdio,
                    McpFailureStage::Decode,
                    McpFailureClass::MalformedResponse,
                    false,
                    format!(
                        "JSON-RPC frame exceeded the {MAX_FRAME_BYTES}-byte cap without a newline"
                    ),
                )
            }
            Err(error) => {
                break McpError::transport_error(
                    &transport.settings.name,
                    TransportKind::Stdio,
                    McpFailureStage::Receive,
                    &error,
                )
            }
        }
    };

    let Some(transport) = transport.upgrade() else {
        return;
    };
    transport.fail(error).await;
}

/// One frame read from the child's stdout.
enum FrameRead {
    /// A complete line, terminator included.
    Frame,
    /// The stream ended.
    Eof,
    /// The line passed [`MAX_FRAME_BYTES`] without a newline.
    Oversized,
}

/// Read one newline-terminated frame, refusing to buffer more than
/// [`MAX_FRAME_BYTES`] bytes.
///
/// `read_until` on its own would grow `buffer` until a newline arrives, so the
/// read is capped: a server that never terminates its frame fails at the cap
/// instead of exhausting memory. The cap counts frame bytes, without the
/// terminator. A short read that is not newline-terminated is not the cap: it is
/// a final frame the server sent before closing, which the decoder handles.
async fn read_frame(
    reader: &mut BufReader<ChildStdout>,
    buffer: &mut Vec<u8>,
) -> io::Result<FrameRead> {
    let limit = MAX_FRAME_BYTES as u64 + 1;
    let mut limited = reader.take(limit);
    let read = limited.read_until(b'\n', buffer).await?;
    if read == 0 {
        return Ok(FrameRead::Eof);
    }
    if buffer.last() != Some(&b'\n') && read as u64 == limit {
        return Ok(FrameRead::Oversized);
    }
    Ok(FrameRead::Frame)
}

/// Read the server's diagnostics and log them at debug level. MCP reserves
/// stderr for logging, so a line here is never an error by itself.
async fn stderr_loop(transport: Weak<StdioTransport>, stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let Some(transport) = transport.upgrade() else {
                    return;
                };
                let text = String::from_utf8_lossy(&buffer);
                let sanitized = crate::redact::sanitize_diagnostic_text(text.trim_end());
                let (line, _) = crate::types::truncate_utf8(&sanitized, MAX_STDERR_LOG_BYTES);
                if !line.is_empty() {
                    tracing::debug!(server = %transport.settings.name, "mcp stdio stderr: {line}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::types::{SideEffects, TrustLevel, DEFAULT_MAX_OUTPUT_BYTES};

    /// A `/bin/sh` echo server: answers the id of every frame that carries one
    /// and ignores notifications, so the same fixture serves both directions.
    const ECHO_SERVER: &str = r#"while IFS= read -r line; do
  case $line in
    *'"id":'*)
      id=${line#*\"id\":}
      id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"echo":true}}\n' "$id"
      ;;
  esac
done"#;

    fn settings(args: &[&str], timeout: Duration) -> ServerSettings {
        ServerSettings {
            name: "fixture".to_string(),
            transport: TransportKind::Stdio,
            enabled: true,
            command: Some("/bin/sh".to_string()),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            request_timeout: Some(timeout),
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Unknown,
            tool_trust: BTreeMap::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// Spawn a real child process behind the transport.
    fn start_fixture(
        script: &str,
    ) -> (
        Arc<dyn McpTransport>,
        mpsc::UnboundedReceiver<TransportEvent>,
    ) {
        start_fixture_with_timeout(script, Duration::from_secs(5))
    }

    /// Spawn a real child process behind the transport with an explicit
    /// per-request deadline.
    fn start_fixture_with_timeout(
        script: &str,
        timeout: Duration,
    ) -> (
        Arc<dyn McpTransport>,
        mpsc::UnboundedReceiver<TransportEvent>,
    ) {
        let (events, receiver) = mpsc::unbounded_channel();
        let transport = create_stdio_transport(&settings(&["-c", script], timeout), events)
            .expect("the fixture must spawn");
        (transport, receiver)
    }

    #[tokio::test]
    async fn jsonl_frames_round_trip_through_a_child_process() {
        let (transport, mut events) = start_fixture(ECHO_SERVER);

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            transport.request("ping", Some(json!({"n": 1})), None),
        )
        .await
        .expect("the request must not hang")
        .expect("the request must succeed");
        assert_eq!(result, json!({"echo": true}));

        // A notification has no id to answer, but it must reach the same pipe.
        transport
            .notify("notifications/initialized", None)
            .await
            .expect("notify");
        assert!(transport.is_connected());
        // The label is the argv, shortened for status output.
        assert!(transport.endpoint_label().starts_with("/bin/sh -c "));

        transport.close().await;
        assert!(!transport.is_connected());
        match events.try_recv() {
            Ok(TransportEvent::Closed { error }) => {
                assert!(error.is_none(), "unexpected error: {error:?}")
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(events.try_recv().is_err(), "`Closed` must be emitted once");

        // `close()` is idempotent and stays idempotent on a dead child.
        transport.close().await;
    }

    #[tokio::test]
    async fn child_exit_is_reported_as_eof_with_the_exit_code() {
        let (transport, _events) = start_fixture("read -r line; exit 7");

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            transport.request("ping", None, None),
        )
        .await
        .expect("the request must not hang")
        .expect_err("a dead subprocess must fail the request");
        assert_eq!(error.failure, McpFailureClass::Eof);
        assert_eq!(error.stage, McpFailureStage::Receive);
        assert!(error.retryable);
        assert!(
            error.message.contains("exited with code 7"),
            "unexpected message: {}",
            error.message
        );
        assert!(!transport.is_connected());

        // Later sends fail instead of silently dropping frames on the floor.
        let send = transport
            .notify("notifications/initialized", None)
            .await
            .expect_err("notify must fail on a dead pipe");
        assert_eq!(send.stage, McpFailureStage::Send);

        transport.close().await;
    }

    #[tokio::test]
    async fn malformed_frame_fails_the_connection() {
        let (transport, mut events) = start_fixture("read -r line; printf 'not json\\n'; sleep 30");

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            transport.request("ping", None, None),
        )
        .await
        .expect("the request must not hang")
        .expect_err("a malformed frame must fail the request");
        assert_eq!(error.failure, McpFailureClass::MalformedResponse);
        assert_eq!(error.stage, McpFailureStage::Decode);
        assert!(!error.retryable);

        let closed = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("the closed event must arrive")
            .expect("the channel must stay open");
        match closed {
            TransportEvent::Closed { error: Some(error) } => {
                assert_eq!(error.failure, McpFailureClass::MalformedResponse)
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(!transport.is_connected());

        transport.close().await;
    }

    #[tokio::test]
    async fn server_notifications_and_requests_are_forwarded() {
        let script = r#"printf '\n{"jsonrpc":"2.0","id":"srv-1","method":"roots/list"}\n{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'; while read -r line; do :; done"#;
        let (transport, mut events) = start_fixture(script);

        // The leading blank line is ignored, not fatal.
        let first = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event must arrive")
            .expect("the channel must stay open");
        match first {
            TransportEvent::Request { id, method, params } => {
                assert_eq!(id, JsonRpcId::String("srv-1".to_string()));
                assert_eq!(method, "roots/list");
                assert_eq!(params, Value::Null);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let second = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event must arrive")
            .expect("the channel must stay open");
        match second {
            TransportEvent::Notification { method, params } => {
                assert_eq!(method, "notifications/tools/list_changed");
                assert_eq!(params, Value::Null);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(transport.is_connected());

        // Answering a server request writes a frame the child drains.
        transport
            .respond(
                JsonRpcId::String("srv-1".to_string()),
                Ok(json!({"roots": []})),
            )
            .await
            .expect("respond");

        transport.close().await;
    }

    #[tokio::test]
    async fn a_request_after_close_fails_instead_of_hanging() {
        let (transport, _events) = start_fixture(ECHO_SERVER);
        transport.close().await;

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            transport.request("ping", None, None),
        )
        .await
        .expect("the request must not hang")
        .expect_err("a closed transport must refuse requests");
        assert_eq!(error.failure, McpFailureClass::Closed);
        assert_eq!(error.stage, McpFailureStage::Send);
    }

    #[tokio::test]
    async fn a_request_never_blocks_on_a_full_stdin_pipe() {
        // The child never reads stdin, so a frame bigger than the pipe buffer
        // can only be parked inside the writer task. The request must still
        // settle on its own deadline instead of hanging on the write.
        let (transport, _events) =
            start_fixture_with_timeout("sleep 30", Duration::from_millis(300));
        let oversized = json!({"blob": "x".repeat(256 * 1024)});

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            transport.request("tools/call", Some(oversized), None),
        )
        .await
        .expect("the request must not hang on the write")
        .expect_err("the request must fail");
        assert_eq!(error.failure, McpFailureClass::Timeout);

        transport.close().await;
    }

    #[tokio::test]
    async fn cancellation_settles_a_pending_request() {
        // A server that never answers: only the caller's token can settle the
        // request.
        let (transport, _events) = start_fixture("while read -r line; do :; done");
        let token = Arc::new(aish_llm::CancellationToken::new());
        let cancel = CancelListener::new(token.clone());
        let listener = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            transport.request("ping", None, Some(listener)),
        )
        .await
        .expect("the request must not hang")
        .expect_err("a cancelled request must fail");
        assert_eq!(error.failure, McpFailureClass::Cancelled);

        // The pending entry is gone, so a late response cannot settle it twice.
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            transport.request("ping", None, Some(cancel.clone())),
        )
        .await
        .expect("the request must not hang")
        .expect_err("the pre-cancelled token must fail immediately");
        assert_eq!(error.failure, McpFailureClass::Cancelled);

        transport.close().await;
    }

    /// Wait until `check` produces a value.
    async fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(value) = check() {
                return value;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for the fixture"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// True while `pid` still names a live process. Signal 0 performs the
    /// existence check without sending anything.
    #[cfg(unix)]
    fn pid_is_alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    #[tokio::test]
    async fn an_oversized_frame_fails_the_connection() {
        // Nine megabytes with no newline: more than one frame may carry.
        let (transport, mut events) = start_fixture("head -c 9000000 /dev/zero | tr '\\0' 'x'");

        let error = tokio::time::timeout(
            Duration::from_secs(20),
            transport.request("ping", None, None),
        )
        .await
        .expect("the request must not hang")
        .expect_err("an endless frame must fail the request");
        assert_eq!(error.failure, McpFailureClass::MalformedResponse);
        assert_eq!(error.stage, McpFailureStage::Decode);
        assert!(!error.retryable);
        assert!(
            error.message.contains("cap without a newline"),
            "unexpected message: {}",
            error.message
        );

        // The connection is torn down with the frame, child included.
        let closed = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("the closed event must arrive")
            .expect("the channel must stay open");
        match closed {
            TransportEvent::Closed { error: Some(error) } => {
                assert_eq!(error.failure, McpFailureClass::MalformedResponse)
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(!transport.is_connected());

        transport.close().await;
    }

    #[tokio::test]
    async fn a_dropped_request_leaves_no_pending_entry() {
        // Fill the map to its cap with futures that are dropped before they
        // settle, the way the tool layer drops a cancelled call.
        let (transport, _events) = start_fixture(ECHO_SERVER);
        let dropped: Vec<_> = (0..MAX_PENDING_REQUESTS)
            .map(|_| Box::pin(transport.request("ping", None, None)))
            .collect();
        for mut pending in dropped {
            assert!(
                futures::FutureExt::now_or_never(pending.as_mut()).is_none(),
                "a server that has not answered yet must keep the request pending"
            );
        }

        // With the entries removed on drop, the map is empty again and a fresh
        // request is accepted (a leaked entry would hit the cap instead).
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            transport.request("ping", None, None),
        )
        .await
        .expect("the request must not hang")
        .expect("a dropped request must not consume a pending slot");
        assert_eq!(result, json!({"echo": true}));

        transport.close().await;
    }

    #[tokio::test]
    async fn the_pending_map_caps_in_flight_requests() {
        // A server that never answers keeps every request pending.
        let (transport, _events) =
            start_fixture_with_timeout("while read -r line; do :; done", Duration::from_secs(30));
        let mut pending: Vec<_> = (0..MAX_PENDING_REQUESTS)
            .map(|_| Box::pin(transport.request("ping", None, None)))
            .collect();
        for request in &mut pending {
            assert!(
                futures::FutureExt::now_or_never(request.as_mut()).is_none(),
                "the server never answers"
            );
        }

        let mut over_the_cap = Box::pin(transport.request("ping", None, None));
        let error = futures::FutureExt::now_or_never(over_the_cap.as_mut())
            .expect("the request past the cap must settle immediately")
            .expect_err("the map must refuse more than the cap allows");
        assert_eq!(error.failure, McpFailureClass::Protocol);
        assert_eq!(error.stage, McpFailureStage::Send);
        assert!(
            error.message.contains(&MAX_PENDING_REQUESTS.to_string()),
            "unexpected message: {}",
            error.message
        );
    }

    #[tokio::test]
    async fn giving_up_notifies_the_server() {
        // A server that records every frame it receives and never answers.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("frames.jsonl");
        let recorder = format!(
            "while IFS= read -r line; do printf '%s\\n' \"$line\" >> \"{}\"; done",
            log.display()
        );
        let (transport, _events) =
            start_fixture_with_timeout(&recorder, Duration::from_millis(300));

        // Cancellation: the caller's token wins before the deadline.
        let token = Arc::new(aish_llm::CancellationToken::new());
        let listener = CancelListener::new(token.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            transport.request("ping", None, Some(listener)),
        )
        .await
        .expect("the request must not hang")
        .expect_err("a cancelled request must fail");
        assert_eq!(error.failure, McpFailureClass::Cancelled);

        // Deadline: the second request is left to time out.
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            transport.request("ping", None, None),
        )
        .await
        .expect("the request must not hang")
        .expect_err("a silent server must hit the deadline");
        assert_eq!(error.failure, McpFailureClass::Timeout);

        // Both gave-up requests are announced so the server can stop working.
        let frames: Vec<Value> = wait_for(|| {
            let text = std::fs::read_to_string(&log).ok()?;
            let frames: Vec<Value> = text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            (frames.len() >= 4).then_some(frames)
        })
        .await;
        let cancellations: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["method"] == json!("notifications/cancelled"))
            .collect();
        assert_eq!(
            cancellations.len(),
            2,
            "expected one notice per abandoned request: {frames:?}"
        );
        assert_eq!(cancellations[0]["params"]["requestId"], json!(1));
        assert_eq!(cancellations[1]["params"]["requestId"], json!(2));
        assert!(
            cancellations[0]["params"]["reason"].is_string(),
            "the notice must carry a reason: {:?}",
            cancellations[0]
        );

        transport.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_sweeps_a_grandchild_of_a_launcher_that_exited() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("grandchild.pid");
        // A launcher that starts the real worker and exits: the session leader
        // is reaped while the worker keeps its process group alive.
        let script = format!(
            "sleep 30 & printf '%s\\n' \"$!\" > \"{}\"",
            pid_file.display()
        );
        let (transport, _events) = start_fixture(&script);

        let pid: i32 = wait_for(|| {
            std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
        })
        .await;
        assert!(pid_is_alive(pid), "the worker must be running");

        transport.close().await;

        wait_for(|| (!pid_is_alive(pid)).then_some(())).await;
    }
}
