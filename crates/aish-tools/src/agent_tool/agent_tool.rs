//! `Agent` tool — synchronous sub-agent spawn entry point for the main LLM.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aish_llm::{
    spawn_builtin, AgentRegistry, ChatMessage, LlmSession, LoopStatus, SpawnResult, Tool,
    ToolResult,
};

use super::prompt;

const MAX_INCOMPLETE_FINAL_TEXT_BYTES: usize = 4 * 1024;
const MAX_INCOMPLETE_OUTPUT_BYTES: usize = 8 * 1024;
const MAX_INCOMPLETE_TOOL_OUTPUT_BYTES: usize = 1024;

/// Injectable spawn backend for tests.
pub type SpawnFn = Arc<
    dyn for<'a> Fn(
            &'a LlmSession,
            &'a str,
            &'a str,
        )
            -> Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send + 'a>>
        + Send
        + Sync,
>;

/// Tool that spawns built-in sub-agents (`explore`, `plan`, `general-purpose`).
pub struct AgentTool {
    registry: AgentRegistry,
    description: String,
    spawn_fn: Option<SpawnFn>,
}

impl AgentTool {
    pub fn new() -> Self {
        let registry = AgentRegistry::builtin();
        let description = format!(
            "{}\n{}\n\nAvailable subagent types:\n{}\n\n{}\n\n{}",
            prompt::DESCRIPTION,
            prompt::ROUTING_SECTION,
            registry.list_for_tool_description(),
            prompt::WHEN_NOT_SECTION,
            prompt::USAGE_SECTION,
        );
        Self {
            registry,
            description,
            spawn_fn: None,
        }
    }

    /// Test-only constructor that injects a custom spawn backend.
    pub fn with_spawn_fn(spawn_fn: SpawnFn) -> Self {
        let mut tool = Self::new();
        tool.spawn_fn = Some(spawn_fn);
        tool
    }

