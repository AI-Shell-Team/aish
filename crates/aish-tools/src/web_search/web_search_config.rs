/// Provider credentials and knobs for the WebSearch tool, resolved from
/// environment variables (omp parity: search credentials are env-first).
///
/// Config-file wiring can be added later; env vars keep the tool usable
/// without a schema migration.
#[derive(Debug, Clone)]
pub struct WebSearchConfig {
    pub brave_api_key: String,
    pub tavily_api_key: String,
    pub searxng_endpoint: String,
    /// Per-provider transport timeout in seconds (clamped to 1..=300).
    pub timeout_secs: u64,
    /// Default result count when the caller omits `limit`.
    pub default_limit: usize,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            brave_api_key: String::new(),
            tavily_api_key: String::new(),
            searxng_endpoint: String::new(),
            timeout_secs: 60,
            default_limit: 10,
        }
    }
}

impl WebSearchConfig {
    /// Resolve from the environment; empty values keep the provider out of
    /// the automatic chain.
    pub fn from_env() -> Self {
        Self {
            brave_api_key: std::env::var("BRAVE_API_KEY").unwrap_or_default(),
            tavily_api_key: std::env::var("TAVILY_API_KEY").unwrap_or_default(),
            searxng_endpoint: std::env::var("SEARXNG_ENDPOINT").unwrap_or_default(),
            timeout_secs: std::env::var("AISH_WEB_SEARCH_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(60),
            default_limit: 10,
        }
    }
}
