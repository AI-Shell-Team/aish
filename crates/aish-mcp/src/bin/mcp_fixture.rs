//! Test fixture: a miniature MCP server that speaks both transports the
//! aish client supports (newline-delimited JSON-RPC over stdio, and Streamable
//! HTTP with JSON or SSE responses).
//!
//! Never shipped and never started by aish itself: only
//! `crates/aish-mcp/tests/*` spawn it, and every condition a test needs to
//! provoke (crash, malformed frame, pagination, credentials, sessions, ...) is
//! selected from the command line. There is no dependency beyond the ones the
//! crate already has; the HTTP side is a hand-written minimal HTTP/1.1 server,
//! because a fixture must not pull a web framework into the dependency graph.
//!
//! Protocol surface (see `crates/aish-mcp/src/client.rs` for the client side):
//!
//! - `initialize` always answers with [`PROTOCOL_VERSION`]; the revision the
//!   client asked for is ignored (no version negotiation to fail on).
//! - `notifications/initialized` is where `crash-after-init` kills the process.
//! - `tools/list` serves eight tools, optionally split into two pages.
//! - `tools/call` dispatches on the tool name.
//! - every other method gets JSON-RPC `-32601`.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Protocol revision the fixture advertises.
const PROTOCOL_VERSION: &str = "2025-11-25";
/// Session id handed out in `--session` mode.
const SESSION_ID: &str = "fixture-session";
/// Tools per page in `--tools-paginated` mode (eight tools, so exactly two).
const PAGE_SIZE: usize = 4;
/// Cursor the first page points at.
const PAGE_CURSOR: &str = "page-2";
/// `slow` sleeps this long when the call passes no `ms`.
const DEFAULT_SLOW_MS: u64 = 60_000;
/// `big` returns this many `x` characters when the call passes no `bytes`.
const DEFAULT_BIG_BYTES: usize = 200_000;
/// Base64 of the 16 bytes `00 01 .. 0f`: a valid 16-byte PNG payload for the
/// `image` tool without pulling in a base64 dependency.
const PNG_BASE64_16: &str = "AAECAwQFBgcICQoLDA0ODw==";
/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC "unknown tool" (MCP reserves -32002 for exactly this).
const UNKNOWN_TOOL: i64 = -32002;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Stdio,
    Http,
}

/// Misbehavior a test asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behavior {
    /// A well-behaved server.
    Normal,
    /// `exit(3)` on `notifications/initialized`.
    CrashAfterInit,
    /// `exit(4)` without answering the first `tools/call`.
    ExitOnCall,
    /// Answer the first `tools/call` with a line that is not JSON.
    BadJson,
    /// Emit `notifications/tools/list_changed` after a `tools/list` response.
    NotifyListChanged,
}

#[derive(Clone, Debug)]
struct Config {
    mode: Mode,
    port: u16,
    behavior: Behavior,
    require_token: Option<String>,
    sse: bool,
    session: bool,
    tools_paginated: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Stdio,
            port: 0,
            behavior: Behavior::Normal,
            require_token: None,
            sse: false,
            session: false,
            tools_paginated: false,
        }
    }
}

/// State that outlives a single message (HTTP serves connections concurrently).
struct Shared {
    bad_json_sent: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            bad_json_sent: AtomicBool::new(false),
        }
    }
}

fn usage() -> &'static str {
    "usage: aish-mcp-fixture [--mode stdio|http] [--port N] [--behavior NAME]\n\
     \x20                       [--require-token TOKEN] [--sse] [--session]\n\
     \x20                       [--tools-paginated]\n\
     behaviors: normal, crash-after-init, exit-on-call, bad-json, notify-list-changed"
}

fn parse_args<I>(args: I) -> Result<Config, String>
where
    I: IntoIterator<Item = String>,
{
    let mut config = Config::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                let value = next_value(&mut args, "--mode")?;
                config.mode = match value.as_str() {
                    "stdio" => Mode::Stdio,
                    "http" => Mode::Http,
                    other => return Err(format!("unknown --mode `{other}`")),
                };
            }
            "--port" => {
                let value = next_value(&mut args, "--port")?;
                config.port = value
                    .parse()
                    .map_err(|_| format!("invalid --port `{value}`"))?;
            }
            "--behavior" => {
                let value = next_value(&mut args, "--behavior")?;
                config.behavior = match value.as_str() {
                    "normal" => Behavior::Normal,
                    "crash-after-init" => Behavior::CrashAfterInit,
                    "exit-on-call" => Behavior::ExitOnCall,
                    "bad-json" => Behavior::BadJson,
                    "notify-list-changed" => Behavior::NotifyListChanged,
                    other => return Err(format!("unknown --behavior `{other}`")),
                };
            }
            "--require-token" => {
                config.require_token = Some(next_value(&mut args, "--require-token")?);
            }
            "--sse" => config.sse = true,
            "--session" => config.session = true,
            "--tools-paginated" => config.tools_paginated = true,
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(config)
}

