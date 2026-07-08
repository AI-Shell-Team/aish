use aish_core::plan::generate_plan_id;
use aish_llm::{Tool, ToolResult};

use super::prompt;

/// Tools visible during planning phase (mirrors aish_core::plan::PLANNING_VISIBLE_TOOLS).
const VISIBLE_TOOLS_DURING_PLANNING: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "ask_user",
    "memory",
    "write_file",
    "edit_file",
    "exit_plan_mode",
];

/// Tool for entering plan mode.
pub struct EnterPlanModeTool;

impl EnterPlanModeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "enter_plan_mode"
    }

    fn description(&self) -> &str {
        prompt::ENTER_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::enter_parameters()
    }

    fn prompt(&self) -> &str {
        prompt::ENTER_PROMPT
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let topic = args
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let plan_id = generate_plan_id();
        let artifact_suggestion = format!(".aish/plans/plan-{}.md", plan_id);
        let visible_tools: Vec<&str> = VISIBLE_TOOLS_DURING_PLANNING.to_vec();

        let meta = serde_json::json!({
            "action": "enter_plan_mode",
            "topic": topic,
            "summary": summary,
            "plan_id": plan_id,
            "phase": "Planning",
            "visible_tools": visible_tools,
            "artifact_suggestion": artifact_suggestion
        });

        ToolResult {
            ok: true,
            output: format!(
                "Entering plan mode for: {}\n\
                Plan ID: {}\n\n\
                During planning, you have access to:\n\
                - Read-only tools: read_file, glob, grep, ask_user, memory\n\
                - Write tools (for plan only): write_file, edit_file\n\
                - exit_plan_mode: when ready to present your plan\n\n\
                Use write_file to create your plan artifact.\n\
                Suggested path: {}",
                topic, plan_id, artifact_suggestion
            ),
            meta: Some(meta),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enter_plan_mode_basic() {
        let tool = EnterPlanModeTool::new();
        assert_eq!(tool.name(), "enter_plan_mode");

        let result = tool.execute(serde_json::json!({
            "topic": "implement feature X"
        }));

        assert!(result.ok);
        assert!(result.output.contains("Entering plan mode"));
        assert!(result.output.contains("implement feature X"));

        let meta = result.meta.unwrap();
        assert_eq!(meta["action"], "enter_plan_mode");
        assert_eq!(meta["topic"], "implement feature X");
        assert_eq!(meta["phase"], "Planning");
        assert!(meta["plan_id"].is_string());
        assert_eq!(meta["plan_id"].as_str().unwrap().len(), 12);
        assert!(meta["visible_tools"].is_array());
        assert!(meta["artifact_suggestion"].is_string());
    }

    #[test]
    fn test_enter_plan_mode_with_summary() {
        let tool = EnterPlanModeTool::new();
        let result = tool.execute(serde_json::json!({
            "topic": "refactor code",
            "summary": "Clean up module structure"
        }));

        assert!(result.ok);
        let meta = result.meta.unwrap();
        assert_eq!(meta["summary"], "Clean up module structure");
    }

    #[test]
    fn test_enter_plan_mode_missing_topic() {
        let tool = EnterPlanModeTool::new();
        let result = tool.execute(serde_json::json!({}));

        assert!(result.ok);
        assert!(result.output.contains("Entering plan mode"));
    }

    #[test]
    fn test_enter_plan_mode_unique_plan_ids() {
        let tool = EnterPlanModeTool::new();
        let r1 = tool.execute(serde_json::json!({"topic": "a"}));
        let r2 = tool.execute(serde_json::json!({"topic": "b"}));

        let id1 = r1.meta.as_ref().unwrap()["plan_id"].as_str().unwrap();
        let id2 = r2.meta.as_ref().unwrap()["plan_id"].as_str().unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_enter_plan_mode_visible_tools() {
        let tool = EnterPlanModeTool::new();
        let result = tool.execute(serde_json::json!({"topic": "test"}));

        let meta = result.meta.unwrap();
        let visible: Vec<&str> = meta["visible_tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        assert!(visible.contains(&"read_file"));
        assert!(visible.contains(&"write_file"));
        assert!(visible.contains(&"edit_file"));
        assert!(visible.contains(&"exit_plan_mode"));
        assert!(!visible.contains(&"bash_exec"));
    }

    #[test]
    fn test_enter_plan_mode_parameters() {
        let tool = EnterPlanModeTool::new();
        let params = tool.parameters();

        assert_eq!(params["type"], "object");
        assert!(params["properties"]["topic"]["description"]
            .as_str()
            .is_some());
        assert!(params["properties"]["summary"]["description"]
            .as_str()
            .is_some());

        let required = params["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "topic");
    }

    #[test]
    fn test_enter_plan_mode_description() {
        let tool = EnterPlanModeTool::new();
        assert!(tool.description().contains("plan mode"));
        assert!(tool.description().contains(".aish/plans/"));
        assert!(tool.description().contains("Agent(subagent_type=plan)"));
    }
}
