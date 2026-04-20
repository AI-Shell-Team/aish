//! ReAct-style agent system for iterative reasoning and tool use.
//!
//! The agent follows the Thought/Action/Observation/Final Answer pattern:
//! 1. Send the query plus conversation history to the LLM.
//! 2. Parse the LLM response for structured ReAct blocks.
//! 3. If the response contains an **Action**, execute the named tool and feed
//!    the result back as an **Observation**.
//! 4. If the response contains a **Final Answer**, return it immediately.
//! 5. Repeat until a final answer is produced or the iteration limit is hit.

use aish_core::{AishError, LlmEvent, LlmEventType};
use tracing::{debug, info, warn};

use crate::client::LlmResponse;
use crate::streaming::StreamParser;
use crate::types::*;
use crate::LlmSession;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tuning knobs for a [`ReActAgent`].
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Maximum number of ReAct iterations before forcing a final answer.
    pub max_iterations: usize,
    /// Temperature passed to the LLM for every completion request.
    pub temperature: Option<f32>,
    /// Max tokens passed to the LLM for every completion request.
    pub max_tokens: Option<u32>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            temperature: Some(0.3),
            max_tokens: Some(4096),
        }
    }
}

// ---------------------------------------------------------------------------
// Step enum (traces of the ReAct loop)
// ---------------------------------------------------------------------------

/// One step produced by the agent loop. Useful for logging / UI rendering.
#[derive(Debug, Clone)]
pub enum AgentStep {
    Thought(String),
    Action {
        tool_name: String,
        args: serde_json::Value,
    },
    Observation(String),
    FinalAnswer(String),
}

// ---------------------------------------------------------------------------
// ReAct prompt parsing helpers
// ---------------------------------------------------------------------------

/// Outcome of parsing a single LLM response in the ReAct loop.
#[derive(Debug, Clone)]
enum ParsedReact {
    /// The LLM emitted one or more actions – we must execute them.
    Actions(Vec<ParsedAction>),
    /// The LLM produced a final answer – the loop should stop.
    FinalAnswer(String),
}

#[derive(Debug, Clone)]
struct ParsedAction {
    tool_name: String,
    args: serde_json::Value,
}

/// Attempt to extract a final answer from free-form text.
fn extract_final_answer(text: &str) -> Option<String> {
    for marker in &["Final Answer:", "FINAL_ANSWER:", "Final answer:"] {
        if let Some(idx) = text.find(marker) {
            let answer = text[idx + marker.len()..].trim().to_string();
            if !answer.is_empty() {
                return Some(answer);
            }
        }
    }
    None
}

/// Parse the LLM response for ReAct-style `Action: tool_name(args)` blocks.
///
/// Returns `None` when nothing actionable was found (meaning the text itself
/// should be treated as the final answer, or will be fed back as-is).
fn parse_actions(text: &str) -> Vec<ParsedAction> {
    let mut actions = Vec::new();
    // Look for lines like:  Action: tool_name({...})
    // or the tool-call format produced when the LLM uses native tool calling.
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Action:") {
            let rest = rest.trim();
            // Expect: tool_name({ ... })
            if let Some(open) = rest.find('(') {
                let tool_name = rest[..open].trim().to_string();
                // Find the matching closing paren
                let args_start = open + 1;
                if let Some(depth) = find_closing_paren(rest, args_start) {
                    let args_str = rest[args_start..depth].trim();
                    let args: serde_json::Value = if args_str.is_empty() {
                        serde_json::Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::Value::String(args_str.to_string()))
                    };
                    actions.push(ParsedAction { tool_name, args });
                }
            }
        }
    }
    actions
}

