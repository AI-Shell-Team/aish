use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::Deserialize;

use super::provider::{
    build_search_client, epoch_now, read_response_body, status_error, transport_error, user_agent,
    SearchProvider,
};
use super::types::{SearchProviderError, SearchResponse, SearchResult};

const PROVIDER_ID: &str = "duckduckgo";
/// omp parity: DuckDuckGo serves a bot-detection challenge (HTTP 200/202 with
/// an anomaly-modal body) when it throttles shared-egress IPs.
const ANOMALY_MARKER: &str = "anomaly-modal";
const HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// Credential-free DuckDuckGo HTML frontend scraper. Always available; the
/// last resort of the automatic chain before failing (omp parity).
pub struct DuckDuckGoProvider;

#[derive(Debug, Deserialize)]
struct DdgApiResponse {
    #[serde(default)]
    results: Vec<DdgApiResult>,
}

#[derive(Debug, Deserialize)]
struct DdgApiResult {
    #[serde(default)]
    text: String,
    #[serde(default)]
    first_url: String,
    #[serde(default)]
    title: String,
}

impl SearchProvider for DuckDuckGoProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn is_available(&self) -> bool {
        true
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = SearchResponse> + Send + 'a>> {
        Box::pin(async move {
            let client = match build_search_client(user_agent()) {
                Ok(client) => client,
                Err(err) => return SearchResponse::failed(PROVIDER_ID, err),
            };
            let response = match client
                .post(HTML_ENDPOINT)
                .header("Accept", "text/html")
                .timeout(timeout)
                .form(&[("q", query), ("kl", "us-en")])
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) => return SearchResponse::failed(PROVIDER_ID, transport_error(&err)),
            };

            let status = response.status();
            if !status.is_success() {
                return SearchResponse::failed(PROVIDER_ID, status_error(status));
            }
            // Bound the body read: search-result pages are far smaller than
            // this, and an unbounded read on a hostile/defective origin is a
            // DoS vector (WebFetch applies the same 10 MB defense).
            let body = match read_response_body(response).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(err) => return SearchResponse::failed(PROVIDER_ID, err),
            };

            if body.contains(ANOMALY_MARKER) {
                return SearchResponse::failed(PROVIDER_ID, SearchProviderError::RateLimited);
            }

            // Parse the embedded JS result array (no-JS HTML keeps one JSON
            // blob in a script tag); fall back to HTML anchor scraping when
            // the blob is absent (challenge interstitials, layout changes).
            let mut entries: Vec<(String, String, Option<String>)> = parse_api_blob(&body)
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, url, _)| !url.is_empty())
                .collect();
            if entries.is_empty() {
                entries = parse_html_anchors(&body);
            }
            let results = entries
                .into_iter()
                .take(limit)
                .map(|(title, url, snippet)| SearchResult {
                    title,
                    url,
                    snippet,
                    published: None,
                    fetched_at: epoch_now(),
                })
                .collect::<Vec<_>>();

            if results.is_empty() {
                return SearchResponse::failed(PROVIDER_ID, SearchProviderError::NoResults);
            }
            SearchResponse::ok(PROVIDER_ID, results)
        })
    }
}

/// Try to parse the JSON payload DuckDuckGo embeds for progressive
/// enhancement. Returns `None` when the page has no such blob.
fn parse_api_blob(body: &str) -> Option<Vec<(String, String, Option<String>)>> {
    let start = body.find('[')?;
    let end = body.rfind(']')? + 1;
    if end <= start {
        return None;
    }
    let slice = &body[start..end];
    let raw_results: Vec<DdgApiResult> = match serde_json::from_str::<DdgApiResponse>(slice) {
        Ok(payload) => payload.results,
        Err(_) => serde_json::from_str(slice).ok()?,
    };
    Some(
        raw_results
            .into_iter()
            .map(|r| {
                (
                    if r.title.is_empty() {
                        r.text.clone()
                    } else {
                        r.title
                    },
                    unwrap_redirect(&r.first_url),
                    Some(r.text),
                )
            })
            .collect(),
    )
}

/// Fallback: scrape plain anchors from the no-JS HTML result list.
fn parse_html_anchors(body: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find("result__a") {
        let segment = &rest[pos..];
        let Some(href_start) = segment.find("href=\"") else {
            break;
        };
        let after_href = &segment[href_start + 6..];
        let Some(href_end) = after_href.find('"') else {
            break;
        };
        let url = unwrap_redirect(&after_href[..href_end]);
        let title = extract_anchor_text(after_href);
        if !url.is_empty() && !title.is_empty() {
            out.push((title, url, None));
        }
        rest = &segment[10.min(segment.len())..];
        if out.len() >= 40 {
            break;
        }
    }
    out
}

/// Unwrap DuckDuckGo redirect wrappers (`//duckduckgo.com/l/?uddg=…`).
fn unwrap_redirect(url: &str) -> String {
    let Some(pos) = url.find("uddg=") else {
        return url.trim_start_matches("//").to_string();
    };
    let encoded = &url[pos + 5..];
    let encoded = encoded.split('&').next().unwrap_or(encoded);
    percent_decode(encoded)
}

fn extract_anchor_text(fragment: &str) -> String {
    let Some(start) = fragment.find('>') else {
        return String::new();
    };
    let after = &fragment[start + 1..];
    let Some(end) = after.find("</a>") else {
        return String::new();
    };
    decode_entities(&after[..end])
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_ddg_redirect_urls() {
        assert_eq!(
            unwrap_redirect("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc"),
            "https://example.com/page"
        );
        assert_eq!(
            unwrap_redirect("https://direct.example.com"),
            "https://direct.example.com"
        );
    }

    #[test]
    fn parses_embedded_api_blob() {
        let body = r#"foo <script>[{"text":"snip","first_url":"//duckduckgo.com/l/?uddg=https%3A%2F%2Fe.com","title":"Title"}]</script>"#;
        let parsed = parse_api_blob(body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "Title");
        assert_eq!(parsed[0].1, "https://e.com");
        assert_eq!(parsed[0].2.as_deref(), Some("snip"));
    }

    #[test]
    fn scrapes_html_anchors() {
        let body = r#"<a class="result__a" href="https://example.com/a">One &amp; Two</a>
<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fb.com">B</a>"#;
        let parsed = parse_html_anchors(body);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "One & Two");
        assert_eq!(parsed[0].1, "https://example.com/a");
        assert_eq!(parsed[1].1, "https://b.com");
    }
}
