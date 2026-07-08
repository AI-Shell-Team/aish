use std::future::Future;
use std::pin::Pin;

use aish_security::SecurityDecision;
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

/// Anthropic-style cache control marker for prompt caching.
/// When set, marks this message as a cache breakpoint.
/// Non-Anthropic providers ignore this field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral".to_string(),
        }
    }
}

/// A single block inside a structured message (OpenAI content-blocks format).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlContent },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlContent {
    pub url: String,
}

/// Message content — serializes as a plain string or a content-blocks array.
#[derive(Debug, Clone)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    /// Return a &str slice if this is plain text, else None.
    pub fn as_text_str(&self) -> Option<&str> {
        match self {
            MessageContent::Text(s) => Some(s),
            MessageContent::Blocks(_) => None,
        }
    }

    /// Return the text content, joining block texts if structured.
    /// Image blocks are skipped — only text is concatenated.
    pub fn to_text(&self) -> Option<String> {
        match self {
            MessageContent::Text(s) => Some(s.clone()),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(""))
                }
            }
        }
    }

    /// Return the byte length of the text portion (for token estimation).
    /// Avoids allocation — iterates blocks directly.
    pub fn text_byte_len(&self) -> usize {
        match self {
            MessageContent::Text(s) => s.len(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.len(),
                    _ => 0,
                })
                .sum(),
        }
    }
}

impl Serialize for MessageContent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MessageContent::Text(text) => text.serialize(s),
            MessageContent::Blocks(blocks) => blocks.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match &value {
            serde_json::Value::String(s) => Ok(MessageContent::Text(s.clone())),
            serde_json::Value::Array(_) => {
                let blocks: Vec<ContentBlock> = serde_json::from_value(value)
                    .map_err(|e| serde::de::Error::custom(e.to_string()))?;
                Ok(MessageContent::Blocks(blocks))
            }
            serde_json::Value::Null => Ok(MessageContent::Text(String::new())),
            _ => Err(serde::de::Error::custom(
                "expected string or array for content",
            )),
        }
    }
}

/// A message in the chat conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // "system", "user", "assistant", "tool"
    pub content: Option<MessageContent>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    /// DeepSeek thinking mode reasoning content — must be echoed back to the API.
    pub reasoning_content: Option<String>,
    /// Anthropic-style cache control marker. Non-Anthropic providers ignore this.
    pub cache_control: Option<CacheControl>,
}