/// Given a string and the index of the character after the opening `(`,
/// return the index of the matching closing `)`.
fn find_closing_paren(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if start >= bytes.len() || bytes[start] != b'(' {
        // The char at `start` is already past the '(' — we actually need to
        // look backwards. But our caller already identified the '(' position
        // as `open`, and passes `open + 1` as `start`. So we start counting
        // from `open` (which is `start - 1`).
    }
    // If the byte at `start` is '(' the caller already advanced past it,
    // so we need to start counting from start-1.
    let effective_start = if start > 0 && bytes.get(start) == Some(&b'(') {
        start
    } else if start > 0 && bytes.get(start - 1) == Some(&b'(') {
        start - 1
    } else {
        start
    };

    let mut depth: i32 = 0;
    let mut idx = effective_start;
    while idx < bytes.len() {
        match bytes[idx] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

/// Top-level parser: decide whether the response is a final answer or
/// contains actions to execute.
fn parse_react_response(content: &str) -> ParsedReact {
    // Check for final answer first (takes priority)
    if let Some(answer) = extract_final_answer(content) {
        return ParsedReact::FinalAnswer(answer);
    }

    // Then check for actions
    let actions = parse_actions(content);
    if !actions.is_empty() {
        return ParsedReact::Actions(actions);
    }

    // If neither, treat the full text as the final answer — the LLM
    // decided to answer directly without using the ReAct format.
    ParsedReact::FinalAnswer(content.to_string())
}

// ---------------------------------------------------------------------------
// ReAct system prompt
// ---------------------------------------------------------------------------

pub const REACT_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are a helpful AI assistant that solves problems step by step using the ReAct framework.

You have access to tools that you can invoke to gather information and take actions.

Follow this exact format in your response:

Thought: <your reasoning about what to do next>
Action: <tool_name>(<json_arguments>)
Observation: <this will be filled by the system after tool execution>

You can use multiple Thought/Action/Observation cycles.
When you have enough information to answer, respond with:

Final Answer: <your complete answer>

Important rules:
- Always start with a Thought explaining your reasoning.
- Use Action to call a tool when you need more information.
- Wait for the Observation before proceeding.
- Provide a Final Answer when you have resolved the query.
- If you cannot resolve the query, explain what you found and what is still unclear.

Available tools will be provided via the standard tool-calling interface.";

// ---------------------------------------------------------------------------
// ReActAgent
// ---------------------------------------------------------------------------

/// A ReAct-style agent that drives an [`LlmSession`] through iterative
/// Thought → Action → Observation cycles until a Final Answer is produced.
pub struct ReActAgent<'a> {
    session: &'a LlmSession,
    config: AgentConfig,
}

impl<'a> ReActAgent<'a> {
    pub fn new(session: &'a LlmSession) -> Self {
        Self {
            session,
            config: AgentConfig::default(),
        }
    }

    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.config = config;
        self
    }

    /// Run the ReAct loop for the given user query.
    ///
    /// Returns the final answer string, or an error if the loop fails.
    /// Respects the cancellation token on the underlying [`LlmSession`].
    pub async fn run(&self, query: &str) -> Result<String, AishError> {
        self.run_with_system_prompt(query, REACT_SYSTEM_PROMPT_TEMPLATE)
            .await
    }

    /// Run the ReAct loop with a custom system prompt.
    pub async fn run_with_system_prompt(
        &self,
        query: &str,
        system_prompt: &str,
    ) -> Result<String, AishError> {
        let cancel = self.session.cancellation_token();
        cancel.reset();

        let mut messages: Vec<ChatMessage> = Vec::new();
        messages.push(ChatMessage::system(system_prompt));
        messages.push(ChatMessage::user(query));

        let tool_specs: Vec<ToolSpec> = self.session.tool_specs();
        let has_tools = !tool_specs.is_empty();

        for iteration in 0..self.config.max_iterations {
            if cancel.is_cancelled() {
                return Err(AishError::Cancelled);
            }

            info!(iteration, "ReAct loop iteration");

            // Call the LLM (non-streaming for easier parsing of the ReAct text).
            let response = self
                .session
                .chat_completion_raw(
                    &messages,
                    if has_tools { Some(&tool_specs) } else { None },
                    false, // non-streaming for ReAct parsing
                    self.config.temperature,
                    self.config.max_tokens,
                )
                .await?;

            // Extract text content and tool calls from the response.
            let (content, tool_calls) = match response {
                LlmResponse::Json(json) => {
                    let (text, tcs, _usage) = StreamParser::parse_response(&json);
                    (text.unwrap_or_default(), tcs)
                }
                LlmResponse::Stream(_) => {
                    // Should not happen since we requested non-streaming,
                    // but handle gracefully.
                    return Err(AishError::Llm(
                        "Unexpected streaming response in ReAct loop".into(),
                    ));
                }
            };

            debug!(?content, ?tool_calls, "LLM response parsed");

            // Emit content delta for UI feedback.
            self.emit_agent_step(AgentStep::Thought(content.clone()));

            // --- Check for native tool calls (LLM used the tool-calling API) ---
            if !tool_calls.is_empty() {
                // Add the assistant message with tool calls to history.
                let mut assistant_msg = ChatMessage::assistant(&content);
                assistant_msg.tool_calls = Some(tool_calls.clone());
                messages.push(assistant_msg);

                // Execute each tool and collect observations.
                for tc in &tool_calls {
                    if cancel.is_cancelled() {
                        return Err(AishError::Cancelled);
                    }

                    let result = self.session.execute_tool_external(tc).await;
                    let obs_text = result.output.clone();

                    self.emit_agent_step(AgentStep::Action {
                        tool_name: tc.name.clone(),
                        args: serde_json::from_str(&tc.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    });
                    self.emit_agent_step(AgentStep::Observation(obs_text.clone()));

                    messages.push(ChatMessage::tool_result(&tc.id, result.output));
                }

                // After tool results, add a user prompt nudging the LLM to continue.
                messages.push(ChatMessage::user(
                    "Continue your analysis. When you have reached a conclusion, respond with \"Final Answer: <your conclusion>\".",
                ));
                continue;
            }

            // --- No native tool calls: parse ReAct text format ---
            let parsed = parse_react_response(&content);

            match parsed {
                ParsedReact::FinalAnswer(answer) => {
                    self.emit_agent_step(AgentStep::FinalAnswer(answer.clone()));
                    return Ok(answer);
                }
                ParsedReact::Actions(actions) => {
                    // Build a combined observation from all actions.
                    let mut all_observations = String::new();

                    for action in &actions {
                        if cancel.is_cancelled() {
                            return Err(AishError::Cancelled);
                        }

                        self.emit_agent_step(AgentStep::Action {
                            tool_name: action.tool_name.clone(),
                            args: action.args.clone(),
                        });

                        // Look up the tool and execute it.
                        let obs = match self
                            .session
                            .execute_tool_by_name(&action.tool_name, action.args.clone())
                        {
                            Ok(result) => result.output,
                            Err(e) => format!("Error executing tool '{}': {}", action.tool_name, e),
                        };

                        self.emit_agent_step(AgentStep::Observation(obs.clone()));
                        all_observations
                            .push_str(&format!("Observation ({}): {}\n", action.tool_name, obs));
                    }

                    // Feed observations back as a user message.
                    messages.push(ChatMessage::assistant(&content));
                    messages.push(ChatMessage::user(all_observations.trim().to_string()));
                }
            }
        }

        // Max iterations reached: return the last content as best-effort answer.
        warn!("ReAct agent hit max iterations");
        Ok(
            "Agent reached the maximum number of iterations without producing a final answer."
                .to_string(),
        )
    }

    fn emit_agent_step(&self, step: AgentStep) {
        let (event_type, data) = match &step {
            AgentStep::Thought(t) => (
                LlmEventType::ContentDelta,
                serde_json::json!({ "thought": t }),
            ),
            AgentStep::Action { tool_name, args } => (
                LlmEventType::ToolExecutionStart,
                serde_json::json!({ "tool_name": tool_name, "args": args }),
            ),
            AgentStep::Observation(o) => (
                LlmEventType::ToolExecutionEnd,
                serde_json::json!({ "observation": o }),
            ),
            AgentStep::FinalAnswer(a) => (
                LlmEventType::ContentDelta,
                serde_json::json!({ "final_answer": a }),
            ),
        };
        self.session.emit_event(LlmEvent {
            event_type,
            data,
            timestamp: now_timestamp(),
            metadata: Some(serde_json::json!({ "source": "react_agent" })),
        });
    }
}

// ---------------------------------------------------------------------------
// SystemDiagnoseAgent (legacy - backward compatibility)
// ---------------------------------------------------------------------------

/// Specialised ReAct agent for system diagnosis tasks.
///
/// Wraps [`ReActAgent`] with a diagnosis-specific system prompt that includes
/// basic system information (OS, hostname, user) and instructs the LLM to use
/// `bash` and `read_file` tools to investigate system issues.
///
/// **Note:** This is the legacy implementation. For new code, prefer using
/// [`crate::DiagnoseAgent`] which creates an isolated sub-session with its own
/// tool registry and context.
pub struct SystemDiagnoseAgent<'a> {
    agent: ReActAgent<'a>,
    system_prompt: String,
}

impl<'a> SystemDiagnoseAgent<'a> {
    /// Create a new diagnosis agent backed by the given session.
    ///
    /// The session should already have `bash_exec` and `read_file` (or similar)
    /// tools registered.
    pub fn new(session: &'a LlmSession) -> Self {
        Self::with_config(session, AgentConfig::default())
    }

    pub fn with_config(session: &'a LlmSession, config: AgentConfig) -> Self {
        let system_prompt = build_diagnose_system_prompt();
        Self {
            agent: ReActAgent::new(session).with_config(config),
            system_prompt,
        }
    }

    /// Run the diagnostic loop for the given query.
    pub async fn diagnose(&self, query: &str) -> Result<String, AishError> {
        self.agent
            .run_with_system_prompt(query, &self.system_prompt)
            .await
    }
}

/// Build the system prompt for the diagnostic agent, embedding basic
/// system information collected at call time.
fn build_diagnose_system_prompt() -> String {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
