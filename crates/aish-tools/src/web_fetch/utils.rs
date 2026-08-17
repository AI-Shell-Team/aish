use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use reqwest::{redirect, Client, StatusCode, Url};

const MAX_URL_LENGTH: usize = 2000;
const MAX_HTTP_CONTENT_LENGTH: usize = 10 * 1024 * 1024;
/// Max retry attempts for transient network errors (connect/timeout) on the
/// initial `.send()`. HTTP status codes are not errors at this layer — they
/// are returned in the response and classified by the caller — so 4xx/5xx do
/// not retry here.
const MAX_FETCH_RETRIES: u32 = 2;
const FETCH_TIMEOUT_SECS: u64 = 60;
const MAX_REDIRECTS: usize = 10;
pub(crate) const MAX_MARKDOWN_LENGTH: usize = 100_000;
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_MAX_ENTRIES: usize = 64;
const USER_AGENT_VALUE: &str = concat!("aish/", env!("CARGO_PKG_VERSION"), " WebFetch");

#[derive(Clone)]
struct CacheEntry {
    fetched_at: Instant,
    url: String,
    code: u16,
    code_text: String,
    bytes: usize,
    content_type: String,
    content: String,
}

static URL_CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    URL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug)]
pub(crate) enum FetchFailure {
    Blocked(String),
    Request(String),
    Redirect(RedirectInfo),
}

#[derive(Debug)]
pub(crate) struct RedirectInfo {
    pub(crate) original_url: String,
    pub(crate) redirect_url: String,
    pub(crate) status_code: u16,
}

pub(crate) struct FetchedContent {
    pub(crate) url: String,
    pub(crate) code: u16,
    pub(crate) code_text: String,
    pub(crate) bytes: usize,
    pub(crate) content_type: String,
    pub(crate) content: String,
    pub(crate) from_cache: bool,
}

pub(crate) fn build_client() -> Result<Client, String> {
    Client::builder()
        .redirect(redirect::Policy::none())
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|error| error.to_string())
}

pub(crate) async fn fetch_url_content(raw_url: &str) -> Result<FetchedContent, FetchFailure> {
    let normalized = validate_and_normalize_url(raw_url).map_err(FetchFailure::Blocked)?;
    ensure_public_host(&normalized)
        .await
        .map_err(FetchFailure::Blocked)?;

    if let Some(entry) = get_cached(raw_url) {
        return Ok(FetchedContent {
            url: entry.url,
            code: entry.code,
            code_text: entry.code_text,
            bytes: entry.bytes,
            content_type: entry.content_type,
            content: entry.content,
            from_cache: true,
        });
    }

    let client = build_client().map_err(FetchFailure::Request)?;
    let response = get_with_permitted_redirects(&client, normalized.clone(), 0).await?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    if is_binary_content_type(&content_type) {
        return Err(FetchFailure::Request(aish_i18n::t(
            "tools.web_fetch.binary_unsupported",
        )));
    }

    let raw = read_limited_body(response)
        .await
        .map_err(FetchFailure::Request)?;
    let bytes = raw.len();
    let text = String::from_utf8_lossy(&raw).to_string();
    let content = if content_type.to_ascii_lowercase().contains("text/html") {
        html_to_readable_text(&text)
    } else {
        text
    };

    let entry = CacheEntry {
        fetched_at: Instant::now(),
        url: normalized.to_string(),
        code: status.as_u16(),
        code_text: status_text(status).to_string(),
        bytes,
        content_type: content_type.clone(),
        content: content.clone(),
    };
    set_cached(raw_url.to_string(), entry);

    Ok(FetchedContent {
        url: normalized.to_string(),
        code: status.as_u16(),
        code_text: status_text(status).to_string(),
        bytes,
        content_type,
        content,
        from_cache: false,
    })
}

/// Send a GET request, retrying transient network-layer failures
/// (`is_connect()` / `is_timeout()`) up to [`MAX_FETCH_RETRIES`] times with
/// short exponential backoff. Non-transient errors (DNS, TLS, body decode)
/// surface immediately. HTTP status codes live in the returned `Response`
/// and are classified by the caller — they are not retried here.
async fn send_with_retry(client: &Client, url: &Url) -> Result<reqwest::Response, FetchFailure> {
    let mut last_err: Option<String> = None;
    for attempt in 0..=MAX_FETCH_RETRIES {
        if attempt > 0 {
            // 500ms, then 1000ms.
            tokio::time::sleep(Duration::from_millis(500u64 << (attempt - 1))).await;
        }
        match client
            .get(url.clone())
            .header(ACCEPT, "text/markdown, text/html, text/plain, */*")
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .await
        {
            Ok(r) => return Ok(r),
            Err(e) if e.is_connect() || e.is_timeout() => {
                last_err = Some(e.to_string());
                continue;
            }
            Err(e) => return Err(FetchFailure::Request(e.to_string())),
        }
    }
    Err(FetchFailure::Request(
        last_err.unwrap_or_else(|| "request failed after retries".into()),
    ))
}

