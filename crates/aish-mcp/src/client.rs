//! MCP protocol layer: handshake, tool discovery and tool invocation.
//!
//! The transport moves frames; this layer decides what they mean. It performs
//! the `initialize` handshake, answers the server-to-client requests the
//! protocol requires (`ping`, `roots/list`), pages `tools/list` until the
//! server stops handing out cursors and runs `tools/call`.
//!
//! Every connection owns an event loop that drains the transport's
//! [`TransportEvent`] stream and republishes it as a [`ServerEvent`], so the
//! owner (the manager) sees server notifications and connection loss without
//! reaching into the transport.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cancel::CancelListener;
use crate::config::ServerSettings;
use crate::transports::{create_http_transport, create_stdio_transport};
use crate::types::{
    InitializeResult, JsonRpcError, McpError, McpFailureClass, McpFailureStage, McpToolDefinition,
    McpTransport, ServerInfo, ToolCallResult, TransportEvent, TransportKind, MCP_PROTOCOL_VERSION,
};

/// JSON-RPC code for a method the server asked the client for and the client
/// does not implement.
const METHOD_NOT_FOUND: i64 = -32601;

/// Hard cap on `tools/list` pages: a server that keeps handing out cursors
/// would otherwise spin forever.
const MAX_TOOL_PAGES: usize = 50;

/// Hard cap on discovered tools, so one server cannot exhaust memory.
const MAX_TOOLS: usize = 5000;

/// Hard cap on one connect attempt. The handshake also honors the configured
/// request deadline, but a server with `request_timeout: 0` (the documented way
/// to disable client-side deadlines) must still not be able to park a driver
/// forever: with this bound a connect attempt always ends and reaps its child.
const CONNECT_DEADLINE: Duration = Duration::from_secs(30);

/// Connection-level traffic the manager observes.
#[derive(Debug)]
pub enum ServerEvent {
    /// Server-initiated notification (`notifications/*`).
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// The connection is gone. Emitted at most once per connection.
    Closed { error: Option<McpError> },
}

/// Inputs a connection needs beyond the resolved [`ServerSettings`].
pub struct ConnectOptions {
    /// Filesystem roots advertised to the server (`roots/list` answers). Empty
    /// means the client declares the capability but has no roots to offer.
    pub roots: Vec<String>,
    /// Cancellation for the handshake and for every request that does not pass
    /// its own listener.
    pub cancel: Option<Arc<CancelListener>>,
    /// Receiver side for [`ServerEvent`]s. The manager owns it for the whole
    /// life of the connection.
    pub events: mpsc::UnboundedSender<ServerEvent>,
}

/// One live MCP server connection.
pub struct Connection {
    settings: ServerSettings,
    transport: Arc<dyn McpTransport>,
    server_info: ServerInfo,
    protocol_version: String,
    instructions: Option<String>,
    events_task: JoinHandle<()>,
    closed: AtomicBool,
}

/// Connect to an MCP server: start its transport, run the handshake and return
/// the live connection.
///
/// A failed handshake closes the transport before returning, so a rejected
/// server never leaves a subprocess or a session behind. Dropping the returned
/// future mid-handshake does the same: the event loop is aborted and the last
/// transport handle is dropped, which makes a stdio child die even though the
/// asynchronous `close()` never ran.
pub async fn connect(
    settings: &ServerSettings,
    options: ConnectOptions,
) -> Result<Connection, McpError> {
    let (transport_events, transport_events_rx) = mpsc::unbounded_channel::<TransportEvent>();
    let transport = match settings.transport {
        TransportKind::Stdio => create_stdio_transport(settings, transport_events)?,
        TransportKind::Http => create_http_transport(settings, transport_events)?,
    };

    // The event loop runs during the handshake on purpose: a server that asks
    // `roots/list` while initializing must be answered, or the handshake
    // deadlocks against a loop that has not started yet.
    let events_task = tokio::spawn(run_event_loop(
        Arc::downgrade(&transport),
        options.roots,
        options.events,
        transport_events_rx,
    ));
    // The loop must not outlive this future: a caller that drops the handshake
    // (a `/mcp test` timeout, a cancelled connect) would otherwise leave the
    // task parked on the event channel for the life of the process.
    let mut guard = EventLoopGuard {
        task: Some(events_task),
    };

    let initialize =
        match tokio::time::timeout(CONNECT_DEADLINE, handshake(&transport, options.cancel)).await {
            Ok(Ok(initialize)) => initialize,
            Ok(Err(error)) => {
                // Nothing owns this transport yet, so tear it down here instead of
                // leaking a live subprocess.
                transport.close().await;
                return Err(error);
            }
            Err(_) => {
                transport.close().await;
                return Err(McpError::new(
                    &settings.name,
                    settings.transport,
                    McpFailureStage::Connect,
                    McpFailureClass::Timeout,
                    true,
                    format!(
                        "the initialize handshake did not complete within {}ms",
                        CONNECT_DEADLINE.as_millis()
                    ),
                ));
            }
        };

    Ok(Connection {
        settings: settings.clone(),
        transport,
        server_info: initialize.server_info,
        protocol_version: initialize.protocol_version,
        instructions: initialize.instructions,
        events_task: guard.disarm(),
        closed: AtomicBool::new(false),
    })
}

