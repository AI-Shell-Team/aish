//! Sub-agent spawn infrastructure (Phase 1).
//!
//! Issue #330: reusable native tool calling loop, cancel cascade, mock LLM tests.

mod mock_llm;
mod spawn;
mod tool_loop;

pub use mock_llm::{mock_text_response, mock_tool_call_response};
pub use spawn::{spawn, SpawnConfig, SpawnResult};
pub use tool_loop::{run_tool_loop_until_done, LoopOutcome, LoopStatus, ToolLoopConfig};
