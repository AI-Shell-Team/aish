use aish_llm::{Tool, ToolResult};

use super::prompt;

/// Tool that signals the agent has reached a final answer.
///
/// When the LLM calls this tool, the agent loop should terminate and return
/// the provided answer as the final result. This is the native tool-calling
/// equivalent of the text-based "Final Answer: ..." marker.
pub struct FinalAnswerTool;

impl FinalAnswerTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FinalAnswerTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for FinalAnswerTool {
    fn name(&self) -> &str {
        "final_answer"
    }

    fn description(&self) -> &str {
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let answer = args.get("answer").and_then(|a| a.as_str()).unwrap_or("");
        ToolResult::success(answer.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aish_llm::Tool;

    #[test]
    fn test_final_answer_tool_name() {
        let tool = FinalAnswerTool::new();
        assert_eq!(tool.name(), "final_answer");
    }

    #[test]
    fn test_final_answer_tool_execute() {
        let tool = FinalAnswerTool::new();
        let args = serde_json::json!({"answer": "The system is healthy"});
        let result = tool.execute(args);
        assert!(result.ok);
        assert_eq!(result.output, "The system is healthy");
    }

    #[test]
    fn test_final_answer_tool_missing_answer() {
        let tool = FinalAnswerTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(result.ok);
        assert_eq!(result.output, "");
    }

    #[test]
    fn test_final_answer_tool_to_spec() {
        let tool = FinalAnswerTool::new();
        let spec = tool.to_spec();
        assert_eq!(spec.r#type, "function");
        assert_eq!(spec.function.name, "final_answer");
        let params = &spec.function.parameters;
        let required = params.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("answer".into())));
    }
}