/// Aborts the connection's event loop when the connect future is dropped.
///
/// The loop is spawned before the handshake completes, so a caller that drops
/// the future (`/mcp test` runs it under a timeout shorter than the handshake
/// deadline) must not leave it running. Dropping this guard also releases the
/// future's transport handle, which is what lets `StdioTransport`'s `Drop` kill
/// the child process group.
struct EventLoopGuard {
    task: Option<JoinHandle<()>>,
}

impl EventLoopGuard {
    /// Hand the event-loop task to its new owner.
    fn disarm(&mut self) -> JoinHandle<()> {
        self.task
            .take()
            .expect("the event loop task is guarded exactly once")
    }
}

impl Drop for EventLoopGuard {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Connection {
    /// Resolved settings this connection was created from.
    pub fn settings(&self) -> &ServerSettings {
        &self.settings
    }

    /// Configured server name (the identity used by status output, logs and
    /// trust records), not the name the server reports for itself.
    pub fn server_name(&self) -> &str {
        &self.settings.name
    }

    /// Protocol revision negotiated during the handshake.
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// `serverInfo` the server reported during the handshake.
    pub fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// `instructions` the server sent for the model, when it sent any.
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    /// True while the transport is connected and this connection was not
    /// closed by the caller.
    pub fn is_alive(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && self.transport.is_connected()
    }

    /// Discover every tool the server exposes, following `nextCursor`.
    ///
    /// Both the page count and the tool count are capped: a misbehaving server
    /// must not be able to spin this loop or exhaust memory.
    pub async fn list_tools(
        &self,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<Vec<McpToolDefinition>, McpError> {
        let mut tools: Vec<McpToolDefinition> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let params = match &cursor {
                Some(cursor) => serde_json::json!({ "cursor": cursor }),
                None => serde_json::json!({}),
            };
            let result = self
                .transport
                .request("tools/list", Some(params), cancel.clone())
                .await?;

            if let Some(entries) = result.get("tools").and_then(Value::as_array) {
                for entry in entries {
                    // An entry without a name cannot be called; skip it rather
                    // than discarding the whole page.
                    if let Some(definition) = McpToolDefinition::from_value(entry) {
                        tools.push(definition);
                    }
                }
            }
            if tools.len() > MAX_TOOLS {
                return Err(
                    self.protocol_error(format!("tools/list returned more than {MAX_TOOLS} tools"))
                );
            }

            pages += 1;
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|cursor| !cursor.is_empty());
            match (&cursor, pages) {
                (None, _) => return Ok(tools),
                (Some(_), pages) if pages >= MAX_TOOL_PAGES => {
                    return Err(
                        self.protocol_error(format!("tools/list exceeded {MAX_TOOL_PAGES} pages"))
                    );
                }
                (Some(_), _) => {}
            }
        }
    }

    /// Call one tool and parse its result. A tool-level failure (`isError`) is
    /// part of the result, not an [`McpError`].
    pub async fn call_tool(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<ToolCallResult, McpError> {
        let params = serde_json::json!({ "name": tool, "arguments": args });
        let result = self
            .transport
            .request("tools/call", Some(params), cancel)
            .await?;
        Ok(ToolCallResult::from_value(&result))
    }

    /// Close the connection. Idempotent; the event loop is aborted rather than
    /// awaited, because a transport that never reports `Closed` would otherwise
    /// block shutdown.
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // Stop forwarding first: the transport's `Closed` right below is the
        // caller's own doing and not news for it.
        self.events_task.abort();
        self.transport.close().await;
    }