fn next_value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next().ok_or_else(|| format!("`{flag}` needs a value"))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("aish-mcp-fixture: {message}");
            eprintln!("{}", usage());
            std::process::exit(2);
        }
    };

    match config.mode {
        Mode::Stdio => run_stdio(config).await,
        Mode::Http => run_http(config).await,
    }
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Answer one JSON-RPC message. Returns every frame the server sends back, in
/// order: a request gets one response, a notification none, and
/// `notify-list-changed` adds one extra notification after the response.
async fn handle_message(message: &Value, config: &Config, shared: &Shared) -> Vec<Value> {
    let id = message.get("id").cloned();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    match method.as_str() {
        // The client's requested revision is deliberately ignored: the fixture
        // answers with the revision it implements.
        "initialize" => vec![success(id, initialize_result())],
        "notifications/initialized" => {
            if config.behavior == Behavior::CrashAfterInit {
                // The transport must report the process death, not a timeout.
                std::process::exit(3);
            }
            Vec::new()
        }
        "tools/list" => {
            let mut frames = vec![success(id, tools_list(&params, config))];
            if config.behavior == Behavior::NotifyListChanged {
                frames.push(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/tools/list_changed",
                    "params": {},
                }));
            }
            frames
        }
        "tools/call" => tools_call(id, &params, config, shared).await,
        // A notification (no id) may not be answered at all.
        _ => match id {
            Some(id) => vec![failure(Some(id), METHOD_NOT_FOUND, "Method not found")],
            None => Vec::new(),
        },
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": "mcp-fixture", "version": "1.0.0" },
        "instructions": "Fixture server for aish MCP tests",
    })
}

/// One `tools/list` page.
fn tools_list(params: &Value, config: &Config) -> Value {
    let tools = tool_definitions();
    if !config.tools_paginated {
        return json!({ "tools": tools });
    }
    let cursor = params.get("cursor").and_then(Value::as_str);
    match cursor {
        // First page: half the tools plus the cursor for the rest.
        None => json!({
            "tools": tools[..PAGE_SIZE].to_vec(),
            "nextCursor": PAGE_CURSOR,
        }),
        // Second page: the rest, no cursor left.
        Some(_) => json!({ "tools": tools[PAGE_SIZE..].to_vec() }),
    }
}

/// Dispatch one `tools/call`.
async fn tools_call(
    id: Option<Value>,
    params: &Value,
    config: &Config,
    shared: &Shared,
) -> Vec<Value> {
    if config.behavior == Behavior::ExitOnCall {
        // Die without answering: the caller must see a connection loss.
        std::process::exit(4);
    }
    if config.behavior == Behavior::BadJson
        && config.mode == Mode::Stdio
        && !shared.bad_json_sent.swap(true, Ordering::SeqCst)
    {
        // A line that is not JSON: the transport must reject the whole stream.
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(b"not json\n");
        let _ = stdout.flush();
        return Vec::new();
    }

    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let result = match name.as_str() {
        "echo" => text_result(text_arg(&args, "text")),
        "read_note" => text_result(format!("[fixture] note at {}", text_arg(&args, "path"))),
        "write_note" => {
            let path = text_arg(&args, "path");
            let content = text_arg(&args, "content");
            text_result(format!("wrote {} bytes to {}", content.len(), path))
        }
        "slow" => {
            let ms = args
                .get("ms")
                .and_then(Value::as_f64)
                .map(|value| value.max(0.0) as u64)
                .unwrap_or(DEFAULT_SLOW_MS);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            text_result(format!("slept {ms}ms"))
        }
        "fail" => json!({
            "content": [{ "type": "text", "text": "fixture failure: nothing to do" }],
            "isError": true,
        }),
        "big" => {
            let bytes = args
                .get("bytes")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_BIG_BYTES as u64) as usize;
            text_result("x".repeat(bytes))
        }
        "structured" => json!({
            "content": [{ "type": "text", "text": "rows: 3" }],
            "structuredContent": { "rows": 3, "ok": true },
        }),
        "image" => json!({
            "content": [{ "type": "image", "mimeType": "image/png", "data": PNG_BASE64_16 }],
        }),
        other => return vec![failure(id, UNKNOWN_TOOL, format!("Unknown tool: {other}"))],
    };

    vec![success(id, result)]
}

