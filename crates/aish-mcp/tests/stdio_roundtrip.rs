//! End-to-end tests of the stdio transport and the protocol layer against the
//! real fixture server (`aish-mcp-fixture`).
//!
//! Every test spawns a genuine child process, speaks real JSONL over its
//! pipes and asserts on the failure taxonomy the way the manager consumes it.
//! The fixture's misbehaviors (`--behavior ...`) are what make the failure
//! paths - timeout, cancellation, crash, malformed frame - reproducible.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aish_llm::CancellationToken;
use aish_mcp::client::{connect, ConnectOptions, ServerEvent};
use aish_mcp::config::ServerSettings;
use aish_mcp::types::{
    render_tool_result, McpContent, McpFailureClass, SideEffects, TransportKind, TrustLevel,
    DEFAULT_MAX_OUTPUT_BYTES,
};
use aish_mcp::{CancelListener, Connection};
use serde_json::json;
use tokio::sync::mpsc;

/// The fixture binary cargo built for this test run.
const FIXTURE: &str = env!("CARGO_BIN_EXE_aish-mcp-fixture");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Settings for a stdio fixture with `args` appended to the mode flag.
fn stdio_settings(args: &[&str], request_timeout: Option<Duration>) -> ServerSettings {
    ServerSettings {
        name: "fixture".to_string(),
        transport: TransportKind::Stdio,
        enabled: true,
        command: Some(FIXTURE.to_string()),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        env: BTreeMap::new(),
        cwd: None,
        url: None,
        headers: BTreeMap::new(),
        request_timeout,
        trust: TrustLevel::Allow,
        side_effects: SideEffects::Read,
        tool_trust: BTreeMap::new(),
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    }
}

/// The roots the client advertises to the server.
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

/// Connect and expect the handshake to fail.
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
async fn handshake_list_tools_and_call() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    // serverInfo/instructions come from `initialize`; the configured name is
    // what status output and audit records use.
    assert_eq!(connection.server_info().name, "mcp-fixture");
    assert_eq!(connection.server_info().version, "1.0.0");
    assert_eq!(
        connection.instructions(),
        Some("Fixture server for aish MCP tests")
    );
    assert_eq!(connection.server_name(), "fixture");
    assert_eq!(connection.protocol_version(), "2025-11-25");
    assert!(connection.is_alive());

    let tools = connection.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.len(), 8, "the fixture exposes eight tools");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "echo",
            "read_note",
            "write_note",
            "slow",
            "fail",
            "big",
            "structured",
            "image"
        ]
    );

    // Annotations are the side-effect hints the approval path consumes.
    let annotation = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("tool `{name}` must be listed"))
            .annotations
    };
    assert_eq!(annotation("echo").read_only_hint, Some(true));
    assert!(annotation("read_note").is_empty());
    assert_eq!(annotation("write_note").destructive_hint, Some(true));
    assert!(annotation("slow").is_empty());
    assert_eq!(
        tools
            .iter()
            .find(|tool| tool.name == "echo")
            .and_then(|tool| tool.input_schema.as_ref())
            .and_then(|schema| schema.get("required"))
            .cloned(),
        Some(json!(["text"]))
    );

    let result = connection
        .call_tool("echo", &json!({ "text": "hello" }), None)
        .await
        .expect("echo must answer");
    assert!(!result.is_error);
    assert_eq!(
        result.content,
        vec![McpContent::Text {
            text: "hello".to_string()
        }]
    );
    assert_eq!(
        render_tool_result(&result, DEFAULT_MAX_OUTPUT_BYTES),
        "hello"
    );

    connection.close().await;
    assert!(!connection.is_alive());
}

#[tokio::test]
async fn paginated_tool_list_is_merged() {
    let settings = stdio_settings(
        &["--mode", "stdio", "--tools-paginated"],
        Some(Duration::from_secs(10)),
    );
    let (connection, _events) = connect_fixture(&settings).await;

    // The client must follow `nextCursor`, so all eight tools are visible even
    // though the server hands them out four at a time.
    let tools = connection.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.len(), 8);
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    for expected in ["echo", "read_note", "write_note", "slow"] {
        assert!(names.contains(&expected), "first page tool `{expected}`");
    }
    for expected in ["fail", "big", "structured", "image"] {
        assert!(names.contains(&expected), "second page tool `{expected}`");
    }

    connection.close().await;
}

