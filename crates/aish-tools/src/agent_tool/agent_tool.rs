//! `Agent` tool — synchronous sub-agent spawn entry point for the main LLM.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aish_llm::{
    spawn_builtin, AgentDefinition, AgentRegistry, LlmSession, LoopStatus, SpawnResult, Tool,
    ToolResult, ToolSpec,
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

/// Optional skill callbacks for `general-purpose` sub-agents.
pub type SkillLookupFn = Arc<dyn Fn(&str) -> Option<crate::SkillInfo> + Send + Sync>;
pub type SkillListFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
pub type SkillCallbacks = (SkillLookupFn, SkillListFn);

/// Tool that spawns built-in sub-agents (`explore`, `plan`, `general-purpose`).
pub struct AgentTool {
    registry: AgentRegistry,
    description: String,
    spawn_fn: Option<SpawnFn>,
    skill_callbacks: Option<SkillCallbacks>,
}

impl AgentTool {
    pub fn new() -> Self {
        Self::with_skill_callbacks(None)
    }

    /// Create an `AgentTool` that can register inherited `skill` tools for general-purpose spawns.
    pub fn with_skill_callbacks(skill_callbacks: Option<SkillCallbacks>) -> Self {
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
            skill_callbacks,
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
            LoopStatus::Cancelled => {
                Self::short_circuit_error("Sub-agent cancelled", "sub_agent_cancelled")
            }
            LoopStatus::Fatal => Self::short_circuit_error("Sub-agent failed", "sub_agent_fatal"),
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

    fn register_read_only_tools(sub: &mut LlmSession, specs: &[ToolSpec]) {
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

    fn register_general_purpose_tools(
        sub: &mut LlmSession,
        specs: &[ToolSpec],
        skill_callbacks: &Option<SkillCallbacks>,
    ) {
        for spec in specs {
            match spec.function.name.as_str() {
                "grep" => sub.register_tool(Box::new(crate::GrepTool::new())),
                "glob" => sub.register_tool(Box::new(crate::GlobTool::new())),
                "read_file" => sub.register_tool(Box::new(crate::ReadFileTool::new())),
                "write_file" => sub.register_tool(Box::new(crate::WriteFileTool::new())),
                "edit_file" => sub.register_tool(Box::new(crate::EditFileTool::new())),
                "bash" => {
                    let mut bash = crate::bash::BashTool::new();
                    bash.set_cancellation_token(sub.cancellation_token_arc());
                    sub.register_tool(Box::new(bash));
                }
                "skill" => {
                    if let Some((lookup, list)) = skill_callbacks {
                        sub.register_tool(Box::new(crate::SkillTool::new(
                            Box::new({
                                let lookup = Arc::clone(lookup);
                                move |name| lookup(name)
                            }),
                            Box::new({
                                let list = Arc::clone(list);
                                move || list()
                            }),
                        )));
                    }
                }
                _ => {}
            }
        }
    }

    fn register_tools_for_definition(
        &self,
        sub: &mut LlmSession,
        def: &AgentDefinition,
        specs: &[ToolSpec],
    ) {
        match def.subagent_type.as_str() {
            "general-purpose" => {
                Self::register_general_purpose_tools(sub, specs, &self.skill_callbacks)
            }
            _ => Self::register_read_only_tools(sub, specs),
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
        Box::pin(async move {
            let (_description, prompt, subagent_type) = match Self::validate_args(&args) {
                Ok(v) => v,
                Err(err) => return err,
            };

            let def = match self.registry.resolve(subagent_type) {
                Ok(def) => def.clone(),
                Err(err) => return ToolResult::error(err),
            };

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
                    self.register_tools_for_definition(sub, &def, specs);
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
    fn description_includes_all_builtins() {
        let tool = AgentTool::new();
        assert!(tool.description().contains("explore"));
        assert!(tool.description().contains("plan"));
        assert!(tool.description().contains("general-purpose"));
        assert!(tool.description().contains("read-only"));
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
    }
}
