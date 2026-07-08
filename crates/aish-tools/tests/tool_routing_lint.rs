//! Routing copy lint: routing tables belong in tool descriptions, not prompts.

use aish_llm::Tool;
use aish_tools::{AgentTool, EnterPlanModeTool};

const ROUTING_TABLE_MARKER: &str = "## Routing: planning vs plan mode vs sub-agents";

#[test]
fn agent_description_contains_planning_routing_table() {
    let tool = AgentTool::new();
    let description = tool.description();
    assert!(description.contains(ROUTING_TABLE_MARKER));
    assert!(description.contains("Agent(subagent_type=plan)"));
    assert!(description.contains("enter_plan_mode"));
    assert!(description.contains("Agent(subagent_type=explore)"));
}

#[test]
fn enter_plan_mode_description_emphasizes_artifact_and_points_to_agent_plan() {
    let tool = EnterPlanModeTool::new();
    let description = tool.description();
    assert!(description.contains(".aish/plans/"));
    assert!(description.contains("Agent(subagent_type=plan)"));
    assert!(description.contains("approval"));
}

#[test]
fn enter_plan_mode_prompt_has_when_not_without_full_routing_table() {
    let tool = EnterPlanModeTool::new();
    assert!(!tool.description().contains(ROUTING_TABLE_MARKER));
    assert!(!tool.prompt().contains(ROUTING_TABLE_MARKER));
    assert!(tool.prompt().contains("When NOT to Use"));
    assert!(tool.prompt().contains("Agent(subagent_type=plan)"));
}

#[test]
fn agent_prompt_is_empty_routing_lives_in_description_only() {
    let tool = AgentTool::new();
    assert!(tool.prompt().trim().is_empty());
    assert!(!tool.prompt().contains(ROUTING_TABLE_MARKER));
}

#[test]
fn agent_description_contains_delegation_guidance() {
    let tool = AgentTool::new();
    let description = tool.description();
    assert!(description.contains("## When NOT to use the Agent tool"));
    assert!(description.contains("## Usage notes"));
    assert!(description.contains("read_file (not Agent)"));
    assert!(description.contains("subagent_type=explore"));
    assert!(description.contains("general-purpose"));
    assert!(description.contains("thoroughness"));
}

#[test]
fn glob_and_grep_descriptions_cover_main_and_sub_agent_guidance() {
    use aish_tools::{GlobTool, GrepTool};

    let glob_tool = GlobTool::new();
    let grep_tool = GrepTool::new();
    let glob = glob_tool.description();
    let grep = grep_tool.description();

    assert!(glob.contains("Agent(subagent_type=explore)"));
    assert!(glob.contains("Inside a sub-agent"));
    assert!(glob.contains("broad recursive pattern"));
    assert!(grep.contains("Agent(subagent_type=explore)"));
    assert!(grep.contains("Inside a sub-agent"));
}

#[test]
fn search_tools_route_open_ended_exploration_to_agent() {
    use aish_tools::bash::BashTool;
    use aish_tools::{GlobTool, GrepTool};

    let bash_tool = BashTool::new();
    let glob_tool = GlobTool::new();
    let grep_tool = GrepTool::new();
    let bash = bash_tool.description();
    let glob = glob_tool.description();
    let grep = grep_tool.description();

    assert!(bash.contains("subagent_type=explore"));
    assert!(glob.contains("Agent(subagent_type=explore)"));
    assert!(grep.contains("Agent(subagent_type=explore)"));
}
