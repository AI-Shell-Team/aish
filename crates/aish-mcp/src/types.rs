//! Wire types shared by the MCP client layers: JSON-RPC 2.0 frames, MCP
//! protocol payloads, transport failures and the status snapshot rendered by
//! `/mcp`.
//!
//! Protocol revision: [`MCP_PROTOCOL_VERSION`].

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cancel::CancelListener;

/// Latest MCP protocol revision this client negotiates.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Default per-request deadline when the config does not set one.
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Default cap on the bytes one MCP tool call may return before truncation.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Longest tool name strict providers accept (`^[a-zA-Z0-9_-]{1,64}$`).
pub const MAX_TOOL_NAME_LEN: usize = 64;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0
// ---------------------------------------------------------------------------

/// JSON-RPC request id. Number ids are what this client sends; string ids are
/// accepted because some servers mint their own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(i64),
    String(String),
}

impl std::fmt::Display for JsonRpcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::String(value) => write!(f, "{value}"),
        }
    }
}

/// JSON-RPC error object as carried by a response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)]
    pub jsonrpc: String,
    pub id: JsonRpcId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// One classified inbound frame.
#[derive(Debug, Clone)]
pub enum IncomingFrame {
    /// Response to one of our requests (correlate by `id`).
    Response(JsonRpcResponse),
    /// Server-initiated notification (no id).
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// Server-to-client request, which MUST be answered.
    Request {
        id: JsonRpcId,
        method: String,
        params: serde_json::Value,
    },
}

/// Classify one decoded JSON value into zero or more inbound frames.
///
/// JSON-RPC batches (arrays) are expanded member by member; frames that do not
/// match any JSON-RPC 2.0 shape are dropped, so one malformed message cannot
/// kill an otherwise healthy connection.
pub fn classify_frames(value: &serde_json::Value) -> Vec<IncomingFrame> {
    match value {
        serde_json::Value::Array(items) => items.iter().flat_map(classify_frames).collect(),
        serde_json::Value::Object(map) => {
            if map.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
                return Vec::new();
            }
            let method = map.get("method").and_then(|v| v.as_str());
            let id = map.get("id").and_then(parse_id);
            match (method, id) {
                (Some(method), Some(id)) => vec![IncomingFrame::Request {
                    id,
                    method: method.to_string(),
                    params: map
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                }],
                (Some(method), None) => vec![IncomingFrame::Notification {
                    method: method.to_string(),
                    params: map
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                }],
                (None, Some(id)) => {
                    let response = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: map.get("result").cloned(),
                        error: map.get("error").and_then(parse_error),
                    };
                    vec![IncomingFrame::Response(response)]
                }
                (None, None) => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn parse_id(value: &serde_json::Value) -> Option<JsonRpcId> {
    match value {
        serde_json::Value::Number(number) => number.as_i64().map(JsonRpcId::Number),
        serde_json::Value::String(text) => Some(JsonRpcId::String(text.clone())),
        _ => None,
    }
}

fn parse_error(value: &serde_json::Value) -> Option<JsonRpcError> {
    serde_json::from_value(value.clone()).ok()
}

// ---------------------------------------------------------------------------
// MCP protocol payloads
// ---------------------------------------------------------------------------

/// `serverInfo` from the `initialize` result.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Parsed `initialize` result.
#[derive(Debug, Clone)]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: serde_json::Value,
    pub server_info: ServerInfo,
    pub instructions: Option<String>,
}

impl InitializeResult {
    /// Parse leniently: a server that omits optional fields is still usable.
    pub fn from_value(value: &serde_json::Value) -> Self {
        let server_info = value.get("serverInfo");
        Self {
            protocol_version: value
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(MCP_PROTOCOL_VERSION)
                .to_string(),
            capabilities: value
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            server_info: ServerInfo {
                name: server_info
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                version: server_info
                    .and_then(|v| v.get("version"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            },
            instructions: value
                .get("instructions")
                .and_then(|v| v.as_str())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }
    }
}

/// Tool behavior hints reported by the server (MCP `annotations`).
///
/// Every hint is optional: most real servers send none, in which case the
/// caller falls back to the side effects the user declared in the config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolAnnotations {
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub idempotent_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
}

impl ToolAnnotations {
    /// Parse the `annotations` object of a `tools/list` entry.
    pub fn from_tool(value: &serde_json::Value) -> Self {
        let annotations = value.get("annotations");
        let hint = |key: &str| {
            annotations
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_bool())
        };
        Self {
            read_only_hint: hint("readOnlyHint"),
            destructive_hint: hint("destructiveHint"),
            idempotent_hint: hint("idempotentHint"),
            open_world_hint: hint("openWorldHint"),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.read_only_hint.is_none()
            && self.destructive_hint.is_none()
            && self.idempotent_hint.is_none()
            && self.open_world_hint.is_none()
    }
}

/// One `tools/list` entry.
#[derive(Debug, Clone)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<serde_json::Value>,
    pub annotations: ToolAnnotations,
}

impl McpToolDefinition {
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        let name = value.get("name").and_then(|v| v.as_str())?.to_string();
        if !is_usable_tool_name(&name) {
            return None;
        }
        Some(Self {
            name,
            description: value
                .get("description")
                .and_then(|v| v.as_str())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            input_schema: value.get("inputSchema").cloned(),
            annotations: ToolAnnotations::from_tool(value),
        })
    }
}

