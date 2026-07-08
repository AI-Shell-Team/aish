//! Central tool visibility rules for prompt assembly.

use aish_core::PlanPhase;

use crate::agents::{parent_has_skill_tool, resolve_tool_names_for_agent, AgentRegistry};
use crate::session::LlmSession;

use super::context::PromptContext;

/// Tools never exposed to sub-agent loops (hard deny).
pub const SUBAGENT_GLOBAL_DENY: &[&str] = &["enter_plan_mode", "exit_plan_mode", "Agent"];

pub struct ToolVisibilityPolicy;

impl ToolVisibilityPolicy {
    /// Registered tool names visible for `context`, sorted lexicographically.
    pub fn visible_tool_names(session: &LlmSession, context: &PromptContext) -> Vec<String> {
        match context {
            PromptContext::MainChat => Self::main_chat_visible(session),
            PromptContext::SubAgent { subagent_type } => {
                Self::sub_agent_visible(session, subagent_type)
            }
        }
    }

    fn main_chat_visible(session: &LlmSession) -> Vec<String> {
        let plan_state = session.plan_state();
        let phase = plan_state.lock().unwrap().phase.clone();
        let mut names: Vec<String> = session
            .tool_specs()
            .into_iter()
            .map(|spec| spec.function.name)
            .filter(|name| tool_visible_in_main_chat_phase(name, &phase))
            .collect();
        names.sort();
        names
    }

    fn sub_agent_visible(session: &LlmSession, subagent_type: &str) -> Vec<String> {
        let tool_specs = session.tool_specs();
        let registered: Vec<String> = tool_specs.into_iter().map(|s| s.function.name).collect();
        let registered_refs: Vec<&str> = registered.iter().map(|s| s.as_str()).collect();
        let parent_has_skill = parent_has_skill_tool(&session.tool_specs());
        let registry = AgentRegistry::builtin();

        let mut names = match registry.resolve(subagent_type) {
            Ok(def) => resolve_tool_names_for_agent(def, &registered_refs, parent_has_skill),
            Err(_) => registered,
        };

        names.retain(|name| !SUBAGENT_GLOBAL_DENY.contains(&name.as_str()));
        names.sort();
        names
    }
}

fn tool_visible_in_main_chat_phase(tool_name: &str, phase: &PlanPhase) -> bool {
    match phase {
        PlanPhase::Normal => true,
        PlanPhase::Planning => aish_core::PLANNING_VISIBLE_TOOLS.contains(&tool_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tool;
    use aish_core::PlanPhase;

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
            serde_json::json!({})
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("ok")
        }
    }

    #[test]
    fn main_chat_normal_includes_all_registered_tools() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::new("bash_exec")));
        session.register_tool(Box::new(MockTool::new("grep")));

        let names = ToolVisibilityPolicy::visible_tool_names(&session, &PromptContext::MainChat);
        assert_eq!(names, vec!["bash_exec".to_string(), "grep".to_string()]);
    }

    #[test]
    fn main_chat_planning_filters_to_planning_visible_tools() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::new("read_file")));
        session.register_tool(Box::new(MockTool::new("bash_exec")));

        {
            let plan_state = session.plan_state();
            let mut state = plan_state.lock().unwrap();
            state.phase = PlanPhase::Planning;
        }

        let names = ToolVisibilityPolicy::visible_tool_names(&session, &PromptContext::MainChat);
        assert_eq!(names, vec!["read_file".to_string()]);
    }

    #[test]
    fn sub_agent_plan_excludes_plan_mode_agent_and_writes() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        for name in [
            "grep",
            "read_file",
            "write_file",
            "enter_plan_mode",
            "Agent",
        ] {
            session.register_tool(Box::new(MockTool::new(name)));
        }

        let names = ToolVisibilityPolicy::visible_tool_names(
            &session,
            &PromptContext::SubAgent {
                subagent_type: "plan".to_string(),
            },
        );
        assert_eq!(names, vec!["grep".to_string(), "read_file".to_string(),]);
    }
}
