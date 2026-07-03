//! Two-layer verification for the setup wizard.
//!
//! Layer 1: Basic connectivity check (sends a minimal chat completion request).
//! Layer 2: Tool support check (sends a chat completion request with tool definitions).

use aish_i18n::t_with_args;
use aish_llm::api::resolve_anthropic_messages_url;
use aish_llm::providers::codex::{load_codex_auth, probe_codex_oauth_connectivity};
use serde_json::json;
use std::collections::HashMap;
use tracing::debug;

fn verify_msg(key: &str, args: &[(&str, &str)]) -> String {
    let map: HashMap<String, String> = args
        .iter()
        .map(|(k, v)| (k.to_string(), (*v).to_string()))
        .collect();
    t_with_args(key, &map)
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Result of a basic connectivity check.
#[derive(Debug, Clone)]
pub struct ConnectivityResult {
    /// Whether the endpoint responded successfully.
    pub ok: bool,
    /// Error message if the check failed.
    pub error: Option<String>,
    /// Round-trip latency in milliseconds.
    pub latency_ms: Option<u64>,
}

/// Result of a tool-support check.
#[derive(Debug, Clone)]
pub struct ToolSupportResult {
    /// Whether the model appears to support tool/function calling.
    pub supports: bool,
    /// Error message if the check failed.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Default timeouts
// ---------------------------------------------------------------------------

/// Default timeout for connectivity checks (seconds).
pub const DEFAULT_CONNECTIVITY_TIMEOUT_S: u64 = 15;
/// Default timeout for tool-support checks (seconds).
pub const DEFAULT_TOOL_SUPPORT_TIMEOUT_S: u64 = 30;

// ---------------------------------------------------------------------------
// Provider-aware verification
// ---------------------------------------------------------------------------

/// Route connectivity checks to the correct API dialect.
pub fn check_connectivity_for_provider(
    provider_key: &str,
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
    codex_auth_path: Option<&std::path::Path>,
) -> ConnectivityResult {
    match provider_key {
        "anthropic" => check_anthropic_connectivity(api_base, api_key, model, timeout_s),
        "openai-codex" if api_key.trim().is_empty() => {
            check_codex_connectivity(codex_auth_path, api_base, model, timeout_s)
        }
        "openai-codex" => check_openai_responses_connectivity(api_base, api_key, model, timeout_s),
        _ => check_connectivity(api_base, api_key, model, timeout_s),
    }
}

/// Route tool-support checks to the correct API dialect.
pub fn check_tool_support_for_provider(
    provider_key: &str,
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
    codex_auth_path: Option<&std::path::Path>,
) -> ToolSupportResult {
    match provider_key {
        "anthropic" => check_anthropic_tool_support(api_base, api_key, model, timeout_s),
        "openai-codex" if api_key.trim().is_empty() => {
            check_codex_tool_support(codex_auth_path, api_base, model, timeout_s)
        }
        "openai-codex" => check_openai_responses_tool_support(api_base, api_key, model, timeout_s),
        _ => check_tool_support(api_base, api_key, model, timeout_s),
    }
}

pub fn check_anthropic_connectivity(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
) -> ConnectivityResult {
    let url = resolve_anthropic_messages_url(api_base);
    debug!("Anthropic connectivity check: POST {}", url);

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ConnectivityResult {
                ok: false,
                error: Some(verify_msg(
                    "cli.setup.verify_http_client_build_failed",
                    &[("detail", &e.to_string())],
                )),
                latency_ms: None,
            };
        }
    };

    let body = json!({
        "model": model,
        "max_tokens": 16,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
    });

    let start = std::time::Instant::now();
    let response = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send();
    let elapsed = start.elapsed().as_millis() as u64;

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ConnectivityResult {
                    ok: true,
                    error: None,
                    latency_ms: Some(elapsed),
                }
            } else {
                let body_text = resp.text().unwrap_or_default();
                ConnectivityResult {
                    ok: false,
                    error: Some(verify_msg(
                        "cli.setup.verify_http_error",
                        &[
                            ("status", &status.as_u16().to_string()),
                            ("url", &url),
                            ("detail", &body_text),
                        ],
                    )),
                    latency_ms: Some(elapsed),
                }
            }
        }
        Err(e) => ConnectivityResult {
            ok: false,
            error: Some(verify_msg(
                "cli.setup.verify_request_failed",
                &[("detail", &e.to_string())],
            )),
            latency_ms: Some(elapsed),
        },
    }
}

