//! Credential-safe rendering helpers.
//!
//! Anything that can reach a terminal, a log line, a status panel or an audit
//! record MUST go through this module first. Two families of secrets matter
//! for MCP: the values the user put in `env`/`headers` (never rendered at all,
//! see [`crate::config::ServerSettings::endpoint_label`]) and secrets that
//! appear inside free text (URLs, server error strings, argument summaries)
//! which are masked here.

use serde_json::Value;

use aish_security::secret::{redact_secrets, SecretScanner};

/// Built-in secret scanner (no custom patterns).
///
/// `SecretScanner::new` compiles the whole built-in pattern set, so it is built
/// once per process instead of per redaction.
static SECRET_SCANNER: std::sync::LazyLock<SecretScanner> =
    std::sync::LazyLock::new(|| SecretScanner::new(&[]));

/// Redact every detected secret in `text` with `[REDACTED:type]` markers.
pub fn redact_secrets_in(text: &str) -> String {
    redact_secrets(text, &SECRET_SCANNER)
}

/// Longest diagnostic text kept after sanitization.
const MAX_DIAGNOSTIC_LEN: usize = 1000;
/// Longest single data string kept after sanitization.
const MAX_DATA_STRING: usize = 256;
/// Longest serialized data payload kept after sanitization.
const MAX_DATA_TOTAL: usize = 2000;
/// Depth and breadth bounds for sanitized JSON.
const MAX_DATA_DEPTH: usize = 5;
const MAX_DATA_ITEMS: usize = 30;

/// Fixed placeholder substituted for every masked value.
pub const REDACTED: &str = "[redacted]";

/// True when a key name suggests its value is a credential.
pub fn is_secret_key(key: &str) -> bool {
    let lower = key.trim().to_ascii_lowercase().replace('-', "_");
    if lower.is_empty() {
        return false;
    }
    const NAMES: [&str; 10] = [
        "authorization",
        "bearer",
        "cookie",
        "secret",
        "password",
        "pwd",
        "token",
        "api_key",
        "apikey",
        "credential",
    ];
    if NAMES.iter().any(|name| lower.contains(name)) {
        return true;
    }
    lower.contains("private_key") || lower.contains("access_key") || lower.contains("signature")
}

/// Render an endpoint URL for logs/status: userinfo, query values and
/// fragments are masked. A bare host/path without a scheme is kept as-is.
///
/// A bare *username* is deliberately kept (only a password is masked) because
/// this is the URL the user configured for their own server and the username
/// identifies it; free text goes through `mask_free_text_url` instead, which
/// masks the whole userinfo and every query value. Do not unify the two.
pub fn redact_url_for_log(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let (before_fragment, _) = match trimmed.split_once('#') {
        Some((head, _)) => (head, true),
        None => (trimmed, false),
    };
    mask_url(before_fragment, true, false)
}

/// Mask one URL. With `keep_username` a bare username survives and only
/// secret-looking query parameters are masked, which is what the endpoint
/// label of the user's own server wants; free-text URLs pass `false` and get
/// the whole userinfo plus every query value masked.
fn mask_url(url: &str, keep_username: bool, all_query_values: bool) -> String {
    let (base, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    let masked_base = mask_userinfo(base, keep_username);
    match query {
        Some(query) if !query.is_empty() => {
            format!("{masked_base}?{}", mask_query(query, all_query_values))
        }
        _ => masked_base,
    }
}

fn mask_userinfo(base: &str, keep_username: bool) -> String {
    let Some(scheme_end) = base.find("://") else {
        return base.to_string();
    };
    let rest = &base[scheme_end + 3..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return base.to_string();
    };
    let user = &authority[..at];
    let host = &authority[at + 1..];
    // Keep a bare username (useful for diagnostics), mask any password; free
    // text has no user to identify, so its userinfo is masked as a whole.
    let masked_user = match user.split_once(':') {
        Some((name, _)) if keep_username => format!("{name}:{REDACTED}"),
        _ if keep_username => user.to_string(),
        _ => REDACTED.to_string(),
    };
    format!(
        "{}{}@{}{}",
        &base[..scheme_end + 3],
        masked_user,
        host,
        &rest[authority_end..]
    )
}

/// Mask the values of an `a=1&b=2` string. With `all_values` every value is
/// masked, otherwise only parameters whose name suggests a credential.
fn mask_query(query: &str, all_values: bool) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if all_values || is_secret_key(name) => format!("{name}={REDACTED}"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Sanitize a free-text diagnostic (server error bodies, transport errors).
pub fn sanitize_diagnostic_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let masked = sanitize_inline(text);
    if masked.len() > MAX_DIAGNOSTIC_LEN {
        let (truncated, _) = crate::types::truncate_utf8(&masked, MAX_DIAGNOSTIC_LEN);
        format!("{truncated}...(truncated)")
    } else {
        masked
    }
}

/// Mask credentials in one rendered argument or value string, without the
/// diagnostic length cap.
///
/// Used where a single argument value is shown to the user or the model (for
/// example the argument summary of the approval panel): the value may embed a
/// URL query, an `Authorization:` shape or a bare secret.
pub(crate) fn sanitize_argument_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    sanitize_inline(text)
}

