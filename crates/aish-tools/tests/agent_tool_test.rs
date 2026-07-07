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
                "subagent_type": "plan"
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
    assert!(result.output.contains("cancelled"));
}
