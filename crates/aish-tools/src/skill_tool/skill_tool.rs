use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aish_llm::{
    spawn_request, AgentDefinition, AgentRegistry, ChatMessage, LlmSession, LoopStatus,
    SpawnLabels, SpawnRequest, SpawnResult, Tool, ToolResult, ToolStrategy,
};
use aish_skills::SkillExecutionContext;

use super::prompt;

/// Callback type for looking up a skill by name.
pub type SkillLookupFn = Box<dyn Fn(&str) -> Option<SkillInfo> + Send + Sync>;
pub type SkillListFn = Box<dyn Fn() -> Vec<String> + Send + Sync>;
type SharedSkillLookupFn = Arc<dyn Fn(&str) -> Option<SkillInfo> + Send + Sync>;
type SharedSkillListFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Skill information returned by the lookup callback.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub content: String,
    pub description: String,
    pub base_dir: String,
    pub context: SkillExecutionContext,
    pub agent: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    /// True if the skill is quarantined (unreviewed registry install). The
    /// skill tool refuses to load quarantined skills so the quarantine does
    /// not depend solely on callers filtering them out of the lookup.
    pub quarantined: bool,
}

#[derive(Debug, Clone)]
pub struct SkillSpawnRequest {
    pub definition: AgentDefinition,
    pub prompt: String,
    pub context_messages: Vec<ChatMessage>,
    pub skill_name: String,
}

pub type SkillSpawnFn = Arc<
    dyn for<'a> Fn(
            &'a LlmSession,
            SkillSpawnRequest,
        )
            -> Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send + 'a>>
        + Send
        + Sync,
>;

/// Tool for invoking skill plugins within the AI conversation.
pub struct SkillTool {
    lookup: SharedSkillLookupFn,
    list: SharedSkillListFn,
    prompt: String,
    spawn_fn: Option<SkillSpawnFn>,
}

impl SkillTool {
    pub fn new(lookup: SkillLookupFn, list: SkillListFn) -> Self {
        Self {
            lookup: Arc::from(lookup),
            list: Arc::from(list),
            prompt: prompt::PROMPT.to_string(),
            spawn_fn: None,
        }
    }

    pub fn with_spawn_fn(lookup: SkillLookupFn, list: SkillListFn, spawn_fn: SkillSpawnFn) -> Self {
        Self {
            lookup: Arc::from(lookup),
            list: Arc::from(list),
            prompt: prompt::PROMPT.to_string(),
            spawn_fn: Some(spawn_fn),
        }
    }

    /// Create a no-op skill tool that always returns "no skills".
    pub fn noop() -> Self {
        Self {
            lookup: Arc::new(|_| None),
            list: Arc::new(Vec::new),
            prompt: prompt::PROMPT.to_string(),
            spawn_fn: None,
        }
    }

    fn render(skill: &SkillInfo, args: &str) -> String {
        skill
            .content
            .replace("{{args}}", args)
            .replace("{{ skill_name }}", &skill.name)
    }

    fn resolve_spawn_request(
        skill: &SkillInfo,
        prompt: &str,
    ) -> Result<SkillSpawnRequest, ToolResult> {
        let agent = skill.agent.as_deref().ok_or_else(|| {
            ToolResult::error(format!(
                "Skill '{}' uses context=subagent but does not declare an agent",
                skill.name
            ))
        })?;
        let registry = AgentRegistry::builtin();
        let mut definition = registry.resolve(agent).map_err(ToolResult::error)?.clone();
        if let Some(skill_tools) = &skill.allowed_tools {
            definition.tool_strategy = intersect_tool_strategy(&definition, skill_tools);
        }
        let rendered = Self::render(skill, prompt);
        let context_messages = vec![ChatMessage::user(format!(
            "<skill-instructions name=\"{}\">\n{}\n</skill-instructions>",
            skill.name, rendered
        ))];

        Ok(SkillSpawnRequest {
            definition,
            prompt: prompt.to_string(),
            context_messages,
            skill_name: skill.name.clone(),
        })
    }
}

