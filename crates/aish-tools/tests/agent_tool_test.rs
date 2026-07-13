use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aish_llm::{LoopStatus, SpawnResult, Tool};
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
                "subagent_type": "troubleshoot"
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
    assert!(result.output.contains("failed"));
    assert_eq!(
        result
            .meta
            .as_ref()
            .and_then(|m| m.get("reason"))
            .and_then(|v| v.as_str()),
        Some("sub_agent_fatal")
    );
}
