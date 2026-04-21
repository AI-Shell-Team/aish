use serde::{Deserialize, Serialize};

/// Result of a callback invoked during LLM processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmCallbackResult {
    Continue,
    Approve,
    Deny,
    Cancel,
}

/// Status of a tool dispatch after execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolDispatchStatus {
    Executed,
    ShortCircuit,
    Rejected,
    Cancelled,
}

/// Result returned by a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub meta: Option<serde_json::Value>,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            ok: true,
            output: output.into(),
            meta: None,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: output.into(),
            meta: None,
        }
    }
}

/// A single tool call requested by the LLM.
///
/// Internally stores flat fields (`id`, `name`, `arguments`) for convenient
/// Rust access, but serializes to the OpenAI API format which nests
/// `name` and `arguments` under a `function` object and adds `type: "function"`.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String, // JSON string
}

impl Serialize for ToolCall {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ToolCall", 3)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("type", "function")?;
        s.serialize_field(
            "function",
            &serde_json::json!({
                "name": self.name,
                "arguments": self.arguments,
            }),
        )?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("id"))?
            .to_string();
        let function = value
            .get("function")
            .ok_or_else(|| serde::de::Error::missing_field("function"))?;
        let name = function
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("function.name"))?
            .to_string();
        let arguments = function
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}")
            .to_string();
        Ok(ToolCall {
            id,
            name,
            arguments,
        })
    }
}

/// A message in the chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "system", "user", "assistant", "tool"
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }
}

/// Specification of a function tool exposed to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub r#type: String, // always "function"
    pub function: FunctionSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

/// Result of a preflight check before tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightResult {
    /// Execution is allowed.
    Allow,
    /// User confirmation is required before execution.
    Confirm { message: String },
    /// Execution is blocked.
    Block { message: String },
}

/// Trait for tool implementations that the LLM can invoke.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    fn to_spec(&self) -> ToolSpec {
        ToolSpec {
            r#type: "function".into(),
            function: FunctionSpec {
                name: self.name().into(),
                description: self.description().into(),
                parameters: self.parameters(),
            },
        }
    }

    /// Optional preflight check before execution.
    /// Default implementation allows all executions.
    fn preflight(&self, _args: &serde_json::Value) -> PreflightResult {
        PreflightResult::Allow
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult;
}

/// Token used to cancel an in-progress LLM request.
pub struct CancellationToken {
    cancelled: std::sync::atomic::AtomicBool,
    callbacks: std::sync::Mutex<Vec<Box<dyn Fn() + Send + 'static>>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: std::sync::atomic::AtomicBool::new(false),
            callbacks: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(cbs) = self.callbacks.lock() {
            for cb in cbs.iter() {
                cb();
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn add_callback(&self, cb: Box<dyn Fn() + Send + 'static>) {
        if let Ok(mut cbs) = self.callbacks.lock() {
            cbs.push(cb);
        }
    }

    /// Set the cancelled flag using only an atomic store.
    /// Async-signal-safe: safe to call from a POSIX signal handler.
    /// Note: registered callbacks are NOT invoked.
    pub fn cancel_atomic(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.cancelled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal tool that always allows execution (default preflight).
    struct AllowTool;

    impl Tool for AllowTool {
        fn name(&self) -> &str {
            "allow_tool"
        }
        fn description(&self) -> &str {
            "A tool that always allows"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn execute(&self, _args: serde_json::Value) -> ToolResult {
            ToolResult::success("ok")
        }
    }

    /// A tool that always blocks via preflight.
    struct BlockTool;

    impl Tool for BlockTool {
        fn name(&self) -> &str {
            "block_tool"
        }
        fn description(&self) -> &str {
            "A tool that always blocks"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn preflight(&self, _args: &serde_json::Value) -> PreflightResult {
            PreflightResult::Block {
                message: "blocked for testing".into(),
            }
        }
        fn execute(&self, _args: serde_json::Value) -> ToolResult {
            ToolResult::success("should not reach")
        }
    }

    /// A tool that requires confirmation via preflight.
    struct ConfirmTool;

    impl Tool for ConfirmTool {
        fn name(&self) -> &str {
            "confirm_tool"
        }
        fn description(&self) -> &str {
            "A tool that requires confirmation"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn preflight(&self, _args: &serde_json::Value) -> PreflightResult {
            PreflightResult::Confirm {
                message: "please confirm".into(),
            }
        }
        fn execute(&self, _args: serde_json::Value) -> ToolResult {
            ToolResult::success("confirmed and executed")
        }
    }

    #[test]
    fn test_preflight_default_allows() {
        let tool = AllowTool;
        let result = tool.preflight(&serde_json::json!({}));
        assert_eq!(result, PreflightResult::Allow);
    }

    #[test]
    fn test_preflight_block() {
        let tool = BlockTool;
        let result = tool.preflight(&serde_json::json!({}));
        assert_eq!(
            result,
            PreflightResult::Block {
                message: "blocked for testing".into()
            }
        );
    }

    #[test]
    fn test_preflight_confirm() {
        let tool = ConfirmTool;
        let result = tool.preflight(&serde_json::json!({}));
        assert_eq!(
            result,
            PreflightResult::Confirm {
                message: "please confirm".into()
            }
        );
    }

    #[test]
    fn test_preflight_result_equality() {
        assert_eq!(PreflightResult::Allow, PreflightResult::Allow);
        assert_ne!(
            PreflightResult::Allow,
            PreflightResult::Block {
                message: String::new()
            }
        );
    }

    #[test]
    fn test_cancel_atomic_sets_is_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel_atomic();
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_cancel_atomic_does_not_invoke_callbacks() {
        let token = CancellationToken::new();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        token.add_callback(Box::new(move || {
            called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        token.cancel_atomic();
        assert!(token.is_cancelled());
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "cancel_atomic should not invoke registered callbacks"
        );
    }
}
