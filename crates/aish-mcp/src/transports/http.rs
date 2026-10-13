//! MCP Streamable HTTP transport.
//!
//! One JSON-RPC frame per HTTP request: every call is an independent `POST` to
//! the configured endpoint (no long-lived connection is held), the response is
//! either a JSON body or a `text/event-stream` body parsed with
//! [`crate::sse::SseDecoder`], and server-initiated traffic observed on either
//! body is forwarded as a [`TransportEvent`]. Header handling follows the
//! reference implementation
//! (oh-my-pi `packages/coding-agent/src/mcp/transports/http.ts` +
//! `transports/header-policy.ts`):
//!
//! - client-generated headers (`Content-Type`, `Accept`, `Mcp-Session-Id`,
//!   `MCP-Protocol-Version`) win case-insensitively over configured ones;
//! - `MCP-Protocol-Version` is transport-owned: configured copies are stripped
//!   and the header only appears once `initialize` negotiated a revision (the
//!   `initialize` request itself must not carry it);
//! - an `Mcp-Session-Id` seen on any response is echoed on later requests and
//!   on the best-effort session `DELETE` issued by `close()`.
//!
//! Deliberate simplification: this transport does NOT open the optional
//! background `GET` SSE listener of the Streamable HTTP spec. Server-initiated
//! notifications and requests are therefore only observed while a POST
//! response body is being drained (a request's SSE stream, or the reply to a
//! notification); a server that speaks exclusively over the standalone GET
//! stream looks silent to this client.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use crate::cancel::CancelListener;
use crate::config::ServerSettings;
use crate::redact::{redact_url_for_log, sanitize_diagnostic_text};
use crate::sse::{SseDecoder, SseEvent};
use crate::types::{
    classify_frames, IncomingFrame, JsonRpcError, JsonRpcId, JsonRpcResponse, McpError,
    McpFailureClass, McpFailureStage, McpTransport, TransportEvent, TransportKind,
};

/// Redirect hops followed for the endpoint.
const MAX_REDIRECTS: usize = 5;
/// Cap on the best-effort session `DELETE` issued by `close()` when the server
/// sets no client-side deadline.
const CLOSE_DELETE_TIMEOUT: Duration = Duration::from_secs(2);
/// Cap on one response body. A JSON body is buffered whole and an SSE body is
/// read until it carries the answer, so a server that streams forever must fail
/// the request instead of growing the buffer until memory runs out.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Cap on the best-effort `notifications/cancelled` POST: it must never delay
/// the cancellation the caller is waiting for.
const CANCEL_NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";

/// Build a Streamable HTTP transport for one server.
///
/// The transport starts connected: HTTP keeps no session to establish, so the
/// first `initialize` POST is itself the connection.
pub fn create_http_transport(
    settings: &ServerSettings,
    events: UnboundedSender<TransportEvent>,
) -> Result<Arc<dyn McpTransport>, McpError> {
    let Some(url) = settings.url.clone() else {
        return Err(McpError::new(
            &settings.name,
            TransportKind::Http,
            McpFailureStage::Connect,
            McpFailureClass::Connect,
            false,
            "MCP HTTP server has no url configured",
        ));
    };
    let client = reqwest::Client::builder()
        // Per-request deadlines are enforced by the transport itself, so the
        // client must not impose a global timeout on long-poll shaped calls.
        // Redirects are restricted to the configured origin: reqwest strips only
        // a fixed set of credential headers on a cross-host hop, so every
        // configured header (an API key, the echoed session id) and - on a
        // 307/308 - the whole JSON-RPC body would be re-sent to whatever host
        // the server points at next. A cross-origin hop is refused instead.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                return attempt.error(TooManyRedirects);
            }
            match attempt.previous().first() {
                Some(origin) if same_origin(origin, attempt.url()) => attempt.follow(),
                _ => attempt.stop(),
            }
        }))
        .build()
        .map_err(|error| {
            McpError::transport_error(
                &settings.name,
                TransportKind::Http,
                McpFailureStage::Connect,
                &ChainedError::new(&error),
            )
        })?;
    // `MCP-Protocol-Version` belongs to the transport: a configured copy could
    // pin a revision the server never negotiated.
    let configured_headers = settings
        .headers
        .iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let (shutdown_tx, _) = watch::channel(false);
    Ok(Arc::new(HttpTransport {
        inner: Arc::new(Inner {
            server: settings.name.clone(),
            url,
            configured_headers,
            client,
            timeout: settings.effective_timeout(),
            events,
            session_id: parking_lot::Mutex::new(None),
            protocol_version: parking_lot::Mutex::new(None),
            connected: AtomicBool::new(true),
            next_request_id: AtomicU64::new(1),
            shutdown_tx,
        }),
    }))
}

/// Shared, internally cloned state of one HTTP transport.
struct Inner {
    server: String,
    url: String,
    /// Configured headers with `MCP-Protocol-Version` already stripped.
    configured_headers: Vec<(String, String)>,
    client: reqwest::Client,
    timeout: Option<Duration>,
    events: UnboundedSender<TransportEvent>,
    session_id: parking_lot::Mutex<Option<String>>,
    protocol_version: parking_lot::Mutex<Option<String>>,
    connected: AtomicBool,
    next_request_id: AtomicU64,
    /// Flipped once by `close()`; every bounded operation races it, which is
    /// how in-flight requests are abandoned.
    shutdown_tx: watch::Sender<bool>,
}

struct HttpTransport {
    inner: Arc<Inner>,
}

