//! Sub-agent spawn with parent cancel cascade.

use std::sync::Arc;

use aish_core::LlmEvent;
use uuid::Uuid;

use crate::prompt::PromptContext;
use crate::session::LlmSession;
use crate::tool_context::ToolExecutionPolicy;
use crate::types::{ChatMessage, LlmCallbackResult, ToolSpec};

use super::event_metadata::forward_sub_agent_event;
use super::registry::{AgentRegistry, ToolStrategy};
use super::tool_loop::{run_tool_loop_until_done, LoopStatus, ToolLoopConfig};
use super::tools::{parent_has_skill_tool, resolve_tools_for_agent};

/// Global ceiling on sub-agent turns (applied via [`effective_max_turns`]).
pub const GLOBAL_MAX_TURNS: u32 = 30;

/// Apply the global turn ceiling to a built-in's configured limit.
pub fn effective_max_turns(def_max_turns: u32) -> u32 {
    def_max_turns.min(GLOBAL_MAX_TURNS)
}

/// Configuration for [`spawn`].
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub max_turns: u32,
    pub system_message: Option<String>,
    pub prompt_context: PromptContext,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            system_message: None,
            prompt_context: PromptContext::MainChat,
        }
    }
}

/// Result of a synchronous sub-agent spawn.
#[derive(Debug, Clone)]
pub struct SpawnResult {
    pub text: String,
    pub status: LoopStatus,
}

/// Create an isolated sub-session from `parent`, apply `configure`, then run the tool loop
/// with parent→child cancellation cascade.
pub async fn spawn<F>(
    parent: &LlmSession,
    prompt: &str,
    config: SpawnConfig,
    configure: F,
) -> SpawnResult
where
    F: FnOnce(&mut LlmSession),
{
    let mut sub = parent.create_subsession();
    configure(&mut sub);

    let parent_cancel = parent.cancellation_token_arc();
    let sub_cancel = sub.cancellation_token_arc();

    let loop_config = ToolLoopConfig {
        max_turns: config.max_turns,
        system_message: config.system_message,
        prompt_context: config.prompt_context,
        ..ToolLoopConfig::default()
    };
    let user_msg = ChatMessage::user(prompt);
    let run = run_tool_loop_until_done(&sub, &user_msg, &[], &loop_config);

    let outcome = tokio::select! {
        outcome = run => outcome,
        () = forward_cancellation(parent_cancel, sub_cancel) => {
            super::tool_loop::LoopOutcome::cancelled()
        }
    };

    SpawnResult {
        text: outcome.text,
        status: outcome.status,
    }
}

/// Spawn a built-in sub-agent from `parent`.
///
/// `register_tools` receives the sub-session and filtered parent tool specs; it must
/// register concrete tool implementations on the sub-session.
pub async fn spawn_builtin<F>(
    parent: &LlmSession,
    registry: &AgentRegistry,
    subagent_type: &str,
    prompt: &str,
    register_tools: F,
) -> Result<SpawnResult, String>
where
    F: FnOnce(&mut LlmSession, &[ToolSpec]),
{
    let def = registry.resolve(subagent_type)?;
    let parent_tools = parent.tool_specs();
    let parent_has_skill = parent_has_skill_tool(&parent_tools);
    let allowed_specs = resolve_tools_for_agent(def, &parent_tools, parent_has_skill);
    let enforce_read_only_bash = matches!(def.tool_strategy, ToolStrategy::Allowlist(_));
    let max_turns = effective_max_turns(def.max_turns);
    let spawn_id = Uuid::new_v4().to_string();
    let agent_type = def.subagent_type.clone();
    let system_prompt = def.system_prompt.clone();

    let result = spawn(
        parent,
        prompt,
        SpawnConfig {
            max_turns,
            system_message: Some(system_prompt),
            prompt_context: PromptContext::SubAgent {
                subagent_type: agent_type.clone(),
            },
        },
        |sub| {
            sub.set_tool_execution_policy(ToolExecutionPolicy {
                enforce_read_only_bash,
            });
            install_sub_agent_event_proxy(sub, &agent_type, &spawn_id);
            register_tools(sub, &allowed_specs);
        },
    )
    .await;

    Ok(result)
}

fn install_sub_agent_event_proxy(sub: &mut LlmSession, agent_type: &str, spawn_id: &str) {
    let Some(parent_cb) = sub.event_callback_arc() else {
        return;
    };
    let agent_type = agent_type.to_string();
    let spawn_id = spawn_id.to_string();

    let proxy: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync> =
        Arc::new(move |event: LlmEvent| {
            forward_sub_agent_event(parent_cb.as_ref(), event, &agent_type, &spawn_id)
        });
    sub.set_event_callback(proxy);
}

