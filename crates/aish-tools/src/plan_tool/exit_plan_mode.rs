use aish_llm::{Tool, ToolResult};

use super::prompt;

/// Tool for exiting plan mode.
pub struct ExitPlanModeTool;

impl ExitPlanModeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "exit_plan_mode"
    }

    fn description(&self) -> &str {
        prompt::EXIT_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::exit_parameters()
    }

    fn prompt(&self) -> &str {
        prompt::EXIT_PROMPT
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let feedback = args
            .get("feedback")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let plan_content = args
            .get("plan_content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut meta = serde_json::json!({
            "action": "exit_plan_mode",
            "decision_required": true,
            "summary": summary
        });

        if let Some(ref fb) = feedback {
            meta["feedback"] = serde_json::json!(fb);
            meta["approval_transition"] = serde_json::json!("changes_requested_to_review");
        }

        if let Some(ref content) = plan_content {
            meta["plan_content_length"] = serde_json::json!(content.len());
        }

        ToolResult {
            ok: true,
            output: "Plan mode exited. The plan is now ready for review and approval.".to_string(),
            meta: Some(meta),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_plan_mode_basic() {
        let tool = ExitPlanModeTool::new();
        assert_eq!(tool.name(), "exit_plan_mode");

        let result = tool.execute(serde_json::json!({}));

        assert!(result.ok);
        assert!(result.output.contains("Plan mode exited"));
        assert!(result.output.contains("ready for review"));

        let meta = result.meta.unwrap();
        assert_eq!(meta["action"], "exit_plan_mode");
        assert_eq!(meta["decision_required"], true);
    }

    #[test]
    fn test_exit_plan_mode_with_summary() {
        let tool = ExitPlanModeTool::new();
        let result = tool.execute(serde_json::json!({
            "summary": "Complete implementation plan"
        }));

        assert!(result.ok);
        let meta = result.meta.unwrap();
        assert_eq!(meta["summary"], "Complete implementation plan");
    }

    #[test]
    fn test_exit_plan_mode_parameters() {
        let tool = ExitPlanModeTool::new();
        let params = tool.parameters();

        assert_eq!(params["type"], "object");
        assert!(params["properties"]["summary"]["description"]
            .as_str()
            .is_some());
        assert!(params["properties"]["feedback"]["description"]
            .as_str()
            .is_some());
        assert!(params["properties"]["plan_content"]["description"]
            .as_str()
            .is_some());

        let required = params["required"].as_array();
        assert!(required.is_none() || required.unwrap().is_empty());
    }

    #[test]
    fn test_exit_plan_mode_with_feedback() {
        let tool = ExitPlanModeTool::new();
        let result = tool.execute(serde_json::json!({
            "summary": "Revised plan",
            "feedback": "Please add more testing steps"
        }));

        assert!(result.ok);
        let meta = result.meta.unwrap();
        assert_eq!(meta["feedback"], "Please add more testing steps");
        assert_eq!(meta["approval_transition"], "changes_requested_to_review");
    }

    #[test]
    fn test_exit_plan_mode_with_plan_content() {
        let tool = ExitPlanModeTool::new();
        let result = tool.execute(serde_json::json!({
            "summary": "Full plan",
            "plan_content": "# Plan\n## Steps\n1. Do stuff\n2. Test"
        }));

        assert!(result.ok);
        let meta = result.meta.unwrap();
        assert!(meta["plan_content_length"].is_number());
    }

    #[test]
    fn test_exit_plan_mode_description() {
        let tool = ExitPlanModeTool::new();
        assert!(tool.description().contains("approval"));
        assert!(tool.description().contains("review"));
    }
}