    fn protocol_error(&self, message: String) -> McpError {
        McpError::new(
            &self.settings.name,
            self.transport.kind(),
            McpFailureStage::Protocol,
            McpFailureClass::Protocol,
            false,
            message,
        )
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("server", &self.settings.name)
            .field("protocol_version", &self.protocol_version)
            .field("alive", &self.is_alive())
            .finish()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // A connection dropped without `close()` must not leave a subprocess
        // running, so hand the (async) teardown to the runtime.
        self.events_task.abort();
        if !self.closed.swap(true, Ordering::SeqCst) {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let transport = self.transport.clone();
                handle.spawn(async move { transport.close().await });
            }
        }
    }
}

/// Run the `initialize` handshake and tell the server we are ready.
async fn handshake(
    transport: &Arc<dyn McpTransport>,
    cancel: Option<Arc<CancelListener>>,
) -> Result<InitializeResult, McpError> {
    let params = serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "roots": { "listChanged": false } },
        "clientInfo": { "name": "aish", "version": env!("CARGO_PKG_VERSION") },
    });

    let value = transport
        .request("initialize", Some(params), cancel)
        .await?;
    let initialize = InitializeResult::from_value(&value);

    // Later requests echo the negotiated revision (Streamable HTTP sends it as
    // a header); the stdio transport ignores it.
    transport.set_protocol_version(initialize.protocol_version.clone());
    transport.notify("notifications/initialized", None).await?;

    Ok(initialize)
}

/// Drain the transport's events: answer server requests, republish
/// notifications and stop once the transport is gone.
///
/// The transport handle is weak on purpose: the loop must never be what keeps a
/// transport - and a stdio child with it - alive, so a connection that loses its
/// owner ends here instead of leaking.
async fn run_event_loop(
    transport: Weak<dyn McpTransport>,
    roots: Vec<String>,
    events: mpsc::UnboundedSender<ServerEvent>,
    mut transport_events: mpsc::UnboundedReceiver<TransportEvent>,
) {
    while let Some(event) = transport_events.recv().await {
        match event {
            TransportEvent::Request {
                id,
                method,
                params: _,
            } => {
                let Some(transport) = transport.upgrade() else {
                    return;
                };
                if let Err(error) = transport
                    .respond(id, answer_server_request(&method, &roots))
                    .await
                {
                    // A dead transport reports its own `Closed` right after.
                    tracing::debug!(
                        server = %transport.endpoint_label(),
                        "failed to answer a server request: {}",
                        error.describe()
                    );
                }
            }
            TransportEvent::Notification { method, params } => {
                if events
                    .send(ServerEvent::Notification { method, params })
                    .is_err()
                {
                    // The owner dropped its receiver: nothing left to forward.
                    return;
                }
            }
            TransportEvent::Closed { error } => {
                let _ = events.send(ServerEvent::Closed { error });
                return;
            }
        }
    }
}

/// Answer a server-to-client request.
///
/// The client declares only `roots` support, so `ping` and `roots/list` are
/// answered from [`ConnectOptions::roots`]; every other method gets the
/// JSON-RPC "method not found" error the protocol requires.
fn answer_server_request(method: &str, roots: &[String]) -> Result<Value, JsonRpcError> {
    match method {
        "ping" => Ok(serde_json::json!({})),
        "roots/list" => Ok(serde_json::json!({
            "roots": roots
                .iter()
                .map(|root| serde_json::json!({
                    "uri": format!("file://{root}"),
                    "name": root_name(root),
                }))
                .collect::<Vec<Value>>(),
        })),
        _ => Err(JsonRpcError {
            code: METHOD_NOT_FOUND,
            message: "Method not found".to_string(),
            data: None,
        }),
    }
}

