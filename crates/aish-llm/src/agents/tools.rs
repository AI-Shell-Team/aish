//! Tool filtering for sub-agent spawn paths.

use super::registry::{AgentDefinition, ToolStrategy};
use crate::types::ToolSpec;

/// Resolve which parent tool names a sub-agent may use.
pub fn resolve_tool_names_for_agent(
    def: &AgentDefinition,
    parent_tool_names: &[&str],
) -> Vec<String> {
    let ToolStrategy::Allowlist(allowed) = &def.tool_strategy;
    allowed
        .iter()
        .filter(|name| parent_tool_names.contains(&name.as_str()))
        .cloned()
        .collect()
}

/// Resolve tool specs from the parent pool for a sub-agent definition.
pub fn resolve_tools_for_agent(def: &AgentDefinition, parent_tools: &[ToolSpec]) -> Vec<ToolSpec> {
    let parent_names: Vec<&str> = parent_tools
        .iter()
        .map(|spec| spec.function.name.as_str())
        .collect();
    let allowed = resolve_tool_names_for_agent(def, &parent_names);
    parent_tools
        .iter()
        .filter(|spec| allowed.iter().any(|n| n == &spec.function.name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let names = resolve_tool_names_for_agent(&explore_def(), &parent);
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
        let resolved = resolve_tools_for_agent(&explore_def(), &parent);
        let names: Vec<_> = resolved.iter().map(|s| s.function.name.as_str()).collect();
        assert_eq!(names, vec!["grep", "read_file", "bash"]);
    }
}
