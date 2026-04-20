use std::collections::HashMap;

use aish_llm::{Tool, ToolSpec};

/// Format tool output as tagged XML for LLM consumption.
pub fn format_tagged_result(
    stdout: &str,
    stderr: &str,
    offload: Option<&serde_json::Value>,
) -> String {
    let mut parts = Vec::new();

    if !stdout.is_empty() {
        parts.push(format!("<stdout>\n{}\n</stdout>", stdout));
    }

    if !stderr.is_empty() {
        parts.push(format!("<stderr>\n{}\n</stderr>", stderr));
    }

    if let Some(off) = offload {
        let path = off
            .get("stdout")
            .and_then(|v| v.get("path"))
            .and_then(|p| p.as_str())
            .unwrap_or("");
        let bytes = off
            .get("stdout")
            .and_then(|v| v.get("bytes"))
            .and_then(|p| p.as_u64())
            .unwrap_or(0);
        if !path.is_empty() {
            parts.push(format!("<offload path=\"{}\" bytes=\"{}\" />", path, bytes));
        }
    }

    parts.join("\n")
}

/// Registry that holds tool implementations and provides lookup by name.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut dyn Tool> {
        match self.tools.get_mut(name) {
            Some(b) => Some(b.as_mut()),
            None => None,
        }
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.to_spec()).collect()
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Create the default tool set with built-in tools.
    pub fn default_tools() -> Self {
        let mut reg = Self::new();
        reg.register(Box::new(crate::bash::BashTool::new()));
        reg.register(Box::new(crate::fs::ReadFileTool::new()));
        reg.register(Box::new(crate::fs::WriteFileTool::new()));
        reg.register(Box::new(crate::fs::EditFileTool::new()));
        reg.register(Box::new(crate::ask_user::AskUserTool::new()));
        reg.register(Box::new(crate::final_answer::FinalAnswerTool::new()));
        reg.register(Box::new(crate::python::PythonTool::new()));
        // MemoryTool and SkillTool require callbacks, so they use noop defaults.
        // The shell will replace them with wired versions at startup.
        reg.register(Box::new(crate::memory_tool::MemoryTool::noop()));
        reg.register(Box::new(crate::skill_tool::SkillTool::noop()));
        reg
    }

    /// Drain all tools from the registry, returning them as a vector of
    /// `(name, tool)` pairs. The registry is left empty after this call.
    pub fn drain_tools(&mut self) -> Vec<(String, Box<dyn Tool>)> {
        let keys: Vec<String> = self.tools.keys().cloned().collect();
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(tool) = self.tools.remove(&key) {
                result.push((key, tool));
            }
        }
        result
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tagged_result_stdout_only() {
        let result = format_tagged_result("hello", "", None);
        assert!(result.contains("<stdout>"));
        assert!(result.contains("hello"));
        assert!(result.contains("</stdout>"));
        assert!(!result.contains("<stderr>"));
        assert!(!result.contains("<offload"));
    }

    #[test]
    fn test_format_tagged_result_with_stderr() {
        let result = format_tagged_result("out", "err", None);
        assert!(result.contains("<stdout>"));
        assert!(result.contains("out"));
        assert!(result.contains("<stderr>"));
        assert!(result.contains("err"));
    }

    #[test]
    fn test_format_tagged_result_with_offload() {
        let offload = serde_json::json!({
            "stdout": {
                "path": "/tmp/out.txt",
                "bytes": 12345
            }
        });
        let result = format_tagged_result("out", "err", Some(&offload));
        assert!(result.contains("<offload path=\"/tmp/out.txt\" bytes=\"12345\" />"));
    }

    #[test]
    fn test_format_tagged_result_empty_streams_omitted() {
        let result = format_tagged_result("", "", None);
        assert!(
            result.is_empty(),
            "empty streams should produce empty output"
        );
    }
}