pub fn check_anthropic_tool_support(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
) -> ToolSupportResult {
    let url = resolve_anthropic_messages_url(api_base);
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ToolSupportResult {
                supports: false,
                error: Some(verify_msg(
                    "cli.setup.verify_http_client_build_failed",
                    &[("detail", &e.to_string())],
                )),
            };
        }
    };

    let body = json!({
        "model": model,
        "max_tokens": 16,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
        "tools": [{
            "name": "ping",
            "description": "ping",
            "input_schema": {"type": "object", "properties": {}},
        }],
        "tool_choice": {"type": "auto"},
    });

    match client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
    {
        Ok(resp) => ToolSupportResult {
            supports: resp.status().is_success(),
            error: if resp.status().is_success() {
                None
            } else {
                Some(verify_msg(
                    "cli.setup.verify_tool_http_error",
                    &[
                        ("status", &resp.status().as_u16().to_string()),
                        ("detail", &resp.text().unwrap_or_default()),
                    ],
                ))
            },
        },
        Err(e) => ToolSupportResult {
            supports: false,
            error: Some(verify_msg(
                "cli.setup.verify_tool_request_failed",
                &[("detail", &e.to_string())],
            )),
        },
    }
}

pub fn check_codex_connectivity(
    codex_auth_path: Option<&std::path::Path>,
    api_base: &str,
    model: &str,
    timeout_s: u64,
) -> ConnectivityResult {
    match load_codex_auth(codex_auth_path) {
        Ok(auth) if auth.access_token.is_empty() => ConnectivityResult {
            ok: false,
            error: Some(verify_msg(
                "cli.setup.verify_request_failed",
                &[("detail", "Codex auth token is empty")],
            )),
            latency_ms: None,
        },
        Ok(_) => match probe_codex_oauth_connectivity(
            codex_auth_path,
            Some(api_base),
            model,
            timeout_s,
            false,
        ) {
            Ok(latency_ms) => ConnectivityResult {
                ok: true,
                error: None,
                latency_ms: Some(latency_ms),
            },
            Err(detail) => ConnectivityResult {
                ok: false,
                error: Some(verify_msg(
                    "cli.setup.verify_request_failed",
                    &[("detail", &detail)],
                )),
                latency_ms: None,
            },
        },
        Err(e) => ConnectivityResult {
            ok: false,
            error: Some(verify_msg(
                "cli.setup.verify_request_failed",
                &[("detail", &e.to_string())],
            )),
            latency_ms: None,
        },
    }
}

pub fn check_openai_responses_connectivity(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
) -> ConnectivityResult {
    let url = format!("{}/responses", api_base.trim_end_matches('/'));
    debug!("OpenAI Responses connectivity check: POST {}", url);

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ConnectivityResult {
                ok: false,
                error: Some(verify_msg(
                    "cli.setup.verify_http_client_build_failed",
                    &[("detail", &e.to_string())],
                )),
                latency_ms: None,
            };
        }
    };

    let body = json!({
        "model": model,
        "instructions": "",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Hi"}],
        }],
        "max_output_tokens": 16,
        "store": false,
        "stream": false,
    });

    let start = std::time::Instant::now();
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send();
    let elapsed = start.elapsed().as_millis() as u64;

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ConnectivityResult {
                    ok: true,
                    error: None,
                    latency_ms: Some(elapsed),
                }
            } else {
                let body_text = resp.text().unwrap_or_default();
                ConnectivityResult {
                    ok: false,
                    error: Some(verify_msg(
                        "cli.setup.verify_http_error",
                        &[
                            ("status", &status.as_u16().to_string()),
                            ("url", &url),
                            ("detail", &body_text),
                        ],
                    )),
                    latency_ms: Some(elapsed),
                }
            }
        }
        Err(e) => ConnectivityResult {
            ok: false,
            error: Some(verify_msg(
                "cli.setup.verify_request_failed",
                &[("detail", &e.to_string())],
            )),
            latency_ms: Some(elapsed),
        },
    }
}