fn text_arg(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

/// The fixed tool set, in listing order.
fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "echo",
            "description": "Echo the given text back.",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            },
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "read_note",
            "description": "Read a note by path.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            },
        }),
        json!({
            "name": "write_note",
            "description": "Write a note, replacing its content.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                },
                "required": ["path", "content"],
            },
            "annotations": { "destructiveHint": true },
        }),
        json!({
            "name": "slow",
            "description": "Sleep before answering; for deadline and cancel tests.",
            "inputSchema": {
                "type": "object",
                "properties": { "ms": { "type": "number" } },
            },
        }),
        json!({
            "name": "fail",
            "description": "Always report a tool-level error.",
            "inputSchema": { "type": "object" },
        }),
        json!({
            "name": "big",
            "description": "Return a large text block.",
            "inputSchema": {
                "type": "object",
                "properties": { "bytes": { "type": "number" } },
            },
        }),
        json!({
            "name": "structured",
            "description": "Return text plus structured content.",
            "inputSchema": { "type": "object" },
        }),
        json!({
            "name": "image",
            "description": "Return a 16-byte PNG image block.",
            "inputSchema": { "type": "object" },
        }),
    ]
}

fn envelope() -> Map<String, Value> {
    let mut frame = Map::with_capacity(3);
    frame.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    frame
}

fn success(id: Option<Value>, result: Value) -> Value {
    let mut frame = envelope();
    frame.insert("id".to_string(), id.unwrap_or(Value::Null));
    frame.insert("result".to_string(), result);
    Value::Object(frame)
}

fn failure(id: Option<Value>, code: i64, message: impl Into<String>) -> Value {
    let mut frame = envelope();
    frame.insert("id".to_string(), id.unwrap_or(Value::Null));
    frame.insert(
        "error".to_string(),
        json!({ "code": code, "message": message.into() }),
    );
    Value::Object(frame)
}

/// Expand one decoded JSON value into the messages it carries (JSON-RPC
/// batches are arrays).
fn messages_of(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    }
}

// ---------------------------------------------------------------------------
// stdio transport: one JSON frame per line on stdin/stdout
// ---------------------------------------------------------------------------

async fn run_stdio(config: Config) {
    let shared = Shared::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    // EOF on stdin means the client is gone: exit quietly.
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            eprintln!("aish-mcp-fixture: ignoring a non-JSON input line");
            continue;
        };

        let mut outgoing: Vec<Value> = Vec::new();
        for message in messages_of(&value) {
            outgoing.extend(handle_message(message, &config, &shared).await);
        }
        if outgoing.is_empty() {
            continue;
        }
        let mut batch = String::new();
        for frame in outgoing {
            batch.push_str(&frame.to_string());
            batch.push('\n');
        }
        if stdout.write_all(batch.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP transport: minimal HTTP/1.1 over one request per connection
// ---------------------------------------------------------------------------

async fn run_http(config: Config) {
    let listener = match TcpListener::bind(("127.0.0.1", config.port)).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "aish-mcp-fixture: cannot bind port {}: {error}",
                config.port
            );
            std::process::exit(2);
        }
    };
    let port = match listener.local_addr() {
        Ok(address) => address.port(),
        Err(error) => {
            eprintln!("aish-mcp-fixture: cannot read the bound port: {error}");
            std::process::exit(2);
        }
    };
    // The tests parse this line to learn the ephemeral port.
    println!("LISTENING {port}");
    let _ = std::io::stdout().flush();

    let config = Arc::new(config);
    let shared = Arc::new(Shared::new());
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let config = Arc::clone(&config);
        let shared = Arc::clone(&shared);
        // One task per connection: the client may hold several.
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, &config, &shared).await {
                eprintln!("aish-mcp-fixture: connection error: {error}");
            }
        });
    }
}

/// One parsed HTTP request head.
struct RequestHead {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
}