/// Display name of a root: its last path segment, or the path itself when it
/// has none (e.g. `/`).
fn root_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use parking_lot::Mutex;
    use serde_json::json;
    use tokio::time::Instant;

    use super::*;
    use crate::types::{JsonRpcId, SideEffects, TrustLevel, DEFAULT_MAX_OUTPUT_BYTES};

    /// Scripted stand-in for a transport: every `request` pops the next
    /// prepared answer and records the call, so the protocol layer can be
    /// tested without a server.
    struct MockTransport {
        scripted: Mutex<VecDeque<Result<Value, McpError>>>,
        requests: Mutex<Vec<(String, Option<Value>)>>,
        notifications: Mutex<Vec<String>>,
        answers: Mutex<VecDeque<(JsonRpcId, Result<Value, JsonRpcError>)>>,
        protocol_version: Mutex<Option<String>>,
        closes: AtomicUsize,
        connected: AtomicBool,
    }

    impl MockTransport {
        fn new(scripted: Vec<Result<Value, McpError>>) -> Self {
            Self {
                scripted: Mutex::new(scripted.into()),
                requests: Mutex::new(Vec::new()),
                notifications: Mutex::new(Vec::new()),
                answers: Mutex::new(VecDeque::new()),
                protocol_version: Mutex::new(None),
                closes: AtomicUsize::new(0),
                connected: AtomicBool::new(true),
            }
        }

        fn recorded_requests(&self) -> Vec<(String, Option<Value>)> {
            self.requests.lock().clone()
        }

        fn take_answer(&self) -> Option<(JsonRpcId, Result<Value, JsonRpcError>)> {
            self.answers.lock().pop_front()
        }
    }

    impl McpTransport for MockTransport {
        fn kind(&self) -> TransportKind {
            TransportKind::Stdio
        }

        fn endpoint_label(&self) -> String {
            "mock".to_string()
        }

        fn request<'a>(
            &'a self,
            method: &'a str,
            params: Option<Value>,
            _cancel: Option<Arc<CancelListener>>,
        ) -> BoxFuture<'a, Result<Value, McpError>> {
            Box::pin(async move {
                self.requests.lock().push((method.to_string(), params));
                self.scripted.lock().pop_front().unwrap_or_else(|| {
                    Err(McpError::new(
                        "mock",
                        TransportKind::Stdio,
                        McpFailureStage::Protocol,
                        McpFailureClass::Protocol,
                        false,
                        format!("the mock has no scripted answer for `{method}`"),
                    ))
                })
            })
        }

        fn notify<'a>(
            &'a self,
            method: &'a str,
            _params: Option<Value>,
        ) -> BoxFuture<'a, Result<(), McpError>> {
            Box::pin(async move {
                self.notifications.lock().push(method.to_string());
                Ok(())
            })
        }

        fn respond<'a>(
            &'a self,
            id: JsonRpcId,
            result: Result<Value, JsonRpcError>,
        ) -> BoxFuture<'a, Result<(), McpError>> {
            Box::pin(async move {
                self.answers.lock().push_back((id, result));
                Ok(())
            })
        }

        fn set_protocol_version(&self, version: String) {
            *self.protocol_version.lock() = Some(version);
        }

        fn is_connected(&self) -> bool {
            self.connected.load(Ordering::SeqCst)
        }

        fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.closes.fetch_add(1, Ordering::SeqCst);
                self.connected.store(false, Ordering::SeqCst);
            })
        }
    }

    /// Wait until `check` produces a value. Used to observe work that happens
    /// inside the event loop task.
    async fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = check() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the condition"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn settings(timeout: Option<Duration>) -> ServerSettings {
        ServerSettings {
            name: "fixture".to_string(),
            transport: TransportKind::Stdio,
            enabled: true,
            command: Some("/bin/sh".to_string()),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            request_timeout: timeout,
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Unknown,
            tool_trust: BTreeMap::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    fn stdio_settings(script: &str, timeout: Duration) -> ServerSettings {
        ServerSettings {
            args: vec!["-c".to_string(), script.to_string()],
            request_timeout: Some(timeout),
            ..settings(Some(timeout))
        }
    }

    /// A connection whose transport is the mock; the event loop is idle because
    /// these tests exercise the request path only.
    fn mock_connection(mock: Arc<MockTransport>) -> Connection {
        Connection {
            settings: settings(Some(Duration::from_secs(5))),
            transport: mock,
            server_info: ServerInfo {
                name: "mock".to_string(),
                version: "1.0.0".to_string(),
            },
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            instructions: None,
            events_task: tokio::spawn(async {}),
            closed: AtomicBool::new(false),
        }
    }

    #[tokio::test]
    async fn handshake_sends_initialize_and_the_initialized_notification() {
        let mock = Arc::new(MockTransport::new(vec![Ok(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "mock", "version": "2.0.0"},
            "instructions": "  be nice  "
        }))]));
        let transport: Arc<dyn McpTransport> = mock.clone();

        let initialize = handshake(&transport, None).await.expect("handshake");
        assert_eq!(initialize.protocol_version, "2025-11-25");
        assert_eq!(initialize.server_info.name, "mock");
        assert_eq!(initialize.server_info.version, "2.0.0");
        assert_eq!(initialize.instructions.as_deref(), Some("be nice"));

        let requests = mock.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "initialize");
        let params = requests[0].1.clone().expect("initialize params");
        assert_eq!(params["protocolVersion"], json!(MCP_PROTOCOL_VERSION));
        assert_eq!(
            params["capabilities"],
            json!({"roots": {"listChanged": false}})
        );
        assert_eq!(params["clientInfo"]["name"], json!("aish"));
        assert_eq!(
            params["clientInfo"]["version"],
            json!(env!("CARGO_PKG_VERSION"))
        );

        assert_eq!(mock.protocol_version.lock().as_deref(), Some("2025-11-25"));
        assert_eq!(
            mock.notifications.lock().as_slice(),
            ["notifications/initialized"]
        );
    }

    #[tokio::test]
    async fn handshake_reports_a_transport_failure() {
        let failure = McpError::eof(
            "mock",
            TransportKind::Stdio,
            "subprocess exited with code 3",
        );
        let mock = Arc::new(MockTransport::new(vec![Err(failure)]));
        let transport: Arc<dyn McpTransport> = mock.clone();

        let error = handshake(&transport, None)
            .await
            .expect_err("handshake must fail");
        assert_eq!(error.failure, McpFailureClass::Eof);
        assert!(mock.notifications.lock().is_empty());
    }

    #[tokio::test]
    async fn list_tools_follows_the_cursor_and_skips_unnamed_tools() {
        let page_one = json!({
            "tools": [
                {"name": "read", "description": "  Read a file  ", "annotations": {"readOnlyHint": true}},
                {"description": "no name"},
            ],
            "nextCursor": "cursor-1"
        });
        let page_two = json!({
            "tools": [{"name": "write", "inputSchema": {"type": "object"}}]
        });
        let mock = Arc::new(MockTransport::new(vec![Ok(page_one), Ok(page_two)]));
        let connection = mock_connection(mock.clone());

        let tools = connection.list_tools(None).await.expect("tools/list");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "write"]
        );
        assert_eq!(tools[0].description.as_deref(), Some("Read a file"));
        assert_eq!(tools[0].annotations.read_only_hint, Some(true));
        assert_eq!(tools[1].input_schema, Some(json!({"type": "object"})));

        let requests = mock.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0, "tools/list");
        assert_eq!(requests[0].1, Some(json!({})));
        assert_eq!(requests[1].1, Some(json!({"cursor": "cursor-1"})));
    }

    #[tokio::test]
    async fn list_tools_stops_at_the_page_cap() {
        let page = || {
            Ok(json!({
                "tools": [{"name": "t"}],
                "nextCursor": "more"
            }))
        };
        let mock = Arc::new(MockTransport::new(
            (0..MAX_TOOL_PAGES + 5).map(|_| page()).collect(),
        ));
        let connection = mock_connection(mock.clone());

        let error = connection
            .list_tools(None)
            .await
            .expect_err("an endless cursor must fail");
        assert_eq!(error.failure, McpFailureClass::Protocol);
        assert_eq!(error.stage, McpFailureStage::Protocol);
        assert!(
            error.message.contains("pages"),
            "unexpected message: {}",
            error.message
        );
        assert_eq!(mock.recorded_requests().len(), MAX_TOOL_PAGES);
    }

    #[tokio::test]
    async fn list_tools_stops_at_the_tool_cap() {
        let tools: Vec<Value> = (0..=MAX_TOOLS)
            .map(|index| json!({"name": format!("tool-{index}")}))
            .collect();
        let mock = Arc::new(MockTransport::new(vec![Ok(json!({"tools": tools}))]));
        let connection = mock_connection(mock);

        let error = connection
            .list_tools(None)
            .await
            .expect_err("too many tools must fail");
        assert_eq!(error.failure, McpFailureClass::Protocol);
        assert!(
            error.message.contains("tools"),
            "unexpected message: {}",
            error.message
        );
    }

    #[tokio::test]
    async fn call_tool_parses_the_result() {
        let mock = Arc::new(MockTransport::new(vec![Ok(json!({
            "content": [{"type": "text", "text": "hello"}],
            "structuredContent": {"rows": 1},
            "isError": false
        }))]));
        let connection = mock_connection(mock.clone());

        let result = connection
            .call_tool("read", &json!({"path": "/tmp/x"}), None)
            .await
            .expect("tools/call");
        assert!(!result.is_error);
        assert_eq!(
            crate::types::render_tool_result(&result, 64 * 1024),
            "hello\n{\n  \"rows\": 1\n}"
        );

        let requests = mock.recorded_requests();
        assert_eq!(requests[0].0, "tools/call");
        assert_eq!(
            requests[0].1,
            Some(json!({"name": "read", "arguments": {"path": "/tmp/x"}}))
        );
    }

    #[tokio::test]
    async fn event_loop_answers_ping_and_roots_list() {
        let mock = Arc::new(MockTransport::new(Vec::new()));
        let (server_events, _server_events_rx) = mpsc::unbounded_channel();
        let (transport_events, transport_events_rx) = mpsc::unbounded_channel();
        let transport: Arc<dyn McpTransport> = mock.clone();
        let task = tokio::spawn(run_event_loop(
            Arc::downgrade(&transport),
            vec!["/home/user/project".to_string()],
            server_events,
            transport_events_rx,
        ));

        transport_events
            .send(TransportEvent::Request {
                id: JsonRpcId::Number(1),
                method: "ping".to_string(),
                params: Value::Null,
            })
            .expect("send ping");
        let (id, answer) = wait_for(|| mock.take_answer()).await;
        assert_eq!(id, JsonRpcId::Number(1));
        assert_eq!(answer.expect("ping result"), json!({}));

        transport_events
            .send(TransportEvent::Request {
                id: JsonRpcId::String("srv-2".to_string()),
                method: "roots/list".to_string(),
                params: Value::Null,
            })
            .expect("send roots/list");
        let (id, answer) = wait_for(|| mock.take_answer()).await;
        assert_eq!(id, JsonRpcId::String("srv-2".to_string()));
        assert_eq!(
            answer.expect("roots result"),
            json!({"roots": [{"uri": "file:///home/user/project", "name": "project"}]})
        );

        task.abort();
    }

    #[tokio::test]
    async fn event_loop_reports_unknown_methods_as_method_not_found() {
        let mock = Arc::new(MockTransport::new(Vec::new()));
        let (server_events, _server_events_rx) = mpsc::unbounded_channel();
        let (transport_events, transport_events_rx) = mpsc::unbounded_channel();
        let transport: Arc<dyn McpTransport> = mock.clone();
        let task = tokio::spawn(run_event_loop(
            Arc::downgrade(&transport),
            Vec::new(),
            server_events,
            transport_events_rx,
        ));

        transport_events
            .send(TransportEvent::Request {
                id: JsonRpcId::Number(9),
                method: "sampling/createMessage".to_string(),
                params: Value::Null,
            })
            .expect("send request");
        let (id, answer) = wait_for(|| mock.take_answer()).await;
        assert_eq!(id, JsonRpcId::Number(9));
        let error = answer.expect_err("an unimplemented method must be refused");
        assert_eq!(error.code, METHOD_NOT_FOUND);
        assert_eq!(error.message, "Method not found");

        task.abort();
    }

    #[tokio::test]
    async fn event_loop_forwards_notifications_and_stops_after_closed() {
        let mock = Arc::new(MockTransport::new(Vec::new()));
        let (server_events, mut server_events_rx) = mpsc::unbounded_channel();
        let (transport_events, transport_events_rx) = mpsc::unbounded_channel();
        let transport: Arc<dyn McpTransport> = mock;
        let task = tokio::spawn(run_event_loop(
            Arc::downgrade(&transport),
            Vec::new(),
            server_events,
            transport_events_rx,
        ));

        transport_events
            .send(TransportEvent::Notification {
                method: "notifications/tools/list_changed".to_string(),
                params: json!({"reason": "edit"}),
            })
            .expect("send notification");
        match server_events_rx.recv().await.expect("notification event") {
            ServerEvent::Notification { method, params } => {
                assert_eq!(method, "notifications/tools/list_changed");
                assert_eq!(params, json!({"reason": "edit"}));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let failure = McpError::eof("mock", TransportKind::Stdio, "gone");
        transport_events
            .send(TransportEvent::Closed {
                error: Some(failure.clone()),
            })
            .expect("send closed");
        match server_events_rx.recv().await.expect("closed event") {
            ServerEvent::Closed { error } => {
                let error = error.expect("error");
                assert_eq!(error.failure, McpFailureClass::Eof);
                assert_eq!(error.message, "gone");
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // The loop stops on `Closed` and drops the sender.
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the event loop must stop")
            .expect("the event loop must not panic");
        assert!(server_events_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let mock = Arc::new(MockTransport::new(Vec::new()));
        let connection = mock_connection(mock.clone());
        assert!(connection.is_alive());

        connection.close().await;
        connection.close().await;
        assert_eq!(mock.closes.load(Ordering::SeqCst), 1);
        assert!(!connection.is_alive());
    }

    #[tokio::test]
    async fn connect_fails_when_the_handshake_deadline_expires() {
        let (events, mut events_rx) = mpsc::unbounded_channel();
        let settings = stdio_settings("while read -r line; do :; done", Duration::from_millis(200));

        let error = connect(
            &settings,
            ConnectOptions {
                roots: Vec::new(),
                cancel: None,
                events,
            },
        )
        .await
        .expect_err("a server that never answers must fail the handshake");
        assert_eq!(error.failure, McpFailureClass::Timeout);
        assert_eq!(error.stage, McpFailureStage::Receive);
        assert!(!error.retryable);

        // A failed handshake leaves nothing behind: the event loop is gone, so
        // the receiver settles on (at most) the teardown event.
        let event = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .expect("the event channel must settle");
        assert!(
            matches!(event, None | Some(ServerEvent::Closed { .. })),
            "unexpected event: {event:?}"
        );
    }

    #[tokio::test]
    async fn connect_reports_a_server_that_dies() {
        let (events, _events_rx) = mpsc::unbounded_channel();
        let settings = stdio_settings("exit 3", Duration::from_secs(5));

        let error = connect(
            &settings,
            ConnectOptions {
                roots: Vec::new(),
                cancel: None,
                events,
            },
        )
        .await
        .expect_err("a dead server must fail the handshake");
        assert!(error.is_connection_loss(), "unexpected error: {error:?}");
    }

    /// True while `pid` still names a live process. Signal 0 performs the
    /// existence check without sending anything.
    #[cfg(unix)]
    fn pid_is_alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_connect_future_reaps_the_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child.pid");
        // A fixture child that records its pid, then never answers
        // `initialize` (so only the caller can end the handshake).
        let script = format!(
            "printf '%s\\n' \"$$\" > \"{}\"; while read -r line; do :; done",
            pid_file.display()
        );
        let settings = stdio_settings(&script, Duration::from_secs(30));
        let (events, _events_rx) = mpsc::unbounded_channel();

        // `/mcp test <name>` does exactly this: a timeout shorter than the
        // handshake deadline drops the connect future mid-handshake.
        let handshake = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_millis(500),
                connect(
                    &settings,
                    ConnectOptions {
                        roots: Vec::new(),
                        cancel: None,
                        events,
                    },
                ),
            )
            .await
        });

        // The child is up once it has written its pid.
        let pid: i32 = wait_for(|| {
            std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
        })
        .await;
        assert!(pid_is_alive(pid), "the fixture child must be running");

        let outcome = handshake.await.expect("the connect task must not panic");
        assert!(outcome.is_err(), "the handshake deadline must expire");

        // Dropping the future must have destroyed the transport (and with it
        // the process group): the child cannot outlive the connection.
        wait_for(|| (!pid_is_alive(pid)).then_some(())).await;
    }
}
