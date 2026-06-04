use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aish_llm::{
    ChatMessage, LlmResponse, LlmSession, PreflightResult, PreflightSecurityContext,
    SecurityPanelMode, StreamParser, Tool, ToolResult,
};
use futures::StreamExt;
use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use reqwest::{redirect, Client, StatusCode, Url};

const TOOL_NAME: &str = "WebFetch";
const MAX_URL_LENGTH: usize = 2000;
const MAX_HTTP_CONTENT_LENGTH: usize = 10 * 1024 * 1024;
const FETCH_TIMEOUT_SECS: u64 = 60;
const MAX_REDIRECTS: usize = 10;
const MAX_MARKDOWN_LENGTH: usize = 100_000;
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
static DESCRIPTION: OnceLock<String> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    URL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.web_fetch.description"))
}

/// Fetch a URL, extract readable text, and answer a focused prompt about it.
pub struct WebFetchTool {
    api_base: String,
    api_key: String,
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

impl WebFetchTool {
    pub fn new(
        api_base: &str,
        api_key: &str,
        model: &str,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            api_base: api_base.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            temperature,
            max_tokens,
        }
    }

    fn build_client() -> Result<Client, String> {
        Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
            .build()
            .map_err(|error| error.to_string())
    }

    async fn fetch_url_content(&self, raw_url: &str) -> Result<FetchedContent, FetchFailure> {
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

        let client = Self::build_client().map_err(FetchFailure::Request)?;
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

    async fn apply_prompt_to_content(
        &self,
        prompt: &str,
        content: &str,
        is_preapproved_domain: bool,
    ) -> Result<String, String> {
        let model_prompt = make_secondary_model_prompt(content, prompt, is_preapproved_domain);
        let session = LlmSession::new(
            &self.api_base,
            &self.api_key,
            &self.model,
            self.temperature.or(Some(0.1)),
            self.max_tokens.or(Some(2048)),
        );
        let messages = vec![ChatMessage::system(""), ChatMessage::user(model_prompt)];
        match session
            .chat_completion_raw(&messages, None, false, Some(0.1), Some(2048))
            .await
            .map_err(|error| error.to_string())?
        {
            LlmResponse::Json(json) => {
                let (content, _reasoning, _tool_calls, _usage) =
                    StreamParser::parse_response(&json);
                Ok(content.unwrap_or_else(|| "No response from model".to_string()))
            }
            LlmResponse::Stream(_) => Err("secondary model unexpectedly returned a stream".into()),
        }
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        get_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The fully-qualified URL to fetch content from"
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt describing what information to extract from the fetched page"
                }
            },
            "required": ["url", "prompt"],
            "additionalProperties": false
        })
    }

    fn preflight(&self, args: &serde_json::Value) -> PreflightResult {
        let url = match args.get("url").and_then(|value| value.as_str()) {
            Some(value) if !value.trim().is_empty() => value,
            _ => {
                return PreflightResult::Block {
                    message: aish_i18n::t("tools.web_fetch.missing_url"),
                    security: Some(PreflightSecurityContext::fallback(
                        TOOL_NAME,
                        None,
                        aish_i18n::t("tools.web_fetch.missing_url"),
                        SecurityPanelMode::Blocked,
                    )),
                }
            }
        };

        let normalized = match validate_and_normalize_url(url) {
            Ok(parsed) => parsed,
            Err(message) => {
                return PreflightResult::Block {
                    message: message.clone(),
                    security: Some(PreflightSecurityContext::fallback(
                        TOOL_NAME,
                        Some(url.to_string()),
                        message,
                        SecurityPanelMode::Blocked,
                    )),
                }
            }
        };

        let hostname = normalized.host_str().unwrap_or("").to_string();
        if is_preapproved_host(&hostname, normalized.path()) {
            return PreflightResult::Allow;
        }

        let message = aish_i18n::t_with_args(
            "tools.web_fetch.confirm_fetch",
            &HashMap::from([("host".to_string(), hostname.clone())]),
        );
        PreflightResult::Confirm {
            message: message.clone(),
            security: Some(PreflightSecurityContext::fallback(
                TOOL_NAME,
                Some(hostname),
                message,
                SecurityPanelMode::Confirm,
            )),
        }
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        ToolResult::error("WebFetch requires async execution; use execute_async")
    }

    fn execute_async<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let url = match args.get("url").and_then(|value| value.as_str()) {
                Some(value) if !value.trim().is_empty() => value.trim(),
                _ => return ToolResult::error(aish_i18n::t("tools.web_fetch.missing_url")),
            };
            let prompt = match args.get("prompt").and_then(|value| value.as_str()) {
                Some(value) if !value.trim().is_empty() => value.trim(),
                _ => return ToolResult::error(aish_i18n::t("tools.web_fetch.missing_prompt")),
            };

            let start = Instant::now();
            let fetched = match self.fetch_url_content(url).await {
                Ok(content) => content,
                Err(FetchFailure::Redirect(info)) => {
                    let message = format_redirect_message(&info, prompt);
                    return ToolResult {
                        ok: true,
                        output: message.clone(),
                        meta: Some(serde_json::json!({
                            "url": url,
                            "redirect_url": info.redirect_url,
                            "code": info.status_code,
                            "result": message,
                            "durationMs": start.elapsed().as_millis() as u64,
                        })),
                    };
                }
                Err(FetchFailure::Blocked(message)) | Err(FetchFailure::Request(message)) => {
                    return ToolResult::error(message)
                }
            };

            let truncated_content = truncate_for_model(&fetched.content);
            let parsed_url = Url::parse(&fetched.url).ok();
            let is_preapproved_domain = parsed_url
                .as_ref()
                .and_then(|parsed| parsed.host_str().map(|host| (host, parsed.path())))
                .is_some_and(|(host, path)| is_preapproved_host(host, path));

            let result = match self
                .apply_prompt_to_content(prompt, &truncated_content, is_preapproved_domain)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let mut args_map = HashMap::new();
                    args_map.insert("error".to_string(), error);
                    return ToolResult::error(aish_i18n::t_with_args(
                        "tools.web_fetch.secondary_model_failed",
                        &args_map,
                    ));
                }
            };

            let duration_ms = start.elapsed().as_millis() as u64;
            let output = format!(
                "Fetched: {}\nStatus: {} {}\nBytes: {}\nDuration: {}ms\nCached: {}\n\n{}",
                fetched.url,
                fetched.code,
                fetched.code_text,
                fetched.bytes,
                duration_ms,
                fetched.from_cache,
                result
            );

            ToolResult {
                ok: true,
                output,
                meta: Some(serde_json::json!({
                    "url": fetched.url,
                    "code": fetched.code,
                    "codeText": fetched.code_text,
                    "bytes": fetched.bytes,
                    "contentType": fetched.content_type,
                    "durationMs": duration_ms,
                    "fromCache": fetched.from_cache,
                    "result": result,
                })),
            }
        })
    }
}

