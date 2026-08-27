use aish_core::MemoryType;
use serde::{Deserialize, Serialize};

/// A single message within the conversation context window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMessage {
    /// One of "system", "user", "assistant", or "tool".
    pub role: String,
    pub content: String,
    pub memory_type: MemoryType,
    /// Tool name for role="tool" messages.
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    /// Tool calls attached to an assistant message. Optional so snapshots
    /// persisted before this field existed still deserialize.
    #[serde(default)]
    pub tool_calls: Option<Vec<aish_core::ContextToolCall>>,
}
