// Plan mode tools for entering and exiting structured planning phase.

use aish_core::plan::generate_plan_id;
use aish_llm::{Tool, ToolResult};

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
///
/// When the AI calls this tool, it transitions to a planning phase where
/// only read-only tools plus write_file/edit_file (for the plan artifact) are available.
/// The tool returns metadata that the session layer uses to initialize plan state.
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
        "Enter plan mode to design an implementation approach before writing code. \
        During planning, only read-only tools and write_file/edit_file (for the plan) are available. \
        When ready, use exit_plan_mode to present the plan for approval."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "The topic or task to plan"
                },
                "summary": {
                    "type": "string",
                    "description": "Brief summary of the planning goal (optional)"
                }
            },
            "required": ["topic"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        // Extract arguments
        let topic = args
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Generate a plan ID for this planning session
        let plan_id = generate_plan_id();

        // Build suggested artifact path (session layer will finalize the actual path)
        let artifact_suggestion = format!(".aish/plans/plan-{}.md", plan_id);

        // Visible tools list for the AI's reference
        let visible_tools: Vec<&str> = VISIBLE_TOOLS_DURING_PLANNING.to_vec();

        // Return metadata for the session layer to initialize plan state.
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

/// Tool for exiting plan mode.
///
/// When the AI calls this tool, it exits the planning phase and presents
/// the plan for user approval. The tool reads the plan artifact content
/// and returns it along with approval instructions.
///
/// The actual approval interaction happens at the shell layer (app.rs),
/// not in the tool itself.
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
        "Exit plan mode and present your plan for approval. \
        The plan will be reviewed by the user before proceeding with implementation."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Brief summary of the plan (optional)"
                },
                "feedback": {
                    "type": "string",
                    "description": "Feedback from user when changes are requested (optional, injected by session layer)"
                },
                "plan_content": {
                    "type": "string",
                    "description": "Full plan content for review (optional, injected by session layer)"
                }
            }
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        // Extract summary
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract feedback (injected by session layer when changes are requested)
        let feedback = args
            .get("feedback")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract plan content (injected by session layer)
        let plan_content = args
            .get("plan_content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // This tool doesn't directly read the artifact or manage state.
        // It signals the session layer to handle the approval workflow.
        // The session layer will:
        // 1. Read the plan artifact
        // 2. Present it to the user for approval via PlanApprovalFlow
        // 3. Create an approved snapshot or relay feedback back to the AI

        // Build metadata with approval state transitions
        let mut meta = serde_json::json!({
            "action": "exit_plan_mode",
            "decision_required": true,
            "summary": summary
        });

        // Include feedback if present (when re-exiting after changes requested)
        if let Some(ref fb) = feedback {
            meta["feedback"] = serde_json::json!(fb);
            meta["approval_transition"] = serde_json::json!("changes_requested_to_review");
        }

        // Include plan content hint if present
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

/// Tool for listing available plan templates.
///
/// Returns a list of structured templates the AI can use when creating
/// plan artifacts during plan mode.
pub struct ListTemplatesTool;

impl ListTemplatesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ListTemplatesTool {
    fn name(&self) -> &str {
        "list_plan_templates"
    }

    fn description(&self) -> &str {
        "List available plan templates for structuring your implementation plan."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let templates = aish_core::plan::get_available_templates();
        let template_list: Vec<serde_json::Value> = templates
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "content": t.content,
                })
            })
            .collect();

        let output = templates
            .iter()
            .map(|t| format!("- **{}**: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n");

        ToolResult {
            ok: true,
            output: format!("Available plan templates:\n{}", output),
            meta: Some(serde_json::json!({
                "templates": template_list
            })),
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

        // Check metadata
        assert!(result.meta.is_some());
        let meta = result.meta.unwrap();
        assert_eq!(meta["action"], "enter_plan_mode");
        assert_eq!(meta["topic"], "implement feature X");
        assert_eq!(meta["phase"], "Planning");

        // New fields
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

        assert!(result.ok); // Should still succeed with default topic
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
    fn test_exit_plan_mode_basic() {
        let tool = ExitPlanModeTool::new();
        assert_eq!(tool.name(), "exit_plan_mode");

        let result = tool.execute(serde_json::json!({}));

        assert!(result.ok);
        assert!(result.output.contains("Plan mode exited"));
        assert!(result.output.contains("ready for review"));

        // Check metadata
        assert!(result.meta.is_some());
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
    fn test_tool_descriptions() {
        let enter = EnterPlanModeTool::new();
        let exit = ExitPlanModeTool::new();
        let templates = ListTemplatesTool::new();

        assert!(enter.description().contains("plan mode"));
        assert!(enter.description().contains("read-only"));

        assert!(exit.description().contains("approval"));
        assert!(exit.description().contains("review"));

        assert!(templates.description().contains("templates"));
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

        // No required parameters
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
    fn test_list_templates_tool() {
        let tool = ListTemplatesTool::new();
        assert_eq!(tool.name(), "list_plan_templates");

        let result = tool.execute(serde_json::json!({}));
        assert!(result.ok);
        assert!(result.output.contains("Available plan templates"));
        assert!(result.output.contains("default"));
        assert!(result.output.contains("bugfix"));
        assert!(result.output.contains("feature"));

        let meta = result.meta.unwrap();
        let templates = meta["templates"].as_array().unwrap();
        assert_eq!(templates.len(), 3);

        // Check that each template has required fields
        for t in templates {
            assert!(t["name"].is_string());
            assert!(t["description"].is_string());
            assert!(t["content"].is_string());
        }
    }

    #[test]
    fn test_list_templates_parameters() {
        let tool = ListTemplatesTool::new();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        // No properties needed
        assert!(params["properties"].as_object().unwrap().is_empty());
    }
}
