//! End-to-end: a real MCP fixture server, through the manager and the gateway
//! tool, driven by the same `LlmSession` preflight/execute path the shell uses.
//!
//! These tests are the integration contract for issue #564: tool discovery and
//! calling work against a real subprocess, and the approval policy really
//! gates the call instead of being decorative.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aish_config::{McpConfig, McpServerConfig};
use aish_llm::approval_memory::ApprovalMemory;
use aish_llm::{ApprovalChoice, LlmSession};
use serde_json::json;

const SERVER: &str = "fixture";

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_aish-mcp-fixture").to_string()
}

fn server_entry(args: &[&str], trust: &str, side_effects: &str) -> McpServerConfig {
    McpServerConfig {
        name: SERVER.to_string(),
        transport: "stdio".to_string(),
        command: Some(fixture_path()),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        trust: trust.to_string(),
        side_effects: side_effects.to_string(),
        request_timeout_ms: Some(5_000),
        ..McpServerConfig::default()
    }
}

/// Build a manager over one fixture server and wait until its tools are known.
async fn connected_manager(entry: McpServerConfig) -> Arc<aish_mcp::McpManager> {
    let mut config = McpConfig::default();
    config.servers.push(entry);
    let (settings, warnings) = aish_mcp::resolve_settings(&config);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let manager = aish_mcp::McpManager::new(settings);
    manager.start(&tokio::runtime::Handle::current());
    let tools = tokio::time::timeout(
        Duration::from_secs(10),
        manager.cached_or_fetch_tools(SERVER, None),
    )
    .await
    .expect("fixture server must connect")
    .expect("tools/list must succeed");
    assert!(tools.iter().any(|tool| tool.name == "echo"));
    manager
}

/// A session with the gateway tool registered and a counting approval callback.
fn session_with_gateway(manager: &Arc<aish_mcp::McpManager>) -> (LlmSession, Arc<AtomicUsize>) {
    let mut session = LlmSession::new(
        "http://127.0.0.1:9/v1",
        "test-key",
        "test-model",
        None,
        None,
    );
    let prompts = Arc::new(AtomicUsize::new(0));
    let counter = prompts.clone();
    session.set_confirmation_callback(Arc::new(move |_ctx| {
        counter.fetch_add(1, Ordering::SeqCst);
        ApprovalChoice::Once
    }));
    for tool in aish_mcp::gateway_tools(manager) {
        session.register_shared_tool(Arc::from(tool));
    }
    (session, prompts)
}

fn call_args(tool: &str, arguments: serde_json::Value) -> serde_json::Value {
    json!({"action": "call", "tool": tool, "arguments": arguments})
}

/// A session with an explicit tool list registered (direct exposure mode).
fn session_with_tools(tools: Vec<Box<dyn aish_llm::Tool>>) -> (LlmSession, Arc<AtomicUsize>) {
    let mut session = LlmSession::new(
        "http://127.0.0.1:9/v1",
        "test-key",
        "test-model",
        None,
        None,
    );
    let prompts = Arc::new(AtomicUsize::new(0));
    let counter = prompts.clone();
    session.set_confirmation_callback(Arc::new(move |_ctx| {
        counter.fetch_add(1, Ordering::SeqCst);
        ApprovalChoice::Once
    }));
    for tool in tools {
        session.register_shared_tool(Arc::from(tool));
    }
    (session, prompts)
}

/// `expose: tools` mode: one tool per remote tool, with its own schema, driven
/// through the same preflight/execute path the shell uses.
#[tokio::test]
async fn direct_tools_expose_the_catalogue_and_keep_the_trust_policy() {
    let manager = connected_manager(server_entry(&[], "allow", "read")).await;
    let catalogue = manager
        .cached_or_fetch_tools(SERVER, None)
        .await
        .expect("catalogue");
    let settings = manager
        .servers()
        .into_iter()
        .find(|settings| settings.name == SERVER)
        .expect("settings");
    let runtime: Arc<dyn aish_mcp::McpRuntime> = manager.clone();
    let tools = aish_mcp::direct_tools(runtime, &settings, &catalogue);
    let names: Vec<String> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert!(
        names.contains(&"mcp__fixture_echo".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"mcp__fixture_write_note".to_string()),
        "{names:?}"
    );

    let (session, prompts) = session_with_tools(tools);

    // `echo` reports `readOnlyHint` and the server is trusted read-only: no prompt.
    let echoed = session
        .execute_tool_by_name("mcp__fixture_echo", json!({"text": "direct-hi"}))
        .await
        .expect("direct call");
    assert!(echoed.ok, "{}", echoed.output);
    assert!(echoed.output.contains("direct-hi"), "{}", echoed.output);
    assert_eq!(prompts.load(Ordering::SeqCst), 0);

    // `write_note` reports `destructiveHint`, so the declaration is narrowed
    // and the call asks even though the server is trusted.
    let written = session
        .execute_tool_by_name(
            "mcp__fixture_write_note",
            json!({"path": "/tmp/aish-mcp-direct.txt", "content": "x"}),
        )
        .await
        .expect("direct call");
    assert!(written.ok, "{}", written.output);
    assert_eq!(prompts.load(Ordering::SeqCst), 1);

    manager.shutdown().await;
}