impl Inner {
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Headers every request starts from: configured entries plus the
    /// negotiated protocol revision.
    fn base_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &self.configured_headers {
            insert_header(&mut headers, name, value);
        }
        if let Some(version) = self.protocol_version.lock().clone() {
            insert_header(&mut headers, PROTOCOL_VERSION_HEADER, &version);
        }
        headers
    }

    /// Headers of a JSON-RPC POST. Generated values replace configured ones
    /// with the same case-insensitive name.
    fn post_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = self.base_headers();
        insert_header(&mut headers, "Content-Type", "application/json");
        insert_header(
            &mut headers,
            "Accept",
            "application/json, text/event-stream",
        );
        if let Some(session) = self.session_id.lock().clone() {
            insert_header(&mut headers, SESSION_ID_HEADER, &session);
        }
        headers
    }

    /// Remember an `Mcp-Session-Id` handed out by the server.
    fn remember_session(&self, response: &reqwest::Response) {
        let Some(value) = response.headers().get(SESSION_ID_HEADER) else {
            return;
        };
        let Ok(session) = value.to_str() else {
            return;
        };
        if session.is_empty() {
            return;
        }
        *self.session_id.lock() = Some(session.to_string());
    }

    /// Normalize an HTTP client, connection or body-read failure into the
    /// failure taxonomy, keeping the cause chain in the message.
    fn transport_failure(&self, stage: McpFailureStage, error: &dyn std::error::Error) -> McpError {
        McpError::transport_error(
            &self.server,
            TransportKind::Http,
            stage,
            &ChainedError::new(error),
        )
    }

    /// POST one payload, learning the session id and rejecting non-2xx statuses.
    async fn send_post(&self, payload: &str) -> Result<reqwest::Response, McpError> {
        let request = self
            .client
            .post(&self.url)
            .headers(self.post_headers())
            .body(payload.to_string());
        let response = request
            .send()
            .await
            .map_err(|error| self.transport_failure(McpFailureStage::Connect, &error))?;
        self.remember_session(&response);
        let status = response.status();
        if !status.is_success() {
            let body = self.read_body(response).await?;
            let code = status.as_u16();
            return Err(McpError::new(
                &self.server,
                TransportKind::Http,
                McpFailureStage::Receive,
                McpFailureClass::HttpStatus,
                matches!(code, 404 | 502 | 503),
                format!(
                    "HTTP {code}: {}",
                    sanitize_diagnostic_text(&String::from_utf8_lossy(&body))
                ),
            ));
        }
        Ok(response)
    }

    /// Read a response body, refusing one past [`MAX_RESPONSE_BYTES`].
    ///
    /// `Response::bytes()` would buffer whatever the server sends; a server
    /// that streams an endless answer must fail the request instead.
    async fn read_body(&self, mut response: reqwest::Response) -> Result<Vec<u8>, McpError> {
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(self.oversized_body());
        }
        let mut body: Vec<u8> = Vec::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|error| self.transport_failure(McpFailureStage::Receive, &error))?;
            let Some(chunk) = chunk else {
                return Ok(body);
            };
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(self.oversized_body());
            }
            body.extend_from_slice(&chunk);
        }
    }

    /// Failure reported for a response body past [`MAX_RESPONSE_BYTES`].
    fn oversized_body(&self) -> McpError {
        McpError::new(
            &self.server,
            TransportKind::Http,
            McpFailureStage::Receive,
            McpFailureClass::MalformedResponse,
            false,
            format!("HTTP response body exceeded the {MAX_RESPONSE_BYTES}-byte cap"),
        )
    }

    /// Send a request and resolve its JSON-RPC result.
    async fn post_request(&self, payload: &str, expected: u64) -> Result<Value, McpError> {
        let response = self.send_post(payload).await?;
        if is_event_stream(&response) {
            self.read_sse_response(response, expected).await
        } else {
            self.read_json_response(response, expected).await
        }
    }

    /// JSON response path: one POST carries one response envelope, which the
    /// reference implementation does not correlate against the request id.
    async fn read_json_response(
        &self,
        response: reqwest::Response,
        expected: u64,
    ) -> Result<Value, McpError> {
        let body = self.read_body(response).await?;
        let value: Value = serde_json::from_slice(&body).map_err(|error| {
            McpError::new(
                &self.server,
                TransportKind::Http,
                McpFailureStage::Decode,
                McpFailureClass::MalformedResponse,
                false,
                format!("Malformed JSON-RPC response: {error}"),
            )
        })?;
        let mut result = None;
        for frame in classify_frames(&value) {
            match frame {
                IncomingFrame::Response(frame) => {
                    if result.is_none() {
                        result = Some(response_result(&self.server, frame)?);
                    }
                }
                other => self.forward_frame(other),
            }
        }
        result.ok_or_else(|| {
            McpError::new(
                &self.server,
                TransportKind::Http,
                McpFailureStage::Decode,
                McpFailureClass::MalformedResponse,
                false,
                format!("Malformed JSON-RPC response for request ID {expected}"),
            )
        })
    }

    /// SSE response path: frames arrive until the one answering `expected`
    /// shows up; every other frame is forwarded to the owner.
    async fn read_sse_response(
        &self,
        response: reqwest::Response,
        expected: u64,
    ) -> Result<Value, McpError> {
        let mut decoder = SseDecoder::new();
        let mut stream = response.bytes_stream();
        let mut total = 0usize;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| self.transport_failure(McpFailureStage::Receive, &error))?;
            total += chunk.len();
            if total > MAX_RESPONSE_BYTES {
                return Err(self.oversized_body());
            }
            for event in decoder.push(&chunk) {
                if let Some(result) = self.consume_event(&event, Some(expected))? {
                    return Ok(result);
                }
            }
        }
        for event in decoder.finish() {
            if let Some(result) = self.consume_event(&event, Some(expected))? {
                return Ok(result);
            }
        }
        // The server accepted the request and then closed the stream without
        // answering it; the request must not be replayed.
        Err(McpError::new(
            &self.server,
            TransportKind::Http,
            McpFailureStage::Receive,
            McpFailureClass::MalformedResponse,
            false,
            format!("No response received for request ID {expected}"),
        ))
    }

    /// Classify one SSE event. Returns the request result when the event
    /// carries the response being awaited.
    fn consume_event(
        &self,
        event: &SseEvent,
        expected: Option<u64>,
    ) -> Result<Option<Value>, McpError> {
        // Control frames (comments, `retry:`/`id:` bookkeeping) carry no data.
        if event.data.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&event.data).map_err(|error| {
            McpError::new(
                &self.server,
                TransportKind::Http,
                McpFailureStage::Decode,
                McpFailureClass::MalformedResponse,
                false,
                format!("Malformed SSE payload: {error}"),
            )
        })?;
        for frame in classify_frames(&value) {
            match frame {
                IncomingFrame::Response(frame) => {
                    if expected.is_some_and(|expected| answers(&frame.id, expected)) {
                        return response_result(&self.server, frame).map(Some);
                    }
                }
                other => self.forward_frame(other),
            }
        }
        Ok(None)
    }

    /// Hand a non-response frame to the transport owner. Responses with an
    /// unrelated id are dropped: only the request that awaits one can use it.
    fn forward_frame(&self, frame: IncomingFrame) {
        if !self.is_connected() {
            return;
        }
        let event = match frame {
            IncomingFrame::Notification { method, params } => {
                TransportEvent::Notification { method, params }
            }
            IncomingFrame::Request { id, method, params } => {
                TransportEvent::Request { id, method, params }
            }
            IncomingFrame::Response(_) => return,
        };
        let _ = self.events.send(event);
    }

    /// POST a notification or a server-request response. The server answers
    /// with 2xx (often 202) and no JSON-RPC frame of its own.
    async fn post_without_response(self: &Arc<Self>, payload: &str) -> Result<(), McpError> {
        let response = self.send_post(payload).await?;
        if is_event_stream(&response) {
            // The server may piggyback server-to-client traffic on this body;
            // its drain must not delay the caller.
            let inner = Arc::clone(self);
            tokio::spawn(async move { inner.drain_sse(response).await });
        }
        Ok(())
    }

    /// Drain a fire-and-forget SSE body until the server closes it or the
    /// transport shuts down, forwarding every frame it carries.
    async fn drain_sse(self: &Arc<Self>, response: reqwest::Response) {
        let mut decoder = SseDecoder::new();
        let mut stream = response.bytes_stream();
        let mut shutdown = self.shutdown_tx.subscribe();
        loop {
            let chunk = tokio::select! {
                _ = shutdown.changed() => return,
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else {
                break;
            };
            let Ok(chunk) = chunk else {
                return;
            };
            for event in decoder.push(&chunk) {
                let _ = self.consume_event(&event, None);
            }
            if decoder.overflowed() {
                // A peer that never terminates a line must not keep this
                // background drain alive: the stream is malformed, so stop
                // reading it.
                return;
            }
        }
        for event in decoder.finish() {
            let _ = self.consume_event(&event, None);
        }
    }

    /// Race `work` against the per-request deadline, caller cancellation and
    /// transport shutdown.
    async fn bounded<T, F>(
        &self,
        cancel: Option<Arc<CancelListener>>,
        work: F,
    ) -> Result<T, McpError>
    where
        F: Future<Output = Result<T, McpError>>,
    {
        let mut shutdown = self.shutdown_tx.subscribe();
        tokio::pin!(work);
        tokio::select! {
            result = &mut work => result,
            _ = shutdown.changed() => Err(McpError::closed(&self.server, TransportKind::Http)),
            _ = wait_for_cancel(cancel) => {
                Err(McpError::cancelled(&self.server, TransportKind::Http))
            }
            limit = wait_for_deadline(self.timeout) => {
                Err(McpError::timeout(&self.server, TransportKind::Http, limit))
            }
        }
    }

    /// Best-effort `notifications/cancelled` for a request the client gave up on.
    ///
    /// A cancellation or a deadline only stopped *this* client from waiting; the
    /// server keeps working on the tool call until it is told otherwise. The
    /// POST runs on its own task so the cancel path never waits on the network,
    /// and it is bounded so the task cannot outlive the request by much.
    fn notify_cancelled(self: &Arc<Self>, id: u64, error: &McpError) {
        if !matches!(
            error.failure,
            McpFailureClass::Cancelled | McpFailureClass::Timeout
        ) || !self.is_connected()
        {
            return;
        }
        let payload = cancelled_payload(
            JsonRpcId::Number(id as i64),
            "the client gave up on the request",
        );
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            let send = inner.post_without_response(&payload);
            let _ = tokio::time::timeout(CANCEL_NOTIFY_TIMEOUT, send).await;
        });
    }

    /// Mark the transport dead, abandon in-flight work, end the server session
    /// and announce the closure exactly once.
    async fn close(&self) {
        if !self.connected.swap(false, Ordering::SeqCst) {
            return;
        }
        self.shutdown_tx.send_replace(true);
        // Read the session out of the lock before awaiting: the guard is not
        // `Send` and must not live across the request below.
        let session = self.session_id.lock().clone();
        if let Some(session) = session {
            let mut headers = self.base_headers();
            insert_header(&mut headers, SESSION_ID_HEADER, &session);
            let request = self.client.delete(&self.url).headers(headers).send();
            let limit = self.timeout.unwrap_or(CLOSE_DELETE_TIMEOUT);
            // Session termination is best-effort: the server may already be gone.
            let _ = tokio::time::timeout(limit, request).await;
            *self.session_id.lock() = None;
        }
        let _ = self.events.send(TransportEvent::Closed { error: None });
    }
}

