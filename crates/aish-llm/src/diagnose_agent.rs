//! Diagnostic agent that investigates system issues using an isolated sub-session.
//!
//! The DiagnoseAgent creates a separate session with a specific tool set
//! (bash, read_file, write_file, edit_file, final_answer) for system diagnosis.
//! It has independent context and doesn't pollute the main session.

use crate::subsession::{SubSession, SubSessionConfig};
use crate::types::Tool;
use aish_core::AishError;

/// Diagnostic agent that investigates system issues using an isolated sub-session.
pub struct DiagnoseAgent {
    config: SubSessionConfig,
    system_prompt: String,
}

impl DiagnoseAgent {
    /// Create a new diagnostic agent with default configuration.
    pub fn new() -> Self {
        Self::with_config(SubSessionConfig::default())
    }

    /// Create a new diagnostic agent with custom configuration.
    pub fn with_config(config: SubSessionConfig) -> Self {
        Self {
            config,
            system_prompt: build_diagnose_prompt(),
        }
    }

    /// Run diagnosis with a specific set of tools in an isolated context.
    ///
    /// # Arguments
    /// * `parent_session` - The parent LLM session to derive credentials from
    /// * `query` - The diagnostic query or system issue to analyze
    /// * `tools` - Tools to register in the diagnostic sub-session
    ///
    /// # Returns
    /// The final diagnostic result as a string
    pub async fn diagnose(
        &self,
        parent_session: &crate::LlmSession,
        query: &str,
        tools: Vec<Box<dyn Tool>>,
    ) -> Result<String, AishError> {
        let mut sub = SubSession::new(parent_session, self.config.clone());

        // Set custom system prompt for diagnosis
        sub.config.system_prompt = Some(self.system_prompt.clone());

        // Register tools to the underlying session
        for tool in tools {
            sub.inner.register_tool(tool);
        }

        // Run the diagnostic loop using the agent directly
        use crate::agent::{AgentConfig, ReActAgent};

        let agent = ReActAgent::new(&sub.inner).with_config(AgentConfig {
            max_iterations: sub.config.max_iterations,
            temperature: Some(0.3),
            max_tokens: Some(4096),
        });

        let system_prompt = sub
            .config
            .system_prompt
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(crate::agent::REACT_SYSTEM_PROMPT_TEMPLATE);

        agent.run_with_system_prompt(query, system_prompt).await
    }
}

impl Default for DiagnoseAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the system prompt for the diagnostic agent, embedding basic
/// system information collected at call time.
pub fn build_diagnose_prompt() -> String {
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".into());
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let os_info = get_os_description();
    let uname = get_uname_info();

    format!(
        "You are a system diagnosis expert. Your job is to investigate and diagnose \
         system issues reported by the user.\n\
         \n\
         System Information:\n\
         - Hostname: {hostname}\n\
         - User: {user}\n\
         - OS: {os_info}\n\
         - Kernel: {uname}\n\
         \n\
         Follow the ReAct format when reasoning:\n\
         Thought: describe your reasoning process\n\
         Action: choose a tool and provide arguments\n\
         Observation: summarize tool output\n\
         Final Answer: provide the final diagnostic conclusion\n\
         \n\
         Available tools will be provided via the tool-calling interface. \
         Use bash_exec to run diagnostic commands and read_file to inspect \
         log files or configuration.\n\
         \n\
         Guidelines:\n\
         - Start by understanding the problem from the user's description.\n\
         - Use commands like `dmesg`, `journalctl`, `ps`, `df`, `free`, `top`, \
           `netstat`, `ss`, `lsof`, etc. to gather information.\n\
         - Check relevant log files in /var/log/ or via journalctl.\n\
         - Provide actionable conclusions and remediation steps.\n\
         - Be thorough but efficient — prefer targeted commands over broad searches.\n\
         \n\
         When ready, respond with:\n\
         Final Answer: <your complete diagnostic conclusion>"
    )
}

fn get_os_description() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find(|l| l.starts_with("PRETTY_NAME="))
                .map(|l| {
                    l.trim_start_matches("PRETTY_NAME=")
                        .trim_matches('"')
                        .to_string()
                })
        })
        .unwrap_or_else(|| "Unknown".to_string())
}

fn get_uname_info() -> String {
    std::process::Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tool;

    // Mock tool for testing
    #[allow(dead_code)]
    struct MockBashTool;

    impl Tool for MockBashTool {
        fn name(&self) -> &str {
            "bash_exec"
        }

        fn description(&self) -> &str {
            "Execute bash commands"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                }
            })
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("mock output")
        }
    }

    #[test]
    fn test_diagnose_agent_default() {
        let agent = DiagnoseAgent::new();
        // Should have default config
        assert_eq!(agent.config.max_iterations, 10);
    }

    #[test]
    fn test_diagnose_agent_with_config() {
        let config = SubSessionConfig {
            max_iterations: 20,
            ..Default::default()
        };
        let agent = DiagnoseAgent::with_config(config);
        assert_eq!(agent.config.max_iterations, 20);
    }

    #[test]
    fn test_diagnose_agent_default_trait() {
        let agent = DiagnoseAgent::default();
        assert_eq!(agent.config.max_iterations, 10);
    }

    #[test]
    fn test_build_diagnose_prompt() {
        let prompt = build_diagnose_prompt();
        // Should contain system information
        assert!(prompt.contains("System Information:"));
        assert!(prompt.contains("Hostname:"));
        assert!(prompt.contains("User:"));
        assert!(prompt.contains("OS:"));
        assert!(prompt.contains("Kernel:"));
        // Should contain ReAct format
        assert!(prompt.contains("Thought:"));
        assert!(prompt.contains("Action:"));
        assert!(prompt.contains("Observation:"));
        assert!(prompt.contains("Final Answer:"));
    }

    #[test]
    fn test_get_os_description() {
        let os = get_os_description();
        // Should return non-empty string (either actual OS or "Unknown")
        assert!(!os.is_empty());
    }

    #[test]
    fn test_get_uname_info() {
        let uname = get_uname_info();
        // Should return non-empty string (either actual uname or "unknown")
        assert!(!uname.is_empty());
    }

    #[test]
    fn test_diagnose_agent_has_custom_system_prompt() {
        let agent = DiagnoseAgent::new();
        // System prompt should contain diagnostic-specific content
        assert!(agent.system_prompt.contains("system diagnosis expert"));
        assert!(agent.system_prompt.contains("System Information:"));
    }
}