async fn get_with_permitted_redirects(
    client: &Client,
    url: Url,
    depth: usize,
) -> Result<reqwest::Response, FetchFailure> {
    if depth > MAX_REDIRECTS {
        return Err(FetchFailure::Request(format!(
            "Too many redirects (exceeded {})",
            MAX_REDIRECTS
        )));
    }

    ensure_public_host(&url)
        .await
        .map_err(FetchFailure::Blocked)?;
    let response = send_with_retry(client, &url).await?;

    if is_redirect_status(response.status()) {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| FetchFailure::Request("Redirect missing Location header".into()))?;
        let redirect_url = url
            .join(location)
            .map_err(|error| FetchFailure::Request(error.to_string()))?;
        validate_url_basics(&redirect_url).map_err(FetchFailure::Blocked)?;
        ensure_public_host(&redirect_url)
            .await
            .map_err(FetchFailure::Blocked)?;

        if is_permitted_redirect(&url, &redirect_url) {
            return Box::pin(get_with_permitted_redirects(
                client,
                redirect_url,
                depth + 1,
            ))
            .await;
        }

        return Err(FetchFailure::Redirect(RedirectInfo {
            original_url: url.to_string(),
            redirect_url: redirect_url.to_string(),
            status_code: response.status().as_u16(),
        }));
    }

    Ok(response)
}

pub(crate) fn validate_and_normalize_url(raw_url: &str) -> Result<Url, String> {
    if raw_url.len() > MAX_URL_LENGTH {
        return Err(aish_i18n::t("tools.web_fetch.invalid_url"));
    }
    let mut parsed =
        Url::parse(raw_url).map_err(|_| aish_i18n::t("tools.web_fetch.invalid_url"))?;
    if parsed.scheme() == "http" {
        parsed
            .set_scheme("https")
            .map_err(|_| aish_i18n::t("tools.web_fetch.invalid_url"))?;
    }
    validate_url_basics(&parsed)?;
    Ok(parsed)
}

fn validate_url_basics(parsed: &Url) -> Result<(), String> {
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err(aish_i18n::t("tools.web_fetch.invalid_url"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(aish_i18n::t("tools.web_fetch.invalid_url"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| aish_i18n::t("tools.web_fetch.invalid_url"))?;
    if host.split('.').count() < 2 && host.parse::<IpAddr>().is_err() {
        return Err(aish_i18n::t("tools.web_fetch.invalid_url"));
    }
    if is_blocked_hostname(host) {
        let mut args = HashMap::new();
        args.insert("host".to_string(), host.to_string());
        return Err(aish_i18n::t_with_args(
            "tools.web_fetch.blocked_private_host",
            &args,
        ));
    }
    Ok(())
}

async fn ensure_public_host(url: &Url) -> Result<(), String> {
    let host = url
        .host_str()
        .ok_or_else(|| aish_i18n::t("tools.web_fetch.invalid_url"))?;
    if is_blocked_hostname(host) {
        let mut args = HashMap::new();
        args.insert("host".to_string(), host.to_string());
        return Err(aish_i18n::t_with_args(
            "tools.web_fetch.blocked_private_host",
            &args,
        ));
    }
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("DNS lookup failed for {}: {}", host, error))?;
    let mut found = false;
    for addr in addrs {
        found = true;
        if is_private_ip(&addr.ip()) {
            let mut args = HashMap::new();
            args.insert("host".to_string(), host.to_string());
            return Err(aish_i18n::t_with_args(
                "tools.web_fetch.blocked_private_host",
                &args,
            ));
        }
    }
    if !found {
        return Err(format!("DNS lookup returned no addresses for {}", host));
    }
    Ok(())
}

fn is_blocked_hostname(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "localhost" | "metadata.google.internal"
    ) {
        return true;
    }
    if normalized.ends_with(".localhost") || normalized.ends_with(".local") {
        return true;
    }
    match normalized.parse::<IpAddr>() {
        Ok(ip) => is_private_ip(&ip),
        Err(_) => false,
    }
}

fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_loopback()
                || addr.is_link_local()
                || addr.is_broadcast()
                || addr.is_documentation()
                || addr.is_unspecified()
                || addr.octets() == [169, 254, 169, 254]
                || addr.octets()[0] == 0
        }
        IpAddr::V6(addr) => {
            addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_unique_local()
                || addr.is_unicast_link_local()
        }
    }
}

