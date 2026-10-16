//! End-to-end tests of the Streamable HTTP transport against the real fixture
//! server (`aish-mcp-fixture --mode http`).
//!
//! The fixture binds an ephemeral port and prints `LISTENING <port>`, so the
//! tests drive a genuine HTTP/1.1 server - JSON responses, SSE responses,
//! sessions, bearer-token rejection - without pulling a web framework into the
//! dependency graph.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use aish_llm::CancellationToken;
use aish_mcp::client::{connect, ConnectOptions, ServerEvent};
use aish_mcp::config::ServerSettings;
use aish_mcp::types::{
    McpContent, McpFailureClass, SideEffects, TransportKind, TrustLevel, DEFAULT_MAX_OUTPUT_BYTES,
};
use aish_mcp::{CancelListener, Connection};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc;

/// The fixture binary cargo built for this test run.
const FIXTURE: &str = env!("CARGO_BIN_EXE_aish-mcp-fixture");

// ---------------------------------------------------------------------------
// Fixture process
// ---------------------------------------------------------------------------

/// A running `aish-mcp-fixture --mode http`, with its stdout drained.
struct HttpFixture {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    port: u16,
}

impl HttpFixture {
    /// Start the fixture in HTTP mode on an ephemeral port and wait until it
    /// announces the port it bound.
    async fn start(extra: &[&str]) -> Self {
        let mut child = Command::new(FIXTURE)
            .arg("--mode")
            .arg("http")
            .arg("--port")
            .arg("0")
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the fixture must spawn");
        let stdout = child.stdout.take().expect("the fixture stdout is piped");
        let mut lines = BufReader::new(stdout).lines();

        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("the fixture must announce its port")
            .expect("the fixture stdout must be readable")
            .expect("the fixture must not exit before binding");
        let port = line
            .strip_prefix("LISTENING ")
            .unwrap_or_else(|| panic!("unexpected fixture output `{line}`"))
            .trim()
            .parse::<u16>()
            .expect("the announced port must be a number");

        Self { child, lines, port }
    }

