//! `Agent` tool — synchronous sub-agent spawn entry point for the main LLM.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aish_llm::{
    spawn_builtin, AgentRegistry, LlmSession, LoopStatus, SpawnResult, Tool, ToolResult, ToolSpec,
};

use super::prompt;

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

/// Tool that spawns built-in sub-agents (Phase 1: `explore` only).
pub struct AgentTool {
    registry: AgentRegistry,
    description: String,
    spawn_fn: Option<SpawnFn>,
}

impl AgentTool {
    pub fn new() -> Self {
        let registry = AgentRegistry::builtin();
        let description = format!(
            "{}\n{}",
            prompt::DESCRIPTION,
            registry.list_for_tool_description()
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

    fn spawn_result_to_tool_result(result: SpawnResult) -> ToolResult {
        match result.status {
            LoopStatus::Complete | LoopStatus::Incomplete => ToolResult::success(result.text),
            LoopStatus::Cancelled => ToolResult::error("Sub-agent cancelled"),
            LoopStatus::Fatal => ToolResult::error("Sub-agent failed"),
        }
    }

    fn register_explore_tools(sub: &mut LlmSession, specs: &[ToolSpec]) {
        for spec in specs {
            match spec.function.name.as_str() {
                "grep" => sub.register_tool(Box::new(crate::GrepTool::new())),
                "glob" => sub.register_tool(Box::new(crate::GlobTool::new())),
                "read_file" => sub.register_tool(Box::new(crate::ReadFileTool::new())),
                "bash" => {
                    let mut bash = crate::bash::BashTool::new();
                    bash.set_cancellation_token(sub.cancellation_token_arc());
                    sub.register_tool(Box::new(bash));
                }
                _ => {}
            }
        }
    }
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
        prompt::parameters()
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        ToolResult::error("Agent requires async execution; use execute_async_in_session")
    }

    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let (_description, prompt, subagent_type) = match Self::validate_args(&args) {
                Ok(v) => v,
                Err(err) => return err,
            };

            if self.registry.resolve(subagent_type).is_err() {
                return ToolResult::error(format!("Unknown subagent_type: {subagent_type}"));
            }

            if let Some(spawn_fn) = &self.spawn_fn {
                let result = match spawn_fn(session, subagent_type, prompt).await {
                    Ok(result) => result,
                    Err(err) => return ToolResult::error(err),
                };
                return Self::spawn_result_to_tool_result(result);
            }

            let registry = self.registry.clone();
            let result =
                match spawn_builtin(session, &registry, subagent_type, prompt, |sub, specs| {
                    Self::register_explore_tools(sub, specs)
                })
                .await
                {
                    Ok(result) => result,
                    Err(err) => return ToolResult::error(err),
                };

            Self::spawn_result_to_tool_result(result)
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
    fn description_includes_explore_builtin() {
        let tool = AgentTool::new();
        assert!(tool.description().contains("explore"));
        assert!(tool.description().contains("read-only"));
    }
}