    fn validate_args(args: &serde_json::Value) -> Result<(&str, &str, &str), ToolResult> {
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolResult::error("Missing required parameter: description"))?;
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolResult::error("Missing required parameter: prompt"))?;
        let subagent_type = args
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolResult::error("Missing required parameter: subagent_type"))?;
        Ok((description, prompt, subagent_type))
    }

    pub(crate) fn spawn_result_to_tool_result(result: SpawnResult) -> ToolResult {
        let partial_messages = result.partial_messages;
        let error_category = result.error_category;
        match result.status {
            LoopStatus::Complete => ToolResult::success(result.text),
            LoopStatus::Incomplete => Self::incomplete_tool_result(result.text, partial_messages),
            LoopStatus::Cancelled => {
                // User-facing copy matches shell Ctrl+C (`shell.interrupted`);
                // meta.reason stays machine-readable for short-circuit handling.
                Self::short_circuit_error(aish_i18n::t("shell.interrupted"), "sub_agent_cancelled")
            }
            LoopStatus::Fatal => {
                let category = error_category.unwrap_or_else(|| "unknown".to_string());
                Self::fatal_tool_result(result.text, category, partial_messages)
            }
        }
    }

    /// Keep the sub-agent's completed tool evidence visible to the parent when
    /// it reaches its turn limit. The text is bounded because this output is
    /// sent back through the parent model's context.
    fn incomplete_tool_result(
        final_text: String,
        partial_messages: Vec<ChatMessage>,
    ) -> ToolResult {
        let mut calls = Vec::new();
        let mut results = HashMap::new();
        for message in &partial_messages {
            if message.role == "assistant" {
                if let Some(tool_calls) = &message.tool_calls {
                    calls.extend(
                        tool_calls
                            .iter()
                            .map(|call| (call.id.clone(), call.name.clone())),
                    );
                }
            } else if message.role == "tool" {
                if let (Some(id), Some(output)) =
                    (message.tool_call_id.as_deref(), message.text_content())
                {
                    results.entry(id).or_insert(output);
                }
            }
        }

        let mut output = truncate_with_marker(
            &final_text,
            MAX_INCOMPLETE_FINAL_TEXT_BYTES,
            "\n...(final text truncated)",
        );
        if !calls.is_empty() {
            output.push_str("\n\nCompleted before turn limit:");
            for (index, (id, name)) in calls.iter().enumerate() {
                let result = results
                    .get(id.as_str())
                    .copied()
                    .unwrap_or("(result unavailable)");
                let result = truncate_with_marker(
                    result,
                    MAX_INCOMPLETE_TOOL_OUTPUT_BYTES,
                    "...(truncated)",
                );
                output.push_str(&format!("\n  {}. {} [{}]: {}", index + 1, name, id, result));
            }
        }
        output = truncate_with_marker(
            &output,
            MAX_INCOMPLETE_OUTPUT_BYTES,
            "\n...(partial results truncated)",
        );

        let completed_tool_count = calls
            .iter()
            .filter(|(id, _)| results.contains_key(id.as_str()))
            .count();
        ToolResult {
            ok: true,
            output,
            meta: Some(serde_json::json!({
                "command_status": "incomplete",
                "reason": "sub_agent_max_turns_reached",
                "completed_tool_count": completed_tool_count,
                "partial_message_count": partial_messages.len(),
            })),
        }
    }

    /// Build a structured fatal `ToolResult` that preserves the sub-agent's
    /// completed work (tool calls + results) and the stable error category.
    /// The parent session can then persist partial evidence so a retry does
    /// not re-execute already-completed tools.
    fn fatal_tool_result(
        final_text: String,
        category: String,
        partial_messages: Vec<ChatMessage>,
    ) -> ToolResult {
        let completed_tool_names: Vec<&str> = partial_messages
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|tc| tc.name.as_str())
            .collect();
        let tool_result_msgs: Vec<&ChatMessage> = partial_messages
            .iter()
            .filter(|m| m.role == "tool")
            .collect();
        let partial_summaries: Vec<serde_json::Value> = tool_result_msgs
            .iter()
            .map(|m| {
                serde_json::json!({
                    "tool_call_id": m.tool_call_id,
                    "name": m.name,
                    "output": m.text_content(),
                })
            })
            .collect();
        // Build a human/LLM-readable summary of completed work so the parent
        // agent can see which tools already ran and their results — not just
        // a bare "Sub-agent failed". This prevents the parent from blindly
        // retrying the same steps. The structured `meta` remains available for
        // programmatic consumers (persistence, retry logic).
        let mut output = format!("Sub-agent failed ({category})");
        if !final_text.is_empty() {
            output.push_str(&format!(": {final_text}"));
        }
        if !completed_tool_names.is_empty() {
            output.push_str("\n\nCompleted before failure:");
            for (i, tool_name) in completed_tool_names.iter().enumerate() {
                let result_text = tool_result_msgs
                    .get(i)
                    .and_then(|m| m.text_content())
                    .unwrap_or("(no result)");
                output.push_str(&format!("\n  {i}. {tool_name}: {result_text}"));
            }
        }
        ToolResult {
            ok: false,
            output,
            meta: Some(serde_json::json!({
                "dispatch_status": "short_circuit",
                "reason": "sub_agent_fatal",
                "error_category": category,
                "completed_tools": completed_tool_names,
                "partial_messages": partial_summaries,
            })),
        }
    }

    fn short_circuit_error(output: impl Into<String>, reason: &str) -> ToolResult {
        ToolResult {
            ok: false,
            output: output.into(),
            meta: Some(serde_json::json!({
                "dispatch_status": "short_circuit",
                "reason": reason,
            })),
        }
    }
}

fn truncate_with_marker(text: &str, max_bytes: usize, marker: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let content_limit = max_bytes.saturating_sub(marker.len());
    let mut end = content_limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], marker)
}

