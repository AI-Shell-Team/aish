//! Tool filtering for sub-agent spawn paths.

use super::registry::{AgentDefinition, ToolStrategy};
use crate::types::ToolSpec;

const SKILL_TOOL_NAME: &str = "skill";

/// Whether the parent session exposes the `skill` tool.
pub fn parent_has_skill_tool(parent_tools: &[ToolSpec]) -> bool {
    parent_tools
        .iter()
        .any(|spec| spec.function.name == SKILL_TOOL_NAME)
}

/// Resolve which parent tool names a sub-agent may use.
pub fn resolve_tool_names_for_agent(
    def: &AgentDefinition,
    parent_tool_names: &[&str],
    parent_has_skill: bool,
) -> Vec<String> {
    match &def.tool_strategy {
        ToolStrategy::Allowlist(allowed) => allowed
            .iter()
            .filter(|name| parent_tool_names.contains(&name.as_str()))
            .cloned()
            .collect(),
        ToolStrategy::Denylist(denied) => parent_tool_names
            .iter()
            .filter(|name| {
                if denied.contains(&name.to_string()) {
                    return false;
                }
                if **name == SKILL_TOOL_NAME && !parent_has_skill {
                    return false;
                }
                true
            })
            .map(|name| (*name).to_string())
            .collect(),
    }
}

