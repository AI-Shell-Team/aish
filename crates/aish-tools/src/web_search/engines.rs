use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::Deserialize;

use super::provider::{
    build_search_client, epoch_now, read_response_json, status_error, transport_error, user_agent,
    SearchProvider,
};
use super::types::{SearchProviderError, SearchResponse, SearchResult};

// ---------------------------------------------------------------------------
// Brave Search API (BRAVE_API_KEY) — omp: GET /res/v1/web/search
// ---------------------------------------------------------------------------

pub struct BraveProvider {
    pub api_key: String,
}

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

#[derive(Debug, Deserialize)]
struct BraveApiResponse {
    #[serde(default)]
    web: Option<BraveWebSection>,
}

#[derive(Debug, Deserialize)]
struct BraveWebSection {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    age: Option<String>,
    #[serde(default)]
    page_age: Option<String>,
}

impl SearchProvider for BraveProvider {
    fn id(&self) -> &'static str {
        "brave"
    }

    fn is_available(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = SearchResponse> + Send + 'a>> {
        Box::pin(async move {
            if self.api_key.trim().is_empty() {
                return SearchResponse::failed("brave", SearchProviderError::Auth);
            }
            let client = match build_search_client(user_agent()) {
                Ok(client) => client,
                Err(err) => return SearchResponse::failed("brave", err),
            };
            let response = match client
                .get(BRAVE_ENDPOINT)
                .query(&[("q", query), ("count", &limit.to_string())])
                .header("X-Subscription-Token", &self.api_key)
                .header("Accept", "application/json")
                .timeout(timeout)
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) => return SearchResponse::failed("brave", transport_error(&err)),
            };
            let status = response.status();
            if !status.is_success() {
                return SearchResponse::failed("brave", status_error(status));
            }
            let payload: BraveApiResponse = match read_response_json(response).await {
                Ok(payload) => payload,
                Err(err) => return SearchResponse::failed("brave", err),
            };

            let fetched_at = epoch_now();
            let results = payload
                .web
                .map(|web| web.results)
                .unwrap_or_default()
                .into_iter()
                .take(limit)
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: (!r.description.is_empty()).then_some(r.description),
                    published: r.page_age.or(r.age),
                    fetched_at,
                })
                .collect::<Vec<_>>();

            if results.is_empty() {
                return SearchResponse::failed("brave", SearchProviderError::NoResults);
            }
            SearchResponse::ok("brave", results)
        })
    }
}

// ---------------------------------------------------------------------------
// Tavily (TAVILY_API_KEY) — omp: POST /search
// ---------------------------------------------------------------------------

pub struct TavilyProvider {
    pub api_key: String,
}

const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

#[derive(Debug, Deserialize)]
struct TavilyApiResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    published_date: Option<String>,
}

impl SearchProvider for TavilyProvider {
    fn id(&self) -> &'static str {
        "tavily"
    }

    fn is_available(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = SearchResponse> + Send + 'a>> {
        Box::pin(async move {
            if self.api_key.trim().is_empty() {
                return SearchResponse::failed("tavily", SearchProviderError::Auth);
            }
            let client = match build_search_client(user_agent()) {
                Ok(client) => client,
                Err(err) => return SearchResponse::failed("tavily", err),
            };
            let response = match client
                .post(TAVILY_ENDPOINT)
                .json(&serde_json::json!({
                    "api_key": self.api_key,
                    "query": query,
                    "max_results": limit,
                }))
                .timeout(timeout)
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) => return SearchResponse::failed("tavily", transport_error(&err)),
            };
            let status = response.status();
            if !status.is_success() {
                return SearchResponse::failed("tavily", status_error(status));
            }
            let payload: TavilyApiResponse = match read_response_json(response).await {
                Ok(payload) => payload,
                Err(err) => return SearchResponse::failed("tavily", err),
            };

            let fetched_at = epoch_now();
            let results = payload
                .results
                .into_iter()
                .take(limit)
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: (!r.content.is_empty()).then_some(r.content),
                    published: r.published_date,
                    fetched_at,
                })
                .collect::<Vec<_>>();

            if results.is_empty() {
                return SearchResponse::failed("tavily", SearchProviderError::NoResults);
            }
            SearchResponse::ok("tavily", results)
        })
    }
}

// ---------------------------------------------------------------------------
// SearXNG (self-hosted, SEARXNG_ENDPOINT) — omp: GET /search?format=json
// ---------------------------------------------------------------------------

pub struct SearxngProvider {
    pub endpoint: String,
}

#[derive(Debug, Deserialize)]
struct SearxngApiResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Debug, Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(rename = "publishedDate", default)]
    published_date: Option<String>,
}

impl SearxngProvider {
    fn normalized_endpoint(&self) -> Option<String> {
        let trimmed = self.endpoint.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

impl SearchProvider for SearxngProvider {
    fn id(&self) -> &'static str {
        "searxng"
    }

    fn is_available(&self) -> bool {
        self.normalized_endpoint().is_some()
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = SearchResponse> + Send + 'a>> {
        Box::pin(async move {
            let Some(endpoint) = self.normalized_endpoint() else {
                return SearchResponse::failed("searxng", SearchProviderError::Auth);
            };
            let client = match build_search_client(user_agent()) {
                Ok(client) => client,
                Err(err) => return SearchResponse::failed("searxng", err),
            };
            let url = format!("{endpoint}/search");
            let response = match client
                .get(&url)
                .query(&[("format", "json"), ("q", query)])
                .timeout(timeout)
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) => return SearchResponse::failed("searxng", transport_error(&err)),
            };
            let status = response.status();
            if !status.is_success() {
                return SearchResponse::failed("searxng", status_error(status));
            }
            let payload: SearxngApiResponse = match read_response_json(response).await {
                Ok(payload) => payload,
                Err(err) => return SearchResponse::failed("searxng", err),
            };

            let fetched_at = epoch_now();
            let results = payload
                .results
                .into_iter()
                .take(limit)
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: (!r.content.is_empty()).then_some(r.content),
                    published: r.published_date,
                    fetched_at,
                })
                .collect::<Vec<_>>();

            if results.is_empty() {
                return SearchResponse::failed("searxng", SearchProviderError::NoResults);
            }
            SearchResponse::ok("searxng", results)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brave_requires_key() {
        let provider = BraveProvider {
            api_key: String::new(),
        };
        assert!(!provider.is_available());
    }

    #[test]
    fn tavily_requires_key() {
        let provider = TavilyProvider {
            api_key: "  ".to_string(),
        };
        assert!(!provider.is_available());
    }

    #[test]
    fn searxng_requires_endpoint() {
        let provider = SearxngProvider {
            endpoint: String::new(),
        };
        assert!(!provider.is_available());
        let configured = SearxngProvider {
            endpoint: "http://localhost:8080/".to_string(),
        };
        assert_eq!(
            configured.normalized_endpoint().as_deref(),
            Some("http://localhost:8080")
        );
    }
}
