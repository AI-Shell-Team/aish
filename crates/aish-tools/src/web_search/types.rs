use std::time::Duration;

/// Unified result of one web search across all providers.
///
/// Mirrors omp's `SearchResponse`: the tool orchestrator owns the fallback
/// chain, providers only translate their upstream payload into this shape.
#[derive(Debug, Clone)]
pub struct SearchResponse {
    /// Provider that produced this response (e.g. "duckduckgo").
    pub provider: &'static str,
    /// Result entries. Empty when the provider found nothing.
    pub results: Vec<SearchResult>,
    /// Provider error description when the attempt failed.
    pub error: Option<SearchProviderError>,
    /// Earlier providers that failed before this response won. Empty when
    /// the first provider succeeded or the whole chain failed (issue #566
    /// criterion 4: partial failures must be observable).
    pub failures: Vec<(&'static str, SearchProviderError)>,
}

impl SearchResponse {
    pub fn ok(provider: &'static str, results: Vec<SearchResult>) -> Self {
        Self {
            provider,
            results,
            error: None,
            failures: Vec::new(),
        }
    }

    pub fn failed(provider: &'static str, error: SearchProviderError) -> Self {
        Self {
            provider,
            results: Vec::new(),
            error: Some(error),
            failures: Vec::new(),
        }
    }

    /// omp parity: a response without renderable content is treated as a
    /// failure so the fallback chain advances to the next provider.
    pub fn is_renderable(&self) -> bool {
        self.error.is_none() && !self.results.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    /// Search-engine snippet. May be empty for engines that do not return one.
    pub snippet: Option<String>,
    /// Publication time when the engine provides one (RFC 3339 or partial date).
    pub published: Option<String>,
    /// Epoch seconds when this entry was fetched from the engine.
    pub fetched_at: i64,
}

/// Distinguishable failure states (issue #566 acceptance criterion 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchProviderError {
    /// Engine returned no results for the query.
    NoResults,
    /// Upstream rate limited or bot-challenged us.
    RateLimited,
    /// Transport timed out before a response arrived.
    Timeout,
    /// Credentials missing or rejected.
    Auth,
    /// Any other network/protocol failure.
    Network(String),
}

impl SearchProviderError {
    pub fn message(&self) -> String {
        match self {
            Self::NoResults => aish_i18n::t("tools.web_search.err_no_results").to_string(),
            Self::RateLimited => aish_i18n::t("tools.web_search.err_rate_limited").to_string(),
            Self::Timeout => aish_i18n::t("tools.web_search.err_timeout").to_string(),
            Self::Auth => aish_i18n::t("tools.web_search.err_auth").to_string(),
            Self::Network(detail) => aish_i18n::t_with_args(
                "tools.web_search.err_network",
                &std::collections::HashMap::from([("error".to_string(), detail.clone())]),
            ),
        }
    }

    /// Stable machine-readable status for `ToolResult.meta.status`.
    pub fn status_code(&self) -> &'static str {
        match self {
            Self::NoResults => "no_results",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Auth => "auth",
            Self::Network(_) => "network",
        }
    }
}

/// Per-provider hard transport timeout before the fallback chain advances
/// (omp `providers.webSearchTimeoutSeconds` parity; default 60s, cap 300s).
pub const DEFAULT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(300);

pub fn clamp_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        DEFAULT_PROVIDER_TIMEOUT
    } else if timeout > MAX_PROVIDER_TIMEOUT {
        MAX_PROVIDER_TIMEOUT
    } else {
        timeout
    }
}

/// Canonical key for deduplicating results: lowercase host without `www.`,
/// no trailing slash, fragment removed, query preserved (omp `public.ts`
/// parity). Two results pointing at the same page collapse into one.
pub fn canonical_url_key(url: &str) -> String {
    let trimmed = url.trim();
    // Drop the scheme (if any); keep host + path + query, drop fragment.
    let after_scheme = match trimmed.find("://") {
        Some(idx) => &trimmed[idx + 3..],
        None => trimmed,
    };
    // Lowercase only the host so case-sensitive paths and queries stay
    // distinct; strip fragment, www. prefix, and trailing slash.
    let no_frag = after_scheme.split('#').next().unwrap_or(after_scheme);
    let (host, path_query) = match no_frag.find('/') {
        Some(idx) => (no_frag[..idx].to_lowercase(), no_frag[idx..].to_string()),
        None => (no_frag.to_lowercase(), String::new()),
    };
    let host_no_www = host
        .strip_prefix("www.")
        .map(str::to_string)
        .unwrap_or(host);
    let path = path_query.trim_end_matches('/');
    format!("{host_no_www}{path}")
}

/// Deduplicate `results` in place, keeping the first occurrence of each
/// canonical URL and preserving order.
pub fn dedupe_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = std::collections::HashSet::new();
    results
        .into_iter()
        .filter(|r| {
            let key = canonical_url_key(&r.url);
            // Reserve-style insert; keep the first occurrence.
            seen.insert(key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_key_normalizes_scheme_www_trailing_slash_fragment() {
        assert_eq!(
            canonical_url_key("https://WWW.Example.com/page/"),
            canonical_url_key("http://example.com/page#section")
        );
        assert_eq!(
            canonical_url_key("https://example.com/a?x=1"),
            canonical_url_key("https://example.com/a?x=1#frag")
        );
        // Query strings differ -> distinct pages.
        assert_ne!(
            canonical_url_key("https://e.com/a?x=1"),
            canonical_url_key("https://e.com/a?x=2")
        );
        // Case-sensitive paths stay distinct (only host is lowercased).
        assert_ne!(
            canonical_url_key("https://github.com/Foo/Bar"),
            canonical_url_key("https://github.com/foo/bar")
        );
    }

    #[test]
    fn dedupe_keeps_first_occurrence_in_order() {
        let mk = |title: &str, url: &str| SearchResult {
            title: title.into(),
            url: url.into(),
            snippet: None,
            published: None,
            fetched_at: 0,
        };
        let deduped = dedupe_results(vec![
            mk("first", "https://a.com/x"),
            mk("b", "https://b.com/"),
            mk("dup-slash", "https://a.com/x/"), // same page as first
            mk("dup-www", "https://www.a.com/x"), // same page as first
            mk("other", "https://a.com/y"),
        ]);
        assert_eq!(deduped.len(), 3);
        assert_eq!(deduped[0].title, "first");
        assert_eq!(deduped[1].title, "b");
        assert_eq!(deduped[2].title, "other");
        assert!(dedupe_results(vec![]).is_empty());
    }

    #[test]
    fn renderable_requires_results_and_no_error() {
        let entry = SearchResult {
            title: "t".into(),
            url: "https://example.com".into(),
            snippet: None,
            published: None,
            fetched_at: 0,
        };
        assert!(!SearchResponse::ok("x", vec![]).is_renderable());
        assert!(SearchResponse::ok("x", vec![entry]).is_renderable());
        assert!(!SearchResponse::failed("x", SearchProviderError::NoResults).is_renderable());
    }

    #[test]
    fn timeout_clamping_matches_omp_semantics() {
        assert_eq!(clamp_timeout(Duration::ZERO), DEFAULT_PROVIDER_TIMEOUT);
        assert_eq!(
            clamp_timeout(Duration::from_secs(10_000)),
            MAX_PROVIDER_TIMEOUT
        );
        assert_eq!(
            clamp_timeout(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }
}
