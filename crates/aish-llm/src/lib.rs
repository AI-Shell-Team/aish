// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments
)]

pub mod agent;
pub mod client;
pub mod diagnose_agent;
pub mod langfuse;
pub mod models;
pub mod oauth;
pub mod provider;
pub mod providers;
pub mod session;
pub mod streaming;
pub mod subsession;
pub mod types;
pub mod usage;

pub use agent::{AgentConfig, AgentStep, ReActAgent, SystemDiagnoseAgent};
pub use client::{LiteLLMClient, LlmClient, LlmResponse};
pub use diagnose_agent::DiagnoseAgent;
pub use langfuse::{LangfuseClient, LangfuseConfig};
pub use models::{
    fetch_models_from_api, fetch_ollama_models, filter_by_tool_support, get_predefined_models,
    model_supports_tools, ModelInfo,
};
pub use oauth::{
    exchange_code_for_tokens, generate_pkce, generate_state, load_tokens, login_with_browser,
    login_with_device_code, open_url, save_tokens, OAuthProviderSpec, OAuthTokens, PkcePair,
};
pub use provider::{
    detect_provider, detect_provider_from_model, refine_provider_from_api_base, ProviderInfo,
};
pub use providers::{
    OpenAiCompatProvider, ProviderCapabilities, ProviderMetadata, ProviderRegistry,
};
pub use session::LlmSession;
pub use streaming::{SseEvent, StreamParser};
pub use subsession::{SubSession, SubSessionConfig};
pub use types::*;
pub use usage::{TokenStats, TokenUsage};