async fn forward_cancellation(
    parent: std::sync::Arc<crate::types::CancellationToken>,
    sub: std::sync::Arc<crate::types::CancellationToken>,
) {
    while !parent.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    sub.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{mock_text_response, mock_tool_call_response, AgentRegistry};
    use crate::client::LlmResponse;
    use crate::types::Tool;
    use aish_context::ContextBudgetPolicy;

    fn disable_context_budget(session: &mut LlmSession) {
        session.set_context_budget_policy(ContextBudgetPolicy {
            enabled: false,
            ..Default::default()
        });
    }

    struct MockTool {
        name: String,
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "mock"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("ok")
        }
    }

    fn register_mock_from_specs(sub: &mut LlmSession, specs: &[ToolSpec]) {
        disable_context_budget(sub);
        for spec in specs {
            sub.register_tool(Box::new(MockTool::new(&spec.function.name)));
        }
    }

    #[tokio::test]
    async fn test_spawn_builtin_explore_mock_sequence() {
        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.register_tool(Box::new(MockTool::new("grep")));
        parent.register_tool(Box::new(MockTool::new("read_file")));
        parent.register_tool(Box::new(MockTool::new("write_file")));

        let registry = AgentRegistry::builtin();
        let result = spawn_builtin(&parent, &registry, "explore", "find nginx", |sub, specs| {
            sub.set_test_chat_responses(vec![
                Ok(mock_tool_call_response(&[("c1", "grep", "{}")])),
                Ok(mock_tool_call_response(&[("c2", "read_file", "{}")])),
                Ok(mock_text_response("nginx at /etc/nginx")),
            ]);
            assert!(specs
                .iter()
                .any(|spec| spec.function.name.as_str() == "read_file"));
            assert!(!specs
                .iter()
                .any(|spec| spec.function.name.as_str() == "write_file"));
            register_mock_from_specs(sub, specs);
        })
        .await
        .expect("spawn_builtin should succeed");

        assert_eq!(result.status, LoopStatus::Complete);
        assert_eq!(result.text, "nginx at /etc/nginx");
    }

    #[tokio::test]
    async fn test_spawn_builtin_does_not_modify_parent_session() {
        // Seam G: LlmSession does not persist op-loop messages; verify parent state
        // (tool registry) is unchanged after spawn. Sub-session messages are discarded.
        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.register_tool(Box::new(MockTool::new("grep")));
        parent.register_tool(Box::new(MockTool::new("write_file")));
        let specs_before = parent.tool_specs();

        let registry = AgentRegistry::builtin();
        let _ = spawn_builtin(&parent, &registry, "explore", "task", |sub, specs| {
            sub.set_test_chat_responses(vec![Ok(mock_text_response("done"))]);
            register_mock_from_specs(sub, specs);
        })
        .await
        .expect("spawn_builtin should succeed");

        assert_eq!(parent.tool_specs().len(), specs_before.len());
        assert!(parent
            .tool_specs()
            .iter()
            .any(|s| s.function.name == "write_file"));
        assert!(!parent
            .tool_specs()
            .iter()
            .any(|s| s.function.name == "read_file"));
    }

    #[tokio::test]
    async fn test_spawn_builtin_plan_mock_sequence() {
        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.register_tool(Box::new(MockTool::new("grep")));
        parent.register_tool(Box::new(MockTool::new("write_file")));
        parent.register_tool(Box::new(MockTool::new("enter_plan_mode")));

        let registry = AgentRegistry::builtin();
        let result = spawn_builtin(
            &parent,
            &registry,
            "plan",
            "design rollout",
            |sub, specs| {
                sub.set_test_chat_responses(vec![Ok(mock_text_response("rollout plan"))]);
                let names: Vec<_> = specs.iter().map(|s| s.function.name.as_str()).collect();
                assert!(names.contains(&"grep"));
                assert!(!names.contains(&"write_file"));
                assert!(!names.contains(&"enter_plan_mode"));
                register_mock_from_specs(sub, specs);
            },
        )
        .await
        .expect("spawn_builtin plan should succeed");

        assert_eq!(result.status, LoopStatus::Complete);
        assert_eq!(result.text, "rollout plan");
    }

    #[tokio::test]
    async fn test_spawn_builtin_general_purpose_excludes_agent() {
        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.register_tool(Box::new(MockTool::new("bash")));
        parent.register_tool(Box::new(MockTool::new("read_file")));
        parent.register_tool(Box::new(MockTool::new("Agent")));

        let registry = AgentRegistry::builtin();
        let result = spawn_builtin(
            &parent,
            &registry,
            "general-purpose",
            "run sub task",
            |sub, specs| {
                sub.set_test_chat_responses(vec![Ok(mock_text_response("sub task done"))]);
                let names: Vec<_> = specs.iter().map(|s| s.function.name.as_str()).collect();
                assert!(names.contains(&"bash"));
                assert!(names.contains(&"read_file"));
                assert!(!names.contains(&"Agent"));
                register_mock_from_specs(sub, specs);
            },
        )
        .await
        .expect("spawn_builtin general-purpose should succeed");

        assert_eq!(result.status, LoopStatus::Complete);
        assert_eq!(result.text, "sub task done");
    }

    #[tokio::test]
    async fn test_spawn_builtin_unknown_type_errors() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let registry = AgentRegistry::builtin();
        let err = spawn_builtin(
            &parent,
            &registry,
            "troubleshoot",
            "task",
            |_sub, _specs| {},
        )
        .await
        .unwrap_err();
        assert!(err.contains("Unknown subagent_type"));
    }

    #[tokio::test]
    async fn test_spawn_mock_llm_tool_sequence() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);

        let result = spawn(
            &parent,
            "find nginx",
            SpawnConfig {
                max_turns: 5,
                ..Default::default()
            },
            |sub| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(vec![
                    Ok(mock_tool_call_response(&[("c1", "grep", "{}")])),
                    Ok(mock_text_response("nginx at /etc/nginx")),
                ]);
                sub.register_tool(Box::new(MockTool::new("grep")));
            },
        )
        .await;

        assert_eq!(result.status, LoopStatus::Complete);
        assert_eq!(result.text, "nginx at /etc/nginx");
    }

    #[tokio::test]
    async fn test_spawn_cascades_parent_cancel() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.cancellation_token().cancel();

        let result = spawn(
            &parent,
            "long task",
            SpawnConfig {
                max_turns: 10,
                ..Default::default()
            },
            |sub| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(vec![Ok(mock_tool_call_response(&[(
                    "c1", "grep", "{}",
                )]))]);
                sub.register_tool(Box::new(MockTool::new("grep")));
            },
        )
        .await;

        assert_eq!(result.status, LoopStatus::Cancelled);
    }

    struct SlowMockTool {
        name: String,
    }

    impl Tool for SlowMockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "mock"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("ok")
        }

        fn execute_async<'a>(
            &'a self,
            _args: serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::types::ToolResult> + Send + 'a>,
        > {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                crate::types::ToolResult::success("ok")
            })
        }
    }

    #[tokio::test]
    async fn test_spawn_cascades_parent_cancel_while_running() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let parent_cancel = parent.cancellation_token_arc();

        let run = spawn(
            &parent,
            "long task",
            SpawnConfig {
                max_turns: 10,
                ..Default::default()
            },
            |sub| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(vec![Ok(mock_tool_call_response(&[(
                    "c1", "grep", "{}",
                )]))]);
                sub.register_tool(Box::new(SlowMockTool {
                    name: "grep".to_string(),
                }));
            },
        );

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            parent_cancel.cancel();
        });

        let result = run.await;
        assert_eq!(result.status, LoopStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_spawn_max_turns_returns_incomplete_with_last_text() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);

        let result = spawn(
            &parent,
            "keep searching",
            SpawnConfig {
                max_turns: 1,
                ..Default::default()
            },
            |sub| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(vec![
                    Ok(mock_tool_call_response(&[("c1", "grep", "{}")])),
                    Ok(mock_text_response("should not reach")),
                ]);
                sub.register_tool(Box::new(MockTool::new("grep")));
            },
        )
        .await;

        assert_eq!(result.status, LoopStatus::Incomplete);
        assert_eq!(result.text, "[incomplete: max turns reached]\n");
    }

    #[tokio::test]
    async fn test_spawn_max_turns_includes_last_assistant_text() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);

        struct TextThenToolMockTool {
            name: String,
        }

        impl Tool for TextThenToolMockTool {
            fn name(&self) -> &str {
                &self.name
            }

            fn description(&self) -> &str {
                "mock"
            }

            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object", "properties": {}})
            }

            fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
                crate::types::ToolResult::success("ok")
            }
        }

        fn mock_tool_call_with_content(text: &str) -> LlmResponse {
            LlmResponse::Json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": text,
                        "tool_calls": [{
                            "id": "c1",
                            "type": "function",
                            "function": {
                                "name": "grep",
                                "arguments": "{}",
                            }
                        }],
                    }
                }]
            }))
        }

        let result = spawn(
            &parent,
            "investigate",
            SpawnConfig {
                max_turns: 1,
                ..Default::default()
            },
            |sub| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(vec![Ok(mock_tool_call_with_content(
                    "partial findings",
                ))]);
                sub.register_tool(Box::new(TextThenToolMockTool {
                    name: "grep".to_string(),
                }));
            },
        )
        .await;

        assert_eq!(result.status, LoopStatus::Incomplete);
        assert_eq!(
            result.text,
            "[incomplete: max turns reached]\npartial findings"
        );
    }

    #[tokio::test]
    async fn test_spawn_fatal_llm_error() {
        use aish_core::AishError;

        let parent = LlmSession::new("http://localhost", "key", "model", None, None);

        let result = spawn(&parent, "task", SpawnConfig::default(), |sub| {
            disable_context_budget(sub);
            sub.set_test_chat_responses(vec![Err(AishError::Llm("upstream failure".into()))]);
        })
        .await;

        assert_eq!(result.status, LoopStatus::Fatal);
        assert!(result.text.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_builtin_plan_max_turns_is_twenty() {
        let registry = AgentRegistry::builtin();
        assert_eq!(registry.resolve("plan").expect("plan").max_turns, 20);
    }

    #[test]
    fn test_effective_max_turns_applies_global_cap() {
        use super::effective_max_turns;

        assert_eq!(effective_max_turns(15), 15);
        assert_eq!(effective_max_turns(20), 20);
        assert_eq!(effective_max_turns(25), 25);
        assert_eq!(effective_max_turns(35), GLOBAL_MAX_TURNS);
    }

    #[tokio::test]
    async fn test_spawn_respects_global_max_turns_at_runtime() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);

        let responses: Vec<_> = (0..GLOBAL_MAX_TURNS + 3)
            .map(|i| Ok(mock_tool_call_response(&[(&format!("c{i}"), "grep", "{}")])))
            .collect();

        let result = spawn(
            &parent,
            "deep task",
            SpawnConfig {
                max_turns: effective_max_turns(100),
                ..Default::default()
            },
            |sub| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(responses);
                sub.register_tool(Box::new(MockTool::new("grep")));
            },
        )
        .await;

        assert_eq!(result.status, LoopStatus::Incomplete);
        assert!(result.text.starts_with("[incomplete: max turns reached]"));
    }

    #[tokio::test]
    async fn test_spawn_builtin_max_turns_exhaustion() {
        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.register_tool(Box::new(MockTool::new("grep")));

        let registry = AgentRegistry::builtin();
        let explore_turns = registry.resolve("explore").expect("explore").max_turns;

        let responses: Vec<_> = (0..explore_turns + 2)
            .map(|i| Ok(mock_tool_call_response(&[(&format!("c{i}"), "grep", "{}")])))
            .collect();

        let result = spawn_builtin(
            &parent,
            &registry,
            "explore",
            "deep search",
            |sub, specs| {
                disable_context_budget(sub);
                sub.set_test_chat_responses(responses);
                register_mock_from_specs(sub, specs);
            },
        )
        .await
        .expect("spawn should succeed");

        assert_eq!(result.status, LoopStatus::Incomplete);
        assert!(result.text.starts_with("[incomplete: max turns reached]"));
    }

    #[tokio::test]
    async fn test_spawn_builtin_forwards_sub_agent_metadata_on_events() {
        use aish_core::LlmEventType;
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};
        use uuid::Uuid;

        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_cb = captured.clone();

        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        parent.register_tool(Box::new(MockTool::new("grep")));
        parent.set_event_callback(Arc::new(move |event| {
            captured_cb.lock().unwrap().push(event);
            None
        }));

        let registry = AgentRegistry::builtin();
        let _ = spawn_builtin(&parent, &registry, "explore", "find nginx", |sub, specs| {
            sub.set_test_chat_responses(vec![
                Ok(mock_tool_call_response(&[("c1", "grep", "{}")])),
                Ok(mock_text_response("done")),
            ]);
            register_mock_from_specs(sub, specs);
        })
        .await
        .expect("spawn_builtin should succeed");

        let events = captured.lock().unwrap();
        let sub_events: Vec<_> = events
            .iter()
            .filter(|event| {
                event.data.get("source").and_then(|value| value.as_str()) == Some("sub_agent")
            })
            .collect();

        assert!(!sub_events.is_empty());
        for event in &sub_events {
            assert_eq!(
                event
                    .data
                    .get("agent_type")
                    .and_then(|value| value.as_str()),
                Some("explore")
            );
            assert_eq!(
                event.data.get("depth").and_then(|value| value.as_u64()),
                Some(super::super::event_metadata::SUB_AGENT_DEPTH as u64)
            );
            let spawn_id = event
                .data
                .get("spawn_id")
                .and_then(|value| value.as_str())
                .expect("spawn_id");
            assert!(Uuid::parse_str(spawn_id).is_ok(), "spawn_id must be a UUID");
        }

        let spawn_ids: HashSet<_> = sub_events
            .iter()
            .filter_map(|event| event.data.get("spawn_id").and_then(|value| value.as_str()))
            .collect();
        assert_eq!(spawn_ids.len(), 1);

        assert!(sub_events
            .iter()
            .any(|event| event.event_type == LlmEventType::GenerationStart));
        assert!(sub_events
            .iter()
            .any(|event| event.event_type == LlmEventType::ToolExecutionStart));
    }
}