impl McpTransport for HttpTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Http
    }

    fn endpoint_label(&self) -> String {
        redact_url_for_log(&self.inner.url)
    }

    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Option<Value>,
        cancel: Option<Arc<CancelListener>>,
    ) -> BoxFuture<'a, Result<Value, McpError>> {
        Box::pin(async move {
            let inner = &self.inner;
            if !inner.is_connected() {
                return Err(McpError::closed(&inner.server, TransportKind::Http));
            }
            let id = inner.next_request_id.fetch_add(1, Ordering::Relaxed);
            let payload = request_payload(id, method, params);
            match inner
                .bounded(cancel, inner.post_request(&payload, id))
                .await
            {
                Ok(value) => Ok(value),
                Err(error) => {
                    inner.notify_cancelled(id, &error);
                    Err(error)
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
            let inner = &self.inner;
            if !inner.is_connected() {
                return Err(McpError::closed(&inner.server, TransportKind::Http));
            }
            let payload = notification_payload(method, params);
            inner
                .bounded(None, inner.post_without_response(&payload))
                .await
        })
    }

    fn respond<'a>(
        &'a self,
        id: JsonRpcId,
        result: Result<Value, JsonRpcError>,
    ) -> BoxFuture<'a, Result<(), McpError>> {
        Box::pin(async move {
            let inner = &self.inner;
            if !inner.is_connected() {
                return Err(McpError::closed(&inner.server, TransportKind::Http));
            }
            let payload = response_payload(&id, result);
            inner
                .bounded(None, inner.post_without_response(&payload))
                .await
        })
    }

    fn set_protocol_version(&self, version: String) {
        *self.inner.protocol_version.lock() = Some(version);
    }

    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.inner.close().await })
    }
}

