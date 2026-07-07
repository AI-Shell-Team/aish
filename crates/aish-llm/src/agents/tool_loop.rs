//! Reusable native tool calling loop for sub-agent spawn paths.

use aish_core::{LlmEvent, LlmEventType};

use crate::client::LlmResponse;
use crate::session::LlmSession;
use crate::streaming::{extract_message_text, StreamParser};
use crate::types::{ChatMessage, MessageContent};
use aish_core::AishError;

use super::outcome::{extract_spawn_outcome, OutcomeConfig, TerminationKind, INCOMPLETE_PREFIX};

/// Configuration for [`run_tool_loop_until_done`].
#[derive(Debug, Clone)]
pub struct ToolLoopConfig {
    /// Maximum LLM turns before returning [`LoopStatus::Incomplete`].
    pub max_turns: u32,
    /// Optional system prompt prepended to the message list.
    pub system_message: Option<String>,
    /// Prefix prepended to the final text when max turns is reached.
    pub incomplete_prefix: String,
}

impl Default for ToolLoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            system_message: None,
            incomplete_prefix: INCOMPLETE_PREFIX.to_string(),
        }
    }
}

impl ToolLoopConfig {
    fn outcome_config(&self) -> OutcomeConfig {
        OutcomeConfig {
            incomplete_prefix: self.incomplete_prefix.clone(),
        }
    }
}

/// Termination status of a tool calling loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStatus {
    Complete,
    Incomplete,
    Cancelled,
    Fatal,
}

/// Result of running the tool calling loop to completion or a terminal condition.
#[derive(Debug)]
pub struct LoopOutcome {
    pub status: LoopStatus,
    pub text: String,
    pub new_messages: Vec<ChatMessage>,
    pub error: Option<AishError>,
}

impl LoopOutcome {
    fn from_spawn_outcome(
        outcome: super::outcome::SpawnOutcome,
        new_messages: Vec<ChatMessage>,
        error: Option<AishError>,
    ) -> Self {
        Self {
            status: outcome.status,
            text: outcome.text,
            new_messages,
            error,
        }
    }

    pub fn cancelled() -> Self {
        let outcome = extract_spawn_outcome(TerminationKind::Cancelled, &OutcomeConfig::default());
        Self::from_spawn_outcome(outcome, Vec::new(), Some(AishError::Cancelled))
    }

    fn fatal(error: AishError) -> Self {
        let outcome = extract_spawn_outcome(TerminationKind::Fatal, &OutcomeConfig::default());
        Self::from_spawn_outcome(outcome, Vec::new(), Some(error))
    }
}

/// Run the native (non-streaming) tool calling loop until the assistant stops calling tools,
/// cancellation, max turns, or a fatal LLM error.
pub async fn run_tool_loop_until_done(
    session: &LlmSession,
    user_msg: &ChatMessage,
    context_messages: &[ChatMessage],
    config: &ToolLoopConfig,
) -> LoopOutcome {
    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(sys) = &config.system_message {
        messages.push(ChatMessage::system(
            session.system_prompt_with_tool_prompts(sys),
        ));
    }
    messages.extend_from_slice(context_messages);
    messages.push(user_msg.clone());

    let tool_specs = session.filtered_tool_specs();
    let has_tools = !tool_specs.is_empty();

    let mut iterations = 0u32;
    let mut last_assistant_text = String::new();
    let mut loop_messages: Vec<ChatMessage> = Vec::new();

    loop {
        if session.cancellation_token().is_cancelled() {
            return LoopOutcome::cancelled();
        }

        if iterations >= config.max_turns {
            let outcome = extract_spawn_outcome(
                TerminationKind::MaxTurnsReached {
                    last_assistant_text: last_assistant_text.clone(),
                },
                &config.outcome_config(),
            );
            return LoopOutcome::from_spawn_outcome(outcome, loop_messages, None);
        }
        iterations += 1;

        messages = session.prepare_messages_for_send(messages).await;

        session.emit_event(LlmEvent {
            event_type: LlmEventType::GenerationStart,
            data: serde_json::json!({}),
            timestamp: now_timestamp(),
            metadata: None,
        });

        let response = match session
            .chat_completion_raw(
                &messages,
                if has_tools { Some(&tool_specs) } else { None },
                false,
                session.loop_temperature(),
                session.loop_max_tokens(),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                session.emit_event(LlmEvent {
                    event_type: LlmEventType::GenerationEnd,
                    data: serde_json::json!({}),
                    timestamp: now_timestamp(),
                    metadata: None,
                });
                return LoopOutcome::fatal(e);
            }
        };

        session.emit_event(LlmEvent {
            event_type: LlmEventType::GenerationEnd,
            data: serde_json::json!({}),
            timestamp: now_timestamp(),
            metadata: None,
        });

        let LlmResponse::Json(json) = response else {
            return LoopOutcome::fatal(AishError::Llm(
                "run_tool_loop_until_done requires non-streaming responses".into(),
            ));
        };

        let (content, _reasoning, tool_calls, usage) = StreamParser::parse_response(&json);
        if let Some(u) = usage {
            session.record_usage_public(u);
        }

        if tool_calls.is_empty() {
            let outcome = extract_spawn_outcome(
                TerminationKind::NaturalStop {
                    assistant_text: content.unwrap_or_default(),
                },
                &config.outcome_config(),
            );
            return LoopOutcome::from_spawn_outcome(outcome, loop_messages, None);
        }

        if let Some(text) = content {
            last_assistant_text = text;
        }

        let assistant_msg = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"));

        if let Some(msg) = assistant_msg {
            let mut chat_msg = ChatMessage::assistant("");
            chat_msg.content = extract_message_text(msg.get("content")).map(MessageContent::Text);
            chat_msg.tool_calls = Some(tool_calls.clone());
            messages.push(chat_msg.clone());
            loop_messages.push(chat_msg);
        }

        for tc in &tool_calls {
            let result = session.execute_tool_external(tc).await;
            let tool_msg = ChatMessage::tool_result(&tc.id, result.output);
            messages.push(tool_msg.clone());
            loop_messages.push(tool_msg);
        }
    }
}

