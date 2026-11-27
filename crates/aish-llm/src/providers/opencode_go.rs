//! OpenCode Go provider adapter and request-header helpers.
//!
//! OpenCode Go is an OpenAI-compatible gateway. Besides the usual
//! `Authorization` header it requires a stable `x-opencode-session` header
//! (and asks clients to identify themselves with their own `User-Agent`).
//!
//! This covers all Go-family plans: **Go**, **Go Plus** and **Go Teams** all
//! share the same `/inference/go` base URL and the same session-header
//! requirement. The plan only changes usage limits and which key types are
//! accepted (Go / Go Plus: personal or service-account key; Go Teams: a
//! member's personal key). Pay-as-you-go Console uses `/inference` (no `/go`)
//! and does not need the session header.
//!
//! Official docs: <https://opencode.ai/docs/go/>
//! Harness setup:  <https://opencode.ai/docs/console/harnesses/>

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use std::sync::OnceLock;

use super::types::{ProviderAdapter, ProviderCapabilities, ProviderMetadata};

/// Provider identifier used in config / model prefixes.
pub const OPENCODE_GO_PROVIDER: &str = "opencode-go";

/// Base URL for the Go plan. Append `/openai/v1` for the OpenAI-compatible
/// chat-completions / responses endpoints.
pub const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/inference/go";

/// OpenAI-compatible base URL (chat completions, responses, model list).
pub const OPENCODE_GO_OPENAI_BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";

/// Session header required by OpenCode Go.
pub const OPENCODE_GO_SESSION_HEADER: &str = "x-opencode-session";

/// User-Agent advertised to OpenCode Go.
pub const OPENCODE_GO_USER_AGENT: &str = concat!("aish/", env!("CARGO_PKG_VERSION"));

/// Returns `true` when `api_base` points at the OpenCode Go gateway.
///
/// Only the Go plan requires `x-opencode-session`; the pay-as-you-go base
/// (`https://opencode.ai/inference`) must not match.
pub fn is_opencode_go_api_base(api_base: &str) -> bool {
    api_base
        .trim()
        .to_ascii_lowercase()
        .contains("opencode.ai/inference/go")
}

/// Process-wide fallback session id for contexts without a shell session
/// (inline completion, one-shot probes, tests).
pub fn fallback_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| format!("ses_{}", uuid::Uuid::new_v4().simple()))
}

/// Build the extra request headers for an OpenCode Go request.
///
/// Returns an empty map for non-OpenCode-Go endpoints so the same client can
/// talk to any OpenAI-compatible provider.
pub fn provider_headers(api_base: &str, session_id: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if !is_opencode_go_api_base(api_base) {
        return headers;
    }

    if let Ok(value) = HeaderValue::from_str(OPENCODE_GO_USER_AGENT) {
        headers.insert(USER_AGENT, value);
    }

    let session_id = session_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| fallback_session_id());
    if let Ok(value) = HeaderValue::from_str(session_id) {
        headers.insert(HeaderName::from_static(OPENCODE_GO_SESSION_HEADER), value);
    }

    headers
}

/// Map OpenCode Go's structured error bodies to actionable hints.
pub fn error_hint(_status: reqwest::StatusCode, body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    if lower.contains("missingsessionid") {
        return Some(
            "OpenCode Go requires the `x-opencode-session` header. \
             Use the built-in `opencode-go` provider so aish can send it."
                .to_string(),
        );
    }
    if lower.contains("entitlementerror") || lower.contains("subscription required") {
        return Some(
            "This workspace has no active OpenCode Go subscription \
             (Go, Go Plus or Go Teams). Subscribe to a Go plan or switch to \
             the pay-as-you-go base URL."
                .to_string(),
        );
    }
    None
}

/// Provider metadata adapter so the setup UI and provider registry can
/// recognise OpenCode Go endpoints.
pub struct OpenCodeGoProviderAdapter;

impl ProviderAdapter for OpenCodeGoProviderAdapter {
    fn provider_id(&self) -> &str {
        OPENCODE_GO_PROVIDER
    }

    fn display_name(&self) -> &str {
        "OpenCode Go"
    }

    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            provider_id: OPENCODE_GO_PROVIDER.to_string(),
            display_name: self.display_name().to_string(),
            dashboard_url: Some("https://opencode.ai/".to_string()),
            api_key_env_var: "OPENCODE_API_KEY".to_string(),
            supports_streaming: true,
            supports_tools: true,
            uses_custom_client: false,
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn matches_model(&self, model: &str) -> bool {
        model
            .trim()
            .to_ascii_lowercase()
            .starts_with("opencode-go/")
    }

    fn matches_api_base(&self, api_base: &str) -> bool {
        is_opencode_go_api_base(api_base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_go_base() {
        assert!(is_opencode_go_api_base(
            "https://opencode.ai/inference/go/openai/v1"
        ));
        assert!(is_opencode_go_api_base("https://opencode.ai/inference/go"));
        assert!(is_opencode_go_api_base(
            "https://OPENCODE.ai/inference/go/openai/v1"
        ));
    }

    #[test]
    fn does_not_match_pay_as_you_go_base() {
        assert!(!is_opencode_go_api_base("https://opencode.ai/inference"));
        assert!(!is_opencode_go_api_base("https://opencode.ai/inference/v1"));
        assert!(!is_opencode_go_api_base("https://api.openai.com/v1"));
    }

    #[test]
    fn headers_include_session_and_user_agent() {
        let headers = provider_headers(
            "https://opencode.ai/inference/go/openai/v1",
            Some("ses_123"),
        );
        assert_eq!(
            headers.get(USER_AGENT).unwrap().to_str().unwrap(),
            OPENCODE_GO_USER_AGENT
        );
        assert_eq!(
            headers
                .get(OPENCODE_GO_SESSION_HEADER)
                .unwrap()
                .to_str()
                .unwrap(),
            "ses_123"
        );
    }

    #[test]
    fn headers_use_fallback_session_when_missing() {
        let headers = provider_headers("https://opencode.ai/inference/go", None);
        assert!(headers
            .get(OPENCODE_GO_SESSION_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ses_"));
    }

    #[test]
    fn headers_empty_for_other_providers() {
        let headers = provider_headers("https://api.openai.com/v1", Some("ses_123"));
        assert!(headers.is_empty());
    }

    #[test]
    fn adapter_matches_models_and_api_base() {
        let adapter = OpenCodeGoProviderAdapter;
        assert!(adapter.matches_model("opencode-go/kimi-k3"));
        assert!(!adapter.matches_model("kimi-k3"));
        assert!(adapter.matches_api_base("https://opencode.ai/inference/go/openai/v1"));
        assert!(!adapter.matches_api_base("https://opencode.ai/inference/v1"));
    }

    #[test]
    fn error_hints_cover_missing_session_and_entitlement() {
        let missing = error_hint(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"type":"error","error":{"type":"MissingSessionID"}}"#,
        );
        assert!(missing.unwrap().contains("x-opencode-session"));

        let entitlement = error_hint(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":{"type":"EntitlementError","message":"OpenCode Go subscription required."}}"#,
        );
        assert!(entitlement.unwrap().to_lowercase().contains("subscription"));
    }
}