impl Serialize for ChatMessage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = 1
            + self.content.is_some() as usize
            + self.tool_calls.is_some() as usize
            + self.tool_call_id.is_some() as usize
            + self.name.is_some() as usize
            + self.reasoning_content.is_some() as usize
            + self.cache_control.is_some() as usize;
        let mut st = s.serialize_struct("ChatMessage", field_count)?;
        st.serialize_field("role", &self.role)?;
        if let Some(content) = &self.content {
            st.serialize_field("content", content)?;
        }
        if let Some(tool_calls) = &self.tool_calls {
            st.serialize_field("tool_calls", tool_calls)?;
        }
        if let Some(tool_call_id) = &self.tool_call_id {
            st.serialize_field("tool_call_id", tool_call_id)?;
        }
        if let Some(name) = &self.name {
            st.serialize_field("name", name)?;
        }
        if let Some(reasoning) = &self.reasoning_content {
            st.serialize_field("reasoning_content", reasoning)?;
        }
        if let Some(cc) = &self.cache_control {
            st.serialize_field("cache_control", cc)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for ChatMessage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct ChatMessageFields {
            role: String,
            content: Option<MessageContent>,
            tool_calls: Option<Vec<ToolCall>>,
            tool_call_id: Option<String>,
            name: Option<String>,
            reasoning_content: Option<String>,
            cache_control: Option<CacheControl>,
        }
        let f = ChatMessageFields::deserialize(d)?;
        Ok(ChatMessage {
            role: f.role,
            content: f.content,
            tool_calls: f.tool_calls,
            tool_call_id: f.tool_call_id,
            name: f.name,
            reasoning_content: f.reasoning_content,
            cache_control: f.cache_control,
        })
    }
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    /// Create a user message with both text and image URLs (content-blocks format).
    /// Empty text is omitted — the message starts with the first image block.
    pub fn user_with_images(text: String, image_urls: Vec<String>) -> Self {
        let mut blocks = Vec::with_capacity(1 + image_urls.len());
        if !text.is_empty() {
            blocks.push(ContentBlock::Text { text });
        }
        for url in image_urls {
            blocks.push(ContentBlock::ImageUrl {
                image_url: ImageUrlContent { url },
            });
        }
        Self {
            role: "user".into(),
            content: Some(MessageContent::Blocks(blocks)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    /// Return the text content as a &str, or None if structured (blocks).
    pub fn text_content(&self) -> Option<&str> {
        self.content.as_ref().and_then(|c| c.as_text_str())
    }

    /// Return the byte length of the text portion (for token estimation).
    pub fn text_byte_len(&self) -> usize {
        self.content
            .as_ref()
            .map(|c| c.text_byte_len())
            .unwrap_or(0)
    }

    /// Return true if this message contains image blocks.
    pub fn has_images(&self) -> bool {
        matches!(
            &self.content,
            Some(MessageContent::Blocks(blocks)) if blocks.iter().any(|b| matches!(b, ContentBlock::ImageUrl { .. }))
        )
    }
}

/// Result of processing an LLM turn, including tool execution messages
/// generated during the tool calling loop.
#[derive(Debug, Clone)]
pub struct ProcessResult {
    /// Final text response from the assistant.
    pub text: String,
    /// New messages appended during this turn (assistant+tool_calls,
    /// tool_result pairs). Callers should extend their conversation
    /// history with these to preserve tool execution context.
    pub new_messages: Vec<ChatMessage>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityPanelMode {
    Confirm,
    Blocked,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityPanel {
    pub mode: SecurityPanelMode,
    pub tool_name: String,
    pub target: Option<String>,
    pub message: String,
    pub risk_level: Option<String>,
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
}

impl SecurityPanel {
    pub fn fallback(
        tool_name: impl Into<String>,
        message: impl Into<String>,
        mode: SecurityPanelMode,
    ) -> Self {
        Self {
            mode,
            tool_name: tool_name.into(),
            target: None,
            message: message.into(),
            risk_level: None,
            reasons: Vec::new(),
            alternatives: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightSecurityContext {
    pub tool_name: String,
    pub target: Option<String>,
    pub message: String,
    pub mode: SecurityPanelMode,
    pub decision: Option<SecurityDecision>,
}

impl PreflightSecurityContext {
    pub fn fallback(
        tool_name: impl Into<String>,
        target: Option<String>,
        message: impl Into<String>,
        mode: SecurityPanelMode,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            target,
            message: message.into(),
            mode,
            decision: None,
        }
    }
}

/// Result of a preflight check before tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightResult {
    /// Execution is allowed.
    Allow,
    /// User confirmation is required before execution.
    Confirm {
        message: String,
        security: Option<PreflightSecurityContext>,
    },
    /// Execution is blocked.
    Block {
        message: String,
        security: Option<PreflightSecurityContext>,
    },
}

/// Whether a tool's [`Tool::prompt`] is included in the system appendix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptVisibility {
    /// Include in appendix when [`Tool::prompt`] is non-empty after trim.
    #[default]
    AppendixWhenNonEmpty,
    /// Never append [`Tool::prompt`] to the system message.
    NeverInAppendix,
}

/// Trait for tool implementations that the LLM can invoke.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    fn prompt(&self) -> &str {
        ""
    }

    /// Controls whether [`Tool::prompt`] is appended to the system message appendix.
    fn prompt_visibility(&self) -> PromptVisibility {
        PromptVisibility::AppendixWhenNonEmpty
    }

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

    /// Context-aware preflight. Default delegates to [`Self::preflight`].
    fn preflight_with_context(
        &self,
        args: &serde_json::Value,
        ctx: &crate::tool_context::ToolContext<'_>,
    ) -> PreflightResult {
        let _ = ctx;
        self.preflight(args)
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult;

    /// Async variant of `execute`. The default implementation delegates to the
    /// synchronous `execute` wrapped in `catch_unwind` so panics are gracefully
    /// converted to `ToolResult::error`. Tools that need async I/O (e.g.
    /// spawning sub-sessions) should override this method.
    fn execute_async<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.execute(args))) {
                Ok(result) => result,
                Err(payload) => {
                    let message = if let Some(s) = payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Tool execution panicked".to_string()
                    };
                    ToolResult::error(format!("Error: {}", message))
                }
            }
        })
    }

    /// Async execution with access to the hosting [`LlmSession`].
    ///
    /// Tools that spawn sub-sessions (e.g. `Agent`) override this to inherit parent
    /// credentials and tool pools. Default delegates to [`Self::execute_async`].
    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a crate::session::LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let _ = session;
        self.execute_async(args)
    }
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
                security: None,
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
                security: None,
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
                message: "blocked for testing".into(),
                security: None,
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
                message: "please confirm".into(),
                security: None,
            }
        );
    }

    #[test]
    fn test_preflight_result_equality() {
        assert_eq!(PreflightResult::Allow, PreflightResult::Allow);
        assert_ne!(
            PreflightResult::Allow,
            PreflightResult::Block {
                message: String::new(),
                security: None,
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

    #[test]
    fn test_cache_control_serialization() {
        let msg = ChatMessage::system("test");
        let json = serde_json::to_string(&msg).unwrap();
        // cache_control should be absent when None
        assert!(!json.contains("cache_control"));

        let mut msg = ChatMessage::system("test");
        msg.cache_control = Some(CacheControl::ephemeral());
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("cache_control"));
        assert!(json.contains("ephemeral"));
    }

    #[test]
    fn test_chat_message_user_serializes_text_content() {
        let msg = ChatMessage::user("hello");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"content\":\"hello\""));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn test_chat_message_user_with_images_serializes_blocks() {
        let msg = ChatMessage::user_with_images(
            "describe this".to_string(),
            vec!["data:image/png;base64,abc".to_string()],
        );
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"type\":\"image_url\""));
    }

    #[test]
    fn test_chat_message_text_content_accessor() {
        let msg = ChatMessage::user("hello");
        assert_eq!(msg.text_content(), Some("hello"));
    }

    #[test]
    fn test_chat_message_has_images() {
        let text_msg = ChatMessage::user("hello");
        assert!(!text_msg.has_images());
        let img_msg = ChatMessage::user_with_images(
            "look".to_string(),
            vec!["data:image/png;base64,abc".to_string()],
        );
        assert!(img_msg.has_images());
    }

    #[test]
    fn test_message_content_text_serializes_as_string() {
        let content = MessageContent::Text("hello".to_string());
        let json = serde_json::to_string(&content).unwrap();
        assert_eq!(json, "\"hello\"");
    }

    #[test]
    fn test_message_content_text_deserializes_from_string() {
        let content: MessageContent = serde_json::from_str("\"hello\"").unwrap();
        assert!(matches!(content, MessageContent::Text(s) if s == "hello"));
    }

    #[test]
    fn test_message_content_blocks_serializes_as_array() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "describe this".to_string(),
            },
            ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/png;base64,abc".to_string(),
                },
            },
        ]);
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.starts_with('['));
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"type\":\"image_url\""));
        assert!(json.contains("\"url\":\"data:image/png;base64,abc\""));
    }

    #[test]
    fn test_message_content_blocks_deserializes_from_array() {
        let json = r#"[{"type":"text","text":"hi"},{"type":"image_url","image_url":{"url":"data:image/png;base64,xyz"}}]"#;
        let content: MessageContent = serde_json::from_str(json).unwrap();
        match content {
            MessageContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
            }
            _ => panic!("expected Blocks"),
        }
    }

    #[test]
    fn test_message_content_as_text_str() {
        assert_eq!(MessageContent::Text("hi".into()).as_text_str(), Some("hi"));
        assert_eq!(MessageContent::Blocks(vec![]).as_text_str(), None);
    }

    #[test]
    fn test_message_content_to_text_from_blocks() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "hello ".to_string(),
            },
            ContentBlock::Text {
                text: "world".to_string(),
            },
            ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:...".to_string(),
                },
            },
        ]);
        assert_eq!(content.to_text(), Some("hello world".to_string()));
    }

    #[test]
    fn test_content_block_roundtrip() {
        let block = ContentBlock::ImageUrl {
            image_url: ImageUrlContent {
                url: "data:image/jpeg;base64,AAA".to_string(),
            },
        };
        let json = serde_json::to_string(&block).unwrap();
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ContentBlock::ImageUrl { .. }));
    }
}
