use aish_llm::{Tool, ToolResult};

use super::prompt;

/// Tool for listing available plan templates.
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
        prompt::TEMPLATES_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::templates_parameters()
    }

    fn prompt(&self) -> &str {
        prompt::TEMPLATES_PROMPT
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
        assert!(params["properties"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_list_templates_description() {
        let tool = ListTemplatesTool::new();
        assert!(tool.description().contains("templates"));
    }
}