/// Resolve tool specs from the parent pool for a sub-agent definition.
pub fn resolve_tools_for_agent(
    def: &AgentDefinition,
    parent_tools: &[ToolSpec],
    parent_has_skill: bool,
) -> Vec<ToolSpec> {
    let parent_names: Vec<&str> = parent_tools
        .iter()
        .map(|spec| spec.function.name.as_str())
        .collect();
    let allowed = resolve_tool_names_for_agent(def, &parent_names, parent_has_skill);
    parent_tools
        .iter()
        .filter(|spec| allowed.iter().any(|n| n == &spec.function.name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentDefinition;
    use crate::types::{FunctionSpec, ToolSpec};

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            r#type: "function".into(),
            function: FunctionSpec {
                name: name.into(),
                description: String::new(),
                parameters: serde_json::json!({}),
            },
        }
    }

    fn explore_def() -> AgentDefinition {
        AgentDefinition::explore()
    }

    fn plan_def() -> AgentDefinition {
        AgentDefinition::plan()
    }

    fn general_purpose_def() -> AgentDefinition {
        AgentDefinition::general_purpose()
    }

    #[test]
    fn test_explore_allowlist_excludes_write_edit_and_agent() {
        let parent = vec![
            "grep",
            "glob",
            "read_file",
            "bash",
            "write_file",
            "edit_file",
            "Agent",
        ];
        let names = resolve_tool_names_for_agent(&explore_def(), &parent, false);
        assert_eq!(
            names,
            vec![
                "grep".to_string(),
                "glob".to_string(),
                "read_file".to_string(),
                "bash".to_string(),
            ]
        );
    }

    #[test]
    fn test_explore_resolves_specs_from_parent_pool() {
        let parent = vec![
            spec("grep"),
            spec("write_file"),
            spec("read_file"),
            spec("bash"),
        ];
        let resolved = resolve_tools_for_agent(&explore_def(), &parent, false);
        let names: Vec<_> = resolved.iter().map(|s| s.function.name.as_str()).collect();
        assert_eq!(names, vec!["grep", "read_file", "bash"]);
    }

    #[test]
    fn test_plan_allowlist_matches_explore_excludes_plan_mode_tools() {
        let parent = vec![
            "grep",
            "glob",
            "read_file",
            "bash",
            "write_file",
            "edit_file",
            "Agent",
            "enter_plan_mode",
            "exit_plan_mode",
        ];
        let names = resolve_tool_names_for_agent(&plan_def(), &parent, false);
        assert_eq!(
            names,
            vec![
                "grep".to_string(),
                "glob".to_string(),
                "read_file".to_string(),
                "bash".to_string(),
            ]
        );
    }

    #[test]
    fn test_plan_resolves_specs_without_plan_mode_tools() {
        let parent = vec![
            spec("grep"),
            spec("enter_plan_mode"),
            spec("exit_plan_mode"),
            spec("read_file"),
        ];
        let resolved = resolve_tools_for_agent(&plan_def(), &parent, false);
        let names: Vec<_> = resolved.iter().map(|s| s.function.name.as_str()).collect();
        assert_eq!(names, vec!["grep", "read_file"]);
    }

    #[test]
    fn test_general_purpose_denylist_excludes_agent_keeps_parent_tools() {
        let parent = vec!["bash", "read_file", "write_file", "Agent", "grep"];
        let names = resolve_tool_names_for_agent(&general_purpose_def(), &parent, false);
        assert_eq!(
            names,
            vec![
                "bash".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
                "grep".to_string(),
            ]
        );
    }

    #[test]
    fn test_general_purpose_includes_skill_when_parent_has_skill() {
        let parent = vec![spec("bash"), spec("skill"), spec("Agent")];
        let resolved = resolve_tools_for_agent(&general_purpose_def(), &parent, true);
        let names: Vec<_> = resolved.iter().map(|s| s.function.name.as_str()).collect();
        assert_eq!(names, vec!["bash", "skill"]);
    }

    #[test]
    fn test_general_purpose_excludes_skill_when_parent_lacks_skill() {
        let parent = vec![spec("bash"), spec("read_file"), spec("Agent")];
        let resolved = resolve_tools_for_agent(&general_purpose_def(), &parent, false);
        let names: Vec<_> = resolved.iter().map(|s| s.function.name.as_str()).collect();
        assert_eq!(names, vec!["bash", "read_file"]);
    }

    #[test]
    fn test_parent_has_skill_tool_detects_skill_in_pool() {
        let parent = vec![spec("bash"), spec("skill")];
        assert!(parent_has_skill_tool(&parent));
        let without = vec![spec("bash")];
        assert!(!parent_has_skill_tool(&without));
    }

    #[test]
    fn test_explore_and_plan_do_not_include_skill_even_when_parent_has_skill() {
        let parent = vec![spec("grep"), spec("skill"), spec("bash")];
        for def in [explore_def(), plan_def()] {
            let resolved = resolve_tools_for_agent(&def, &parent, true);
            let names: Vec<_> = resolved.iter().map(|s| s.function.name.as_str()).collect();
            assert!(!names.contains(&"skill"));
        }
    }

    #[test]
    fn test_troubleshoot_allowlist_includes_readonly_plus_skill_excludes_write_agent() {
        let parent = vec![
            "grep",
            "glob",
            "read_file",
            "bash",
            "skill",
            "write_file",
            "edit_file",
            "Agent",
        ];
        let names = resolve_tool_names_for_agent(&troubleshoot_def(), &parent, true);
        assert_eq!(
            names,
            vec![
                "grep".to_string(),
                "glob".to_string(),
                "read_file".to_string(),
                "bash".to_string(),
                "skill".to_string(),
            ]
        );
    }

    #[test]
    fn test_troubleshoot_excludes_skill_when_not_in_parent_pool() {
        let parent = vec!["grep", "bash", "read_file", "Agent"];
        let names = resolve_tool_names_for_agent(&troubleshoot_def(), &parent, false);
        assert!(!names.iter().any(|n| n == "skill"));
        assert!(!names.iter().any(|n| n == "Agent"));
    }

    #[test]
    fn test_command_diagnose_allowlist_excludes_skill_and_writes() {
        let parent = vec![
            "grep",
            "glob",
            "read_file",
            "bash",
            "skill",
            "write_file",
            "Agent",
        ];
        let names = resolve_tool_names_for_agent(&command_diagnose_def(), &parent, true);
        assert_eq!(
            names,
            vec![
                "grep".to_string(),
                "glob".to_string(),
                "read_file".to_string(),
                "bash".to_string(),
            ]
        );
        assert!(!names.iter().any(|n| n == "skill"));
    }

    fn troubleshoot_def() -> AgentDefinition {
        AgentDefinition::troubleshoot()
    }

    fn command_diagnose_def() -> AgentDefinition {
        AgentDefinition::command_diagnose("sys".into())
    }
}