#[derive(Debug)]
enum FetchFailure {
    Blocked(String),
    Request(String),
    Redirect(RedirectInfo),
}

#[derive(Debug)]
struct RedirectInfo {
    original_url: String,
    redirect_url: String,
    status_code: u16,
}

struct FetchedContent {
    url: String,
    code: u16,
    code_text: String,
    bytes: usize,
    content_type: String,
    content: String,
    from_cache: bool,
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
    let response = client
        .get(url.clone())
        .header(ACCEPT, "text/markdown, text/html, text/plain, */*")
        .header(USER_AGENT, USER_AGENT_VALUE)
        .send()
        .await
        .map_err(|error| FetchFailure::Request(error.to_string()))?;

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

fn validate_and_normalize_url(raw_url: &str) -> Result<Url, String> {
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
    strip_www(original_host) == strip_www(redirect_host)
}

fn strip_www(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
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
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
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

fn truncate_for_model(content: &str) -> String {
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

fn make_secondary_model_prompt(
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

fn format_redirect_message(info: &RedirectInfo, prompt: &str) -> String {
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

fn is_preapproved_host(hostname: &str, pathname: &str) -> bool {
    for entry in PREAPPROVED_HOSTS {
        if let Some((host, prefix)) = entry.split_once('/') {
            if hostname == host {
                let prefix = format!("/{}", prefix);
                if pathname == prefix || pathname.starts_with(&(prefix + "/")) {
                    return true;
                }
            }
            continue;
        }
        if hostname == *entry {
            return true;
        }
    }
    false
}

const PREAPPROVED_HOSTS: &[&str] = &[
    "platform.claude.com",
    "code.claude.com",
    "modelcontextprotocol.io",
    "github.com/anthropics",
    "agentskills.io",
    "docs.python.org",
    "en.cppreference.com",
    "docs.oracle.com",
    "learn.microsoft.com",
    "developer.mozilla.org",
    "go.dev",
    "pkg.go.dev",
    "www.php.net",
    "docs.swift.org",
    "kotlinlang.org",
    "ruby-doc.org",
    "doc.rust-lang.org",
    "www.typescriptlang.org",
    "react.dev",
    "angular.io",
    "vuejs.org",
    "nextjs.org",
    "expressjs.com",
    "nodejs.org",
    "bun.sh",
    "jquery.com",
    "getbootstrap.com",
    "tailwindcss.com",
    "d3js.org",
    "threejs.org",
    "redux.js.org",
    "webpack.js.org",
    "jestjs.io",
    "reactrouter.com",
    "docs.djangoproject.com",
    "flask.palletsprojects.com",
    "fastapi.tiangolo.com",
    "pandas.pydata.org",
    "numpy.org",
    "www.tensorflow.org",
    "pytorch.org",
    "scikit-learn.org",
    "matplotlib.org",
    "requests.readthedocs.io",
    "jupyter.org",
    "laravel.com",
    "symfony.com",
    "wordpress.org",
    "docs.spring.io",
    "hibernate.org",
    "tomcat.apache.org",
    "gradle.org",
    "maven.apache.org",
    "asp.net",
    "dotnet.microsoft.com",
    "nuget.org",
    "blazor.net",
    "reactnative.dev",
    "docs.flutter.dev",
    "developer.apple.com",
    "developer.android.com",
    "keras.io",
    "spark.apache.org",
    "huggingface.co",
    "www.kaggle.com",
    "www.mongodb.com",
    "redis.io",
    "www.postgresql.org",
    "dev.mysql.com",
    "www.sqlite.org",
    "graphql.org",
    "prisma.io",
    "docs.aws.amazon.com",
    "cloud.google.com",
    "kubernetes.io",
    "www.docker.com",
    "www.terraform.io",
    "www.ansible.com",
    "vercel.com/docs",
    "docs.netlify.com",
    "devcenter.heroku.com",
    "cypress.io",
    "selenium.dev",
    "docs.unity.com",
    "docs.unrealengine.com",
    "git-scm.com",
    "nginx.org",
    "httpd.apache.org",
];

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
    fn preapproved_host_supports_path_prefix_boundary() {
        assert!(is_preapproved_host("github.com", "/anthropics/claude-code"));
        assert!(!is_preapproved_host(
            "github.com",
            "/anthropics-evil/project"
        ));
        assert!(is_preapproved_host("doc.rust-lang.org", "/book/"));
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

    #[tokio::test]
    #[ignore]
    async fn live_fetch_url() {
        if std::env::var("AISH_LIVE_WEBFETCH").ok().as_deref() != Some("1") {
            eprintln!("set AISH_LIVE_WEBFETCH=1 to run this live network smoke test");
            return;
        }

        let url = std::env::var("AISH_LIVE_WEBFETCH_URL")
            .unwrap_or_else(|_| "https://github.com/mattpocock/skills".to_string());
        let expected = std::env::var("AISH_LIVE_WEBFETCH_EXPECT").ok();
        let tool = WebFetchTool::new("", "", "", Some(0.1), Some(256));
        let fetched = tool
            .fetch_url_content(&url)
            .await
            .expect("expected live page fetch to succeed");

        println!(
            "fetched {} status={} bytes={} content_type={} chars={}",
            fetched.url,
            fetched.code,
            fetched.bytes,
            fetched.content_type,
            fetched.content.len()
        );
        println!(
            "preview:\n{}",
            truncate_for_model(&fetched.content)
                .chars()
                .take(800)
                .collect::<String>()
        );

        assert_eq!(fetched.code, 200);
        if let Some(expected) = expected {
            assert!(
                fetched
                    .content
                    .to_ascii_lowercase()
                    .contains(&expected.to_ascii_lowercase()),
                "expected fetched content to contain {expected:?}"
            );
        }
    }
}