// ---------------------------------------------------------------------------
// Payload and header helpers
// ---------------------------------------------------------------------------

fn request_payload(id: u64, method: &str, params: Option<Value>) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params.unwrap_or_else(|| json!({})),
    })
    .to_string()
}

fn notification_payload(method: &str, params: Option<Value>) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params.unwrap_or_else(|| json!({})),
    })
    .to_string()
}

fn response_payload(id: &JsonRpcId, result: Result<Value, JsonRpcError>) -> String {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string(),
    }
}

/// `notifications/cancelled` body for a request the client gave up on.
fn cancelled_payload(id: JsonRpcId, reason: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": id, "reason": reason },
    })
    .to_string()
}

/// Error handed back by the redirect policy when the hop cap is reached.
#[derive(Debug)]
struct TooManyRedirects;

impl std::fmt::Display for TooManyRedirects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("too many redirects")
    }
}

impl std::error::Error for TooManyRedirects {}

/// True when two URLs share scheme, host and effective port.
///
/// The port comparison uses the scheme default when the URL carries none, so
/// `http://host` and `http://host:80` are one origin.
fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// True when a response frame answers request `expected`.
///
/// This transport only mints numeric ids; a string id can only belong to a
/// response nobody here is waiting for.
fn answers(id: &JsonRpcId, expected: u64) -> bool {
    match id {
        JsonRpcId::Number(id) => u64::try_from(*id) == Ok(expected),
        JsonRpcId::String(_) => false,
    }
}

/// Unwrap a response frame into its result, or the JSON-RPC error it carries.
fn response_result(server: &str, response: JsonRpcResponse) -> Result<Value, McpError> {
    if let Some(error) = response.error {
        let failure = McpError::json_rpc(server, TransportKind::Http, error.code, error.message);
        return Err(match error.data {
            Some(data) => failure.with_data(data),
            None => failure,
        });
    }
    Ok(response.result.unwrap_or(Value::Null))
}

/// Set one header, replacing any configured entry with the same
/// case-insensitive name. Invalid names/values are dropped rather than
/// poisoning every request they appear in.
fn insert_header(headers: &mut reqwest::header::HeaderMap, name: &str, value: &str) {
    let (Ok(name), Ok(value)) = (
        reqwest::header::HeaderName::from_bytes(name.as_bytes()),
        reqwest::header::HeaderValue::from_str(value),
    ) else {
        tracing::debug!(header = name, "ignoring invalid MCP HTTP header");
        return;
    };
    headers.insert(name, value);
}

fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

/// A transport failure whose message carries the whole cause chain.
///
/// `reqwest::Error`'s `Display` stops at the request level ("error sending
/// request for url (...)"), while the reason the failure taxonomy keys on
/// ("connection refused", "timed out", "connection reset") only appears in the
/// causes. Flattening the chain keeps that reason classifiable and visible.
#[derive(Debug)]
struct ChainedError(String);

impl ChainedError {
    fn new(error: &dyn std::error::Error) -> Self {
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        Self(message)
    }
}

impl std::fmt::Display for ChainedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ChainedError {}

/// Resolve with the configured deadline once it elapses; never resolves when
/// the server disabled client-side deadlines (`timeout: 0`).
async fn wait_for_deadline(deadline: Option<Duration>) -> Duration {
    match deadline {
        Some(limit) => {
            tokio::time::sleep(limit).await;
            limit
        }
        None => std::future::pending().await,
    }
}

