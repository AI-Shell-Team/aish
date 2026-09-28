use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use super::provider::{
    build_search_client, epoch_now, status_error, transport_error, user_agent, SearchProvider,
};
use super::types::{SearchProviderError, SearchResponse, SearchResult};

const PROVIDER_ID: &str = "bing";
const ENDPOINT: &str = "https://www.bing.com/search";
/// Full browser UA string: Bing rejects non-browser-shaped clients less
/// aggressively with one (omp parity: shared Chromium headers for scrapers).
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// Credential-free Bing SERP scraper. Placed ahead of DuckDuckGo in the
/// automatic chain: Bing is reachable from networks that block DDG, and its
/// `b_algo` result markup is stable enough to scrape without a headless
/// browser.
pub struct BingProvider;

impl SearchProvider for BingProvider {
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
                .get(ENDPOINT)
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "text/html,application/xhtml+xml")
                .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
                .query(&[("q", query), ("count", &limit.to_string())])
                .timeout(timeout)
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
            // Bound the body read (omp/DoS defense parity with the DDG adapter).
            const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
            let body = match response.bytes().await {
                Ok(bytes) if bytes.len() <= MAX_BODY_BYTES => {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                Ok(_) => {
                    return SearchResponse::failed(
                        PROVIDER_ID,
                        SearchProviderError::Network("response body too large".to_string()),
                    )
                }
                Err(err) => return SearchResponse::failed(PROVIDER_ID, transport_error(&err)),
            };

            let entries = parse_serp(&body);
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

/// Extract `(title, url, snippet)` triples from the `b_algo` result blocks of
/// a Bing SERP. Falls back to an empty vec on layout changes — the caller
/// then reports `NoResults` and the chain advances.
fn parse_serp(html: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    for block in split_algo_blocks(html) {
        let Some((url, title)) = extract_headline(block) else {
            continue;
        };
        let snippet = extract_snippet(block);
        out.push((title, url, snippet));
    }
    out
}

/// Yield the HTML of each `<li class="b_algo" ...>...</li>` result block.
fn split_algo_blocks(html: &str) -> impl Iterator<Item = &str> {
    let mut rest = html;
    std::iter::from_fn(move || {
        let start = rest.find("<li class=\"b_algo\"")?;
        let after_start = &rest[start..];
        let end_rel = after_start.find("</li>").map(|p| p + "</li>".len())?;
        let block = &after_start[..end_rel];
        rest = &rest[start + end_rel..];
        Some(block)
    })
}

/// Pull the first `<h2><a href="URL">TITLE</a></h2>` headline from a block.
fn extract_headline(block: &str) -> Option<(String, String)> {
    let h2_start = block.find("<h2")?;
    let after_h2 = &block[h2_start..];
    let h2_end = after_h2.find("</h2>")?;
    let headline = &after_h2[..h2_end];
    let anchor_start = headline.find("<a")?;
    let after_anchor = &headline[anchor_start..];
    let href_start = after_anchor.find("href=\"")? + "href=\"".len();
    let after_href = &after_anchor[href_start..];
    let href_end = after_href.find('"')?;
    let url = decode_entities(&after_href[..href_end]);
    if url.is_empty() {
        return None;
    }
    let text_start = after_anchor.find('>')? + 1;
    let text_end = after_anchor[text_start..].find("</a>")? + text_start;
    let title = strip_inline_tags(&after_anchor[text_start..text_end]);
    if title.is_empty() {
        return None;
    }
    Some((url, title))
}

/// Pull the first `<p>` summary text from a block, if present.
fn extract_snippet(block: &str) -> Option<String> {
    let p_start = block.find("<p")?;
    let after_p = &block[p_start..];
    let text_start = after_p.find('>')? + 1;
    let text_end = after_p[text_start..].find("</p>")? + text_start;
    let snippet = strip_inline_tags(&after_p[text_start..text_end]);
    (!snippet.is_empty()).then_some(snippet)
}

/// Strip query-highlight tags Bing wraps around matched terms, then decode
/// HTML entities (including the `&#0183;` / `&ensp;` separators Bing uses).
fn strip_inline_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('<') {
        out.push_str(&rest[..pos]);
        match rest[pos..].find('>') {
            Some(end) => rest = &rest[pos + end + 1..],
            None => {
                // Unterminated tag: keep the rest verbatim.
                out.push_str(&rest[pos..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out)
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&ensp;", " ")
        .replace("&#0183;", "·")
        .replace("&middot;", "·")
        .replace("&#183;", "·")
        .replace("&hellip;", "…")
        .replace("&#8230;", "…")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<ol id="b_results">
<li class="b_algo" data-id="1"><h2><a href="https://qwen.ai/blog?id=qwen3">Qwen3: Think Deeper, Act Faster</a></h2><div class="b_caption"><p>2025&#0183; The post-trained models, such as Qwen3-30B-A3B.</p></div></li>
<li class="b_algo"><h2><a href="https://github.com/QwenLM/Qwen3">GitHub - QwenLM/Qwen3</a></h2><p>Large language model repo.</p></li>
</ol>"#;

    #[test]
    fn parses_b_algo_blocks_with_url_title_snippet() {
        let parsed = parse_serp(SAMPLE);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "Qwen3: Think Deeper, Act Faster");
        assert_eq!(parsed[0].1, "https://qwen.ai/blog?id=qwen3");
        assert_eq!(
            parsed[0].2.as_deref(),
            Some("2025· The post-trained models, such as Qwen3-30B-A3B.")
        );
        assert_eq!(parsed[1].0, "GitHub - QwenLM/Qwen3");
        assert_eq!(parsed[1].1, "https://github.com/QwenLM/Qwen3");
        assert_eq!(parsed[1].2.as_deref(), Some("Large language model repo."));
    }

    #[test]
    fn strips_highlight_tags_and_decodes_separators() {
        let html = r#"<li class="b_algo"><h2><a href="https://e.com">Qwen3 overview &middot; highlights</a></h2><p>runs on <strong>16GB</strong> VRAM&#0183;quantized&#8230;</p></li>"#;
        let parsed = parse_serp(html);
        assert_eq!(parsed[0].0, "Qwen3 overview · highlights");
        assert_eq!(parsed[0].2.as_deref(), Some("runs on 16GB VRAM·quantized…"));
    }

    #[test]
    fn empty_or_changed_layout_yields_no_entries() {
        assert!(parse_serp("<ol><li>no results</li></ol>").is_empty());
        assert!(parse_serp("").is_empty());
    }

    #[test]
    fn decodes_entities_in_titles_and_urls() {
        let html = r#"<li class="b_algo"><h2><a href="https://e.com/a?x=1&amp;y=2">A &amp; B</a></h2><p>s</p></li>"#;
        let parsed = parse_serp(html);
        assert_eq!(parsed[0].0, "A & B");
        assert_eq!(parsed[0].1, "https://e.com/a?x=1&y=2");
    }
}
