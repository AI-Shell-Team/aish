//! Unified prompt + tool spec assembly seam.

use std::collections::HashSet;

use crate::session::LlmSession;
use crate::types::{PromptVisibility, ToolSpec};

use super::context::PromptContext;
use super::visibility::ToolVisibilityPolicy;

/// Output of [`PromptAssembly::build`].
#[derive(Debug, Clone)]
pub struct PromptBundle {
    pub system_message: String,
    pub tool_specs: Vec<ToolSpec>,
}

pub struct PromptAssembly;

impl PromptAssembly {
    pub fn build(session: &LlmSession, context: PromptContext, base_system: &str) -> PromptBundle {
        let visible_names = ToolVisibilityPolicy::visible_tool_names(session, &context);
        let visible_set: HashSet<&str> = visible_names.iter().map(|s| s.as_str()).collect();

        let tool_specs: Vec<ToolSpec> = session
            .registered_tools()
            .filter(|tool| visible_set.contains(tool.name()))
            .map(|tool| tool.to_spec())
            .collect();

        let system_message =
            merge_system_with_appendix(base_system, tool_appendix(session, &visible_names));

        PromptBundle {
            system_message,
            tool_specs,
        }
    }
}

fn merge_system_with_appendix(base_system: &str, appendix: Option<String>) -> String {
    match appendix {
        None => base_system.to_string(),
        Some(section) if base_system.trim().is_empty() => section,
        Some(section) => format!("{}\n\n{}", base_system.trim_end(), section),
    }
}

fn tool_appendix(session: &LlmSession, visible_names: &[String]) -> Option<String> {
    let visible_set: HashSet<&str> = visible_names.iter().map(|s| s.as_str()).collect();
    let mut prompts: Vec<(&str, &str)> = session
        .registered_tools()
        .filter(|tool| visible_set.contains(tool.name()))
        .filter(|tool| match tool.prompt_visibility() {
            PromptVisibility::NeverInAppendix => false,
            PromptVisibility::AppendixWhenNonEmpty => true,
        })
        .filter_map(|tool| {
            let prompt = tool.prompt().trim();
            if prompt.is_empty() {
                None
            } else {
                Some((tool.name(), prompt))
            }
        })
        .collect();

    if prompts.is_empty() {
        return None;
    }

    prompts.sort_by(|a, b| a.0.cmp(b.0));

    let mut section = String::from("## Tool Instructions\n");
    for (name, prompt) in prompts {
        section.push_str("\n### ");
        section.push_str(name);
        section.push('\n');
        section.push_str(prompt);
        section.push('\n');
    }
    Some(section)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PromptVisibility, Tool};

    struct MockTool {
        name: String,
        prompt: String,
        visibility: PromptVisibility,
    }

    impl MockTool {
        fn with_prompt(name: &str, prompt: &str) -> Self {
            Self {
                name: name.to_string(),
                prompt: prompt.to_string(),
                visibility: PromptVisibility::AppendixWhenNonEmpty,
            }
        }

        fn hidden_prompt(name: &str, prompt: &str) -> Self {
            Self {
                name: name.to_string(),
                prompt: prompt.to_string(),
                visibility: PromptVisibility::NeverInAppendix,
            }
        }
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "mock tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        fn prompt(&self) -> &str {
            &self.prompt
        }

        fn prompt_visibility(&self) -> PromptVisibility {
            self.visibility
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("ok")
        }
    }

    #[test]
    fn build_main_chat_appends_tool_appendix() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::with_prompt("grep", "Search logs.")));

        let bundle = PromptAssembly::build(&session, PromptContext::MainChat, "Oracle.\n");

        assert!(bundle.system_message.starts_with("Oracle."));
        assert!(bundle.system_message.contains("## Tool Instructions"));
        assert!(bundle.system_message.contains("Search logs."));
        assert_eq!(bundle.tool_specs.len(), 1);
        assert_eq!(bundle.tool_specs[0].function.name, "grep");
    }

    #[test]
    fn build_sub_agent_omits_plan_mode_from_specs_and_appendix() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::with_prompt("grep", "Search read-only.")));
        session.register_tool(Box::new(MockTool::with_prompt(
            "enter_plan_mode",
            "Do not use in sub-agent.",
        )));

        let bundle = PromptAssembly::build(
            &session,
            PromptContext::SubAgent {
                subagent_type: "plan".to_string(),
            },
            "Sub prompt.",
        );

        let names: Vec<_> = bundle
            .tool_specs
            .iter()
            .map(|s| s.function.name.as_str())
            .collect();
        assert_eq!(names, vec!["grep"]);
        assert!(!bundle.system_message.contains("enter_plan_mode"));
        assert!(!bundle.system_message.contains("Do not use in sub-agent."));
    }

    #[test]
    fn build_respects_never_in_appendix_visibility() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::hidden_prompt(
            "bash",
            "Hidden usage details.",
        )));

        let bundle = PromptAssembly::build(&session, PromptContext::MainChat, "Oracle.");

        assert!(!bundle.system_message.contains("## Tool Instructions"));
        assert!(!bundle.system_message.contains("Hidden usage details."));
        assert_eq!(bundle.tool_specs.len(), 1);
    }
}