/// Tool name that means "the whole server" in the trust picker and in
/// `/mcp trust <server> * <level>`.
///
/// Exported so the shell and the config layer share one spelling, and rejected
/// as a real tool name when a catalogue is parsed, so a server cannot collide
/// with it.
pub const ANY_TOOL: &str = "*";

/// Longest tool name accepted from a server.
///
/// Much larger than [`MAX_TOOL_NAME_LEN`] (which caps the names aish mints) so
/// a long-but-legitimate name still works, while a megabyte-long one cannot
/// bloat the prompt or the picker.
const MAX_SERVER_TOOL_NAME_LEN: usize = 128;

/// True when a server-supplied tool name can be used as an identifier.
///
/// Rejected names are dropped when the catalogue is parsed: a name carrying
/// control characters (including the picker's own `\0` row sentinel) or equal
/// to the picker's `*` row would be ambiguous in the interactive menu, and the
/// model could not address it reliably either.
fn is_usable_tool_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == ANY_TOOL {
        return false;
    }
    if trimmed.chars().count() > MAX_SERVER_TOOL_NAME_LEN {
        return false;
    }
    !trimmed.chars().any(char::is_control)
}

/// One block of a `tools/call` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        bytes: usize,
    },
    Resource {
        uri: String,
        mime_type: Option<String>,
    },
    Other {
        kind: String,
    },
}

/// Parsed `tools/call` result.
#[derive(Debug, Clone, Default)]
pub struct ToolCallResult {
    pub content: Vec<McpContent>,
    pub structured_content: Option<serde_json::Value>,
    /// The server reported a tool-level error (`isError: true`).
    pub is_error: bool,
}

