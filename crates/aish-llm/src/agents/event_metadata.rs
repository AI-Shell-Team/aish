//! Sub-agent event metadata enrichment for TUI observability (PRD §4.5).

use aish_core::LlmEvent;
use serde_json::{json, Value};

/// Phase 1 fixed nesting depth (no nested sub-agents).
pub const SUB_AGENT_DEPTH: u32 = 1;

pub fn enrich_event_data_with_sub_agent_labels(
    data: Value,
    agent_type: &str,
    spawn_id: &str,
    skill_name: Option<&str>,
) -> Value {
    if let Some(obj) = data.as_object() {
        let mut new_obj = obj.clone();
        insert_sub_agent_metadata_fields(&mut new_obj, agent_type, spawn_id);
        if let Some(skill_name) = skill_name {
            new_obj.insert("skill_name".to_string(), json!(skill_name));
        }
        Value::Object(new_obj)
    } else {
        let mut enriched = json!({
            "source": "sub_agent",
            "agent_type": agent_type,
            "depth": SUB_AGENT_DEPTH,
            "spawn_id": spawn_id,
            "original_data": data,
        });
        if let Some(skill_name) = skill_name {
            enriched["skill_name"] = json!(skill_name);
        }
        enriched
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

pub fn forward_sub_agent_event_with_skill(
    parent_cb: &dyn Fn(LlmEvent) -> Option<crate::types::LlmCallbackResult>,
    event: LlmEvent,
    agent_type: &str,
    spawn_id: &str,
    skill_name: Option<&str>,
) -> Option<crate::types::LlmCallbackResult> {
    parent_cb(LlmEvent {
        event_type: event.event_type,
        data: enrich_event_data_with_sub_agent_labels(event.data, agent_type, spawn_id, skill_name),
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
        let enriched = enrich_event_data_with_sub_agent_labels(data, "explore", "uuid-1", None);

        assert_eq!(enriched["source"], "sub_agent");
        assert_eq!(enriched["agent_type"], "explore");
        assert_eq!(enriched["depth"], SUB_AGENT_DEPTH);
        assert_eq!(enriched["spawn_id"], "uuid-1");
        assert_eq!(enriched["tool_name"], "grep");
    }

    #[test]
    fn enrich_wraps_non_object_data() {
        let data = json!("plain");
        let enriched = enrich_event_data_with_sub_agent_labels(data, "plan", "uuid-2", None);

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

        forward_sub_agent_event_with_skill(
            &cb,
            LlmEvent {
                event_type: LlmEventType::ToolExecutionStart,
                data: json!({"tool_name": "read_file"}),
                timestamp: 1.0,
                metadata: None,
            },
            "general-purpose",
            "spawn-abc",
            None,
        );

        let event = seen.lock().unwrap().take().expect("callback invoked");
        assert_eq!(event.event_type, LlmEventType::ToolExecutionStart);
        assert_eq!(event.data["source"], "sub_agent");
        assert_eq!(event.data["agent_type"], "general-purpose");
        assert_eq!(event.data["spawn_id"], "spawn-abc");
    }

    #[test]
    fn enrich_can_include_skill_name() {
        let enriched = enrich_event_data_with_sub_agent_labels(
            json!({"tool_name": "bash"}),
            "troubleshoot",
            "spawn-skill",
            Some("host-diagnose"),
        );

        assert_eq!(enriched["source"], "sub_agent");
        assert_eq!(enriched["agent_type"], "troubleshoot");
        assert_eq!(enriched["skill_name"], "host-diagnose");
    }
}