impl Default for AgentTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for AgentTool {
    fn name(&self) -> &str {
        "Agent"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters(&self.registry.subagent_types())
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        ToolResult::error("Agent requires async execution; use execute_async_in_session")
    }

    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        use futures::FutureExt;
        // Wrap the sub-agent run in `catch_unwind` so a panicking sub-session
        // degrades to a `ToolResult::error` instead of unwinding through the
        // caller. This matches the framework's `Tool::execute_async` contract
        // (types.rs) and is essential under parallel `join_all` execution,
        // where an uncontained panic would abort every concurrent sibling and
        // the whole turn (e.g. a poisoned `Arc<Mutex>` shared via the parent
        // session cascades a `.lock().unwrap()` panic to all siblings).
        let inner = std::panic::AssertUnwindSafe(async move {
            let (_description, prompt, subagent_type) = match Self::validate_args(&args) {
                Ok(v) => v,
                Err(err) => return err,
            };

            if let Err(err) = self.registry.resolve(subagent_type) {
                return ToolResult::error(err);
            }

            if let Some(spawn_fn) = &self.spawn_fn {
                let result = match spawn_fn(session, subagent_type, prompt).await {
                    Ok(result) => result,
                    Err(err) => return ToolResult::error(err),
                };
                if result.status == LoopStatus::Cancelled {
                    session.cancellation_token().cancel();
                }
                return Self::spawn_result_to_tool_result(result);
            }

            let registry = self.registry.clone();
            let result =
                match spawn_builtin(session, &registry, subagent_type, prompt, |_sub, _specs| {})
                    .await
                {
                    Ok(result) => result,
                    Err(err) => return ToolResult::error(err),
                };

            if result.status == LoopStatus::Cancelled {
                // Ensure the parent session is marked cancelled so the shell
                // prints a single `已中断` even when Ctrl+C arrived as PTY 0x03.
                session.cancellation_token().cancel();
            }

            Self::spawn_result_to_tool_result(result)
        });
        Box::pin(async move {
            match inner.catch_unwind().await {
                Ok(result) => result,
                Err(payload) => {
                    let message = if let Some(s) = payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Agent sub-agent execution panicked".to_string()
                    };
                    ToolResult::error(format!("Error: {}", message))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_is_agent() {
        let tool = AgentTool::new();
        assert_eq!(tool.name(), "Agent");
    }

    #[test]
    fn description_includes_all_builtins() {
        let tool = AgentTool::new();
        assert!(tool.description().contains("explore"));
        assert!(tool.description().contains("plan"));
        assert!(tool.description().contains("general-purpose"));
        assert!(tool.description().contains("troubleshoot"));
        assert!(tool.description().contains("read-only"));
        assert!(!tool.description().contains("command-diagnose"));
    }

    #[test]
    fn description_includes_planning_routing_table() {
        let tool = AgentTool::new();
        assert!(tool
            .description()
            .contains("## Routing: planning vs plan mode vs sub-agents"));
        assert!(tool.description().contains("Do NOT use enter_plan_mode"));
        assert!(tool
            .description()
            .contains("## When NOT to use the Agent tool"));
    }

    #[test]
    fn parameters_enum_lists_all_builtins() {
        let tool = AgentTool::new();
        let params = tool.parameters();
        let enum_values = params["properties"]["subagent_type"]["enum"]
            .as_array()
            .expect("subagent_type enum");
        let names: Vec<_> = enum_values.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"explore"));
        assert!(names.contains(&"plan"));
        assert!(names.contains(&"general-purpose"));
        assert!(names.contains(&"troubleshoot"));
        assert!(!names.contains(&"command-diagnose"));
        assert!(!names.contains(&"diagnose"));
    }

    #[test]
    fn incomplete_tool_result_caps_partial_evidence() {
        let tool_calls = (0..12)
            .map(|index| aish_llm::ToolCall {
                id: format!("call-{index}"),
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            })
            .collect();
        let mut partial_messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }];
        for index in (0..12).rev() {
            partial_messages.push(ChatMessage::tool_result(
                format!("call-{index}"),
                "界".repeat(600),
            ));
        }

        let result = AgentTool::spawn_result_to_tool_result(SpawnResult {
            text: "[incomplete: max turns reached]\n".to_string(),
            status: LoopStatus::Incomplete,
            partial_messages,
            ..Default::default()
        });

        assert!(result.output.starts_with("[incomplete: max turns reached]"));
        assert!(result.output.len() <= MAX_INCOMPLETE_OUTPUT_BYTES);
        assert!(result.output.contains("...(partial results truncated)"));
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|meta| meta.get("completed_tool_count"))
                .and_then(serde_json::Value::as_u64),
            Some(12)
        );
    }

    #[test]
    fn description_encourages_parallel_independent_agents() {
        let tool = AgentTool::new();
        // The tool prompt must steer the model toward emitting multiple Agent
        // calls in one response — that is what unlocks concurrent execution.
        let desc = tool.description();
        assert!(desc.contains("Parallelize"), "missing parallel guidance");
        assert!(
            desc.contains("this single response"),
            "must tell the model to emit calls in one response"
        );
        assert!(
            desc.contains("concurrently"),
            "must state the calls run concurrently"
        );
        assert!(
            desc.contains("I/O-bound"),
            "must explain why read-only tasks parallelize well"
        );
    }
}
