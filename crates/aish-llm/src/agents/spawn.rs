//! Sub-agent spawn with parent cancel cascade.

use crate::session::LlmSession;
use crate::types::ChatMessage;

use super::tool_loop::{run_tool_loop_until_done, LoopStatus, ToolLoopConfig};

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
    use crate::agents::{mock_text_response, mock_tool_call_response};
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