/// Resolve as soon as the caller cancels; never resolves without a listener.
async fn wait_for_cancel(cancel: Option<Arc<CancelListener>>) {
    match cancel {
        Some(listener) => listener.cancelled().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;

    use super::*;
    use crate::types::{SideEffects, TrustLevel, DEFAULT_MAX_OUTPUT_BYTES};

    // -----------------------------------------------------------------------
    // Minimal HTTP/1.1 fake server
    // -----------------------------------------------------------------------

    /// One request as the fake server saw it on the wire.
    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl RecordedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }

        fn json_body(&self) -> Value {
            serde_json::from_slice(&self.body).expect("request body is JSON")
        }

        /// `true` when a header with this name is present.
        fn has_header(&self, name: &str) -> bool {
            self.header(name).is_some()
        }
    }

    /// The canned response for one request. `pieces` are written as separate
    /// HTTP chunks when `chunked`, which also forces the client to observe
    /// several body reads instead of one coalesced buffer.
    struct Reply {
        status: u16,
        reason: &'static str,
        content_type: String,
        headers: Vec<(String, String)>,
        pieces: Vec<Vec<u8>>,
        chunked: bool,
        piece_delay: Duration,
        delay: Duration,
    }

    impl Reply {
        fn new(status: u16, reason: &'static str, content_type: &str, pieces: Vec<String>) -> Self {
            Self {
                status,
                reason,
                content_type: content_type.to_string(),
                headers: Vec::new(),
                pieces: pieces.into_iter().map(String::into_bytes).collect(),
                chunked: false,
                piece_delay: Duration::ZERO,
                delay: Duration::ZERO,
            }
        }

        fn json(body: impl Into<String>) -> Self {
            Self::new(200, "OK", "application/json", vec![body.into()])
        }

        fn sse(pieces: Vec<String>) -> Self {
            let mut reply = Self::new(200, "OK", "text/event-stream", pieces);
            reply.chunked = true;
            reply.piece_delay = Duration::from_millis(20);
            reply
        }

        fn plain(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
            Self::new(status, reason, "text/plain", vec![body.into()])
        }

        fn header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_string(), value.to_string()));
            self
        }

        /// Wait before sending response headers, to exercise client deadlines.
        fn after(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    type Handler = Box<dyn Fn(&RecordedRequest) -> Reply + Send + 'static>;

    fn handler<F>(f: F) -> Handler
    where
        F: Fn(&RecordedRequest) -> Reply + Send + 'static,
    {
        Box::new(f)
    }

    /// A JSON-RPC response echoing the request id, which keeps the tests
    /// independent of the transport's id allocation.
    fn json_response(request: &RecordedRequest, result: Value) -> Reply {
        Reply::json(
            json!({ "jsonrpc": "2.0", "id": request.json_body()["id"].clone(), "result": result })
                .to_string(),
        )
    }

    struct FakeServer {
        url: String,
        requests: mpsc::UnboundedReceiver<RecordedRequest>,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeServer {
        /// Serve exactly one request per handler, in order, on random ports.
        async fn start(handlers: Vec<Handler>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("addr");
            let (tx, requests) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                for handler in handlers {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let request = read_request(&mut socket).await;
                    let reply = handler(&request);
                    let _ = tx.send(request);
                    write_reply(&mut socket, reply).await;
                }
            });
            Self {
                url: format!("http://{addr}/mcp"),
                requests,
                task,
            }
        }

        async fn next_request(&mut self) -> RecordedRequest {
            self.requests
                .recv()
                .await
                .expect("server recorded a request")
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn read_request(socket: &mut TcpStream) -> RecordedRequest {
        let mut buffer = Vec::new();
        let mut scratch = [0u8; 1024];
        let head_end = loop {
            let read = socket.read(&mut scratch).await.expect("read headers");
            assert!(read > 0, "connection closed before headers completed");
            buffer.extend_from_slice(&scratch[..read]);
            if let Some(position) = find(&buffer, b"\r\n\r\n") {
                break position + 4;
            }
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split(' ');
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        let content_length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buffer[head_end..].to_vec();
        while body.len() < content_length {
            let read = socket.read(&mut scratch).await.expect("read body");
            if read == 0 {
                break;
            }
            body.extend_from_slice(&scratch[..read]);
        }
        RecordedRequest {
            method,
            path,
            headers,
            body,
        }
    }

    async fn write_reply(socket: &mut TcpStream, reply: Reply) {
        if !reply.delay.is_zero() {
            tokio::time::sleep(reply.delay).await;
        }
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n",
            reply.status, reply.reason, reply.content_type
        );
        for (name, value) in &reply.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if reply.chunked {
            head.push_str("Transfer-Encoding: chunked\r\n\r\n");
        } else {
            let length: usize = reply.pieces.iter().map(Vec::len).sum();
            head.push_str(&format!("Content-Length: {length}\r\n\r\n"));
        }
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        for piece in &reply.pieces {
            if reply.chunked {
                let prefix = format!("{:x}\r\n", piece.len());
                if socket.write_all(prefix.as_bytes()).await.is_err() {
                    return;
                }
            }
            if socket.write_all(piece).await.is_err() {
                return;
            }
            if reply.chunked && socket.write_all(b"\r\n").await.is_err() {
                return;
            }
            let _ = socket.flush().await;
            if !reply.piece_delay.is_zero() {
                tokio::time::sleep(reply.piece_delay).await;
            }
        }
        if reply.chunked {
            let _ = socket.write_all(b"0\r\n\r\n").await;
        }
        let _ = socket.flush().await;
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    // -----------------------------------------------------------------------
    // Transport fixtures
    // -----------------------------------------------------------------------

    fn settings(
        url: &str,
        headers: BTreeMap<String, String>,
        timeout: Option<Duration>,
    ) -> ServerSettings {
        ServerSettings {
            name: "test-server".to_string(),
            transport: TransportKind::Http,
            enabled: true,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: Some(url.to_string()),
            headers,
            request_timeout: timeout,
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Unknown,
            tool_trust: BTreeMap::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    fn transport(
        url: &str,
        headers: BTreeMap<String, String>,
        timeout: Option<Duration>,
    ) -> (
        Arc<dyn McpTransport>,
        mpsc::UnboundedReceiver<TransportEvent>,
    ) {
        let (events, receiver) = mpsc::unbounded_channel();
        let transport = create_http_transport(&settings(url, headers, timeout), events)
            .expect("transport is created");
        (transport, receiver)
    }

    fn test_transport(
        server: &FakeServer,
    ) -> (
        Arc<dyn McpTransport>,
        mpsc::UnboundedReceiver<TransportEvent>,
    ) {
        transport(&server.url, BTreeMap::new(), Some(Duration::from_secs(5)))
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn json_response_completes_the_request() {
        let mut server = FakeServer::start(vec![handler(|request| {
            json_response(request, json!({ "tools": [] })).header("Mcp-Session-Id", "sess-1")
        })])
        .await;
        let (transport, mut events) = test_transport(&server);

        let result = transport
            .request("tools/list", None, None)
            .await
            .expect("JSON response resolves the request");
        assert_eq!(result, json!({ "tools": [] }));

        let request = server.next_request().await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/mcp");
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(
            request.header("accept"),
            Some("application/json, text/event-stream")
        );
        assert!(!request.has_header("mcp-session-id"));
        assert!(!request.has_header("mcp-protocol-version"));
        assert_eq!(request.json_body()["jsonrpc"], json!("2.0"));
        assert_eq!(request.json_body()["method"], json!("tools/list"));
        assert_eq!(request.json_body()["params"], json!({}));
        assert!(events.try_recv().is_err(), "no spurious transport events");

        transport.close().await;
    }

    #[tokio::test]
    async fn session_id_and_protocol_version_are_echoed() {
        let mut server = FakeServer::start(vec![
            handler(|request| {
                json_response(request, json!({ "protocolVersion": "2025-11-25" }))
                    .header("Mcp-Session-Id", "sess-42")
            }),
            handler(|request| json_response(request, json!({ "tools": [] }))),
        ])
        .await;
        // A configured protocol version header must never reach the wire: the
        // transport owns that header.
        let mut configured = BTreeMap::new();
        configured.insert("MCP-Protocol-Version".to_string(), "1999-01-01".to_string());
        configured.insert("X-Api-Key".to_string(), "secret".to_string());
        let (transport, _events) = transport(&server.url, configured, Some(Duration::from_secs(5)));

        transport
            .request(
                "initialize",
                Some(json!({ "protocolVersion": "2025-11-25" })),
                None,
            )
            .await
            .expect("initialize succeeds");
        transport.set_protocol_version("2025-11-25".to_string());
        transport
            .request("tools/list", None, None)
            .await
            .expect("second request succeeds");

        let first = server.next_request().await;
        assert!(!first.has_header("mcp-session-id"));
        assert_eq!(first.header("mcp-protocol-version"), None);
        assert_eq!(first.header("x-api-key"), Some("secret"));

        let second = server.next_request().await;
        assert_eq!(second.header("mcp-session-id"), Some("sess-42"));
        assert_eq!(second.header("mcp-protocol-version"), Some("2025-11-25"));

        transport.close().await;
    }

    #[tokio::test]
    async fn sse_response_is_decoded_across_chunks() {
        let mut server = FakeServer::start(vec![handler(|request| {
            let id = request.json_body()["id"].to_string();
            Reply::sse(vec![
                // A server-initiated notification rides the same stream.
                "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":1}}\n\n"
                    .to_string(),
                // The answering frame is split mid-line and mid-JSON.
                "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":".to_string(),
                id,
                ",\"result\":{\"ok\":true}}\n\n".to_string(),
            ])
        })])
        .await;
        let (transport, mut events) = test_transport(&server);

        let result = transport
            .request("tools/call", Some(json!({ "name": "echo" })), None)
            .await
            .expect("SSE response resolves the request");
        assert_eq!(result, json!({ "ok": true }));

        match events.recv().await.expect("notification forwarded") {
            TransportEvent::Notification { method, params } => {
                assert_eq!(method, "notifications/progress");
                assert_eq!(params, json!({ "progress": 1 }));
            }
            other => panic!("unexpected transport event: {other:?}"),
        }
        let request = server.next_request().await;
        assert_eq!(request.json_body()["method"], json!("tools/call"));

        transport.close().await;
    }

    #[tokio::test]
    async fn sse_stream_without_matching_response_is_malformed() {
        let server = FakeServer::start(vec![handler(|_| {
            Reply::sse(vec![
                "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/ping\",\"params\":{}}\n\n"
                    .to_string(),
            ])
        })])
        .await;
        let (transport, _events) = test_transport(&server);

        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("a stream that ends without the response must fail");
        assert_eq!(error.failure, McpFailureClass::MalformedResponse);
        assert_eq!(error.stage, McpFailureStage::Receive);
        assert!(!error.retryable);
        assert!(
            error
                .message
                .contains("No response received for request ID 1"),
            "unexpected message: {}",
            error.message
        );
    }

    #[tokio::test]
    async fn non_success_status_becomes_http_status_error() {
        let server = FakeServer::start(vec![handler(|_| {
            Reply::plain(500, "Internal Server Error", "boom")
        })])
        .await;
        let (transport, _events) = test_transport(&server);
        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("500 is an error");
        assert_eq!(error.failure, McpFailureClass::HttpStatus);
        assert_eq!(error.stage, McpFailureStage::Receive);
        assert!(!error.retryable, "500 is not retryable");
        assert!(
            error.message.contains("HTTP 500: boom"),
            "{}",
            error.message
        );

        let retryable_server = FakeServer::start(vec![handler(|_| {
            Reply::plain(503, "Service Unavailable", "later")
        })])
        .await;
        let (retryable_transport, _events) = test_transport(&retryable_server);
        let error = retryable_transport
            .request("tools/list", None, None)
            .await
            .expect_err("503 is an error");
        assert_eq!(error.failure, McpFailureClass::HttpStatus);
        assert!(error.retryable, "503 is retryable");
        assert!(
            error.message.contains("HTTP 503: later"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn refused_connection_is_a_connect_error() {
        // Bind a port, then release it: nothing listens there any more.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let (transport, _events) = transport(
            &format!("http://{addr}/mcp"),
            BTreeMap::new(),
            Some(Duration::from_secs(5)),
        );
        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("nothing is listening");
        assert_eq!(error.failure, McpFailureClass::Connect);
        assert_eq!(error.stage, McpFailureStage::Connect);
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn close_sends_delete_with_the_session_id() {
        let mut server = FakeServer::start(vec![
            handler(|request| {
                json_response(request, json!({ "protocolVersion": "2025-11-25" }))
                    .header("Mcp-Session-Id", "sess-9")
            }),
            handler(|_| Reply::plain(200, "OK", "")),
        ])
        .await;
        let (transport, mut events) = test_transport(&server);

        transport
            .request("initialize", None, None)
            .await
            .expect("initialize succeeds");
        assert!(transport.is_connected());
        let _ = server.next_request().await;

        transport.close().await;
        assert!(!transport.is_connected());

        let delete = server.next_request().await;
        assert_eq!(delete.method, "DELETE");
        assert_eq!(delete.path, "/mcp");
        assert_eq!(delete.header("mcp-session-id"), Some("sess-9"));
        assert!(delete.body.is_empty());

        match events.recv().await.expect("closure announced") {
            TransportEvent::Closed { error } => assert!(error.is_none()),
            other => panic!("unexpected transport event: {other:?}"),
        }
        // Closing twice announces nothing more and sends nothing more.
        transport.close().await;
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn notify_posts_without_id_and_accepts_202() {
        let mut server =
            FakeServer::start(vec![handler(|_| Reply::plain(202, "Accepted", ""))]).await;
        let (transport, _events) = test_transport(&server);

        transport
            .notify("notifications/initialized", None)
            .await
            .expect("202 Accepted is success for notifications");

        let request = server.next_request().await;
        assert_eq!(request.method, "POST");
        let body = request.json_body();
        assert_eq!(body["jsonrpc"], json!("2.0"));
        assert_eq!(body["method"], json!("notifications/initialized"));
        assert!(
            body.get("id").is_none(),
            "notifications carry no id: {body}"
        );

        transport.close().await;
    }

    #[tokio::test]
    async fn respond_posts_the_answer_frame() {
        let mut server =
            FakeServer::start(vec![handler(|_| Reply::plain(202, "Accepted", ""))]).await;
        let (transport, _events) = test_transport(&server);

        transport
            .respond(JsonRpcId::Number(7), Ok(json!({ "roots": [] })))
            .await
            .expect("the answer is accepted");

        let body = server.next_request().await.json_body();
        assert_eq!(body["id"], json!(7));
        assert_eq!(body["result"], json!({ "roots": [] }));
        assert!(body.get("method").is_none());

        transport.close().await;
    }

    #[tokio::test]
    async fn request_deadline_is_enforced() {
        let server = FakeServer::start(vec![handler(|request| {
            json_response(request, json!({})).after(Duration::from_millis(500))
        })])
        .await;
        let (transport, _events) = transport(
            &server.url,
            BTreeMap::new(),
            Some(Duration::from_millis(50)),
        );

        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("a silent server must hit the deadline");
        assert_eq!(error.failure, McpFailureClass::Timeout);
        assert!(!error.retryable);
        assert!(
            error.message.contains("request timeout"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_request() {
        let server = FakeServer::start(vec![handler(|request| {
            json_response(request, json!({})).after(Duration::from_secs(30))
        })])
        .await;
        let (transport, _events) = transport(&server.url, BTreeMap::new(), None);

        let token = Arc::new(aish_llm::CancellationToken::new());
        let listener = CancelListener::new(Arc::clone(&token));
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });

        let error = transport
            .request("tools/list", None, Some(listener))
            .await
            .expect_err("cancellation must abort the request");
        assert_eq!(error.failure, McpFailureClass::Cancelled);
        assert!(!error.retryable);
        canceller.await.expect("canceller ran");
    }

    #[tokio::test]
    async fn requests_after_close_report_closed() {
        let (transport, _events) = transport(
            "http://127.0.0.1:1/mcp",
            BTreeMap::new(),
            Some(Duration::from_millis(100)),
        );
        transport.close().await;

        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("a closed transport refuses work");
        assert_eq!(error.failure, McpFailureClass::Closed);
        assert_eq!(error.stage, McpFailureStage::Send);
    }

    #[test]
    fn endpoint_label_is_redacted() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let transport = create_http_transport(
            &settings(
                "https://user:pass@example.com/mcp?access_token=abc",
                BTreeMap::new(),
                None,
            ),
            events,
        )
        .expect("transport is created");
        assert_eq!(transport.kind(), TransportKind::Http);
        let label = transport.endpoint_label();
        assert!(!label.contains("pass"), "credentials leaked: {label}");
        assert!(!label.contains("abc"), "credentials leaked: {label}");
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

    #[tokio::test]
    async fn an_oversized_body_fails_the_request() {
        // A well-formed answer whose body is one byte past the cap.
        let body = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"blob\":\"{}\"}}}}",
            "x".repeat(MAX_RESPONSE_BYTES)
        );
        let server = FakeServer::start(vec![handler(move |_| Reply::json(body.clone()))]).await;
        let (transport, _events) = test_transport(&server);

        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("a body past the cap must fail the request");
        assert_eq!(error.failure, McpFailureClass::MalformedResponse);
        assert_eq!(error.stage, McpFailureStage::Receive);
        assert!(!error.retryable);
        assert!(
            error.message.contains("cap"),
            "unexpected message: {}",
            error.message
        );
        // The body was too big, not the server dead.
        assert!(transport.is_connected());

        transport.close().await;
    }

    #[tokio::test]
    async fn a_cross_origin_redirect_is_refused() {
        // The other origin the redirect points at: had the client followed it,
        // the connection would show up in this listener's backlog.
        let other = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let other_port = other.local_addr().expect("addr").port();
        let target = format!("http://127.0.0.1:{other_port}/mcp");
        let server = FakeServer::start(vec![handler(move |_| {
            Reply::plain(307, "Temporary Redirect", "").header("Location", &target)
        })])
        .await;
        let (transport, _events) = test_transport(&server);

        let error = transport
            .request(
                "tools/call",
                Some(json!({ "name": "write", "arguments": {} })),
                None,
            )
            .await
            .expect_err("a cross-origin redirect must not be followed");
        assert_eq!(error.failure, McpFailureClass::HttpStatus);
        assert!(
            error.message.contains("307"),
            "unexpected message: {}",
            error.message
        );

        // Neither the JSON-RPC body nor any configured header reached the
        // redirect target.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), other.accept())
                .await
                .is_err(),
            "the redirect target must never be contacted"
        );

        transport.close().await;
    }

    #[tokio::test]
    async fn a_same_origin_redirect_is_followed() {
        let port = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let redirect_port = Arc::clone(&port);
        let mut server = FakeServer::start(vec![
            handler(move |_| {
                let location = format!(
                    "http://127.0.0.1:{}/moved",
                    redirect_port.load(std::sync::atomic::Ordering::SeqCst)
                );
                Reply::plain(307, "Temporary Redirect", "").header("Location", &location)
            }),
            handler(|request| json_response(request, json!({ "ok": true }))),
        ])
        .await;
        port.store(
            reqwest::Url::parse(&server.url)
                .expect("the fake URL is valid")
                .port()
                .expect("the fake URL has a port"),
            std::sync::atomic::Ordering::SeqCst,
        );
        let (transport, _events) = test_transport(&server);

        let result = transport
            .request("tools/list", Some(json!({ "page": 2 })), None)
            .await
            .expect("a same-origin redirect is followed");
        assert_eq!(result, json!({ "ok": true }));

        let first = server.next_request().await;
        assert_eq!(first.path, "/mcp");
        let second = server.next_request().await;
        assert_eq!(second.path, "/moved");
        assert_eq!(
            second.json_body()["params"],
            json!({ "page": 2 }),
            "the redirected request must carry the body"
        );

        transport.close().await;
    }

    #[tokio::test]
    async fn giving_up_notifies_the_server() {
        // The first POST is held past the client deadline; the second is the
        // cancellation notice the client sends once it gives up.
        let mut server = FakeServer::start(vec![
            handler(|request| json_response(request, json!({})).after(Duration::from_millis(200))),
            handler(|_| Reply::plain(202, "Accepted", "")),
        ])
        .await;
        let (transport, _events) = transport(
            &server.url,
            BTreeMap::new(),
            Some(Duration::from_millis(50)),
        );

        let error = transport
            .request("tools/list", None, None)
            .await
            .expect_err("a silent server must hit the deadline");
        assert_eq!(error.failure, McpFailureClass::Timeout);

        let abandoned = server.next_request().await;
        assert_eq!(abandoned.json_body()["method"], json!("tools/list"));
        let notice = server.next_request().await;
        let body = notice.json_body();
        assert_eq!(body["method"], json!("notifications/cancelled"));
        assert_eq!(body["params"]["requestId"], json!(1));
        assert!(
            body.get("id").is_none(),
            "notifications carry no id: {body}"
        );
        assert!(
            body["params"]["reason"].is_string(),
            "the notice must carry a reason: {body}"
        );

        transport.close().await;
    }

    #[tokio::test]
    async fn a_lost_http_server_is_marked_failed() {
        // initialize, the `notifications/initialized` POST, then `tools/list`.
        let server = FakeServer::start(vec![
            handler(|request| {
                json_response(
                    request,
                    json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "fake", "version": "1.0.0" },
                    }),
                )
            }),
            handler(|_| Reply::plain(202, "Accepted", "")),
            handler(|request| json_response(request, json!({ "tools": [{ "name": "echo" }] }))),
        ])
        .await;
        let manager = crate::manager::McpManager::new(crate::config::McpSettings {
            enabled: true,
            request_timeout: Some(Duration::from_secs(5)),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            servers: vec![settings(
                &server.url,
                BTreeMap::new(),
                Some(Duration::from_secs(5)),
            )],
        });
        manager.start(&tokio::runtime::Handle::current());
        wait_for(|| {
            matches!(
                manager.snapshots()[0].status,
                crate::types::ServerStatus::Connected { .. }
            )
            .then_some(())
        })
        .await;

        // The server goes away without telling the client: an HTTP transport has
        // no read loop, so only a call can discover it.
        drop(server);

        let error = manager
            .call("test-server", "echo", &json!({ "text": "hi" }), None)
            .await
            .expect_err("a call to a dead server must fail");
        assert!(error.is_connection_loss(), "unexpected error: {error:?}");

        // The status must stop claiming `Connected`.
        wait_for(|| {
            matches!(
                manager.snapshots()[0].status,
                crate::types::ServerStatus::Failed { .. }
            )
            .then_some(())
        })
        .await;

        // The driver now owns the teardown, so shutting down stays prompt.
        manager.shutdown().await;
    }
}