#[tokio::test]
async fn tool_level_error_is_not_a_transport_error() {
    // No `--mode` flag: stdio is the fixture's default.
    let settings = stdio_settings(&[], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    let result = connection
        .call_tool("fail", &json!({}), None)
        .await
        .expect("a tool-level failure is still a valid response");
    assert!(result.is_error);
    assert_eq!(text_of(&result), "fixture failure: nothing to do");

    connection.close().await;
}

#[tokio::test]
async fn unknown_tool_is_a_json_rpc_error() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    let error = connection
        .call_tool("nope", &json!({}), None)
        .await
        .expect_err("an unknown tool must fail");
    assert_eq!(error.failure, McpFailureClass::JsonRpc);
    assert_eq!(error.code, Some(-32002));
    assert!(
        error.message.contains("Unknown tool: nope"),
        "{}",
        error.message
    );
    assert!(!error.is_connection_loss());

    connection.close().await;
}

#[tokio::test]
async fn large_output_is_truncated_only_when_rendered() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(30)));
    let (connection, _events) = connect_fixture(&settings).await;

    let result = connection
        .call_tool("big", &json!({ "bytes": 200000 }), None)
        .await
        .expect("big must answer");
    assert_eq!(text_of(&result).len(), 200000, "the payload arrives whole");

    let rendered = render_tool_result(&result, 4096);
    assert!(
        rendered.len() <= 4096 + 64,
        "rendered {} bytes, cap is 4096 plus the marker",
        rendered.len()
    );
    assert!(
        rendered.contains("(truncated at 4096 bytes)"),
        "missing truncation marker: {:?}",
        &rendered[rendered.len().saturating_sub(60)..]
    );
    assert!(rendered.starts_with("xxx"));

    connection.close().await;
}

#[tokio::test]
async fn structured_content_is_not_duplicated_in_the_text() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    let result = connection
        .call_tool("structured", &json!({}), None)
        .await
        .expect("structured must answer");
    assert_eq!(
        result.structured_content,
        Some(json!({ "rows": 3, "ok": true }))
    );

    let rendered = render_tool_result(&result, DEFAULT_MAX_OUTPUT_BYTES);
    assert_eq!(
        rendered.matches("rows: 3").count(),
        1,
        "the text block must appear once: {rendered:?}"
    );
    assert!(
        rendered.contains("\"rows\": 3"),
        "structured rows: {rendered:?}"
    );
    assert!(
        rendered.contains("\"ok\": true"),
        "structured ok: {rendered:?}"
    );

    connection.close().await;
}

#[tokio::test]
async fn image_blocks_render_as_a_placeholder() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(10)));
    let (connection, _events) = connect_fixture(&settings).await;

    let result = connection
        .call_tool("image", &json!({}), None)
        .await
        .expect("image must answer");
    // Aish tool results are text: the binary payload is summarised, and the
    // size is the base64 length scaled back to bytes (padding not subtracted).
    assert_eq!(
        result.content,
        vec![McpContent::Image {
            mime_type: "image/png".to_string(),
            bytes: 18,
        }]
    );
    assert_eq!(
        render_tool_result(&result, DEFAULT_MAX_OUTPUT_BYTES),
        "[image image/png (18 bytes)]"
    );

    connection.close().await;
}

#[tokio::test]
async fn list_changed_notification_reaches_the_owner() {
    let settings = stdio_settings(
        &["--mode", "stdio", "--behavior", "notify-list-changed"],
        Some(Duration::from_secs(10)),
    );
    let (connection, mut events) = connect_fixture(&settings).await;

    let tools = connection.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.len(), 8);

    // The tool cache is invalidated from this event, so it must survive the
    // trip through transport, protocol layer and event loop.
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("a server event must arrive")
        .expect("the event channel must stay open");
    match event {
        ServerEvent::Notification { method, .. } => {
            assert_eq!(method, "notifications/tools/list_changed");
        }
        other => panic!("unexpected server event: {other:?}"),
    }

    connection.close().await;
}

