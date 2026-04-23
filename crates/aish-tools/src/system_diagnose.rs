//! System diagnose tool that the LLM can invoke to investigate system issues.
//!
//! When called, creates an isolated sub-session with diagnostic tools
//! (bash, read_file, etc.) and runs a ReAct loop to produce a diagnosis.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use aish_core::LlmEvent;
use aish_llm::diagnose_agent::build_diagnose_prompt;
use aish_llm::types::LlmCallbackResult;
use aish_llm::{DiagnoseAgent, LlmSession, SubSessionConfig, Tool, ToolResult};
use aish_security::SecurityDecision;

/// Shared event callback holder that can be set after tool construction.
///
/// Uses `Mutex<Option<Arc<...>>>` because the event callback is created
/// after the tool is registered in the session.
pub type SharedEventCallback =
    Arc<Mutex<Option<Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>>>>;

/// Optional skill lookup/list callbacks for the diagnose agent.
///
/// When provided, a [`SkillTool`] is added to the diagnose tool set so the
/// agent can invoke skill plugins during diagnosis.
pub type SkillCallbacks = Option<(
    Arc<dyn Fn(&str) -> Option<crate::skill_tool::SkillInfo> + Send + Sync>,
    Arc<dyn Fn() -> Vec<String> + Send + Sync>,
)>;

/// Tool that allows the LLM to trigger an isolated system diagnosis session.
///
/// Registered under the name `system_diagnose_agent`, matching the Python version.
/// When the main LLM decides a user query requires deep system investigation,
/// it calls this tool with a `query` describing the problem. The tool spawns
/// an isolated ReAct agent with its own context, tools, and system prompt.
pub struct SystemDiagnoseTool {
    api_base: String,
    api_key: String,
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    security_check: Option<Arc<dyn Fn(&str) -> SecurityDecision + Send + Sync>>,
    event_callback: SharedEventCallback,
    skill_callbacks: SkillCallbacks,
}

impl SystemDiagnoseTool {
    pub fn new(
        api_base: &str,
        api_key: &str,
        model: &str,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        security_check: Option<Arc<dyn Fn(&str) -> SecurityDecision + Send + Sync>>,
        event_callback: SharedEventCallback,
    ) -> Self {
        Self {
            api_base: api_base.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            temperature,
            max_tokens,
            security_check,
            event_callback,
            skill_callbacks: None,
        }
    }

    /// Attach skill lookup/list callbacks so the diagnose agent can invoke skills.
    pub fn with_skill_callbacks(mut self, callbacks: SkillCallbacks) -> Self {
        self.skill_callbacks = callbacks;
        self
    }
}

impl Tool for SystemDiagnoseTool {
    fn name(&self) -> &str {
        "system_diagnose_agent"
    }

    fn description(&self) -> &str {
        "Advanced log analysis and system diagnosis agent that can read files, \
         analyze patterns, and provide detailed diagnostic reports"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The diagnostic query or system issue to analyze. \
                                    Describe the problem, symptoms, or specific logs to investigate."
                }
            },
            "required": ["query"]
        })
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        ToolResult::error("system_diagnose_agent requires async execution; use execute_async")
    }

    fn execute_async<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if query.is_empty() {
                return ToolResult::error("Missing required parameter: query");
            }

            // Create a temporary parent session to derive client credentials.
            let mut parent = LlmSession::new(
                &self.api_base,
                &self.api_key,
                &self.model,
                self.temperature,
                self.max_tokens,
            );

            // Set up event callback proxy that forwards sub-session events
            // to the main session's callback with "source" tagging (matching
            // the Python event_proxy_callback pattern).
            let maybe_cb = self.event_callback.lock().unwrap().clone();
            if let Some(cb) = maybe_cb {
                let proxy_cb: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync> =
                    Arc::new(move |event: LlmEvent| {
                        let mut modified_data = match event.data.as_object() {
                            Some(obj) => {
                                let mut new_obj = obj.clone();
                                new_obj.insert(
                                    "source".to_string(),
                                    serde_json::json!("system_diagnose_agent"),
                                );
                                serde_json::Value::Object(new_obj)
                            }
                            None => serde_json::json!({
                                "source": "system_diagnose_agent"
                            }),
                        };
                        // Preserve non-object data by merging
                        if !event.data.is_object() {
                            modified_data = serde_json::json!({
                                "source": "system_diagnose_agent",
                                "original_data": event.data
                            });
                        }
                        let forwarded = LlmEvent {
                            event_type: event.event_type,
                            data: modified_data,
                            timestamp: event.timestamp,
                            metadata: event.metadata,
                        };
                        cb(forwarded)
                    });
                parent.set_event_callback(proxy_cb);
            }

            // Build diagnose prompt with output language instruction
            let mut prompt = build_diagnose_prompt();
            let lang = aish_i18n::current_language();
            let lang_name = aish_i18n::language_name(&lang);
            prompt.push_str(&format!(
                "\n\noutput language: use {} to communicate with the user.",
                lang_name
            ));

            let config = SubSessionConfig {
                max_context_messages: 30,
                max_iterations: 10,
                system_prompt: Some(prompt),
            };

            let agent = DiagnoseAgent::with_config(config);

            // Build bash tool with the same security policy as the main session.
            let bash_tool = match &self.security_check {
                Some(check) => {
                    let check = Arc::clone(check);
                    crate::SecureBashTool::with_security_check(move |cmd| check(cmd))
                }
                None => crate::SecureBashTool::new(),
            };

            // Tools available for diagnosis
            let mut tools: Vec<Box<dyn Tool>> = vec![
                Box::new(bash_tool),
                Box::new(crate::fs::ReadFileTool::new()),
                Box::new(crate::fs::WriteFileTool::new()),
                Box::new(crate::fs::EditFileTool::new()),
                Box::new(crate::FinalAnswerTool::new()),
            ];

            // Add skill tool when callbacks are available (matches Python version)
            if let Some((lookup, list)) = &self.skill_callbacks {
                let lookup = Arc::clone(lookup);
                let list = Arc::clone(list);
                tools.push(Box::new(crate::SkillTool::new(
                    Box::new(move |name| lookup(name)),
                    Box::new(move || list()),
                )));
            }

            match agent.diagnose(&parent, &query, tools).await {
                Ok(result) => ToolResult::success(result),
                Err(e) => ToolResult::error(format!("Diagnosis failed: {}", e)),
            }
        })
    }
}
