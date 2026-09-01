use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aish_llm::{ChatMessage, LoopStatus, SpawnResult, Tool, ToolCall};
use aish_tools::{AgentTool, SpawnFn};

fn mock_spawn_ok<'a>(
    _session: &'a aish_llm::LlmSession,
    _ty: &'a str,
    _prompt: &'a str,
) -> Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send + 'a>> {
    Box::pin(async {
        Ok(SpawnResult {
            text: "explore conclusion".to_string(),
            status: LoopStatus::Complete,
            error_category: None,
            partial_messages: Vec::new(),
        })
    })
}

fn mock_spawn_cancelled<'a>(
    _session: &'a aish_llm::LlmSession,
    _ty: &'a str,
    _prompt: &'a str,
) -> Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send + 'a>> {
    Box::pin(async {
        Ok(SpawnResult {
            text: String::new(),
            status: LoopStatus::Cancelled,
            ..Default::default()
        })
    })
}

fn mock_spawn_incomplete<'a>(
    _session: &'a aish_llm::LlmSession,
    _ty: &'a str,
    _prompt: &'a str,
) -> Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send + 'a>> {
    Box::pin(async {
        Ok(SpawnResult {
            text: "[incomplete: max turns reached]\npartial conclusion".to_string(),
            status: LoopStatus::Incomplete,
            ..Default::default()
        })
    })
}

fn mock_spawn_fatal<'a>(
    _session: &'a aish_llm::LlmSession,
    _ty: &'a str,
    _prompt: &'a str,
) -> Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send + 'a>> {
    Box::pin(async {
        Ok(SpawnResult {
            text: String::new(),
            status: LoopStatus::Fatal,
            error_category: Some("llm".to_string()),
            partial_messages: vec![
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "c1".to_string(),
                        name: "grep".to_string(),
                        arguments: r#"{"pattern":"x"}"#.to_string(),
                    }]),
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                },
                ChatMessage::tool_result("c1", "SUBAGENT_TOOL_OK"),
            ],
        })
    })
}

#[tokio::test]
async fn test_agent_tool_missing_prompt_errors() {
    let tool = AgentTool::new();
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "description": "find nginx",
                "subagent_type": "explore"
            }),
            &session,
        )
        .await;
    assert!(!result.ok);
    assert!(result.output.contains("prompt"));
}

#[tokio::test]
async fn test_agent_tool_unknown_subagent_type_errors() {
    let tool = AgentTool::new();
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "description": "plan task",
                "prompt": "make a plan",
                "subagent_type": "command-diagnose"
            }),
            &session,
        )
        .await;
    assert!(!result.ok);
    assert!(result.output.contains("Unknown subagent_type"));
}

#[tokio::test]
async fn test_agent_tool_mock_spawn_success() {
    let spawn_fn: SpawnFn = Arc::new(mock_spawn_ok);
    let tool = AgentTool::with_spawn_fn(spawn_fn);
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "description": "find nginx",
                "prompt": "locate nginx config",
                "subagent_type": "explore"
            }),
            &session,
        )
        .await;
    assert!(result.ok);
    assert_eq!(result.output, "explore conclusion");
}

#[tokio::test]
async fn test_agent_tool_mock_spawn_cancelled_errors() {
    let spawn_fn: SpawnFn = Arc::new(mock_spawn_cancelled);
    let tool = AgentTool::with_spawn_fn(spawn_fn);
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    assert!(
        !session.cancellation_token().is_cancelled(),
        "precondition: parent session starts uncancelled"
    );
    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "description": "find nginx",
                "prompt": "locate nginx config",
                "subagent_type": "explore"
            }),
            &session,
        )
        .await;
    assert!(!result.ok);
    assert_eq!(result.output, aish_i18n::t("shell.interrupted"));
    assert_eq!(
        result
            .meta
            .as_ref()
            .and_then(|m| m.get("dispatch_status"))
            .and_then(|v| v.as_str()),
        Some("short_circuit")
    );
    assert_eq!(
        result
            .meta
            .as_ref()
            .and_then(|m| m.get("reason"))
            .and_then(|v| v.as_str()),
        Some("sub_agent_cancelled")
    );
    assert!(
        session.cancellation_token().is_cancelled(),
        "Cancelled spawn must cancel the parent session so the shell prints one interrupt line"
    );
}

#[tokio::test]
async fn test_agent_tool_mock_spawn_incomplete_success_with_prefix() {
    let spawn_fn: SpawnFn = Arc::new(mock_spawn_incomplete);
    let tool = AgentTool::with_spawn_fn(spawn_fn);
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "description": "long search",
                "prompt": "search everything",
                "subagent_type": "explore"
            }),
            &session,
        )
        .await;
    assert!(result.ok);
    assert!(result.output.starts_with("[incomplete: max turns reached]"));
    assert!(result.output.contains("partial conclusion"));
}

#[tokio::test]
async fn test_agent_tool_mock_spawn_fatal_errors() {
    let spawn_fn: SpawnFn = Arc::new(mock_spawn_fatal);
    let tool = AgentTool::with_spawn_fn(spawn_fn);
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "description": "run task",
                "prompt": "do work",
                "subagent_type": "explore"
            }),
            &session,
        )
        .await;
    assert!(!result.ok);
    // Output must be enriched with completed work, not bare "failed".
    assert!(
        result.output.contains("Sub-agent failed (llm)"),
        "output should name the category: got {}",
        result.output
    );
    assert!(
        result.output.contains("Completed before failure"),
        "output should list completed work: got {}",
        result.output
    );
    assert!(
        result.output.contains("SUBAGENT_TOOL_OK"),
        "output should include tool result evidence: got {}",
        result.output
    );
    // Structured meta for programmatic consumers.
    assert_eq!(
        result.meta.as_ref().and_then(|m| m.get("reason")),
        Some(&serde_json::Value::String("sub_agent_fatal".to_string()))
    );
    assert_eq!(
        result.meta.as_ref().and_then(|m| m.get("error_category")),
        Some(&serde_json::Value::String("llm".to_string()))
    );
    let completed = result
        .meta
        .as_ref()
        .and_then(|m| m.get("completed_tools"))
        .and_then(|v| v.as_array());
    assert_eq!(
        completed.map(|c| c.len()),
        Some(1),
        "completed_tools should have one entry"
    );
    assert_eq!(
        completed.and_then(|c| c.first()).and_then(|v| v.as_str()),
        Some("grep")
    );
    let partial = result
        .meta
        .as_ref()
        .and_then(|m| m.get("partial_messages"))
        .and_then(|v| v.as_array());
    assert_eq!(
        partial.map(|c| c.len()),
        Some(1),
        "partial_messages should have one tool result"
    );
    assert!(
        partial
            .and_then(|c| c.first())
            .and_then(|v| v.get("output"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("SUBAGENT_TOOL_OK")),
        "partial_messages should contain the tool result"
    );
}
