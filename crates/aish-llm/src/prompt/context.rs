//! LLM prompt assembly contexts (Phase A: MainChat + SubAgent only).

/// Which LLM loop is requesting a prompt bundle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PromptContext {
    /// Main shell chat loop; plan phase read from [`crate::session::LlmSession::plan_state`].
    #[default]
    MainChat,
    /// Built-in sub-agent spawn loop.
    SubAgent { subagent_type: String },
}
