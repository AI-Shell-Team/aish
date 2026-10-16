//! Transports: framing and I/O only.
//!
//! - [`stdio`] — newline-delimited JSON-RPC over a child process.
//! - [`http`] — MCP Streamable HTTP: JSON-RPC over POST, with JSON or SSE
//!   responses and an optional background SSE listener.
//!
//! Both implementations forward every non-response frame as a
//! [`crate::types::TransportEvent`] and never interpret MCP semantics; the
//! protocol layer lives in [`crate::client`].

mod http;
mod stdio;

pub use http::create_http_transport;
pub use stdio::create_stdio_transport;
