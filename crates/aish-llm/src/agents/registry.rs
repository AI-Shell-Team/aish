//! Built-in sub-agent definitions and registry (Phase 1).

use std::collections::HashMap;

use super::builtin_prompts::{
    EXPLORE_SYSTEM_PROMPT, GENERAL_PURPOSE_SYSTEM_PROMPT, PLAN_SYSTEM_PROMPT,
};

/// Read-only tool allowlist shared by `explore` and `plan` built-ins.
pub const READ_ONLY_ALLOWLIST: &[&str] = &["grep", "glob", "read_file", "bash"];

/// Tool filtering strategy for a built-in sub-agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStrategy {
    /// Only tools in the allowlist (intersected with parent pool).
    Allowlist(Vec<String>),
    /// Parent pool minus denied tools (intersected with parent pool).
    Denylist(Vec<String>),
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

fn read_only_allowlist() -> Vec<String> {
    READ_ONLY_ALLOWLIST
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

impl AgentDefinition {
    pub fn explore() -> Self {
        Self {
            subagent_type: "explore".to_string(),
            when_to_use: "Read-only investigation across logs, configs, services, or paths. \
Use for open-ended search; specify thoroughness in the Agent prompt: quick, medium, or thorough."
                .to_string(),
            system_prompt: EXPLORE_SYSTEM_PROMPT.to_string(),
            max_turns: 15,
            tool_strategy: ToolStrategy::Allowlist(read_only_allowlist()),
        }
    }

    pub fn plan() -> Self {
        Self {
            subagent_type: "plan".to_string(),
            when_to_use:
                "Read-only architect for implementation plans, runbooks, or design advice \
returned as one conclusion — NOT for enter_plan_mode or writing `.aish/plans/` files."
                    .to_string(),
            system_prompt: PLAN_SYSTEM_PROMPT.to_string(),
            max_turns: 20,
            tool_strategy: ToolStrategy::Allowlist(read_only_allowlist()),
        }
    }

    pub fn general_purpose() -> Self {
        Self {
            subagent_type: "general-purpose".to_string(),
            when_to_use: "Focused sub-tasks that need the parent's tool pool (including writes) \
without nested Agent delegation — not for broad read-only exploration (use explore)."
                .to_string(),
            system_prompt: GENERAL_PURPOSE_SYSTEM_PROMPT.to_string(),
            max_turns: 25,
            tool_strategy: ToolStrategy::Denylist(vec!["Agent".to_string()]),
        }
    }
}

/// Registry of built-in sub-agent types available via the `Agent` tool.
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    /// Phase 1 built-ins: `explore`, `plan`, and `general-purpose`.
    pub fn builtin() -> Self {
        let mut agents = HashMap::new();
        for def in [
            AgentDefinition::explore(),
            AgentDefinition::plan(),
            AgentDefinition::general_purpose(),
        ] {
            agents.insert(def.subagent_type.clone(), def);
        }
        Self { agents }
    }

    pub fn resolve(&self, subagent_type: &str) -> Result<&AgentDefinition, String> {
        self.agents
            .get(subagent_type)
            .ok_or_else(|| format!("Unknown subagent_type: {subagent_type}"))
    }

    /// Built-in type names for JSON schema enums.
    pub fn subagent_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self.agents.keys().cloned().collect();
        types.sort();
        types
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
    fn test_explore_system_prompt_covers_search_strategy() {
        let def = AgentDefinition::explore();
        assert!(def.system_prompt.contains("READ-ONLY"));
        assert!(def.system_prompt.contains("broad recursive pattern"));
        assert!(def.system_prompt.contains("thoroughness"));
    }

    #[test]
    fn test_plan_system_prompt_is_read_only_architect() {
        let def = AgentDefinition::plan();
        assert!(def.system_prompt.contains("READ-ONLY"));
        assert!(def.system_prompt.contains("Do not execute the plan"));
    }

    #[test]
    fn test_resolve_explore_succeeds() {
        let registry = AgentRegistry::builtin();
        let def = registry.resolve("explore").expect("explore should exist");
        assert_eq!(def.subagent_type, "explore");
        assert_eq!(def.max_turns, 15);
    }

    #[test]
    fn test_resolve_plan_succeeds() {
        let registry = AgentRegistry::builtin();
        let def = registry.resolve("plan").expect("plan should exist");
        assert_eq!(def.subagent_type, "plan");
        assert_eq!(def.max_turns, 20);
        assert!(matches!(def.tool_strategy, ToolStrategy::Allowlist(_)));
    }

    #[test]
    fn test_resolve_general_purpose_succeeds() {
        let registry = AgentRegistry::builtin();
        let def = registry
            .resolve("general-purpose")
            .expect("general-purpose should exist");
        assert_eq!(def.subagent_type, "general-purpose");
        assert_eq!(def.max_turns, 25);
        assert!(matches!(def.tool_strategy, ToolStrategy::Denylist(_)));
    }

    #[test]
    fn test_resolve_unknown_type_errors() {
        let registry = AgentRegistry::builtin();
        let err = registry.resolve("troubleshoot").unwrap_err();
        assert!(err.contains("Unknown subagent_type"));
    }

    #[test]
    fn test_list_for_tool_description_includes_all_builtins_when_to_use() {
        let registry = AgentRegistry::builtin();
        let desc = registry.list_for_tool_description();
        assert!(desc.contains("`explore`"));
        assert!(desc.contains("`plan`"));
        assert!(desc.contains("`general-purpose`"));
        assert!(desc.contains("read-only"));
    }

    #[test]
    fn test_subagent_types_lists_all_builtins() {
        let registry = AgentRegistry::builtin();
        assert_eq!(
            registry.subagent_types(),
            vec![
                "explore".to_string(),
                "general-purpose".to_string(),
                "plan".to_string(),
            ]
        );
    }

    #[test]
    fn test_explore_allowlist_is_read_only_tools() {
        let def = AgentDefinition::explore();
        assert_eq!(
            def.tool_strategy,
            ToolStrategy::Allowlist(read_only_allowlist()),
        );
    }

    #[test]
    fn test_plan_allowlist_matches_explore() {
        let explore = AgentDefinition::explore();
        let plan = AgentDefinition::plan();
        assert_eq!(explore.tool_strategy, plan.tool_strategy);
    }
}