fn now_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{mock_text_response, mock_tool_call_response};
    use crate::types::Tool;
    use aish_context::ContextBudgetPolicy;
    use aish_core::AishError;

    fn disable_context_budget(session: &mut LlmSession) {
        session.set_context_budget_policy(ContextBudgetPolicy {
            enabled: false,
            ..Default::default()
        });
    }

    struct MockTool {
        name: String,
        output: String,
    }

    impl MockTool {
        fn new(name: &str, output: &str) -> Self {
            Self {
                name: name.to_string(),
                output: output.to_string(),
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
            crate::types::ToolResult::success(&self.output)
        }
    }

    fn test_session_with_responses(responses: Vec<Result<LlmResponse, AishError>>) -> LlmSession {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        disable_context_budget(&mut session);
        session.set_test_chat_responses(responses);
        session
    }

    #[tokio::test]
    async fn test_loop_completes_on_text_response() {
        let mut session = test_session_with_responses(vec![Ok(mock_text_response("done"))]);
        session.register_tool(Box::new(MockTool::new("grep", "hits")));

        let outcome = run_tool_loop_until_done(
            &session,
            &ChatMessage::user("search"),
            &[],
            &ToolLoopConfig {
                max_turns: 5,
                ..Default::default()
            },
        )
        .await;

        assert_eq!(outcome.status, LoopStatus::Complete);
        assert_eq!(outcome.text, "done");
    }

    #[tokio::test]
    async fn test_loop_executes_tool_calls_then_completes() {
        let mut session = test_session_with_responses(vec![
            Ok(mock_tool_call_response(&[(
                "c1",
                "grep",
                r#"{"pattern":"nginx"}"#,
            )])),
            Ok(mock_text_response("found nginx.conf")),
        ]);
        session.register_tool(Box::new(MockTool::new("grep", "grep output")));

        let outcome = run_tool_loop_until_done(
            &session,
            &ChatMessage::user("find nginx"),
            &[],
            &ToolLoopConfig {
                max_turns: 5,
                ..Default::default()
            },
        )
        .await;

        assert_eq!(outcome.status, LoopStatus::Complete);
        assert_eq!(outcome.text, "found nginx.conf");
        assert!(outcome
            .new_messages
            .iter()
            .any(|m| { m.role == "tool" && m.text_content() == Some("grep output") }));
    }

    #[tokio::test]
    async fn test_loop_incomplete_at_max_turns() {
        let mut session = test_session_with_responses(vec![
            Ok(mock_tool_call_response(&[("c1", "grep", "{}")])),
            Ok(mock_tool_call_response(&[("c2", "grep", "{}")])),
        ]);
        session.register_tool(Box::new(MockTool::new("grep", "out")));

        let outcome = run_tool_loop_until_done(
            &session,
            &ChatMessage::user("keep going"),
            &[],
            &ToolLoopConfig {
                max_turns: 1,
                ..Default::default()
            },
        )
        .await;

        assert_eq!(outcome.status, LoopStatus::Incomplete);
        assert!(outcome.text.starts_with("[incomplete: max turns reached]"));
    }

    #[tokio::test]
    async fn test_loop_fatal_on_llm_error() {
        let mut session =
            test_session_with_responses(vec![Err(AishError::Llm("rate limited".into()))]);
        session.register_tool(Box::new(MockTool::new("grep", "out")));

        let outcome = run_tool_loop_until_done(
            &session,
            &ChatMessage::user("task"),
            &[],
            &ToolLoopConfig::default(),
        )
        .await;

        assert_eq!(outcome.status, LoopStatus::Fatal);
        assert!(outcome.text.is_empty());
        assert!(outcome.error.is_some());
    }

    #[tokio::test]
    async fn test_loop_cancelled_when_token_fired() {
        let mut session =
            test_session_with_responses(vec![Ok(mock_tool_call_response(&[("c1", "grep", "{}")]))]);
        session.register_tool(Box::new(MockTool::new("grep", "out")));
        session.cancellation_token().cancel();

        let outcome = run_tool_loop_until_done(
            &session,
            &ChatMessage::user("task"),
            &[],
            &ToolLoopConfig::default(),
        )
        .await;

        assert_eq!(outcome.status, LoopStatus::Cancelled);
    }
}