// ---------------------------------------------------------------------------
// Failure paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deadline_expiry_is_a_timeout() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_millis(300)));
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
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(30)));
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
async fn server_crash_surfaces_as_a_connection_loss() {
    let settings = stdio_settings(
        &["--mode", "stdio", "--behavior", "crash-after-init"],
        Some(Duration::from_secs(10)),
    );

    // The fixture dies on `notifications/initialized`, so either the handshake
    // itself notices or the first call does; a deadline is never the answer.
    let (events, _receiver) = mpsc::unbounded_channel();
    let options = ConnectOptions {
        roots: roots(),
        cancel: None,
        events,
    };
    match connect(&settings, options).await {
        Ok(connection) => {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let error = connection
                .call_tool("echo", &json!({ "text": "x" }), None)
                .await
                .expect_err("the server is gone");
            assert_ne!(error.failure, McpFailureClass::Timeout);
            assert!(
                matches!(
                    error.failure,
                    McpFailureClass::Eof | McpFailureClass::Closed | McpFailureClass::Connect
                ),
                "unexpected failure: {}",
                error.describe()
            );
            assert!(error.is_connection_loss(), "{}", error.describe());
            connection.close().await;
        }
        Err(error) => {
            assert_ne!(error.failure, McpFailureClass::Timeout);
            assert!(
                matches!(
                    error.failure,
                    McpFailureClass::Eof | McpFailureClass::Closed | McpFailureClass::Connect
                ),
                "unexpected failure: {}",
                error.describe()
            );
        }
    }
}

#[tokio::test]
async fn a_garbled_frame_is_a_malformed_response() {
    let settings = stdio_settings(
        &["--mode", "stdio", "--behavior", "bad-json"],
        Some(Duration::from_secs(5)),
    );
    let (connection, _events) = connect_fixture(&settings).await;

    let error = connection
        .call_tool("echo", &json!({ "text": "x" }), None)
        .await
        .expect_err("the fixture answers with garbage");
    assert_eq!(error.failure, McpFailureClass::MalformedResponse);
    assert!(
        error.message.contains("invalid JSON-RPC frame"),
        "{}",
        error.message
    );

    connection.close().await;
}

#[tokio::test]
async fn a_dying_server_is_reported_as_connection_loss() {
    let settings = stdio_settings(
        &["--mode", "stdio", "--behavior", "exit-on-call"],
        Some(Duration::from_secs(10)),
    );
    let (connection, _events) = connect_fixture(&settings).await;

    let error = connection
        .call_tool("echo", &json!({ "text": "x" }), None)
        .await
        .expect_err("the fixture dies instead of answering");
    assert!(
        error.is_connection_loss(),
        "expected a connection loss, got {}",
        error.describe()
    );
    assert_ne!(error.failure, McpFailureClass::Timeout);

    connection.close().await;
}

#[tokio::test]
async fn close_releases_the_subprocess() {
    let settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(10)));
    let (first, _events) = connect_fixture(&settings).await;
    assert!(first.is_alive());
    first.close().await;
    assert!(!first.is_alive());
    // Idempotent: a second close must not hang or panic.
    first.close().await;
    drop(first);

    // A fresh instance connects immediately; nothing is left holding the pipe.
    let (second, _events) = connect_fixture(&settings).await;
    let result = second
        .call_tool("echo", &json!({ "text": "again" }), None)
        .await
        .expect("the second instance must answer");
    assert_eq!(text_of(&result), "again");
    second.close().await;
}

#[tokio::test]
async fn a_missing_command_is_a_connect_failure() {
    let mut settings = stdio_settings(&["--mode", "stdio"], Some(Duration::from_secs(5)));
    settings.command = Some("/nonexistent/aish-mcp-fixture".to_string());

    let error = connect_error(&settings).await;
    assert_eq!(error.failure, McpFailureClass::Connect);
    assert!(error.retryable);
}