fn is_redirect_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn is_permitted_redirect(original: &Url, redirect_url: &Url) -> bool {
    if original.scheme() != redirect_url.scheme() || original.port() != redirect_url.port() {
        return false;
    }
    if !redirect_url.username().is_empty() || redirect_url.password().is_some() {
        return false;
    }
    let Some(original_host) = original.host_str() else {
        return false;
    };
    let Some(redirect_host) = redirect_url.host_str() else {
        return false;
    };
    super::preapproved::host_key(original_host) == super::preapproved::host_key(redirect_host)
}

async fn read_limited_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_CONTENT_LENGTH as u64)
    {
        return Err(aish_i18n::t("tools.web_fetch.content_too_large"));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        if buffer.len() + chunk.len() > MAX_HTTP_CONTENT_LENGTH {
            return Err(aish_i18n::t("tools.web_fetch.content_too_large"));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

fn status_text(status: StatusCode) -> &'static str {
    status.canonical_reason().unwrap_or("")
}

fn is_binary_content_type(content_type: &str) -> bool {
    let lower = content_type.to_ascii_lowercase();
    if lower.starts_with("text/") {
        return false;
    }
    if lower.contains("json") || lower.contains("xml") || lower.contains("javascript") {
        return false;
    }
    lower.contains("application/pdf")
        || lower.starts_with("image/")
        || lower.starts_with("audio/")
        || lower.starts_with("video/")
        || lower.contains("application/octet-stream")
}

fn html_to_readable_text(html: &str) -> String {
    let without_scripts = regex_replace_all(
        html,
        r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>|<noscript\b[^>]*>.*?</noscript>|<svg\b[^>]*>.*?</svg>|<canvas\b[^>]*>.*?</canvas>",
        "\n",
    );
    let with_breaks = regex_replace_all(
        &without_scripts,
        r"(?i)</?(p|div|br|li|tr|td|th|h[1-6]|section|article|header|footer|main|pre|blockquote)\b[^>]*>",
        "\n",
    );
    let without_tags = regex_replace_all(&with_breaks, r"(?is)<[^>]+>", " ");
    normalize_text_whitespace(&decode_html_entities(&without_tags))
}

fn regex_replace_all(input: &str, pattern: &str, replacement: &str) -> String {
    match Regex::new(pattern) {
        Ok(regex) => regex.replace_all(input, replacement).to_string(),
        Err(_) => input.to_string(),
    }
}

fn decode_html_entities(input: &str) -> String {
    // Decode all entities in a single pass so that `&amp;lt;` (which
    // represents the literal text `&lt;`) is not double-decoded into `<`.
    //
    // The previous chained `.replace()` approach decoded `&amp;` first,
    // turning `&amp;lt;` into `&lt;`, which was then decoded again into
    // `<` — corrupting the original content.
    static ENTITY_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = ENTITY_RE
        .get_or_init(|| Regex::new(r"&(?:amp|lt|gt|quot|apos|nbsp|#39);").expect("valid regex"));
    re.replace_all(input, |caps: &regex::Captures| match &caps[0] {
        "&amp;" => "&",
        "&lt;" => "<",
        "&gt;" => ">",
        "&quot;" => "\"",
        "&apos;" | "&#39;" => "'",
        "&nbsp;" => " ",
        _ => unreachable!(),
    })
    .to_string()
}

fn normalize_text_whitespace(input: &str) -> String {
    let mut lines = Vec::new();
    for line in input.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
    }
    lines.join("\n")
}

pub(crate) fn truncate_for_model(content: &str) -> String {
    if content.chars().count() <= MAX_MARKDOWN_LENGTH {
        return content.to_string();
    }
    let mut truncated = content
        .chars()
        .take(MAX_MARKDOWN_LENGTH)
        .collect::<String>();
    truncated.push_str("\n\n[Content truncated due to length...]");
    truncated
}

pub(crate) fn format_redirect_message(info: &RedirectInfo, prompt: &str) -> String {
    let status_text = match info.status_code {
        301 => "Moved Permanently",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        _ => "Found",
    };
    format!(
        "REDIRECT DETECTED: The URL redirects to a different host.\n\nOriginal URL: {}\nRedirect URL: {}\nStatus: {} {}\n\nTo complete your request, fetch the redirected URL with these parameters:\n- url: \"{}\"\n- prompt: \"{}\"",
        info.original_url,
        info.redirect_url,
        info.status_code,
        status_text,
        info.redirect_url,
        prompt
    )
}