/// Narrow an agent tool strategy by a skill's `allowed-tools` to the common usable set.
///
/// Allowlist agents: intersection (agent order preserved).
/// Denylist agents: materialize an allowlist of `skill_tools` minus denied names — not a
/// denylist intersection, because skill metadata only lists positives.
fn intersect_tool_strategy(definition: &AgentDefinition, skill_tools: &[String]) -> ToolStrategy {
    match &definition.tool_strategy {
        ToolStrategy::Allowlist(agent_tools) => ToolStrategy::Allowlist(
            agent_tools
                .iter()
                .filter(|tool| {
                    skill_tools
                        .iter()
                        .any(|skill_tool| aish_llm::tool_names_match(tool, skill_tool))
                })
                .cloned()
                .collect(),
        ),
        ToolStrategy::Denylist(denied) => ToolStrategy::Allowlist(
            skill_tools
                .iter()
                .map(|tool| aish_llm::canonicalize_tool_name(tool))
                .filter(|tool| {
                    !denied
                        .iter()
                        .any(|denied| aish_llm::tool_names_match(tool, denied))
                })
                .collect(),
        ),
    }
}

fn build_prompt(
    lookup: &dyn Fn(&str) -> Option<SkillInfo>,
    list: &dyn Fn() -> Vec<String>,
) -> String {
    let catalog = list()
        .into_iter()
        .filter_map(|name| {
            lookup(&name).map(|skill| format!("- {}: {}", skill.name, skill.description))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if catalog.is_empty() {
        prompt::PROMPT.to_string()
    } else {
        format!("{}\n\nAvailable skills:\n{}", prompt::PROMPT, catalog)
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        &self.prompt
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
            Some(n) => n.trim(),
            None => return ToolResult::error("Missing 'skill_name' parameter"),
        };
        let user_args = args.get("args").and_then(|v| v.as_str()).unwrap_or("");

        if skill_name.is_empty() {
            return ToolResult::error("Skill name cannot be empty");
        }

        match (self.lookup)(skill_name) {
            Some(skill) => {
                if skill.quarantined {
                    return ToolResult::error(format!(
                        "Skill '{}' is quarantined (untrusted, not reviewed). Trust it first.",
                        skill.name
                    ));
                }
                if skill.context == SkillExecutionContext::SubAgent {
                    return ToolResult::error(format!(
                        "Skill '{}' uses context=subagent and must run via session async spawn; \
synchronous skill.execute cannot expand it inline",
                        skill.name
                    ));
                }
                let rendered = Self::render(&skill, user_args);

                ToolResult {
                    ok: true,
                    output: rendered,
                    meta: Some(serde_json::json!({
                        "skill_name": skill.name,
                        "description": skill.description,
                    })),
                }
            }
            None => {
                let available = (self.list)();
                let available_str = if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                };
                ToolResult::error(format!(
                    "Skill '{}' not found. Available skills: {}",
                    skill_name, available_str
                ))
            }
        }
    }

    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let skill_name = match args.get("skill_name").and_then(|value| value.as_str()) {
                Some(name) if !name.trim().is_empty() => name.trim(),
                _ => return ToolResult::error("Missing 'skill_name' parameter"),
            };
            let user_args = args
                .get("args")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let Some(skill) = (self.lookup)(skill_name) else {
                return self.execute(args);
            };

            if skill.quarantined {
                return ToolResult::error(format!(
                    "Skill '{}' is quarantined (untrusted, not reviewed). Trust it first.",
                    skill.name
                ));
            }

            if skill.context == SkillExecutionContext::Inline || session.is_sub_agent() {
                return ToolResult {
                    ok: true,
                    output: Self::render(&skill, user_args),
                    meta: Some(serde_json::json!({
                        "skill_name": skill.name,
                        "description": skill.description,
                    })),
                };
            }

            if user_args.trim().is_empty() {
                return ToolResult::error(format!(
                    "Skill '{}' runs in a sub-agent; args must include the user's task",
                    skill.name
                ));
            }

            let request = match Self::resolve_spawn_request(&skill, user_args) {
                Ok(request) => request,
                Err(error) => return error,
            };
            let result = if let Some(spawn_fn) = &self.spawn_fn {
                spawn_fn(session, request).await
            } else {
                let mut spawn = SpawnRequest::new(&request.definition, &request.prompt);
                spawn.context_messages = request.context_messages;
                spawn.labels = SpawnLabels {
                    skill_name: Some(request.skill_name),
                };
                Ok(spawn_request(session, spawn, |_sub, _specs| {}).await)
            };
            match result {
                Ok(result) => {
                    if result.status == LoopStatus::Cancelled {
                        session.cancellation_token().cancel();
                    }
                    crate::AgentTool::spawn_result_to_tool_result(result)
                }
                Err(error) => ToolResult::error(error),
            }
        })
    }

    fn for_sub_session(&self, _sub: &LlmSession) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            lookup: Arc::clone(&self.lookup),
            list: Arc::clone(&self.list),
            prompt: build_prompt(self.lookup.as_ref(), self.list.as_ref()),
            spawn_fn: self.spawn_fn.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(name: &str, description: &str, content: &str) -> SkillInfo {
        SkillInfo {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            base_dir: "/tmp".to_string(),
            context: SkillExecutionContext::Inline,
            agent: None,
            allowed_tools: None,
            quarantined: false,
        }
    }

    fn make_tool(skills: Vec<SkillInfo>) -> SkillTool {
        let skills_clone = skills.clone();
        let lookup =
            Box::new(move |name: &str| skills_clone.iter().find(|s| s.name == name).cloned());
        let names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
        let list = Box::new(move || names.clone());
        SkillTool::new(lookup, list)
    }

    #[test]
    fn test_skill_execute_found() {
        let tool = make_tool(vec![make_skill(
            "greet",
            "Greets the user",
            "Hello, {{args}}! Welcome to {{ skill_name }}.",
        )]);
        let result = tool.execute(serde_json::json!({
            "skill_name": "greet",
            "args": "Alice"
        }));
        assert!(result.ok);
        assert!(result.output.contains("Hello, Alice!"));
        assert!(result.output.contains("Welcome to greet."));
        let meta = result.meta.unwrap();
        assert_eq!(meta["skill_name"], "greet");
        assert_eq!(meta["description"], "Greets the user");
    }

    #[test]
    fn test_skill_execute_not_found() {
        let tool = make_tool(vec![make_skill("greet", "Greets the user", "Hello!")]);
        let result = tool.execute(serde_json::json!({
            "skill_name": "missing"
        }));
        assert!(!result.ok);
        assert!(result.output.contains("'missing' not found"));
        assert!(result.output.contains("greet"));
    }

    #[test]
    fn test_skill_execute_no_name() {
        let tool = SkillTool::noop();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
        assert!(result.output.contains("Missing 'skill_name'"));
    }

    #[test]
    fn subsession_adapter_lists_available_skills_without_changing_main_prompt() {
        let tool = make_tool(vec![make_skill(
            "host-diagnose",
            "Diagnose host performance",
            "Inspect the host",
        )]);
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let sub = parent.create_subsession();
        let child_tool = tool
            .for_sub_session(&sub)
            .expect("SkillTool should adapt for sub-session discovery");

        assert!(!tool.prompt().contains("host-diagnose"));
        assert!(child_tool.prompt().contains("host-diagnose"));
        assert!(child_tool.prompt().contains("Diagnose host performance"));
    }

    #[test]
    fn skill_web_fetch_alias_intersects_webfetch() {
        let def = AgentDefinition::general_purpose();
        match intersect_tool_strategy(&def, &["web_fetch".into(), "bash".into()]) {
            ToolStrategy::Allowlist(names) => {
                assert!(names.iter().any(|name| name == "WebFetch"));
                assert!(names.iter().any(|name| name == "bash"));
            }
            other => panic!("expected allowlist, got {other:?}"),
        }
    }
}
