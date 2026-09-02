use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::time::{Duration, Instant};

pub struct ApiConnectivityChecker;

impl ApiConnectivityChecker {
    pub fn new() -> Self {
        Self
    }

    fn resolve_config(&self) -> (Option<String>, String) {
        let config = aish_config::ConfigLoader::load(None).unwrap_or_default();
        let api_key = if let Ok(key) = std::env::var("AISH_API_KEY") {
            Some(key)
        } else if !config.api_key.is_empty() {
            Some(config.api_key)
        } else {
            None
        };
        let api_base = if config.api_base.is_empty() {
            "https://api.openai.com/v1".to_string()
        } else {
            config.api_base
        };
        (api_key, api_base)
    }

    fn check_connectivity(&self) -> CheckItem {
        let (api_key, api_base) = self.resolve_config();
        let api_key = match api_key {
            Some(k) => k,
            None => {
                return CheckItem::warn("api", "No API key configured")
                    .hint("Run 'aish setup' to configure");
            }
        };

        let url = format!("{}/models", api_base.trim_end_matches('/'));

        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return CheckItem::fail("api", format!("Failed to create HTTP client: {}", e));
            }
        };

        let req = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key));

        let start = Instant::now();
        match req.send() {
            Ok(resp) => {
                let latency = start.elapsed().as_millis();
                let status = resp.status();
                if status.is_success() {
                    CheckItem::pass(
                        "api",
                        format!("API connectivity: OK ({}ms, {})", latency, api_base),
                    )
                } else if status.as_u16() == 401 || status.as_u16() == 403 {
                    // Auth failed but the server IS reachable — connectivity OK.
                    CheckItem::pass(
                        "api",
                        format!(
                            "API reachable ({}ms, {}); auth returned {} — verify key if AI calls fail",
                            latency, api_base, status
                        ),
                    )
                } else if status.as_u16() == 404 {
                    // /models not implemented by this provider (common for
                    // OpenAI-compatible gateways). The server responded, so
                    // connectivity is fine; chat/completions may still work.
                    CheckItem::pass(
                        "api",
                        format!(
                            "API reachable ({}ms, {}); /models not implemented — AI calls use /chat/completions",
                            latency, api_base
                        ),
                    )
                } else if status.is_server_error() {
                    CheckItem::fail(
                        "api",
                        format!("API connectivity: server error (HTTP {})", status),
                    )
                    .hint("The API server may be temporarily unavailable")
                } else if status.is_client_error() {
                    // Other 4xx (400, 405, …): server is up, endpoint rejected
                    // the probe. Connectivity itself is fine.
                    CheckItem::pass(
                        "api",
                        format!(
                            "API reachable ({}ms, {}); probe returned {} — AI calls may still work",
                            latency, api_base, status
                        ),
                    )
                } else {
                    CheckItem::warn(
                        "api",
                        format!(
                            "API connectivity: unexpected status (HTTP {}, {}ms)",
                            status, latency
                        ),
                    )
                }
            }
            Err(e) => {
                let hint = if e.is_timeout() {
                    "Request timed out — check network connectivity"
                } else if e.is_connect() {
                    "Connection refused — check API base URL and network"
                } else {
                    "Check network connectivity and API base URL"
                };
                CheckItem::fail("api", format!("API connectivity: {} ({})", e, api_base)).hint(hint)
            }
        }
    }
}

impl Checker for ApiConnectivityChecker {
    fn name(&self) -> &str {
        "API Connectivity"
    }

    fn check(&self) -> Vec<CheckResult> {
        let item = self.check_connectivity();
        let status = item.status.clone();
        vec![CheckResult {
            checker: self.name().to_string(),
            items: vec![item],
            status,
        }]
    }

    fn fix(&self, _item: &CheckItem) -> FixResult {
        FixResult {
            success: false,
            message: "Cannot auto-fix connectivity issues".to_string(),
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