pub(crate) fn make_secondary_model_prompt(
    markdown_content: &str,
    prompt: &str,
    is_preapproved_domain: bool,
) -> String {
    let guidelines = if is_preapproved_domain {
        "Provide a concise response based on the content above. Include relevant details, code examples, and documentation excerpts as needed."
    } else {
        "Provide a concise response based only on the content above. In your response:\n - Enforce a strict 125-character maximum for quotes from any source document. Open Source Software is ok as long as we respect the license.\n - Use quotation marks for exact language from articles; any language outside of the quotation should never be word-for-word the same.\n - You are not a lawyer and never comment on the legality of your own prompts and responses.\n - Never produce or reproduce exact song lyrics."
    };
    format!(
        "Web page content:\n---\n{}\n---\n\n{}\n\n{}\n",
        markdown_content, prompt, guidelines
    )
}

fn get_cached(key: &str) -> Option<CacheEntry> {
    let mut guard = cache().lock().ok()?;
    let now = Instant::now();
    guard.retain(|_, entry| now.duration_since(entry.fetched_at) < CACHE_TTL);
    guard.get(key).cloned()
}

fn set_cached(key: String, entry: CacheEntry) {
    if let Ok(mut guard) = cache().lock() {
        if guard.len() >= CACHE_MAX_ENTRIES {
            if let Some(oldest_key) = guard
                .iter()
                .min_by_key(|(_, value)| value.fetched_at)
                .map(|(cache_key, _)| cache_key.clone())
            {
                guard.remove(&oldest_key);
            }
        }
        guard.insert(key, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_http_to_https() {
        let url = validate_and_normalize_url("http://example.com/path").unwrap();
        assert_eq!(url.as_str(), "https://example.com/path");
    }

    #[test]
    fn rejects_private_hosts() {
        assert!(validate_and_normalize_url("https://localhost/").is_err());
        assert!(validate_and_normalize_url("https://127.0.0.1/").is_err());
        assert!(validate_and_normalize_url("https://169.254.169.254/").is_err());
        assert!(validate_and_normalize_url("https://10.0.0.5/").is_err());
    }

    #[test]
    fn redirect_only_allows_same_origin_or_www_equivalent() {
        let original = Url::parse("https://example.com/docs").unwrap();
        let same = Url::parse("https://www.example.com/docs").unwrap();
        let other = Url::parse("https://evil.example.net/docs").unwrap();
        let http = Url::parse("http://example.com/docs").unwrap();
        assert!(is_permitted_redirect(&original, &same));
        assert!(!is_permitted_redirect(&original, &other));
        assert!(!is_permitted_redirect(&original, &http));
        let mixed_case = Url::parse("https://WWW.EXAMPLE.COM/docs").unwrap();
        assert!(is_permitted_redirect(&original, &mixed_case));
        let gist = Url::parse("https://gist.github.com/docs").unwrap();
        let github = Url::parse("https://github.com/docs").unwrap();
        assert!(!is_permitted_redirect(&github, &gist));
    }

    #[test]
    fn html_to_readable_text_removes_scripts_and_tags() {
        let html = "<html><head><script>bad()</script></head><body><h1>Hello &amp; hi</h1><p>World</p></body></html>";
        let text = html_to_readable_text(html);
        assert!(text.contains("Hello & hi"));
        assert!(text.contains("World"));
        assert!(!text.contains("bad()"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn secondary_prompt_includes_quote_restriction_for_unapproved_domains() {
        let prompt = make_secondary_model_prompt("content", "summarize", false);
        assert!(prompt.contains("125-character maximum"));
        assert!(prompt.contains("summarize"));
    }

    #[test]
    fn decode_html_entities_basic() {
        assert_eq!(decode_html_entities("&lt;"), "<");
        assert_eq!(decode_html_entities("&gt;"), ">");
        assert_eq!(decode_html_entities("&amp;"), "&");
        assert_eq!(decode_html_entities("&quot;"), "\"");
        assert_eq!(decode_html_entities("&#39;"), "'");
        assert_eq!(decode_html_entities("&apos;"), "'");
        assert_eq!(decode_html_entities("&nbsp;"), " ");
    }

    #[test]
    fn decode_html_entities_no_double_decode() {
        // `&amp;lt;` in HTML represents the literal text `&lt;`, not `<`.
        // The old chained-replace approach decoded `&amp;` first,
        // turning `&amp;lt;` into `&lt;`, then decoded `&lt;` into `<`.
        // A single-pass decoder must leave `&amp;lt;` → `&lt;`.
        assert_eq!(decode_html_entities("&amp;lt;"), "&lt;");
        assert_eq!(decode_html_entities("&amp;gt;"), "&gt;");
        assert_eq!(decode_html_entities("&amp;quot;"), "&quot;");
        assert_eq!(decode_html_entities("&amp;#39;"), "&#39;");
        assert_eq!(decode_html_entities("&amp;apos;"), "&apos;");
    }
}
