//! Sub-agent spawn with parent cancel cascade.

use std::sync::Arc;

use aish_core::LlmEvent;
use uuid::Uuid;

use crate::session::LlmSession;
use crate::tool_context::ToolExecutionPolicy;
use crate::types::{ChatMessage, LlmCallbackResult, ToolSpec};

use super::registry::AgentRegistry;
use super::tool_loop::{run_tool_loop_until_done, LoopStatus, ToolLoopConfig};
use super::tools::resolve_tools_for_agent;

/// Global ceiling on sub-agent turns (applied via `min(def.max_turns, GLOBAL_MAX_TURNS)`).
pub const GLOBAL_MAX_TURNS: u32 = 30;

/// Configuration for [`spawn`].
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub max_turns: u32,
    pub system_message: Option<String>,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            system_message: None,
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

/// Spawn a built-in sub-agent (`explore` in Phase 1 slice) from `parent`.
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
    let allowed_specs = resolve_tools_for_agent(def, &parent.tool_specs());
    let max_turns = def.max_turns.min(GLOBAL_MAX_TURNS);
    let spawn_id = Uuid::new_v4().to_string();
    let agent_type = def.subagent_type.clone();
    let system_prompt = def.system_prompt.clone();

    let result = spawn(
        parent,
        prompt,
        SpawnConfig {
            max_turns,
            system_message: Some(system_prompt),
        },
        |sub| {
            sub.set_tool_execution_policy(ToolExecutionPolicy {
                enforce_read_only_bash: true,
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
            let modified_data = if let Some(obj) = event.data.as_object() {
                let mut new_obj = obj.clone();
                new_obj.insert("source".to_string(), serde_json::json!("sub_agent"));
                new_obj.insert("agent_type".to_string(), serde_json::json!(agent_type));
                new_obj.insert("depth".to_string(), serde_json::json!(1));
                new_obj.insert("spawn_id".to_string(), serde_json::json!(spawn_id));
                serde_json::Value::Object(new_obj)
            } else {
                serde_json::json!({
                    "source": "sub_agent",
                    "agent_type": agent_type,
                    "depth": 1,
                    "spawn_id": spawn_id,
                    "original_data": event.data,
                })
            };
            parent_cb(LlmEvent {
                event_type: event.event_type,
                data: modified_data,
                timestamp: event.timestamp,
                metadata: event.metadata,
            })
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
        parent.register_tool(Box::new(MockTool::new("write_file")));

        let registry = AgentRegistry::builtin();
        let result = spawn_builtin(&parent, &registry, "explore", "find nginx", |sub, specs| {
            sub.set_test_chat_responses(vec![
                Ok(mock_tool_call_response(&[("c1", "grep", "{}")])),
                Ok(mock_tool_call_response(&[("c2", "read_file", "{}")])),
                Ok(mock_text_response("nginx at /etc/nginx")),
            ]);
            register_mock_from_specs(sub, specs);
            sub.register_tool(Box::new(MockTool::new("read_file")));
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
    async fn test_spawn_builtin_unknown_type_errors() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let registry = AgentRegistry::builtin();
        let err = spawn_builtin(&parent, &registry, "plan", "task", |_sub, _specs| {})
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
}