impl ToolCallResult {
    pub fn from_value(value: &serde_json::Value) -> Self {
        let content = value
            .get("content")
            .and_then(|v| v.as_array())
            .map(|items| items.iter().filter_map(parse_content_block).collect())
            .unwrap_or_default();
        Self {
            content,
            structured_content: value.get("structuredContent").cloned(),
            is_error: value
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }
}

fn parse_content_block(value: &serde_json::Value) -> Option<McpContent> {
    let kind = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("other");
    match kind {
        "text" => Some(McpContent::Text {
            text: value
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "image" => Some(McpContent::Image {
            mime_type: value
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream")
                .to_string(),
            bytes: value
                .get("data")
                .and_then(|v| v.as_str())
                .map(|data| data.len() * 3 / 4)
                .unwrap_or(0),
        }),
        "resource" => {
            let resource = value.get("resource").unwrap_or(value);
            Some(McpContent::Resource {
                uri: resource
                    .get("uri")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                mime_type: resource
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string()),
            })
        }
        other => Some(McpContent::Other {
            kind: other.to_string(),
        }),
    }
}

/// Flatten a `tools/call` result into the text aish feeds to the model.
///
/// Binary and non-text blocks become short placeholders (aish tool results are
/// text); `structuredContent` is appended unless a text block already carries
/// it. The rendering is capped at `max_bytes` on a UTF-8 boundary.
pub fn render_tool_result(result: &ToolCallResult, max_bytes: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut text_seen = String::new();
    for block in &result.content {
        match block {
            McpContent::Text { text } => {
                text_seen.push_str(text);
                parts.push(text.clone());
            }
            McpContent::Image { mime_type, bytes } => {
                parts.push(format!("[image {} ({} bytes)]", mime_type, bytes))
            }
            McpContent::Resource { uri, mime_type } => match mime_type {
                Some(mime) => parts.push(format!("[resource {} ({})]", uri, mime)),
                None => parts.push(format!("[resource {}]", uri)),
            },
            McpContent::Other { kind } => parts.push(format!("[{}]", kind)),
        }
    }
    if let Some(structured) = &result.structured_content {
        let rendered = serde_json::to_string_pretty(structured).unwrap_or_default();
        let trimmed = rendered.trim();
        // Do not duplicate structured content a text block already carries.
        if !trimmed.is_empty() && !text_seen.contains(trimmed) {
            parts.push(trimmed.to_string());
        }
    }
    let joined = parts.join("\n");
    let (mut text, truncated) = truncate_utf8(&joined, max_bytes);
    if truncated {
        text.push_str(&format!("\n...(truncated at {} bytes)", max_bytes));
    }
    text
}

/// Truncate `input` to at most `max_bytes` on a UTF-8 character boundary.
///
/// Returns the truncated text and whether anything was dropped.
pub fn truncate_utf8(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    (input[..end].to_string(), true)
}

/// Human-readable argument summary for the approval panel.
///
/// Scalars are shown inline when short, nested containers as bounded compact
/// JSON so the user can see the nested values they are approving, and every
/// rendered value is masked for credentials before it can reach a terminal, an
/// audit record or a log.
pub fn args_summary(args: &serde_json::Value, max_len: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let serde_json::Value::Object(map) = args {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys.iter().take(ARG_SUMMARY_MAX_KEYS) {
            let value = &map[key.as_str()];
            parts.push(format!("{}={}", key, summarize_value(value)));
        }
        if keys.len() > ARG_SUMMARY_MAX_KEYS {
            parts.push(format!("(+{} more)", keys.len() - ARG_SUMMARY_MAX_KEYS));
        }
    } else if !args.is_null() {
        parts.push(summarize_value(args));
    }
    let joined = parts.join(", ");
    // Mask once per value (above) and once on the joined text, where an
    // assignment shape such as `password=hunter2` is recognizable.
    let redacted = crate::redact::sanitize_argument_text(&joined);
    let (text, truncated) = truncate_utf8(&redacted, max_len);
    if truncated {
        format!("{text}...")
    } else {
        text
    }
}

/// Longest scalar string rendered inline; longer ones are shown as a shape.
const ARG_SUMMARY_INLINE_CHARS: usize = 48;

/// Cap on one rendered container value in the approval panel.
///
/// Nested arguments are rendered as compact JSON so their content is visible;
/// this keeps a single large value from pushing the remaining arguments out of
/// the panel.
const ARG_SUMMARY_VALUE_CHARS: usize = 200;

/// Marker appended to a container value cut at [`ARG_SUMMARY_VALUE_CHARS`].
const ARG_SUMMARY_TRUNCATION: &str = "...";

/// Number of arguments rendered before the panel summarizes the rest.
const ARG_SUMMARY_MAX_KEYS: usize = 8;

fn summarize_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => {
            if text.chars().count() <= ARG_SUMMARY_INLINE_CHARS {
                crate::redact::sanitize_argument_text(text)
            } else {
                format!("<{} chars>", text.chars().count())
            }
        }
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => summarize_container(value),
    }
}

