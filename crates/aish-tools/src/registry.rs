use std::collections::HashMap;

use aish_llm::{Tool, ToolSpec};

/// Format tool output as tagged XML for LLM consumption.
/// Matches Python's `_build_bash_tagged_result()` format:
///   <stdout>preview</stdout>
///   <stderr>preview</stderr>
///   <return_code>0</return_code>
///   <offload>{"status":"offloaded","stdout_path":"...","hint":"Read offload paths for full output"}</offload>
pub fn format_tagged_result(
    stdout: &str,
    stderr: &str,
    return_code: i32,
    offload: Option<&serde_json::Value>,
) -> String {
    let mut parts = Vec::new();

    if !stdout.is_empty() {
        parts.push(format!("<stdout>\n{}\n</stdout>", stdout));
    }

    if !stderr.is_empty() {
        parts.push(format!("<stderr>\n{}\n</stderr>", stderr));
    }

    // Always include return_code, matching Python's behavior
    parts.push(format!("<return_code>\n{}\n</return_code>", return_code));

    if let Some(off) = offload {
        // Format as compact JSON inside <offload> tags, matching Python
        let offload_json = serde_json::to_string(off).unwrap_or_else(|_| "{}".to_string());
        parts.push(format!("<offload>\n{}\n</offload>", offload_json));
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
        let result = format_tagged_result("hello", "", 0, None);
        assert!(result.contains("<stdout>"));
        assert!(result.contains("hello"));
        assert!(result.contains("</stdout>"));
        assert!(!result.contains("<stderr>"));
        assert!(result.contains("<return_code>"));
        assert!(result.contains("<return_code>\n0\n</return_code>"));
    }

    #[test]
    fn test_format_tagged_result_with_stderr() {
        let result = format_tagged_result("out", "err", 0, None);
        assert!(result.contains("<stdout>"));
        assert!(result.contains("out"));
        assert!(result.contains("<stderr>"));
        assert!(result.contains("err"));
    }

    #[test]
    fn test_format_tagged_result_with_offload() {
        let offload = serde_json::json!({
            "status": "offloaded",
            "stdout": {
                "path": "/tmp/out.txt",
                "bytes": 12345
            },
            "hint": "Read offload paths for full output"
        });
        let result = format_tagged_result("out", "err", 0, Some(&offload));
        assert!(result.contains("<offload>"));
        assert!(result.contains("/tmp/out.txt"));
        assert!(result.contains("Read offload paths for full output"));
        assert!(result.contains("</offload>"));
    }

    #[test]
    fn test_format_tagged_result_nonzero_exit() {
        let result = format_tagged_result("out", "err", 1, None);
        assert!(result.contains("<return_code>\n1\n</return_code>"));
    }
}
