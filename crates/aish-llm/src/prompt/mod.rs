//! Prompt assembly framework (Phase A: MainChat + SubAgent).

mod assembly;
mod context;
mod visibility;

pub use assembly::{PromptAssembly, PromptBundle};
pub use context::PromptContext;
pub use visibility::{ToolVisibilityPolicy, SUBAGENT_GLOBAL_DENY};