/// Render a nested container as compact JSON so the approval panel shows what
/// the model is actually asking to run, bounded and masked for credentials.
///
/// Containers go through [`crate::redact::sanitize_data`] first: it masks
/// values held under secret-looking keys — `{"password": "..."}` is invisible
/// to the textual masking below, which only recognizes `password=...` shapes —
/// and bounds depth, item count and string length. The serialized text then
/// passes the same argument masking as every other rendered value.
fn summarize_container(value: &serde_json::Value) -> String {
    let sanitized = crate::redact::sanitize_data(value);
    // A `Value` always serializes; the fallback only keeps the panel readable.
    let rendered =
        serde_json::to_string(&sanitized).unwrap_or_else(|_| "<unrenderable>".to_string());
    let masked = crate::redact::sanitize_argument_text(&rendered);
    match masked.char_indices().nth(ARG_SUMMARY_VALUE_CHARS) {
        Some((end, _)) => format!("{}{}", &masked[..end], ARG_SUMMARY_TRUNCATION),
        None => masked,
    }
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// Which transport produced a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Stdio,
    Http,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Failure classification, ported from oh-my-pi `mcp/errors.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureClass {
    /// Transport could not be established.
    Connect,
    /// The request deadline expired.
    Timeout,
    /// The peer closed the stream/process.
    Eof,
    /// The connection was reset.
    Reset,
    /// A response could not be decoded into a valid envelope.
    MalformedResponse,
    /// The server returned a JSON-RPC error object.
    JsonRpc,
    /// Non-2xx HTTP status.
    HttpStatus,
    /// The transport was closed while in use.
    Closed,
    /// Protocol-level violation (unexpected handshake answer, bad version).
    Protocol,
    /// The caller cancelled the request.
    Cancelled,
    /// Anything else.
    Unknown,
}

impl McpFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Eof => "eof",
            Self::Reset => "reset",
            Self::MalformedResponse => "malformed_response",
            Self::JsonRpc => "json_rpc",
            Self::HttpStatus => "http_status",
            Self::Closed => "closed",
            Self::Protocol => "protocol",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

/// Where in the request lifecycle a failure happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpFailureStage {
    Connect,
    Send,
    Receive,
    Decode,
    Protocol,
}

impl McpFailureStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Send => "send",
            Self::Receive => "receive",
            Self::Decode => "decode",
            Self::Protocol => "protocol",
        }
    }
}

/// One structured MCP failure.
///
/// Free-text fields are sanitized of credentials before they are stored, so a
/// `McpError` is always safe to display or audit.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct McpError {
    pub server: String,
    pub transport: TransportKind,
    pub stage: McpFailureStage,
    pub failure: McpFailureClass,
    pub retryable: bool,
    pub message: String,
    pub code: Option<i64>,
    pub data: Option<serde_json::Value>,
}

impl McpError {
    pub fn new(
        server: impl Into<String>,
        transport: TransportKind,
        stage: McpFailureStage,
        failure: McpFailureClass,
        retryable: bool,
        message: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            transport,
            stage,
            failure,
            retryable,
            message: crate::redact::sanitize_diagnostic_text(&message.into()),
            code: None,
            data: None,
        }
    }

    pub fn with_code(mut self, code: i64) -> Self {
        self.code = Some(code);
        self
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(crate::redact::sanitize_data(&data));
        self
    }

    /// The server closed the connection or process unexpectedly.
    pub fn eof(server: &str, transport: TransportKind, message: impl Into<String>) -> Self {
        Self::new(
            server,
            transport,
            McpFailureStage::Receive,
            McpFailureClass::Eof,
            true,
            message,
        )
    }

    /// The request deadline expired.
    pub fn timeout(server: &str, transport: TransportKind, timeout: Duration) -> Self {
        Self::new(
            server,
            transport,
            McpFailureStage::Receive,
            McpFailureClass::Timeout,
            false,
            format!("request timeout after {}ms", timeout.as_millis()),
        )
    }

    /// The caller cancelled the request.
    pub fn cancelled(server: &str, transport: TransportKind) -> Self {
        Self::new(
            server,
            transport,
            McpFailureStage::Receive,
            McpFailureClass::Cancelled,
            false,
            "request cancelled by the caller",
        )
    }

    /// The transport was already closed when the request started.
    pub fn closed(server: &str, transport: TransportKind) -> Self {
        Self::new(
            server,
            transport,
            McpFailureStage::Send,
            McpFailureClass::Closed,
            false,
            "transport is closed",
        )
    }

    /// A JSON-RPC error object returned by the server.
    pub fn json_rpc(
        server: &str,
        transport: TransportKind,
        code: i64,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            server,
            transport,
            McpFailureStage::Protocol,
            McpFailureClass::JsonRpc,
            false,
            format!("MCP error {code}: {}", message.into()),
        )
        .with_code(code)
    }

    /// Wrap an `io`/`reqwest` style error into the taxonomy.
    pub fn transport_error(
        server: &str,
        transport: TransportKind,
        stage: McpFailureStage,
        error: &dyn std::error::Error,
    ) -> Self {
        let message = error.to_string();
        let lower = message.to_ascii_lowercase();
        let failure = if lower.contains("timeout") || lower.contains("timed out") {
            McpFailureClass::Timeout
        } else if lower.contains("connection refused")
            || lower.contains("connectionrefused")
            || lower.contains("dns")
            || lower.contains("failed to connect")
            || lower.contains("transport is closed")
        {
            McpFailureClass::Connect
        } else if lower.contains("connection reset") {
            McpFailureClass::Reset
        } else if lower.contains("broken pipe")
            || lower.contains("unexpected eof")
            || lower.contains("closed")
            || lower.contains("end of file")
        {
            McpFailureClass::Eof
        } else if lower.contains("http ") {
            McpFailureClass::HttpStatus
        } else {
            McpFailureClass::Unknown
        };
        let retryable = matches!(
            failure,
            McpFailureClass::Connect | McpFailureClass::Reset | McpFailureClass::Eof
        );
        Self::new(server, transport, stage, failure, retryable, message)
    }

    /// True when the failure means the connection itself is gone (the manager
    /// marks the server as failed once a call reports this).
    pub fn is_connection_loss(&self) -> bool {
        matches!(
            self.failure,
            McpFailureClass::Connect
                | McpFailureClass::Eof
                | McpFailureClass::Reset
                | McpFailureClass::Closed
        )
    }

    /// One-line technical description for status output and logs.
    pub fn describe(&self) -> String {
        let mut text = format!(
            "{} {}: {}",
            self.stage.as_str(),
            self.failure.as_str(),
            self.message
        );
        if let Some(code) = self.code {
            text.push_str(&format!(" (code {code})"));
        }
        text
    }
}

// ---------------------------------------------------------------------------
// Side effects and trust
// ---------------------------------------------------------------------------

/// Side effect scope a call may have.
///
/// Ordering is severity order: `Read` is the only level that can ever be
/// auto-approved; `Unknown` (nothing declared) is treated as the most
/// dangerous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SideEffects {
    Read,
    Network,
    Write,
    Unknown,
}

impl SideEffects {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "read" | "read_only" | "readonly" => Some(Self::Read),
            "network" => Some(Self::Network),
            "write" => Some(Self::Write),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Network => "network",
            Self::Write => "write",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for SideEffects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Trust state recorded per server and per tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// Ask before every call (default).
    Ask,
    /// Auto-approve calls that are read-only; everything else still asks.
    Allow,
    /// Refuse calls; the server stays connected for inspection.
    Block,
}

impl TrustLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "allow" | "trusted" => Some(Self::Allow),
            "block" | "deny" => Some(Self::Block),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::Block => "block",
        }
    }
}

impl std::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Status snapshot
// ---------------------------------------------------------------------------

/// Connection state of one configured server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    /// `enabled: false` or MCP disabled.
    Disabled,
    /// Handshake or `tools/list` in flight.
    Connecting,
    /// Handshake completed.
    Connected {
        protocol_version: String,
        tool_count: usize,
    },
    /// The last attempt failed; `reason` is a redacted technical description.
    Failed { reason: String },
}

impl ServerStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Connecting => "connecting",
            Self::Connected { .. } => "connected",
            Self::Failed { .. } => "failed",
        }
    }
}

/// One discovered tool, as shown by `/mcp tools <server>`.
#[derive(Debug, Clone)]
pub struct ToolSummary {
    pub name: String,
    pub description: Option<String>,
    pub side_effects: SideEffects,
    /// Parameter names the server marks as required in `inputSchema`.
    ///
    /// Carried so the gateway's system-prompt fragment can show them: a model
    /// that only sees tool names guesses the arguments, and servers with
    /// required parameters answer with a validation error instead of a result.
    pub required: Vec<String>,
}

impl ToolSummary {
    /// Required parameter names from a tool's `inputSchema`.
    pub fn required_from_schema(schema: Option<&serde_json::Value>) -> Vec<String> {
        schema
            .and_then(|schema| schema.get("required"))
            .and_then(|required| required.as_array())
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| name.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Everything `/mcp` and `/status` need about one server. Contains no
/// credentials: `endpoint` is redacted and env/header values are never stored.
#[derive(Debug, Clone)]
pub struct ServerSnapshot {
    pub name: String,
    pub transport: TransportKind,
    pub endpoint: String,
    pub status: ServerStatus,
    pub trust: TrustLevel,
    pub declared_side_effects: SideEffects,
    pub tools: Vec<ToolSummary>,
    /// Whether the server sent `instructions` (the text itself is not shown).
    pub instructions: bool,
}

// ---------------------------------------------------------------------------
// Transport abstraction
// ---------------------------------------------------------------------------

/// Server-initiated traffic a transport surfaces to its owner.
#[derive(Debug)]
pub enum TransportEvent {
    /// Notification (no id).
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// Server-to-client request that MUST be answered with
    /// [`McpTransport::respond`].
    Request {
        id: JsonRpcId,
        method: String,
        params: serde_json::Value,
    },
    /// The transport is gone. Emitted at most once, after `is_connected()` has
    /// turned false.
    Closed { error: Option<McpError> },
}

/// Transport layer contract: framing, I/O, deadlines and teardown only.
///
/// Protocol concerns (method order, handshake, id correlation of server
/// requests) live in [`crate::client`]; transports forward every non-response
/// frame as a [`TransportEvent`] and never interpret MCP semantics.
pub trait McpTransport: Send + Sync {
    fn kind(&self) -> TransportKind;

    /// Redacted endpoint label for status output (never a credential).
    fn endpoint_label(&self) -> String;

    /// Send a request and await its response.
    ///
    /// The request is bounded by the transport's configured deadline; `cancel`
    /// aborts it early. Returns the JSON-RPC `result`.
    #[allow(clippy::type_complexity)]
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Option<serde_json::Value>,
        cancel: Option<Arc<CancelListener>>,
    ) -> futures::future::BoxFuture<'a, Result<serde_json::Value, McpError>>;

    /// Send a notification (no response expected).
    fn notify<'a>(
        &'a self,
        method: &'a str,
        params: Option<serde_json::Value>,
    ) -> futures::future::BoxFuture<'a, Result<(), McpError>>;

    /// Answer a server-to-client request.
    fn respond<'a>(
        &'a self,
        id: JsonRpcId,
        result: Result<serde_json::Value, JsonRpcError>,
    ) -> futures::future::BoxFuture<'a, Result<(), McpError>>;

    /// Record the negotiated protocol revision; transports that carry it on
    /// later requests (Streamable HTTP) use it, others ignore it.
    fn set_protocol_version(&self, version: String);

    fn is_connected(&self) -> bool;

    /// Close the transport and release its resources (bounded).
    fn close<'a>(&'a self) -> futures::future::BoxFuture<'a, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(value: serde_json::Value) -> serde_json::Value {
        value
    }

    /// A tool name a server sends must be addressable by the model and by the
    /// interactive pickers, which reserve `\0` (row sentinel) and `*` (whole
    /// server); names that are not are dropped when the catalogue is parsed.
    #[test]
    fn rejects_unaddressable_tool_names() {
        let parse =
            |name: &str| McpToolDefinition::from_value(&serde_json::json!({ "name": name }));
        assert!(parse("read_file").is_some());
        assert!(parse("  spaced  ").is_some(), "trimmed names stay usable");

        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
        assert!(
            parse(ANY_TOOL).is_none(),
            "the whole-server row is reserved"
        );
        assert!(
            parse("\u{0}mcp-tools-list").is_none(),
            "the picker's row sentinel must not be reachable from a server"
        );
        assert!(parse("bad\nname").is_none(), "control characters");
        assert!(parse("bad\u{1b}[31m").is_none(), "escape sequences");
        assert!(
            parse(&"n".repeat(MAX_SERVER_TOOL_NAME_LEN + 1)).is_none(),
            "unbounded names must not reach the prompt or the picker"
        );
        assert!(parse(&"n".repeat(MAX_SERVER_TOOL_NAME_LEN)).is_some());
    }

    #[test]
    fn classifies_response_notification_and_request() {
        let response = classify_frames(&obj(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": {"ok": true}
        })));
        assert!(matches!(response.as_slice(), [IncomingFrame::Response(_)]));

        let notification = classify_frames(&obj(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/tools/list_changed"
        })));
        assert!(matches!(
            notification.as_slice(),
            [IncomingFrame::Notification { .. }]
        ));

        let request = classify_frames(&obj(serde_json::json!({
            "jsonrpc": "2.0", "id": "abc", "method": "ping"
        })));
        assert!(matches!(
            request.as_slice(),
            [IncomingFrame::Request { .. }]
        ));
    }

    #[test]
    fn drops_frames_that_are_not_jsonrpc_2() {
        assert!(classify_frames(&obj(serde_json::json!({"id": 1, "result": {}}))).is_empty());
        assert!(classify_frames(&obj(serde_json::json!("hello"))).is_empty());
        assert!(classify_frames(&obj(serde_json::json!({"jsonrpc": "2.0"}))).is_empty());
    }

    #[test]
    fn expands_batches() {
        let frames = classify_frames(&obj(serde_json::json!([
            {"jsonrpc": "2.0", "id": 1, "result": {}},
            {"jsonrpc": "2.0", "method": "notifications/message"}
        ])));
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn parses_initialize_leniently() {
        let parsed = InitializeResult::from_value(&serde_json::json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "fixture", "version": "1.0.0"},
            "instructions": "  be nice  "
        }));
        assert_eq!(parsed.protocol_version, "2025-11-25");
        assert_eq!(parsed.server_info.name, "fixture");
        assert_eq!(parsed.instructions.as_deref(), Some("be nice"));

        let empty = InitializeResult::from_value(&serde_json::json!({}));
        assert_eq!(empty.protocol_version, MCP_PROTOCOL_VERSION);
        assert!(empty.instructions.is_none());
    }

    #[test]
    fn renders_text_image_and_structured_content() {
        let result = ToolCallResult {
            content: vec![
                McpContent::Text {
                    text: "hello".into(),
                },
                McpContent::Image {
                    mime_type: "image/png".into(),
                    bytes: 2048,
                },
            ],
            structured_content: Some(serde_json::json!({"rows": 3})),
            is_error: false,
        };
        let rendered = render_tool_result(&result, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("[image image/png (2048 bytes)]"));
        assert!(rendered.contains("\"rows\": 3"));
    }

    #[test]
    fn does_not_duplicate_structured_content_already_in_text() {
        let structured = "{\n  \"rows\": 3\n}";
        let result = ToolCallResult {
            content: vec![McpContent::Text {
                text: structured.to_string(),
            }],
            structured_content: Some(serde_json::json!({"rows": 3})),
            is_error: false,
        };
        assert_eq!(
            render_tool_result(&result, DEFAULT_MAX_OUTPUT_BYTES),
            structured
        );
    }

    #[test]
    fn truncates_rendered_output_on_char_boundary() {
        let result = ToolCallResult {
            content: vec![McpContent::Text {
                text: "汉字汉字汉字".into(),
            }],
            structured_content: None,
            is_error: false,
        };
        let rendered = render_tool_result(&result, 7);
        // 7 bytes is not a character boundary for 3-byte CJK, so the text is
        // cut at 6 bytes.
        assert!(rendered.starts_with("汉字"));
        assert!(rendered.contains("...(truncated at 7 bytes)"));
    }

    #[test]
    fn parses_tool_annotations() {
        let annotations = ToolAnnotations::from_tool(&serde_json::json!({
            "name": "read_file",
            "annotations": {"readOnlyHint": true, "destructiveHint": false}
        }));
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert!(ToolAnnotations::from_tool(&serde_json::json!({"name": "x"})).is_empty());
    }

    #[test]
    fn summarizes_arguments_without_leaking_secrets() {
        let summary = args_summary(
            &serde_json::json!({
                "path": "/tmp/a.txt",
                "content": "x".repeat(200),
                "options": {"a": 1},
                "token": "ghp_012345678901234567890123456789012345"
            }),
            400,
        );
        assert!(summary.contains("path=/tmp/a.txt"));
        assert!(summary.contains("content=<200 chars>"));
        assert!(summary.contains(r#"options={"a":1}"#));
        assert!(!summary.contains("ghp_012345678901234567890123456789012345"));
    }

    /// An approval panel that renders `cmd=<1 keys>` for a nested argument asks
    /// the user to bless bytes they cannot see; the nested value must be shown.
    #[test]
    fn shows_nested_argument_values() {
        let summary = args_summary(&serde_json::json!({"cmd": {"nested": "rm -rf /"}}), 300);
        assert!(summary.contains("rm -rf /"), "{summary}");
        assert!(!summary.contains("<1 keys>"), "{summary}");
        assert!(
            summary.contains(r#"cmd={"nested":"rm -rf /"}"#),
            "{summary}"
        );
    }

    /// Credentials embedded in an argument value must be masked, whether the
    /// value is a scalar or hidden inside a container, without swallowing the
    /// readable part of the value.
    #[test]
    fn masks_credentials_inside_argument_values() {
        let summary = args_summary(
            &serde_json::json!({"cmd": "connect --host db password=hunter2 --retry 3"}),
            300,
        );
        assert!(!summary.contains("hunter2"), "{summary}");
        assert!(summary.contains("[redacted]"), "{summary}");
        assert!(summary.contains("connect --host db"), "{summary}");
        assert!(summary.contains("--retry 3"), "{summary}");

        let keyed = args_summary(&serde_json::json!({"password": "hunter2"}), 300);
        assert!(!keyed.contains("hunter2"), "{keyed}");

        let nested = args_summary(&serde_json::json!({"cmd": {"env": "api_key=abc123"}}), 300);
        assert!(!nested.contains("abc123"), "{nested}");

        // `{"password": "..."}` is not a `password=...` shape, so the textual
        // masking cannot see it: the container must be sanitized structurally
        // before it is rendered, or showing nested values would leak the value.
        let keyed_nested = args_summary(&serde_json::json!({"cmd": {"password": "hunter2"}}), 300);
        assert!(!keyed_nested.contains("hunter2"), "{keyed_nested}");
        assert!(
            keyed_nested.contains(r#"cmd={"password":"#),
            "{keyed_nested}"
        );
    }

    /// One huge nested value must not push the remaining arguments out of the
    /// panel: containers are capped per value, with a marker.
    #[test]
    fn caps_long_nested_argument_values() {
        let summary = args_summary(
            &serde_json::json!({"cmd": {"blob": "x".repeat(4_000)}, "next": 1}),
            300,
        );
        assert!(summary.contains("..."), "{summary}");
        assert!(summary.contains("next=1"), "{summary}");
        assert!(!summary.contains(&"x".repeat(201)), "{summary}");
    }

    #[test]
    fn transport_error_classification() {
        let error =
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused");
        let classified = McpError::transport_error(
            "srv",
            TransportKind::Stdio,
            McpFailureStage::Connect,
            &error,
        );
        assert_eq!(classified.failure, McpFailureClass::Connect);
        assert!(classified.retryable);
        assert!(classified.is_connection_loss());
    }
}