pub fn check_openai_responses_tool_support(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
) -> ToolSupportResult {
    let url = format!("{}/responses", api_base.trim_end_matches('/'));
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ToolSupportResult {
                supports: false,
                error: Some(verify_msg(
                    "cli.setup.verify_http_client_build_failed",
                    &[("detail", &e.to_string())],
                )),
            };
        }
    };

    let body = json!({
        "model": model,
        "instructions": "",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Hi"}],
        }],
        "tools": [{
            "type": "function",
            "name": "ping",
            "description": "ping",
            "parameters": {"type": "object", "properties": {}},
        }],
        "tool_choice": "auto",
        "max_output_tokens": 16,
        "store": false,
        "stream": false,
    });

    match client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
    {
        Ok(resp) => ToolSupportResult {
            supports: resp.status().is_success(),
            error: if resp.status().is_success() {
                None
            } else {
                Some(verify_msg(
                    "cli.setup.verify_tool_http_error",
                    &[
                        ("status", &resp.status().as_u16().to_string()),
                        ("detail", &resp.text().unwrap_or_default()),
                    ],
                ))
            },
        },
        Err(e) => ToolSupportResult {
            supports: false,
            error: Some(verify_msg(
                "cli.setup.verify_tool_request_failed",
                &[("detail", &e.to_string())],
            )),
        },
    }
}

pub fn check_codex_tool_support(
    codex_auth_path: Option<&std::path::Path>,
    api_base: &str,
    model: &str,
    timeout_s: u64,
) -> ToolSupportResult {
    match load_codex_auth(codex_auth_path) {
        Ok(auth) if auth.access_token.is_empty() => ToolSupportResult {
            supports: false,
            error: Some("Codex auth token is empty".to_string()),
        },
        Ok(_) => match probe_codex_oauth_connectivity(
            codex_auth_path,
            Some(api_base),
            model,
            timeout_s,
            true,
        ) {
            Ok(_) => ToolSupportResult {
                supports: true,
                error: None,
            },
            Err(detail) => ToolSupportResult {
                supports: false,
                error: Some(detail),
            },
        },
        Err(e) => ToolSupportResult {
            supports: false,
            error: Some(e.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Verification functions
// ---------------------------------------------------------------------------

/// Check basic connectivity to the chat completions endpoint.
///
/// Sends a minimal `POST {api_base}/chat/completions` with a single "Hi"
/// user message and `max_tokens: 5`.  Returns latency on success or an
/// explanatory error on failure.
pub fn check_connectivity(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
) -> ConnectivityResult {
    let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
    debug!("Connectivity check: POST {}", url);

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ConnectivityResult {
                ok: false,
                error: Some(verify_msg(
                    "cli.setup.verify_http_client_build_failed",
                    &[("detail", &e.to_string())],
                )),
                latency_ms: None,
            }
        }
    };

    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Hi"}],
        "max_tokens": 5,
    });

    let start = std::time::Instant::now();
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send();

    let elapsed = start.elapsed().as_millis() as u64;

    match response {
        Ok(resp) => {
            let status = resp.status();
            debug!("Connectivity response status: {} ({}ms)", status, elapsed);

            if status.is_success() {
                ConnectivityResult {
                    ok: true,
                    error: None,
                    latency_ms: Some(elapsed),
                }
            } else {
                let status_code = status.as_u16();
                // Try to extract error body for a better message.
                let body_text = resp.text().unwrap_or_default();
                let detail = if body_text.len() > 300 {
                    let mut end = 300;
                    while end > 0 && !body_text.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...", &body_text[..end])
                } else {
                    body_text
                };
                ConnectivityResult {
                    ok: false,
                    error: Some(verify_msg(
                        "cli.setup.verify_http_error",
                        &[
                            ("status", &status_code.to_string()),
                            ("url", &url),
                            ("detail", &detail),
                        ],
                    )),
                    latency_ms: Some(elapsed),
                }
            }
        }
        Err(e) => {
            debug!("Connectivity error ({}ms): {}", elapsed, e);
            let msg = if e.is_timeout() {
                verify_msg(
                    "cli.setup.verify_connect_timeout",
                    &[("timeout", &timeout_s.to_string()), ("url", &url)],
                )
            } else if e.is_connect() {
                verify_msg("cli.setup.verify_connect_refused", &[("url", &url)])
            } else {
                verify_msg(
                    "cli.setup.verify_request_failed",
                    &[("detail", &e.to_string())],
                )
            };
            ConnectivityResult {
                ok: false,
                error: Some(msg),
                latency_ms: Some(elapsed),
            }
        }
    }
}

