//! JSON-RPC 2.0 frame encoding.
//!
//! Everything the transports put on the wire is built here, so the envelope
//! shape lives in one place: `"jsonrpc": "2.0"`, a request id, a method, and
//! `params` only when the caller actually has parameters (the MCP servers that
//! reject a `params: null` request are common enough that omitting the field is
//! the safe default).
//!
//! Decoding is the mirror image and lives in [`crate::types::classify_frames`].

use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Map, Value};

use crate::types::{JsonRpcError, JsonRpcId};

/// Wire value of the `jsonrpc` field on every frame.
const JSONRPC_VERSION: &str = "2.0";

/// Per-transport allocator for outgoing request ids.
///
/// JSON-RPC 2.0 permits string ids as well, but the wider MCP ecosystem
/// (reference SDKs included) issues incrementing integers, so servers are best
/// tested against numbers. The counter is per instance and never reset, so ids
/// stay unique for the life of a transport even though a reconnect rejects and
/// clears every pending request.
pub struct RequestIdAllocator {
    next_id: AtomicI64,
}

impl RequestIdAllocator {
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
        }
    }

    /// Allocate the next id. The first id is `1`.
    pub fn next(&self) -> JsonRpcId {
        JsonRpcId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for RequestIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode a JSON-RPC request frame. `params` is omitted when `None`.
pub fn encode_request(id: &JsonRpcId, method: &str, params: Option<&Value>) -> Value {
    let mut frame = envelope();
    frame.insert("id".to_string(), id_value(id));
    frame.insert("method".to_string(), Value::String(method.to_string()));
    insert_params(&mut frame, params);
    Value::Object(frame)
}

/// Encode a JSON-RPC notification frame: a request without an id, which the
/// server must not answer.
pub fn encode_notification(method: &str, params: Option<&Value>) -> Value {
    let mut frame = envelope();
    frame.insert("method".to_string(), Value::String(method.to_string()));
    insert_params(&mut frame, params);
    Value::Object(frame)
}

/// Encode a JSON-RPC response frame, either carrying a `result` or an `error`.
pub fn encode_response(id: &JsonRpcId, result: Result<&Value, &JsonRpcError>) -> Value {
    let mut frame = envelope();
    frame.insert("id".to_string(), id_value(id));
    match result {
        Ok(result) => {
            frame.insert("result".to_string(), result.clone());
        }
        Err(error) => {
            let mut body = Map::with_capacity(3);
            body.insert("code".to_string(), Value::Number(error.code.into()));
            body.insert("message".to_string(), Value::String(error.message.clone()));
            if let Some(data) = &error.data {
                body.insert("data".to_string(), data.clone());
            }
            frame.insert("error".to_string(), Value::Object(body));
        }
    }
    Value::Object(frame)
}

fn envelope() -> Map<String, Value> {
    let mut frame = Map::with_capacity(4);
    frame.insert(
        "jsonrpc".to_string(),
        Value::String(JSONRPC_VERSION.to_string()),
    );
    frame
}

fn insert_params(frame: &mut Map<String, Value>, params: Option<&Value>) {
    if let Some(params) = params {
        frame.insert("params".to_string(), params.clone());
    }
}

fn id_value(id: &JsonRpcId) -> Value {
    match id {
        JsonRpcId::Number(number) => Value::Number((*number).into()),
        JsonRpcId::String(text) => Value::String(text.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn encodes_a_request_without_params() {
        let frame = encode_request(&JsonRpcId::Number(1), "ping", None);
        assert_eq!(frame, json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}));
        assert!(frame.get("params").is_none());
    }

    #[test]
    fn encodes_a_request_with_params_and_a_string_id() {
        let params = json!({"cursor": "next"});
        let frame = encode_request(
            &JsonRpcId::String("abc".into()),
            "tools/list",
            Some(&params),
        );
        assert_eq!(
            frame,
            json!({
                "jsonrpc": "2.0",
                "id": "abc",
                "method": "tools/list",
                "params": {"cursor": "next"}
            })
        );
    }

    #[test]
    fn encodes_a_notification_without_an_id() {
        let frame = encode_notification("notifications/initialized", Some(&json!({})));
        assert_eq!(
            frame,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
        );
        assert!(frame.get("id").is_none());
    }

    #[test]
    fn encodes_a_successful_response() {
        let frame = encode_response(&JsonRpcId::Number(7), Ok(&json!({"ok": true})));
        assert_eq!(
            frame,
            json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}})
        );
        assert!(frame.get("error").is_none());
    }

    #[test]
    fn encodes_an_error_response_and_omits_absent_data() {
        let error = JsonRpcError {
            code: -32601,
            message: "Method not found".to_string(),
            data: None,
        };
        let frame = encode_response(&JsonRpcId::Number(2), Err(&error));
        assert_eq!(
            frame,
            json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32601, "message": "Method not found"}})
        );

        let with_data = JsonRpcError {
            code: -32000,
            message: "boom".to_string(),
            data: Some(json!({"detail": "x"})),
        };
        let frame = encode_response(&JsonRpcId::Number(3), Err(&with_data));
        assert_eq!(
            frame["error"],
            json!({"code": -32000, "message": "boom", "data": {"detail": "x"}})
        );
        assert!(frame.get("result").is_none());
    }

    #[test]
    fn allocates_increasing_unique_ids() {
        let allocator = RequestIdAllocator::new();
        assert_eq!(allocator.next(), JsonRpcId::Number(1));
        assert_eq!(allocator.next(), JsonRpcId::Number(2));

        let shared = Arc::new(RequestIdAllocator::new());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let allocator = shared.clone();
            handles.push(std::thread::spawn(move || {
                (0..64)
                    .map(|_| match allocator.next() {
                        JsonRpcId::Number(number) => number,
                        JsonRpcId::String(text) => panic!("unexpected string id {text}"),
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let mut ids: Vec<i64> = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("allocator thread"))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4 * 64);
    }
}