#[tokio::test]
async fn list_and_describe_expose_the_catalogue_without_approval() {
    let manager = connected_manager(server_entry(&[], "ask", "unknown")).await;
    let (session, prompts) = session_with_gateway(&manager);

    let listed = session
        .execute_tool_by_name(&format!("mcp__{SERVER}"), json!({"action": "list"}))
        .await
        .expect("list must run");
    assert!(listed.ok, "{}", listed.output);
    assert!(listed.output.contains("echo"), "{}", listed.output);
    assert!(listed.output.contains("write_note"), "{}", listed.output);

    let described = session
        .execute_tool_by_name(
            &format!("mcp__{SERVER}"),
            json!({"action": "describe", "tool": "write_note"}),
        )
        .await
        .expect("describe must run");
    assert!(described.ok, "{}", described.output);
    assert!(described.output.contains("inputSchema") || described.output.contains("properties"));

    // Discovery is a read-only protocol operation: never gated.
    assert_eq!(prompts.load(Ordering::SeqCst), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn call_returns_the_remote_tool_result() {
    let manager = connected_manager(server_entry(&[], "allow", "read")).await;
    let (session, prompts) = session_with_gateway(&manager);

    let result = session
        .execute_tool_by_name(
            &format!("mcp__{SERVER}"),
            call_args("echo", json!({"text": "hello from mcp"})),
        )
        .await
        .expect("call must run");

    assert!(result.ok, "{}", result.output);
    assert!(
        result.output.contains("hello from mcp"),
        "{}",
        result.output
    );
    assert_eq!(result.meta.as_ref().unwrap()["command_status"], "ok");
    assert_eq!(result.meta.as_ref().unwrap()["mcp_tool"], "echo");
    // `trust: allow` + declared read-only + `readOnlyHint: true`: no prompt.
    assert_eq!(prompts.load(Ordering::SeqCst), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn undeclared_side_effects_require_approval() {
    // The user declared nothing (`unknown`), so nothing may auto-run.
    let manager = connected_manager(server_entry(&[], "allow", "unknown")).await;
    let (session, prompts) = session_with_gateway(&manager);

    let result = session
        .execute_tool_by_name(
            &format!("mcp__{SERVER}"),
            call_args("echo", json!({"text": "needs a yes"})),
        )
        .await
        .expect("call must run");

    assert!(result.ok, "{}", result.output);
    assert_eq!(prompts.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
}

#[tokio::test]
async fn server_annotations_narrow_a_read_only_declaration() {
    // The user declared the server read-only, but `write_note` reports
    // `destructiveHint: true`; the stricter of the two wins.
    let manager = connected_manager(server_entry(&[], "allow", "read")).await;
    let (session, prompts) = session_with_gateway(&manager);

    let result = session
        .execute_tool_by_name(
            &format!("mcp__{SERVER}"),
            call_args(
                "write_note",
                json!({"path": "/tmp/aish-mcp-test.txt", "content": "x"}),
            ),
        )
        .await
        .expect("call must run");

    assert!(result.ok, "{}", result.output);
    assert_eq!(prompts.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
}

#[tokio::test]
async fn per_tool_block_refuses_before_calling_the_server() {
    let mut entry = server_entry(&[], "allow", "read");
    entry
        .tools
        .insert("write_note".to_string(), "block".to_string());
    let manager = connected_manager(entry).await;
    let (session, prompts) = session_with_gateway(&manager);

    let result = session
        .execute_tool_by_name(
            &format!("mcp__{SERVER}"),
            call_args("write_note", json!({"path": "/tmp/nope", "content": "x"})),
        )
        .await
        .expect("preflight must answer");

    assert!(!result.ok);
    assert_eq!(prompts.load(Ordering::SeqCst), 0);
    // A refused call never reaches the server: `echo` still works afterwards.
    let echo = session
        .execute_tool_by_name(
            &format!("mcp__{SERVER}"),
            call_args("echo", json!({"text": "still alive"})),
        )
        .await
        .expect("call must run");
    assert!(echo.ok, "{}", echo.output);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_failing_remote_tool_does_not_report_a_dispatch_failure() {
    let manager = connected_manager(server_entry(&[], "allow", "read")).await;
    let (session, _prompts) = session_with_gateway(&manager);

    let result = session
        .execute_tool_by_name(&format!("mcp__{SERVER}"), call_args("fail", json!({})))
        .await
        .expect("call must run");

    assert!(!result.ok);
    assert!(
        result.output.contains("fixture failure"),
        "{}",
        result.output
    );
    // The tool ran and reported a tool-level error: the status must report the
    // remote command's failure, not that aish failed to dispatch the call.
    let meta = result.meta.as_ref().expect("mcp results carry meta");
    assert_eq!(meta["command_status"], "exit_nonzero");
    assert_eq!(meta["mcp_server"], SERVER);
    assert_eq!(meta["mcp_tool"], "fail");
    manager.shutdown().await;
}

#[tokio::test]
async fn changed_arguments_invalidate_a_remembered_approval() {
    let manager = connected_manager(server_entry(&[], "allow", "unknown")).await;
    let tools = aish_mcp::gateway_tools(&manager);
    let gateway = tools
        .iter()
        .find(|tool| tool.name() == format!("mcp__{SERVER}"))
        .expect("gateway tool");
    let tool = gateway.as_ref();

    let first = tool.approval_memory_key(&call_args("echo", json!({"text": "a"})));
    let same = tool.approval_memory_key(&call_args("echo", json!({"text": "a"})));
    let changed = tool.approval_memory_key(&call_args("echo", json!({"text": "b"})));
    let other_tool = tool.approval_memory_key(&call_args("read_note", json!({"path": "a"})));

    assert!(first.is_some());
    assert_eq!(first, same, "the same arguments must reuse the approval");
    assert_ne!(
        first, changed,
        "changed arguments must require a new approval"
    );
    assert_ne!(first, other_tool);
    assert!(tool
        .approval_memory_key(&json!({"action": "list"}))
        .is_none());
    manager.shutdown().await;
}

/// "Remember this session" is honoured for MCP calls through the session's
/// approval memory, which the MCP preflight consults *after* its block arm —
/// the session-level shortcut deliberately skips MCP tools, so a remembered
/// "yes" can never outvote `trust: block`.
#[tokio::test]
async fn remembered_session_approval_skips_the_panel_and_never_outvotes_a_block() {
    let manager = connected_manager(server_entry(&[], "ask", "read")).await;
    let mut session = LlmSession::new("http://127.0.0.1:9/v1", "key", "model", None, None);
    let prompts = Arc::new(AtomicUsize::new(0));
    let counter = prompts.clone();
    session.set_confirmation_callback(Arc::new(move |_ctx| {
        counter.fetch_add(1, Ordering::SeqCst);
        ApprovalChoice::RememberSession
    }));
    session.set_approval_memory(Arc::new(parking_lot::Mutex::new(ApprovalMemory::new())));
    for tool in aish_mcp::gateway_tools(&manager) {
        session.register_shared_tool(Arc::from(tool));
    }
    let gateway = format!("mcp__{SERVER}");

    let first = session
        .execute_tool_by_name(&gateway, call_args("echo", json!({"text": "a"})))
        .await
        .expect("call must run");
    assert!(first.ok, "{}", first.output);
    assert_eq!(
        prompts.load(Ordering::SeqCst),
        1,
        "declared read alone asks"
    );

    // Same exact call: the remembered approval applies, so no second panel.
    let repeated = session
        .execute_tool_by_name(&gateway, call_args("echo", json!({"text": "a"})))
        .await
        .expect("call must run");
    assert!(repeated.ok, "{}", repeated.output);
    assert_eq!(
        prompts.load(Ordering::SeqCst),
        1,
        "the remembered approval must skip the panel"
    );

    // Different arguments carry a different key and must ask again.
    let changed = session
        .execute_tool_by_name(&gateway, call_args("echo", json!({"text": "b"})))
        .await
        .expect("call must run");
    assert!(changed.ok, "{}", changed.output);
    assert_eq!(prompts.load(Ordering::SeqCst), 2);

    // The same remembered call, on a tool configured to refuse: block wins.
    let blocked_settings = {
        let mut settings = manager.server_settings(SERVER).expect("server settings");
        settings.trust = aish_mcp::TrustLevel::Block;
        settings
    };
    let blocked_tool = aish_mcp::McpGatewayTool::new(manager.clone(), blocked_settings);
    let mut blocked_session = LlmSession::new("http://127.0.0.1:9/v1", "key", "model", None, None);
    let blocked_prompts = Arc::new(AtomicUsize::new(0));
    let seen = blocked_prompts.clone();
    blocked_session.set_confirmation_callback(Arc::new(move |_ctx| {
        seen.fetch_add(1, Ordering::SeqCst);
        ApprovalChoice::Once
    }));
    blocked_session.set_approval_memory(Arc::new(parking_lot::Mutex::new(ApprovalMemory::new())));
    blocked_session
        .register_shared_tool(Arc::from(Box::new(blocked_tool) as Box<dyn aish_llm::Tool>));
    let refused = blocked_session
        .execute_tool_by_name(&gateway, call_args("echo", json!({"text": "a"})))
        .await
        .expect("the block must be reported, not raised");
    assert!(!refused.ok, "{}", refused.output);
    assert_eq!(blocked_prompts.load(Ordering::SeqCst), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn status_snapshot_never_exposes_credentials() {
    let mut entry = server_entry(&[], "ask", "network");
    entry.env.insert(
        "FIXTURE_TOKEN".to_string(),
        "super-secret-value".to_string(),
    );
    entry.headers = BTreeMap::new();
    let manager = connected_manager(entry).await;

    let rendered = aish_mcp::render_status(&manager).join("\n");
    assert!(!rendered.contains("super-secret-value"), "{rendered}");
    let summary = aish_mcp::render_summary_line(&manager).expect("summary");
    assert!(summary.contains("1"), "{summary}");
    manager.shutdown().await;
}