    /// Client settings for one path on this fixture.
    fn settings(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        request_timeout: Option<Duration>,
    ) -> ServerSettings {
        ServerSettings {
            name: "fixture".to_string(),
            transport: TransportKind::Http,
            enabled: true,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: Some(format!("http://127.0.0.1:{}{path}", self.port)),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            request_timeout,
            trust: TrustLevel::Allow,
            side_effects: SideEffects::Read,
            tool_trust: BTreeMap::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    /// Wait for a stdout line with `prefix`, giving up after `budget`.
    async fn wait_for_line(&mut self, prefix: &str, budget: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, self.lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    if line.starts_with(prefix) {
                        return Some(line);
                    }
                }
                // EOF, an I/O error or the timeout: no such line.
                Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return None,
            }
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn roots() -> Vec<String> {
    vec![std::env::current_dir()
        .expect("the test needs a working directory")
        .to_string_lossy()
        .into_owned()]
}

/// Connect and return the connection plus the event channel that keeps the
/// connection's event loop alive.
async fn connect_fixture(
    settings: &ServerSettings,
) -> (Connection, mpsc::UnboundedReceiver<ServerEvent>) {
    let (events, receiver) = mpsc::unbounded_channel();
    let options = ConnectOptions {
        roots: roots(),
        cancel: None,
        events,
    };
    let connection = connect(settings, options)
        .await
        .expect("the fixture must connect");
    (connection, receiver)
}

/// Connect and return the handshake failure.
async fn connect_error(settings: &ServerSettings) -> aish_mcp::McpError {
    let (events, _receiver) = mpsc::unbounded_channel();
    let options = ConnectOptions {
        roots: roots(),
        cancel: None,
        events,
    };
    match connect(settings, options).await {
        Ok(_) => panic!("the handshake was expected to fail"),
        Err(error) => error,
    }
}

fn text_of(result: &aish_mcp::ToolCallResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            McpContent::Text { text } => text.clone(),
            other => panic!("expected a text block, got {other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn json_responses_round_trip() {
    let fixture = HttpFixture::start(&[]).await;
    let settings = fixture.settings("/mcp", &[], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    assert_eq!(connection.server_info().name, "mcp-fixture");
    assert_eq!(connection.protocol_version(), "2025-11-25");

    let tools = connection.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.len(), 8);
    assert!(tools.iter().any(|tool| tool.name == "echo"));

    let result = connection
        .call_tool("echo", &json!({ "text": "over http" }), None)
        .await
        .expect("echo must answer");
    assert!(!result.is_error);
    assert_eq!(text_of(&result), "over http");

    connection.close().await;
}

#[tokio::test]
async fn sse_responses_round_trip() {
    let fixture = HttpFixture::start(&["--sse"]).await;
    let settings = fixture.settings("/mcp", &[], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    assert_eq!(connection.server_info().name, "mcp-fixture");

    let tools = connection.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.len(), 8, "every page arrives as an SSE frame");

    let result = connection
        .call_tool("echo", &json!({ "text": "over sse" }), None)
        .await
        .expect("echo must answer");
    assert_eq!(text_of(&result), "over sse");

    connection.close().await;
}

#[tokio::test]
async fn a_session_is_terminated_on_close() {
    let mut fixture = HttpFixture::start(&["--session"]).await;
    let settings = fixture.settings("/mcp", &[], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    // A second request runs with the session the handshake response handed out.
    let tools = connection.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.len(), 8);

    connection.close().await;
    // `close()` terminates the session best effort; the fixture only prints the
    // id it received, so this also proves the id was echoed back.
    let line = fixture
        .wait_for_line("DELETED ", Duration::from_secs(2))
        .await
        .expect("the fixture must report the session deletion");
    assert_eq!(line, "DELETED fixture-session");
}

// ---------------------------------------------------------------------------
// Credentials, failures and deadlines
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_tokens_gate_the_endpoint_and_stay_redacted() {
    let fixture = HttpFixture::start(&["--require-token", "s3cret"]).await;

    // Without credentials the handshake is rejected with the status code.
    let settings = fixture.settings("/mcp", &[], Some(Duration::from_secs(10)));
    let error = connect_error(&settings).await;
    assert_eq!(error.failure, McpFailureClass::HttpStatus);
    assert!(error.message.contains("401"), "{}", error.message);
    assert!(
        !error.message.contains("Bearer s3cret"),
        "a credential reached the diagnostic: {}",
        error.message
    );

    // A stale credential is echoed by the server's error body; the client must
    // sanitize it before the message can reach a log or an audit record.
    let settings = fixture.settings(
        "/mcp",
        &[("Authorization", "Bearer s3cret-stale")],
        Some(Duration::from_secs(10)),
    );
    let error = connect_error(&settings).await;
    assert_eq!(error.failure, McpFailureClass::HttpStatus);
    assert!(error.message.contains("401"), "{}", error.message);
    assert!(
        !error.message.contains("Bearer s3cret"),
        "a credential reached the diagnostic: {}",
        error.message
    );

    // With the right token the same endpoint works.
    let settings = fixture.settings(
        "/mcp",
        &[("Authorization", "Bearer s3cret")],
        Some(Duration::from_secs(10)),
    );
    let (connection, _events) = connect_fixture(&settings).await;
    let result = connection
        .call_tool("echo", &json!({ "text": "authorized" }), None)
        .await
        .expect("the authorized call must succeed");
    assert_eq!(text_of(&result), "authorized");
    connection.close().await;
}

#[tokio::test]
async fn an_unreachable_endpoint_is_a_connect_failure() {
    // Bind a port and release it, so nothing is listening there.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the probe listener must bind");
    let port = listener.local_addr().expect("the probe address").port();
    drop(listener);

    let settings = ServerSettings {
        name: "fixture".to_string(),
        transport: TransportKind::Http,
        enabled: true,
        command: None,
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: None,
        url: Some(format!("http://127.0.0.1:{port}/mcp")),
        headers: BTreeMap::new(),
        request_timeout: Some(Duration::from_secs(5)),
        trust: TrustLevel::Allow,
        side_effects: SideEffects::Read,
        tool_trust: BTreeMap::new(),
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    };

    let error = connect_error(&settings).await;
    assert_eq!(error.failure, McpFailureClass::Connect);
    assert!(error.is_connection_loss());
}

#[tokio::test]
async fn deadline_expiry_is_a_timeout() {
    let fixture = HttpFixture::start(&[]).await;
    let settings = fixture.settings("/mcp", &[], Some(Duration::from_millis(300)));
    let (connection, _events) = connect_fixture(&settings).await;

    let error = connection
        .call_tool("slow", &json!({ "ms": 5000 }), None)
        .await
        .expect_err("the call must time out");
    assert_eq!(error.failure, McpFailureClass::Timeout);
    assert!(!error.retryable);
    assert!(!error.is_connection_loss());

    connection.close().await;
}

#[tokio::test]
async fn caller_cancellation_is_reported() {
    let fixture = HttpFixture::start(&[]).await;
    let settings = fixture.settings("/mcp", &[], Some(Duration::from_secs(30)));
    let (connection, _events) = connect_fixture(&settings).await;

    let token = Arc::new(CancellationToken::new());
    let listener = CancelListener::new(token.clone());
    let cancel = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        token.cancel();
    });

    let error = connection
        .call_tool("slow", &json!({ "ms": 5000 }), Some(listener))
        .await
        .expect_err("the cancelled call must fail");
    assert_eq!(error.failure, McpFailureClass::Cancelled);
    cancel.await.expect("the canceller task must finish");

    connection.close().await;
}

#[tokio::test]
async fn a_get_is_rejected_with_method_not_allowed() {
    // The client never issues a GET, so this guard is checked on the wire.
    let fixture = HttpFixture::start(&[]).await;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", fixture.port))
        .await
        .expect("the fixture must accept a connection");
    stream
        .write_all(b"GET /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .expect("the request must be written");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("the response must be readable");
    assert!(
        response.starts_with("HTTP/1.1 405 "),
        "unexpected response: {}",
        response.lines().next().unwrap_or_default()
    );
}

#[tokio::test]
async fn an_unknown_path_is_a_status_failure() {
    // An unrelated path must not be answered with a JSON-RPC frame; the
    // rejection surfaces as an HTTP status failure.
    let fixture = HttpFixture::start(&[]).await;
    let settings = fixture.settings("/nope", &[], Some(Duration::from_secs(10)));

    let error = connect_error(&settings).await;
    assert_eq!(error.failure, McpFailureClass::HttpStatus);
    assert!(error.message.contains("404"), "{}", error.message);
    assert!(!error.is_connection_loss());
}