/// Masking pipeline shared by diagnostics and rendered argument values.
fn sanitize_inline(text: &str) -> String {
    // URL spans are masked before the generic scanner so that URLs keep their
    // shape instead of being swallowed by a broad pattern match.
    let masked = mask_urls_in_text(text);
    let masked = redact_secrets_in(&masked);
    let masked = mask_assignments(&masked);
    let masked = mask_auth_schemes(&masked);
    mask_flag_values(&masked)
}

fn is_value_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == b',' || byte == b';' || byte == b'"' || byte == b'\''
}

/// Skip one value starting at `index`. A scheme prefix (`Bearer`/`Basic`)
/// pulls the following token in as well, so `Bearer abc` is fully consumed.
fn skip_value(text: &str, bytes: &[u8], mut index: usize) -> usize {
    let start = index;
    while index < bytes.len() && !is_value_delimiter(bytes[index]) {
        index += 1;
    }
    let word = text[start..index].to_ascii_lowercase();
    if word == "bearer" || word == "basic" {
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        while index < bytes.len() && !is_value_delimiter(bytes[index]) {
            index += 1;
        }
    }
    index
}

/// Mask `Authorization: Bearer xyz`, `token=xyz`, `api_key: xyz` shapes.
///
/// `copied` tracks how much of the input already reached the output, so a
/// masked value never drops the text in front of it.
fn mask_assignments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b':' || byte == b'=' {
            let key_name = last_token(&text[copied..index]);
            if !key_name.is_empty() && is_secret_key(key_name) {
                out.push_str(&text[copied..=index]);
                out.push(' ');
                out.push_str(REDACTED);
                let mut value_start = index + 1;
                while value_start < bytes.len() && bytes[value_start] == b' ' {
                    value_start += 1;
                }
                let value_end = skip_value(text, bytes, value_start);
                copied = value_end;
                index = value_end;
                continue;
            }
        }
        index += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// Last whitespace/quote/comma delimited token of `segment`.
fn last_token(segment: &str) -> &str {
    segment
        .rsplit(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '"' || c == '\'')
        .next()
        .unwrap_or("")
}

/// Mask `<scheme> <credential>` occurrences anywhere in the text, for schemes
/// that carry a credential without an assignment separator.
fn mask_auth_schemes(text: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut words = text.split(' ').peekable();
    while let Some(word) = words.next() {
        let scheme = word
            .trim_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_ascii_lowercase();
        parts.push(word);
        if (scheme == "bearer" || scheme == "basic") && words.peek().is_some() {
            words.next();
            parts.push(REDACTED);
        }
    }
    parts.join(" ")
}

