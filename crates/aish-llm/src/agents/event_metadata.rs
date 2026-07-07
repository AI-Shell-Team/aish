//! Sub-agent event metadata enrichment for TUI observability (PRD §4.5).

use aish_core::LlmEvent;
use serde_json::{json, Value};

/// Phase 1 fixed nesting depth (no nested sub-agents).
pub const SUB_AGENT_DEPTH: u32 = 1;

/// Merge sub-agent observability fields into an event's `data` payload.
pub fn enrich_event_data_with_sub_agent_metadata(
    data: Value,
    agent_type: &str,
    spawn_id: &str,
) -> Value {
    if let Some(obj) = data.as_object() {
        let mut new_obj = obj.clone();
        insert_sub_agent_metadata_fields(&mut new_obj, agent_type, spawn_id);
        Value::Object(new_obj)
    } else {
        json!({
            "source": "sub_agent",
            "agent_type": agent_type,
            "depth": SUB_AGENT_DEPTH,
            "spawn_id": spawn_id,
            "original_data": data,
        })
    }
}

fn insert_sub_agent_metadata_fields(
    obj: &mut serde_json::Map<String, Value>,
    agent_type: &str,
    spawn_id: &str,
) {
    obj.insert("source".to_string(), json!("sub_agent"));
    obj.insert("agent_type".to_string(), json!(agent_type));
    obj.insert("depth".to_string(), json!(SUB_AGENT_DEPTH));
    obj.insert("spawn_id".to_string(), json!(spawn_id));
}

/// Forward `event` to `parent_cb` with sub-agent metadata merged into `data`.
pub fn forward_sub_agent_event(
    parent_cb: &dyn Fn(LlmEvent) -> Option<crate::types::LlmCallbackResult>,
    event: LlmEvent,
    agent_type: &str,
    spawn_id: &str,
) -> Option<crate::types::LlmCallbackResult> {
    parent_cb(LlmEvent {
        event_type: event.event_type,
        data: enrich_event_data_with_sub_agent_metadata(event.data, agent_type, spawn_id),
        timestamp: event.timestamp,
        metadata: event.metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aish_core::LlmEventType;

    #[test]
    fn enrich_merges_into_object_data() {
        let data = json!({"tool_name": "grep", "tool_args": {}});
        let enriched = enrich_event_data_with_sub_agent_metadata(data, "explore", "uuid-1");

        assert_eq!(enriched["source"], "sub_agent");
        assert_eq!(enriched["agent_type"], "explore");
        assert_eq!(enriched["depth"], SUB_AGENT_DEPTH);
        assert_eq!(enriched["spawn_id"], "uuid-1");
        assert_eq!(enriched["tool_name"], "grep");
    }

    #[test]
    fn enrich_wraps_non_object_data() {
        let data = json!("plain");
        let enriched = enrich_event_data_with_sub_agent_metadata(data, "plan", "uuid-2");

        assert_eq!(enriched["source"], "sub_agent");
        assert_eq!(enriched["agent_type"], "plan");
        assert_eq!(enriched["depth"], SUB_AGENT_DEPTH);
        assert_eq!(enriched["spawn_id"], "uuid-2");
        assert_eq!(enriched["original_data"], "plain");
    }

    #[test]
    fn forward_sub_agent_event_preserves_event_type() {
        let seen = std::sync::Mutex::new(None);
        let cb = |event: LlmEvent| {
            *seen.lock().unwrap() = Some(event);
            None
        };

        forward_sub_agent_event(
            &cb,
            LlmEvent {
                event_type: LlmEventType::ToolExecutionStart,
                data: json!({"tool_name": "read_file"}),
                timestamp: 1.0,
                metadata: None,
            },
            "general-purpose",
            "spawn-abc",
        );

        let event = seen.lock().unwrap().take().expect("callback invoked");
        assert_eq!(event.event_type, LlmEventType::ToolExecutionStart);
        assert_eq!(event.data["source"], "sub_agent");
        assert_eq!(event.data["agent_type"], "general-purpose");
        assert_eq!(event.data["spawn_id"], "spawn-abc");
    }
}
