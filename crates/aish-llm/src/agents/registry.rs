//! Built-in sub-agent definitions and registry (Phase 1 explore slice).

use std::collections::HashMap;

/// Tool filtering strategy for a built-in sub-agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStrategy {
    /// Only tools in the allowlist (intersected with parent pool).
    Allowlist(Vec<String>),
}

/// Definition of a built-in sub-agent type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    pub subagent_type: String,
    pub when_to_use: String,
    pub system_prompt: String,
    pub max_turns: u32,
    pub tool_strategy: ToolStrategy,
}

impl AgentDefinition {
    pub fn explore() -> Self {
        Self {
            subagent_type: "explore".to_string(),
            when_to_use: "Search logs, configs, services, network state, or code locations; read-only investigation.".to_string(),
            system_prompt: "You are a read-only explore sub-agent for shell operations. Investigate using only your allowed tools. Do not modify files or spawn nested agents. Return a concise conclusion.".to_string(),
            max_turns: 15,
            tool_strategy: ToolStrategy::Allowlist(vec![
                "grep".to_string(),
                "glob".to_string(),
                "read_file".to_string(),
                "bash".to_string(),
            ]),
        }
    }
}

/// Registry of built-in sub-agent types available via the `Agent` tool.
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    /// Phase 1 explore slice: only `explore` is registered.
    pub fn builtin() -> Self {
        let explore = AgentDefinition::explore();
        let mut agents = HashMap::new();
        agents.insert(explore.subagent_type.clone(), explore);
        Self { agents }
    }

    pub fn resolve(&self, subagent_type: &str) -> Result<&AgentDefinition, String> {
        self.agents
            .get(subagent_type)
            .ok_or_else(|| format!("Unknown subagent_type: {subagent_type}"))
    }

    /// Format built-in descriptions for the `Agent` tool description.
    pub fn list_for_tool_description(&self) -> String {
        let mut lines: Vec<String> = self
            .agents
            .values()
            .map(|def| format!("- `{}`: {}", def.subagent_type, def.when_to_use))
            .collect();
        lines.sort();
        lines.join("\n")
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_explore_succeeds() {
        let registry = AgentRegistry::builtin();
        let def = registry.resolve("explore").expect("explore should exist");
        assert_eq!(def.subagent_type, "explore");
        assert_eq!(def.max_turns, 15);
    }

    #[test]
    fn test_resolve_unknown_type_errors() {
        let registry = AgentRegistry::builtin();
        let err = registry.resolve("plan").unwrap_err();
        assert!(err.contains("Unknown subagent_type"));
    }

    #[test]
    fn test_list_for_tool_description_includes_explore_when_to_use() {
        let registry = AgentRegistry::builtin();
        let desc = registry.list_for_tool_description();
        assert!(desc.contains("`explore`"));
        assert!(desc.contains("read-only"));
    }

    #[test]
    fn test_explore_allowlist_is_read_only_tools() {
        let def = AgentDefinition::explore();
        let ToolStrategy::Allowlist(tools) = &def.tool_strategy;
        assert_eq!(
            tools,
            &vec![
                "grep".to_string(),
                "glob".to_string(),
                "read_file".to_string(),
                "bash".to_string(),
            ]
        );
    }
}
