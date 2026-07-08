//! Contract tests for [`PromptAssembly::build`] (Phase A/B).

use aish_core::PlanPhase;
use aish_llm::{LlmSession, PromptAssembly, PromptContext, Tool};
use aish_tools::{AgentTool, EnterPlanModeTool};

struct MockTool {
    name: String,
    prompt: String,
}

impl MockTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            prompt: String::new(),
        }
    }

    fn with_prompt(name: &str, prompt: &str) -> Self {
        Self {
            name: name.to_string(),
            prompt: prompt.to_string(),
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

    fn prompt(&self) -> &str {
        &self.prompt
    }

    fn execute(&self, _args: serde_json::Value) -> aish_llm::ToolResult {
        aish_llm::ToolResult::success("ok")
    }
}

fn register_main_chat_toolkit(session: &mut LlmSession) {
    for name in [
        "grep",
        "read_file",
        "bash_exec",
        "Agent",
        "enter_plan_mode",
        "exit_plan_mode",
    ] {
        session.register_tool(Box::new(MockTool::new(name)));
    }
}

#[test]
fn main_chat_normal_includes_agent_and_plan_mode_tools() {
    let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
    register_main_chat_toolkit(&mut session);

    let bundle = PromptAssembly::build(&session, PromptContext::MainChat, "oracle");

    let names: Vec<_> = bundle
        .tool_specs
        .iter()
        .map(|s| s.function.name.as_str())
        .collect();
    assert!(names.contains(&"Agent"));
    assert!(names.contains(&"enter_plan_mode"));
    assert_eq!(names.len(), 6);
}

#[test]
fn main_chat_planning_limits_to_planning_visible_tools() {
    let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
    register_main_chat_toolkit(&mut session);
    session.register_tool(Box::new(MockTool::new("write_file")));

    {
        let plan_state = session.plan_state();
        let mut state = plan_state.lock().unwrap();
        state.phase = PlanPhase::Planning;
    }

    let bundle = PromptAssembly::build(&session, PromptContext::MainChat, "oracle");

    let names: Vec<_> = bundle
        .tool_specs
        .iter()
        .map(|s| s.function.name.as_str())
        .collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"write_file"));
    assert!(names.contains(&"exit_plan_mode"));
    assert!(!names.contains(&"bash_exec"));
    assert!(!names.contains(&"Agent"));
    assert!(!names.contains(&"enter_plan_mode"));
}

#[test]
fn sub_agent_plan_excludes_plan_mode_agent_and_write_tools() {
    let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
    register_main_chat_toolkit(&mut session);
    session.register_tool(Box::new(MockTool::with_prompt("grep", "grep usage")));
    session.register_tool(Box::new(MockTool::with_prompt(
        "enter_plan_mode",
        "plan mode usage",
    )));

    let bundle = PromptAssembly::build(
        &session,
        PromptContext::SubAgent {
            subagent_type: "plan".to_string(),
        },
        "sub system",
    );

    let names: Vec<_> = bundle
        .tool_specs
        .iter()
        .map(|s| s.function.name.as_str())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"grep"));
    assert!(names.contains(&"read_file"));
    assert!(!bundle.system_message.contains("enter_plan_mode"));
    assert!(!bundle.system_message.contains("plan mode usage"));
    assert!(bundle.system_message.contains("grep usage"));
}

#[test]
fn sub_session_preflight_blocks_enter_plan_mode() {
    let parent = LlmSession::new("http://localhost", "key", "model", None, None);
    let mut sub = parent.create_subsession();
    sub.register_tool(Box::new(MockTool::new("enter_plan_mode")));

    assert!(sub.is_sub_agent());

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let result = rt
        .block_on(sub.execute_tool_by_name("enter_plan_mode", serde_json::json!({})))
        .expect("tool registered");
    assert!(!result.ok);
    assert!(result
        .output
        .contains("not available in sub-agent sessions"));
}

#[test]
fn main_chat_agent_tool_spec_includes_routing_table() {
    let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
    session.register_tool(Box::new(AgentTool::new()));
    session.register_tool(Box::new(EnterPlanModeTool::new()));

    let bundle = PromptAssembly::build(&session, PromptContext::MainChat, "oracle");

    let agent_spec = bundle
        .tool_specs
        .iter()
        .find(|s| s.function.name == "Agent")
        .expect("Agent spec");
    assert!(agent_spec
        .function
        .description
        .contains("## Routing: planning vs plan mode vs sub-agents"));
    assert!(agent_spec
        .function
        .description
        .contains("Agent(subagent_type=plan)"));

    let enter_spec = bundle
        .tool_specs
        .iter()
        .find(|s| s.function.name == "enter_plan_mode")
        .expect("enter_plan_mode spec");
    assert!(enter_spec.function.description.contains(".aish/plans/"));
}
