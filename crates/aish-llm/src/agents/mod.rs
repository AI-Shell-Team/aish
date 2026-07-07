//! Sub-agent spawn infrastructure (Phase 1).
//!
//! Issue #330: reusable native tool calling loop, cancel cascade, mock LLM tests.
//! Issue #331: explore vertical slice — registry, tool filter, spawn_builtin.
//! Issue #332: plan + general-purpose built-ins and tool filtering.

mod mock_llm;
mod outcome;
mod registry;
mod spawn;
mod tool_loop;
mod tools;

pub use mock_llm::{mock_text_response, mock_tool_call_response};
pub use outcome::{
    extract_spawn_outcome, OutcomeConfig, SpawnOutcome, TerminationKind, INCOMPLETE_PREFIX,
};
pub use registry::{AgentDefinition, AgentRegistry, ToolStrategy};
pub use spawn::{
    effective_max_turns, spawn, spawn_builtin, SpawnConfig, SpawnResult, GLOBAL_MAX_TURNS,
};
pub use tool_loop::{run_tool_loop_until_done, LoopOutcome, LoopStatus, ToolLoopConfig};
pub use tools::{parent_has_skill_tool, resolve_tool_names_for_agent, resolve_tools_for_agent};
