use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::de::DeserializeOwned;

use super::types::{SearchProviderError, SearchResponse};

/// One search backend. Implementations translate an upstream payload into a
/// [`SearchResponse`]; the orchestrator (`WebSearchTool`) owns ordering,
/// fallback, and result capping.
///
/// Mirrors omp's provider interface: `search()` receives the already-parsed
/// query plus shared knobs and returns a per-provider response (never a
/// panic-prone error type), so one bad provider cannot break the chain.
pub trait SearchProvider: Send + Sync {
    /// Stable provider id used in error messages and `meta.provider`.
    fn id(&self) -> &'static str;

    /// Whether this provider can run right now (credentials present, etc.).
    /// Credential-free engines always return true.
    fn is_available(&self) -> bool;

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = SearchResponse> + Send + 'a>>;
}

/// Default per-provider result window shared by all adapters (omp parity).
pub const MAX_RESULT_COUNT: usize = 20;

pub fn clamp_result_count(requested: usize) -> usize {
    requested.clamp(1, MAX_RESULT_COUNT)
}

/// Current unix epoch seconds; shared by all providers for `fetched_at`.
pub(crate) fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Shared User-Agent for all search providers.
pub(crate) fn user_agent() -> &'static str {
    concat!("aish/", env!("CARGO_PKG_VERSION"), " WebSearch")
}

/// Shared reqwest client for all providers: browser-like UA, no proxy env
/// surprises, per-call timeouts applied at the request level.
pub(crate) fn build_search_client(
    user_agent: &str,
) -> Result<reqwest::Client, SearchProviderError> {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .build()
        .map_err(|e| SearchProviderError::Network(e.to_string()))
}

/// Map transport failures to the distinguishable error taxonomy.
pub(crate) fn transport_error(err: &reqwest::Error) -> SearchProviderError {
    if err.is_timeout() {
        SearchProviderError::Timeout
    } else if err.is_status() {
        match err.status() {
            Some(code) if code.as_u16() == 429 => SearchProviderError::RateLimited,
            Some(code) if code.as_u16() == 401 || code.as_u16() == 403 => SearchProviderError::Auth,
            _ => SearchProviderError::Network(err.to_string()),
        }
    } else {
        SearchProviderError::Network(err.to_string())
    }
}

/// Classify an HTTP status returned by an upstream search engine.
pub(crate) fn status_error(status: reqwest::StatusCode) -> SearchProviderError {
    match status.as_u16() {
        429 => SearchProviderError::RateLimited,
        401 | 403 => SearchProviderError::Auth,
        503 => SearchProviderError::RateLimited,
        _ => SearchProviderError::Network(format!("HTTP {}", status.as_u16())),
    }
}

/// Shared body size limit for all provider responses (2 MiB).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Read a response body incrementally, failing fast once it exceeds
/// [`MAX_BODY_BYTES`] instead of buffering an unbounded payload in memory.
pub(crate) async fn read_response_body(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, SearchProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| transport_error(&err))?
    {
        if chunk.len() > MAX_BODY_BYTES - body.len() {
            return Err(SearchProviderError::Network(
                "response body too large".to_string(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Read a bounded body and decode it as JSON (issue #566: bounded content).
pub(crate) async fn read_response_json<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, SearchProviderError> {
    let body = read_response_body(response).await?;
    serde_json::from_slice(&body).map_err(|err| SearchProviderError::Network(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_count_clamped_into_engine_window() {
        assert_eq!(clamp_result_count(0), 1);
        assert_eq!(clamp_result_count(5), 5);
        assert_eq!(clamp_result_count(500), MAX_RESULT_COUNT);
    }

    #[test]
    fn status_error_maps_rate_limit_and_auth() {
        assert_eq!(
            status_error(reqwest::StatusCode::TOO_MANY_REQUESTS),
            SearchProviderError::RateLimited
        );
        assert_eq!(
            status_error(reqwest::StatusCode::UNAUTHORIZED),
            SearchProviderError::Auth
        );
    }
}