/// Check whether the model supports tool/function calling.
///
/// Sends a chat completion request that includes a trivial `ping` tool
/// definition with `tool_choice: "auto"`.  If the response is successful
/// we consider tool support to be present.  A 400-level error or an
/// error message mentioning "tools" is interpreted as lack of support.
pub fn check_tool_support(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout_s: u64,
) -> ToolSupportResult {
    let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
    debug!("Tool-support check: POST {}", url);

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ToolSupportResult {
                supports: false,
                error: Some(verify_msg(
                    "cli.setup.verify_http_client_build_failed",
                    &[("detail", &e.to_string())],
                )),
            }
        }
    };

    let tool_def = json!({
        "type": "function",
        "function": {
            "name": "ping",
            "description": "ping",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    });

    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Hi"}],
        "max_tokens": 5,
        "tools": [tool_def],
        "tool_choice": "auto",
    });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send();

    match response {
        Ok(resp) => {
            let status = resp.status();
            debug!("Tool-support response status: {}", status);

            if status.is_success() {
                ToolSupportResult {
                    supports: true,
                    error: None,
                }
            } else {
                let status_code = status.as_u16();
                let body_text = resp.text().unwrap_or_default();
                let detail = if body_text.len() > 300 {
                    let mut end = 300;
                    while end > 0 && !body_text.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...", &body_text[..end])
                } else {
                    body_text
                };
                // A client error (4xx) likely means the endpoint rejected
                // the tools parameter.
                let supports = false;
                ToolSupportResult {
                    supports,
                    error: Some(verify_msg(
                        "cli.setup.verify_tool_http_error",
                        &[("status", &status_code.to_string()), ("detail", &detail)],
                    )),
                }
            }
        }
        Err(e) => {
            debug!("Tool-support check error: {}", e);
            let msg = if e.is_timeout() {
                verify_msg(
                    "cli.setup.verify_tool_timeout",
                    &[("timeout", &timeout_s.to_string())],
                )
            } else {
                verify_msg(
                    "cli.setup.verify_tool_request_failed",
                    &[("detail", &e.to_string())],
                )
            };
            ToolSupportResult {
                supports: false,
                error: Some(msg),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connectivity_result_defaults() {
        let result = ConnectivityResult {
            ok: true,
            error: None,
            latency_ms: Some(42),
        };
        assert!(result.ok);
        assert!(result.error.is_none());
        assert_eq!(result.latency_ms, Some(42));
    }

    #[test]
    fn test_tool_support_result_defaults() {
        let result = ToolSupportResult {
            supports: true,
            error: None,
        };
        assert!(result.supports);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_check_connectivity_bad_url() {
        aish_i18n::set_locale("en-US");
        // Use a URL that should not be reachable.
        let result = check_connectivity("http://127.0.0.1:1", "test-key", "test-model", 2);
        assert!(!result.ok);
        assert!(result.error.is_some());
        // Should contain something about connection failure.
        let err = result.error.unwrap();
        assert!(
            err.contains("Connection refused")
                || err.contains("unreachable")
                || err.contains("connect")
                || err.contains("error"),
            "Unexpected error message: {}",
            err
        );
    }

    #[test]
    fn test_check_tool_support_bad_url() {
        let result = check_tool_support("http://127.0.0.1:1", "test-key", "test-model", 2);
        assert!(!result.supports);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_connectivity_timeout_is_respected() {
        // A very short timeout to a non-routable address should fail quickly.
        let start = std::time::Instant::now();
        let result = check_connectivity(
            "http://192.0.2.1", // TEST-NET, should be unreachable
            "test-key",
            "test-model",
            1,
        );
        let elapsed = start.elapsed();
        assert!(!result.ok);
        // Should timeout within roughly 2x the configured timeout (allowing
        // for overhead).
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "Took too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_default_timeouts() {
        assert_eq!(DEFAULT_CONNECTIVITY_TIMEOUT_S, 15);
        assert_eq!(DEFAULT_TOOL_SUPPORT_TIMEOUT_S, 30);
    }
}
