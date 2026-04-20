//! Isolated sub-session support for agent isolation.
//!
//! A sub-session shares LLM client credentials with its parent but has
//! independent tool registry, cancellation token, and event handling.

use crate::session::LlmSession;
use crate::types::Tool;
use std::collections::HashMap;

/// Configuration for an isolated sub-session.
#[derive(Debug, Clone)]
pub struct SubSessionConfig {
    /// Maximum number of context messages before trimming.
    pub max_context_messages: usize,
    /// Maximum number of ReAct iterations.
    pub max_iterations: usize,
    /// Optional custom system prompt.
    pub system_prompt: Option<String>,
}

impl Default for SubSessionConfig {
    fn default() -> Self {
        Self {
            max_context_messages: 50,
            max_iterations: 10,
            system_prompt: None,
        }
    }
}

/// An isolated sub-session running within a parent session.
///
/// Has its own context, tool registry, and cancellation.
/// Shares LLM client credentials with parent.
pub struct SubSession {
    /// The underlying session (created from parent credentials).
    pub(crate) inner: LlmSession,
    /// Configuration for this sub-session.
    pub config: SubSessionConfig,
    /// Tools registered in this sub-session.
    tools: HashMap<String, Box<dyn Tool>>,
}

impl SubSession {
    /// Create a new isolated sub-session from a parent session.
    ///
    /// The sub-session shares the parent's LLM client credentials but has:
    /// - Independent tool registry (starts empty)
    /// - Independent cancellation token
    /// - Independent event callbacks
    /// - Independent plan state
    pub fn new(parent: &LlmSession, config: SubSessionConfig) -> Self {
        let inner = parent.create_subsession();
        Self {
            inner,
            config,
            tools: HashMap::new(),
        }
    }

    /// Register a tool in this sub-session.
    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    /// Get tool specs for all registered tools.
    pub fn tool_specs(&self) -> Vec<crate::types::ToolSpec> {
        self.tools.values().map(|t| t.to_spec()).collect()
    }

    /// Get a reference to the underlying session.
    pub fn inner(&self) -> &LlmSession {
        &self.inner
    }

    /// Get a mutable reference to the underlying session.
    pub fn inner_mut(&mut self) -> &mut LlmSession {
        &mut self.inner
    }

    /// Register all tools to the underlying session.
    pub fn register_tools_to_session(&mut self) {
        for (_name, tool) in self.tools.drain() {
            self.inner.register_tool(tool);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock tool for testing
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
            "Mock tool for testing"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("mock result")
        }
    }

    #[test]
    fn test_subsession_config_default() {
        let config = SubSessionConfig::default();
        assert_eq!(config.max_context_messages, 50);
        assert_eq!(config.max_iterations, 10);
        assert!(config.system_prompt.is_none());
    }

    #[test]
    fn test_subsession_config_custom() {
        let config = SubSessionConfig {
            max_context_messages: 100,
            max_iterations: 20,
            system_prompt: Some("Custom prompt".to_string()),
        };
        assert_eq!(config.max_context_messages, 100);
        assert_eq!(config.max_iterations, 20);
        assert_eq!(config.system_prompt, Some("Custom prompt".to_string()));
    }

    #[test]
    fn test_subsession_from_parent() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let config = SubSessionConfig::default();
        let sub = SubSession::new(&parent, config);

        // Subsession should have independent cancellation
        assert!(!sub.inner().cancellation_token().is_cancelled());

        // Cancel parent
        parent.cancellation_token().cancel();

        // Sub should NOT be cancelled
        assert!(parent.cancellation_token().is_cancelled());
        assert!(!sub.inner().cancellation_token().is_cancelled());
    }

    #[test]
    fn test_subsession_tool_registration() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let config = SubSessionConfig::default();
        let mut sub = SubSession::new(&parent, config);

        // Register a tool
        sub.register_tool(Box::new(MockTool::new("test_tool")));

        // Tool should be in the registry
        assert_eq!(sub.tool_specs().len(), 1);
        assert_eq!(sub.tool_specs()[0].function.name, "test_tool");
    }

    #[test]
    fn test_subsession_independent_tools() {
        let mut parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let config = SubSessionConfig::default();
        let mut sub = SubSession::new(&parent, config);

        // Register different tools in parent and sub
        parent.register_tool(Box::new(MockTool::new("parent_tool")));
        sub.register_tool(Box::new(MockTool::new("sub_tool")));

        // Parent should only see parent_tool
        let parent_specs = parent.tool_specs();
        let parent_names: Vec<_> = parent_specs
            .iter()
            .map(|s| s.function.name.as_str())
            .collect();
        assert!(parent_names.contains(&"parent_tool"));
        assert!(!parent_names.contains(&"sub_tool"));

        // Sub should only see sub_tool
        let sub_specs = sub.tool_specs();
        let sub_names: Vec<_> = sub_specs.iter().map(|s| s.function.name.as_str()).collect();
        assert!(sub_names.contains(&"sub_tool"));
        assert!(!sub_names.contains(&"parent_tool"));
    }

    #[test]
    fn test_subsession_independent_cancellation_token() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let config = SubSessionConfig::default();
        let sub = SubSession::new(&parent, config);

        // Both should have independent cancellation tokens
        assert!(!parent.cancellation_token().is_cancelled());
        assert!(!sub.inner().cancellation_token().is_cancelled());

        // Cancel parent
        parent.cancellation_token().cancel();

        // Only parent should be cancelled
        assert!(parent.cancellation_token().is_cancelled());
        assert!(!sub.inner().cancellation_token().is_cancelled());
    }

    #[test]
    fn test_subsession_register_tools_to_session() {
        let parent = LlmSession::new("http://localhost", "key", "model", None, None);
        let config = SubSessionConfig::default();
        let mut sub = SubSession::new(&parent, config);

        // Register tools in subsession
        sub.register_tool(Box::new(MockTool::new("tool1")));
        sub.register_tool(Box::new(MockTool::new("tool2")));

        // Register to underlying session
        sub.register_tools_to_session();

        // Tools should now be in the underlying session
        let specs = sub.inner().tool_specs();
        assert_eq!(specs.len(), 2);

        // And subsession's tool registry should be empty
        assert_eq!(sub.tool_specs().len(), 0);
    }
}