/// Mask every URL found inside free text: the whole userinfo and every query
/// and fragment value are replaced.
///
/// Free text is treated more aggressively than the endpoint label of the
/// user's own configured server, which deliberately keeps a bare username for
/// identification: a URL inside a server error body or a transport error can
/// belong to anybody, so nothing in it is trusted.
fn mask_urls_in_text(text: &str) -> String {
    text.split(' ')
        .map(mask_url_chunk)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Mask the URL inside one whitespace-delimited chunk, leaving any
/// punctuation around it untouched.
fn mask_url_chunk(chunk: &str) -> String {
    let bytes = chunk.as_bytes();
    let Some(start) = url_span_start(chunk) else {
        return chunk.to_string();
    };
    let end = start
        + bytes[start..]
            .iter()
            .position(|byte| is_url_delimiter(*byte))
            .unwrap_or(bytes.len() - start);
    let url = mask_free_text_url(&chunk[start..end]);
    let (head, tail) = (&chunk[..start], &chunk[end..]);
    format!("{head}{url}{tail}")
}

/// Start of the URL inside one whitespace-delimited chunk: the scheme in front
/// of `://`, or the `?`/`&` that opens a bare query string.
fn url_span_start(chunk: &str) -> Option<usize> {
    if let Some(scheme_end) = chunk.find("://") {
        let bytes = chunk.as_bytes();
        let mut start = scheme_end;
        while start > 0 && is_scheme_char(bytes[start - 1]) {
            start -= 1;
        }
        return Some(start);
    }
    match chunk.as_bytes().first() {
        Some(b'?') | Some(b'&') => Some(0),
        _ => None,
    }
}

fn is_scheme_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

/// Byte that ends a URL inside free text.
fn is_url_delimiter(byte: u8) -> bool {
    is_value_delimiter(byte) || matches!(byte, b')' | b']' | b'}' | b'<' | b'>' | b'|' | b'`')
}

/// Mask a URL found in free text: whole userinfo, every query value and every
/// fragment value.
fn mask_free_text_url(url: &str) -> String {
    let (before_fragment, fragment) = match url.split_once('#') {
        Some((head, fragment)) => (head, Some(fragment)),
        None => (url, None),
    };
    let masked = mask_url(before_fragment, false, true);
    match fragment {
        Some(fragment) if !fragment.is_empty() => {
            format!("{masked}#{}", mask_query(fragment, true))
        }
        _ => masked,
    }
}

/// Mask the value of a secret-looking CLI flag: `--token s3cret`,
/// `--api-key=abc` and short forms such as `-p s3cret`. stdio argv is rendered
/// (see [`crate::config::ServerSettings::endpoint_label`]) and `${VAR}`
/// references are expanded before that, so a credential passed as a flag must
/// not survive.
fn mask_flag_values(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let at_boundary = index == 0 || is_value_delimiter(bytes[index - 1]);
        if bytes[index] != b'-' || !at_boundary {
            index += 1;
            continue;
        }
        let mut name_end = index;
        while name_end < bytes.len()
            && !is_value_delimiter(bytes[name_end])
            && bytes[name_end] != b'='
        {
            name_end += 1;
        }
        let name = text[index..name_end].trim_start_matches('-');
        if name.is_empty() || !is_secret_key(name) {
            index = name_end.max(index + 1);
            continue;
        }
        let mut value_start = name_end;
        if value_start < bytes.len() && bytes[value_start] == b'=' {
            value_start += 1;
        }
        while value_start < bytes.len() && bytes[value_start] == b' ' {
            value_start += 1;
        }
        let value_end = skip_value(text, bytes, value_start);
        if value_end == value_start {
            index = name_end.max(index + 1);
            continue;
        }
        out.push_str(&text[copied..value_start]);
        out.push_str(REDACTED);
        copied = value_end;
        index = value_end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Sanitize an arbitrary JSON payload for diagnostics: bounded depth, item
/// count, string length and total size, with secret-looking keys masked.
pub fn sanitize_data(value: &Value) -> Value {
    let mut budget = MAX_DATA_TOTAL;
    sanitize_value(value, 0, &mut budget)
}

fn sanitize_value(value: &Value, depth: usize, budget: &mut usize) -> Value {
    if depth > MAX_DATA_DEPTH || *budget == 0 {
        return Value::String("...".to_string());
    }
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map.iter().take(MAX_DATA_ITEMS) {
                let rendered = if is_secret_key(key) {
                    Value::String(REDACTED.to_string())
                } else {
                    sanitize_value(item, depth + 1, budget)
                };
                let size = rendered.to_string().len();
                if size > *budget {
                    out.insert(key.clone(), Value::String("...(truncated)".to_string()));
                    *budget = 0;
                    break;
                }
                *budget -= size;
                out.insert(key.clone(), rendered);
            }
            if map.len() > MAX_DATA_ITEMS {
                out.insert(
                    "...".to_string(),
                    Value::String(format!("{} more keys", map.len() - MAX_DATA_ITEMS)),
                );
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items.iter().take(MAX_DATA_ITEMS) {
                out.push(sanitize_value(item, depth + 1, budget));
            }
            if items.len() > MAX_DATA_ITEMS {
                out.push(Value::String(format!(
                    "... {} more items",
                    items.len() - MAX_DATA_ITEMS
                )));
            }
            Value::Array(out)
        }
        Value::String(text) => {
            let redacted = redact_secrets_in(text);
            let (mut kept, truncated) = crate::types::truncate_utf8(&redacted, MAX_DATA_STRING);
            if truncated {
                kept.push_str("...");
            }
            *budget = budget.saturating_sub(kept.len());
            Value::String(kept)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_url_userinfo_and_query() {
        assert_eq!(
            redact_url_for_log("https://user:pa55@example.com/mcp?token=abc&page=2"),
            "https://user:[redacted]@example.com/mcp?token=[redacted]&page=2"
        );
        assert_eq!(
            redact_url_for_log("https://example.com/mcp"),
            "https://example.com/mcp"
        );
        assert_eq!(redact_url_for_log(""), "");
    }

    #[test]
    fn keeps_the_text_in_front_of_a_masked_value() {
        let sanitized = sanitize_diagnostic_text("HTTP 500: token=abc123 reject");
        assert!(sanitized.contains("HTTP 500:"), "{sanitized}");
        assert!(!sanitized.contains("abc123"), "{sanitized}");
    }

    #[test]
    fn preserves_messages_without_secrets() {
        for input in [
            "No response received for request ID 1",
            "request timeout after 50ms",
            "HTTP 500: boom",
        ] {
            assert_eq!(sanitize_diagnostic_text(input), input);
        }
    }

    #[test]
    fn masks_bearer_prefixed_values() {
        let sanitized = sanitize_diagnostic_text("HTTP 401: Authorization: Bearer abcdef123456");
        assert!(!sanitized.contains("abcdef123456"), "{sanitized}");

        let bare = sanitize_diagnostic_text("HTTP 401: Bearer abcdef123456");
        assert!(!bare.contains("abcdef123456"), "{bare}");
    }

    #[test]
    fn masks_assignment_shapes() {
        let sanitized = sanitize_diagnostic_text("api_key=sk-live-1234567890 rejected");
        assert!(!sanitized.contains("sk-live-1234567890"), "{sanitized}");
    }

    #[test]
    fn masks_secret_in_url_query_inside_text() {
        let sanitized =
            sanitize_diagnostic_text("GET https://api.example.com/x?access_token=deadbeef failed");
        assert!(!sanitized.contains("deadbeef"), "{sanitized}");
    }

    #[test]
    fn masks_every_url_query_value_in_free_text() {
        let sanitized = sanitize_diagnostic_text(
            "error sending request for url (https://host.example.com/mcp?key=SUPERSECRET)",
        );
        assert!(!sanitized.contains("SUPERSECRET"), "{sanitized}");
        assert!(
            sanitized.contains("https://host.example.com/mcp?key=[redacted]"),
            "{sanitized}"
        );

        let benign = sanitize_diagnostic_text("GET https://host.example.com/list?page=2 failed");
        assert!(!benign.contains("page=2"), "{benign}");
        assert!(benign.contains("page=[redacted]"), "{benign}");

        let query_only = sanitize_diagnostic_text("no url here ?key=SUPERSECRET end");
        assert!(!query_only.contains("SUPERSECRET"), "{query_only}");
    }

    #[test]
    fn masks_whole_userinfo_in_free_text() {
        for input in [
            "GET https://user:pass@h.example.com/x failed",
            "https://token@h/x",
        ] {
            let sanitized = sanitize_diagnostic_text(input);
            assert!(!sanitized.contains("pass"), "{sanitized}");
            assert!(!sanitized.contains("token"), "{sanitized}");
            assert!(sanitized.contains("[redacted]@h"), "{sanitized}");
        }
    }

    #[test]
    fn keeps_prose_assignments_keyed_on_secret_names() {
        assert_eq!(sanitize_diagnostic_text("count=5 items"), "count=5 items");
        let masked = sanitize_diagnostic_text("count=5 token=xyz");
        assert!(masked.contains("count=5"), "{masked}");
        assert!(!masked.contains("xyz"), "{masked}");
    }

    #[test]
    fn masks_flag_values() {
        let sanitized = sanitize_diagnostic_text("mcp: npx --token s3cret --verbose");
        assert!(!sanitized.contains("s3cret"), "{sanitized}");
        assert!(sanitized.contains("--token [redacted]"), "{sanitized}");
        assert!(sanitized.contains("--verbose"), "{sanitized}");

        let equals = sanitize_diagnostic_text("run --api-key=abc123 now");
        assert!(!equals.contains("abc123"), "{equals}");

        let short = sanitize_diagnostic_text("run -c /bin/sh -x");
        assert_eq!(short, "run -c /bin/sh -x");
    }

    #[test]
    fn sanitizes_nested_data_with_bounds() {
        let payload = serde_json::json!({
            "auth": {"authorization": "Bearer secret-token"},
            "items": (0..40).map(|i| i.to_string()).collect::<Vec<_>>(),
            "note": "x".repeat(500)
        });
        let sanitized = sanitize_data(&payload);
        assert_eq!(
            sanitized["auth"]["authorization"],
            Value::String(REDACTED.into())
        );
        assert!(sanitized["items"].as_array().unwrap().len() <= MAX_DATA_ITEMS + 1);
        assert!(sanitized["note"].as_str().unwrap().len() <= MAX_DATA_STRING + 3);
    }

    #[test]
    fn detects_secret_keys() {
        assert!(is_secret_key("Authorization"));
        assert!(is_secret_key("X-Api-Key"));
        assert!(is_secret_key("client_secret"));
        assert!(!is_secret_key("path"));
        assert!(!is_secret_key(""));
    }
}