async fn handle_connection(
    mut stream: TcpStream,
    config: &Config,
    shared: &Shared,
) -> std::io::Result<()> {
    let Some((head, body)) = read_request(&mut stream).await? else {
        return Ok(());
    };

    let reply = match (head.method.as_str(), head.path.as_str()) {
        ("POST", "/mcp") => {
            // `--require-token`: the header must be exactly `Bearer <token>`.
            if let Some(token) = &config.require_token {
                let received = head.headers.get("authorization").cloned();
                if received.as_deref() != Some(format!("Bearer {token}").as_str()) {
                    let body = unauthorized_body(received.as_deref());
                    Some(Reply::new(
                        401,
                        "Unauthorized",
                        "text/plain",
                        body.into_bytes(),
                    ))
                } else {
                    handle_post(&body, config, shared).await
                }
            } else {
                handle_post(&body, config, shared).await
            }
        }
        ("DELETE", "/mcp") => {
            // The client terminates a session with DELETE; prove it carried the
            // id the fixture handed out, or that it never got one.
            let session = head
                .headers
                .get("mcp-session-id")
                .cloned()
                .unwrap_or_else(|| "none".to_string());
            println!("DELETED {session}");
            let _ = std::io::stdout().flush();
            Some(Reply::empty(204, "No Content"))
        }
        ("GET", "/mcp") => Some(Reply::new(
            405,
            "Method Not Allowed",
            "text/plain",
            b"method not allowed".to_vec(),
        )),
        _ => Some(Reply::new(
            404,
            "Not Found",
            "text/plain",
            b"not found".to_vec(),
        )),
    };

    if let Some(reply) = reply {
        let mut extra: Vec<(&str, String)> = Vec::new();
        if config.session && reply.status < 300 && head.method == "POST" {
            extra.push(("Mcp-Session-Id", SESSION_ID.to_string()));
        }
        write_reply(&mut stream, reply, &extra).await?;
    }
    Ok(())
}

/// The 401 body. The received credential is echoed the way an HTTP server
/// would log it, so the client's redaction is exercised by the tests.
fn unauthorized_body(received: Option<&str>) -> String {
    format!(
        "unauthorized: Authorization: {}",
        received.unwrap_or("(missing)")
    )
}

struct Reply {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn new(status: u16, reason: &'static str, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            reason,
            content_type,
            body,
        }
    }

    fn empty(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }
}

async fn write_reply(
    stream: &mut TcpStream,
    reply: Reply,
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reply.reason);
    head.push_str(&format!("Content-Type: {}\r\n", reply.content_type));
    head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // One request per connection keeps the parser trivial and is all the
    // client needs.
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    if !reply.body.is_empty() {
        stream.write_all(&reply.body).await?;
    }
    stream.flush().await
}

/// Read one request head (until `\r\n\r\n`) plus its `Content-Length` body.
async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<(RequestHead, Vec<u8>)>> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(position) = find_head_end(&buffer) {
            break position;
        }
        if buffer.len() > 64 * 1024 {
            return Ok(None);
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let path = target.split('?').next().unwrap_or_default().to_string();

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);

    Ok(Some((
        RequestHead {
            method,
            path,
            headers,
        },
        body,
    )))
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Handle one JSON-RPC POST: answer requests, accept notifications silently.
async fn handle_post(body: &[u8], config: &Config, shared: &Shared) -> Option<Reply> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Some(Reply::new(
            400,
            "Bad Request",
            "application/json",
            br#"{"error":"invalid json"}"#.to_vec(),
        ));
    };
    let batched = value.is_array();

    let mut responses: Vec<Value> = Vec::new();
    for message in messages_of(&value) {
        for frame in handle_message(message, config, shared).await {
            // A frame without an id is a server notification, not an answer.
            if frame.get("id").is_some() {
                responses.push(frame);
            }
        }
    }

    if responses.is_empty() {
        // Notifications and responses only: HTTP 202 with no body is what the
        // Streamable HTTP transport expects.
        return Some(Reply::empty(202, "Accepted"));
    }

    if config.sse {
        let mut body = String::new();
        for response in &responses {
            body.push_str("event: message\ndata: ");
            body.push_str(&response.to_string());
            body.push_str("\n\n");
        }
        return Some(Reply::new(
            200,
            "OK",
            "text/event-stream",
            body.into_bytes(),
        ));
    }

    let payload = if batched {
        Value::Array(responses)
    } else {
        responses.remove(0)
    };
    Some(Reply::new(
        200,
        "OK",
        "application/json",
        payload.to_string().into_bytes(),
    ))
}
