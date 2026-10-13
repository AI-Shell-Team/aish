// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments,
    clippy::useless_conversion
)]

//! MCP (Model Context Protocol) client support for aish.
//!
//! Layering (mirrors the oh-my-pi implementation, see `docs/` in that repo):
//!
//! - [`jsonrpc`] / [`sse`] — frame encoding, incremental SSE decoding.
//! - [`transports`] — framing and I/O: JSONL over a child process (stdio) and
//!   Streamable HTTP over `reqwest` (JSON or SSE responses).
//! - [`client`] — MCP protocol layer: `initialize` handshake, tool listing and
//!   tool calls, server-to-client request answers (`ping`, `roots/list`).
//! - [`manager`] — lifecycle: background connect, per-server status, tool cache
//!   invalidation, reconnect/reload, bounded shutdown.
//! - [`tool`] — the `Tool` implementation the LLM sees. Tool schemas are NOT
//!   injected into the prompt at startup; one gateway tool per server exposes
//!   `list` / `describe` / `call` actions so the model discovers tools on
//!   demand.
//! - [`trust`] — side-effect model: a call is auto-approved only when the user
//!   declared the server read-only *and* trusted it.
//!
//! Safety invariants:
//!
//! - MCP calls go through the normal aish approval path
//!   ([`aish_llm::Tool::preflight_with_context`]) and are never auto-approved
//!   for undeclared, write, network or privilege-bearing tools.
//! - An approval is bound to one exact argument set (see
//!   [`tool::McpGatewayTool::approval_memory_key`]); changed arguments require
//!   a new approval.
//! - Credentials (env values, header values, URL query strings) never reach
//!   status output, logs or audit records; see [`redact`].

pub mod cancel;
pub mod client;
pub mod commands;
pub mod config;
pub mod jsonrpc;
pub mod manager;
pub mod prompt;
pub mod redact;
pub mod sse;
pub mod status;
pub mod tool;
pub mod transports;
pub mod trust;
pub mod types;

pub use cancel::CancelListener;
pub use client::{connect, ConnectOptions, Connection, ServerEvent};
pub use commands::{
    add_to_config, parse_command, remove_from_config, server_names, set_enabled_in_config,
    set_trust_in_config, AddArgs, McpCommand,
};
pub use config::{resolve_settings, ExposeMode, McpSettings, ServerSettings};
pub use manager::{ApplyOutcome, McpManager, McpRuntime};
pub use status::{render_server_tools, render_status, render_summary_line};
pub use tool::{
    direct_tool_name, direct_tools, gateway_tool_name, gateway_tools, gateway_tools_with,
    McpDirectTool, McpGatewayTool,
};
pub use trust::{
    args_digest, combine_side_effects, evaluate_call_trust, reported_side_effects, TrustDecision,
};
pub use types::{
    args_summary, classify_frames, render_tool_result, McpContent, McpError, McpFailureClass,
    McpFailureStage, McpToolDefinition, ServerInfo, ServerSnapshot, ServerStatus, SideEffects,
    ToolAnnotations, ToolCallResult, ToolSummary, TransportEvent, TransportKind, TrustLevel,
    ANY_TOOL, MCP_PROTOCOL_VERSION,
};
