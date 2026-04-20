//! Two-layer verification for the setup wizard.
//!
//! Layer 1: Basic connectivity check (sends a minimal chat completion request).
//! Layer 2: Tool support check (sends a chat completion request with tool definitions).

use serde_json::json;
use tracing::debug;

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
                error: Some(format!("Failed to build HTTP client: {}", e)),
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
                    format!("{}...", &body_text[..300])
                } else {
                    body_text
                };
                ConnectivityResult {
                    ok: false,
                    error: Some(format!("HTTP {} from {}: {}", status_code, url, detail)),
                    latency_ms: Some(elapsed),
                }
            }
        }
        Err(e) => {
            debug!("Connectivity error ({}ms): {}", elapsed, e);
            let msg = if e.is_timeout() {
                format!(
                    "Request timed out after {}s connecting to {}",
                    timeout_s, url
                )
            } else if e.is_connect() {
                format!("Connection refused or unreachable: {}", url)
            } else {
                format!("Request failed: {}", e)
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
                error: Some(format!("Failed to build HTTP client: {}", e)),
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
                    format!("{}...", &body_text[..300])
                } else {
                    body_text
                };
                // A client error (4xx) likely means the endpoint rejected
                // the tools parameter.
                let supports = false;
                ToolSupportResult {
                    supports,
                    error: Some(format!(
                        "HTTP {} — tool support not detected: {}",
                        status_code, detail
                    )),
                }
            }
        }
        Err(e) => {
            debug!("Tool-support check error: {}", e);
            let msg = if e.is_timeout() {
                format!(
                    "Request timed out after {}s during tool-support check",
                    timeout_s
                )
            } else {
                format!("Request failed during tool-support check: {}", e)
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
