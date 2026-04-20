//! OAuth 2.0 + PKCE authentication module.
//!
//! Supports browser-based authorization code flow and device code flow
//! for authenticating with LLM providers (Google, Anthropic, OpenAI, etc.).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Static configuration for an OAuth provider.
#[derive(Debug, Clone)]
pub struct OAuthProviderSpec {
    pub provider_id: String,
    pub display_name: String,
    pub client_id: String,
    /// Space-separated OAuth scopes.
    pub scope: String,
    pub authorize_url: String,
    pub token_url: String,
    /// If present, the provider supports the device-authorization grant.
    pub device_authorization_url: Option<String>,
    /// Extra query parameters appended to the authorization URL.
    pub extra_query_params: Vec<(String, String)>,
    /// Timeout for individual HTTP requests (seconds).
    pub http_timeout_secs: u64,
    /// Polling interval for device-code flow (seconds).
    pub device_poll_interval_secs: u64,
}

impl OAuthProviderSpec {
    /// Convenience constructor with sensible defaults.
    pub fn new(
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        client_id: impl Into<String>,
        scope: impl Into<String>,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            display_name: display_name.into(),
            client_id: client_id.into(),
            scope: scope.into(),
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            device_authorization_url: None,
            extra_query_params: Vec::new(),
            http_timeout_secs: 30,
            device_poll_interval_secs: 5,
        }
    }
}

/// Token response persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Seconds until expiry from the time of issuance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// PKCE pair returned by [`generate_pkce`].
pub struct PkcePair {
    pub code_verifier: String,
    pub code_challenge: String,
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

/// Generate a PKCE code_verifier / code_challenge pair.
///
/// The verifier is 64 random bytes encoded as base64url (no padding).
/// The challenge is the SHA-256 hash of the verifier, also base64url-encoded.
pub fn generate_pkce() -> PkcePair {
    let mut rng = rand::rng();
    let mut bytes = [0u8; 64];
    rng.fill(&mut bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);

    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(digest);

    PkcePair {
        code_verifier,
        code_challenge,
    }
}

/// Generate a random state parameter for CSRF protection.
pub fn generate_state() -> String {
    let mut rng = rand::rng();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Browser-based authorization code flow
// ---------------------------------------------------------------------------

/// Run the browser-based OAuth flow.
///
/// 1. Bind a TCP listener on `127.0.0.1:{callback_port}`.
/// 2. Open the user's browser at the authorization URL.
/// 3. Wait for the callback with the authorization `code`.
/// 4. Exchange the code for tokens.
pub fn login_with_browser(
    provider: &OAuthProviderSpec,
    callback_port: u16,
    open_browser: bool,
) -> Result<OAuthTokens, String> {
    let redirect_uri = format!("http://127.0.0.1:{}", callback_port);
    let pkce = generate_pkce();
    let state = generate_state();

    // Build authorization URL.
    let mut auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.authorize_url,
        provider.client_id,
        redirect_uri,
        provider.scope,
        state,
        pkce.code_challenge,
    );
    for (k, v) in &provider.extra_query_params {
        auth_url.push_str(&format!("&{}={}", k, v));
    }

    // Bind listener before opening browser to avoid race.
    let listener = TcpListener::bind(format!("127.0.0.1:{}", callback_port))
        .map_err(|e| format!("Failed to bind port {}: {}", callback_port, e))?;

    if open_browser {
        open_url(&auth_url);
        println!(
            "Opening browser for {} login. If it does not open, visit:\n{}",
            provider.display_name, auth_url
        );
    } else {
        println!(
            "Please open the following URL in your browser to authenticate with {}:\n{}",
            provider.display_name, auth_url
        );
    }

    // Wait for a single callback.
    listener.set_nonblocking(false).ok();
    let (mut stream, _addr) = listener
        .accept()
        .map_err(|e| format!("Failed to accept callback connection: {}", e))?;

    // Read HTTP request.
    let mut buf = [0u8; 4096];
    let n = stream
        .read(&mut buf)
        .map_err(|e| format!("Failed to read callback request: {}", e))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Send a simple HTML response so the user sees a success page.
    let html = "\
        HTTP/1.1 200 OK\r\n\
        Content-Type: text/html; charset=utf-8\r\n\
        Connection: close\r\n\
        \r\n\
        <html><body><h2>Authentication successful!</h2>\
        <p>You can close this tab and return to the terminal.</p>\
        </body></html>";
    stream.write_all(html.as_bytes()).ok();
    let _ = stream.flush();

    // Parse query params from the GET path.
    let code = parse_callback_query_param(&request, "code").ok_or_else(|| {
        let error = parse_callback_query_param(&request, "error");
        match error {
            Some(e) => format!("OAuth error: {}", e),
            None => "No authorization code received".to_string(),
        }
    })?;

    let returned_state = parse_callback_query_param(&request, "state");
    if returned_state.as_deref() != Some(&state) {
        return Err("State mismatch - possible CSRF attack".to_string());
    }

    exchange_code_for_tokens(provider, &code, &redirect_uri, &pkce.code_verifier)
}

// ---------------------------------------------------------------------------
// Device code flow
// ---------------------------------------------------------------------------

/// Run the device-code OAuth flow.
///
/// 1. POST to the device authorization endpoint to get a user code + verification URL.
/// 2. Display instructions and poll the token endpoint until the user completes auth.
pub fn login_with_device_code(provider: &OAuthProviderSpec) -> Result<OAuthTokens, String> {
    let device_url = provider
        .device_authorization_url
        .as_ref()
        .ok_or_else(|| "Provider does not support device code flow".to_string())?;

    let client = build_http_client(provider)?;

    // Request device code.
    let resp: HashMap<String, serde_json::Value> = client
        .post(device_url)
        .form(&[
            ("client_id", provider.client_id.as_str()),
            ("scope", provider.scope.as_str()),
        ])
        .send()
        .map_err(|e| format!("Device code request failed: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse device code response: {}", e))?;

    let user_code = resp["user_code"]
        .as_str()
        .ok_or("Missing user_code in device code response")?
        .to_string();
    let verification_uri = resp["verification_uri"]
        .as_str()
        .or_else(|| {
            let v: &serde_json::Value = &resp["verification_url"];
            v.as_str()
        })
        .ok_or("Missing verification_uri in device code response")?
        .to_string();
    let device_code = resp["device_code"]
        .as_str()
        .ok_or("Missing device_code in device code response")?
        .to_string();

    let interval = resp["interval"]
        .as_u64()
        .unwrap_or(provider.device_poll_interval_secs);

    println!("\nTo authenticate, visit:\n  {}", verification_uri);
    println!("And enter code: {}\n", user_code);

    open_url(&verification_uri);

    // Poll for token.
    loop {
        thread::sleep(Duration::from_secs(interval));

        let result: HashMap<String, serde_json::Value> = client
            .post(&provider.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", provider.client_id.as_str()),
                ("device_code", device_code.as_str()),
            ])
            .send()
            .map_err(|e| format!("Token polling request failed: {}", e))?
            .json()
            .map_err(|e| format!("Failed to parse token polling response: {}", e))?;

        if let Some(error) = result.get("error").and_then(|v| v.as_str()) {
            match error {
                "authorization_pending" => continue,
                "slow_down" => {
                    thread::sleep(Duration::from_secs(5));
                    continue;
                }
                _ => {
                    let desc = result
                        .get("error_description")
                        .and_then(|v| v.as_str())
                        .unwrap_or(error);
                    return Err(format!("Device code auth failed: {}", desc));
                }
            }
        }

        return parse_token_response(&result);
    }
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

/// Exchange an authorization code for tokens.
pub fn exchange_code_for_tokens(
    provider: &OAuthProviderSpec,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokens, String> {
    let client = build_http_client(provider)?;

    let resp: HashMap<String, serde_json::Value> = client
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", provider.client_id.as_str()),
            ("code_verifier", code_verifier),
        ])
        .send()
        .map_err(|e| format!("Token exchange request failed: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    // Check for OAuth error.
    if let Some(error) = resp
        .get("error")
        .and_then(|v: &serde_json::Value| v.as_str())
    {
        let desc = resp
            .get("error_description")
            .and_then(|v: &serde_json::Value| v.as_str())
            .unwrap_or(error);
        return Err(format!("Token exchange error: {}", desc));
    }

    parse_token_response(&resp)
}

// ---------------------------------------------------------------------------
// Token persistence
// ---------------------------------------------------------------------------

/// Save tokens to a JSON file.
pub fn save_tokens(tokens: &OAuthTokens, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create token directory: {}", e))?;
    }
    let json = serde_json::to_string_pretty(tokens)
        .map_err(|e| format!("Failed to serialize tokens: {}", e))?;
    fs::write(path, json).map_err(|e| format!("Failed to write token file: {}", e))?;
    Ok(())
}

/// Load tokens from a JSON file.
pub fn load_tokens(path: &Path) -> Result<OAuthTokens, String> {
    let data = fs::read_to_string(path).map_err(|e| format!("Failed to read token file: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to parse token file: {}", e))
}

// ---------------------------------------------------------------------------
// Browser URL opening
// ---------------------------------------------------------------------------

/// Open a URL in the user's default browser.
pub fn open_url(url: &str) {
    let _ = Command::new("xdg-open")
        .arg(url)
        .spawn()
        .or_else(|_| Command::new("open").arg(url).spawn())
        .or_else(|_| Command::new("cmd.exe").args(["/c", "start", url]).spawn());
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn build_http_client(provider: &OAuthProviderSpec) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(provider.http_timeout_secs))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

/// Extract a query parameter value from a raw HTTP request.
///
/// The request looks like `GET /callback?code=xxx&state=yyy HTTP/1.1\r\n...`.
/// We parse only the first line's query portion.
fn parse_callback_query_param(request: &str, key: &str) -> Option<String> {
    // First line: GET /path?query HTTP/1.1
    let first_line = request.lines().next()?;
    let path_part = first_line.split(' ').nth(1)?;
    let query_string = path_part.split('?').nth(1)?;
    for pair in query_string.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next()?;
        let v = kv.next().unwrap_or("");
        if k == key {
            return Some(percent_decode(v.to_string()));
        }
    }
    None
}

/// Minimal percent-decode for query parameter values.
fn percent_decode(input: String) -> String {
    let mut result = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                result.push(byte);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_default()
}

/// Parse the JSON response from a token endpoint into [`OAuthTokens`].
fn parse_token_response(resp: &HashMap<String, serde_json::Value>) -> Result<OAuthTokens, String> {
    let access_token = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("Missing access_token in response")?
        .to_string();

    Ok(OAuthTokens {
        access_token,
        refresh_token: resp
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(String::from),
        id_token: resp
            .get("id_token")
            .and_then(|v| v.as_str())
            .map(String::from),
        token_type: resp
            .get("token_type")
            .and_then(|v| v.as_str())
            .unwrap_or("Bearer")
            .to_string(),
        scope: resp.get("scope").and_then(|v| v.as_str()).map(String::from),
        expires_in: resp.get("expires_in").and_then(|v| v.as_u64()),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_pkce() {
        let pkce = generate_pkce();
        assert!(!pkce.code_verifier.is_empty(), "verifier must not be empty");
        assert!(
            !pkce.code_challenge.is_empty(),
            "challenge must not be empty"
        );
        assert_ne!(
            pkce.code_verifier, pkce.code_challenge,
            "verifier and challenge must differ"
        );

        // Verify challenge = SHA256(verifier) base64url.
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.code_verifier.as_bytes()));
        assert_eq!(pkce.code_challenge, expected);
    }

    #[test]
    fn test_generate_state() {
        let state1 = generate_state();
        let state2 = generate_state();
        assert_eq!(state1.len(), 43, "32 bytes -> 43 base64url chars");
        assert_ne!(state1, state2, "states must be random");
    }

    #[test]
    fn test_save_and_load_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let tokens = OAuthTokens {
            access_token: "at_test123".to_string(),
            refresh_token: Some("rt_test456".to_string()),
            id_token: None,
            token_type: "Bearer".to_string(),
            scope: Some("openid email".to_string()),
            expires_in: Some(3600),
        };

        save_tokens(&tokens, &path).unwrap();
        assert!(path.exists(), "token file should exist after save");

        let loaded = load_tokens(&path).unwrap();
        assert_eq!(loaded.access_token, "at_test123");
        assert_eq!(loaded.refresh_token, Some("rt_test456".to_string()));
        assert_eq!(loaded.id_token, None);
        assert_eq!(loaded.token_type, "Bearer");
        assert_eq!(loaded.scope, Some("openid email".to_string()));
        assert_eq!(loaded.expires_in, Some(3600));
    }

    #[test]
    fn test_oauth_tokens_serialize() {
        let tokens = OAuthTokens {
            access_token: "at_abc".to_string(),
            refresh_token: Some("rt_def".to_string()),
            id_token: None,
            token_type: "Bearer".to_string(),
            scope: None,
            expires_in: None,
        };

        let json = serde_json::to_string_pretty(&tokens).unwrap();

        // access_token and refresh_token present, id_token/scope/expires_in absent.
        assert!(json.contains("\"access_token\": \"at_abc\""));
        assert!(json.contains("\"refresh_token\": \"rt_def\""));
        assert!(!json.contains("id_token"));
        assert!(!json.contains("expires_in"));

        // Round-trip.
        let deserialized: OAuthTokens = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.access_token, "at_abc");
        assert_eq!(deserialized.refresh_token, Some("rt_def".to_string()));
        assert_eq!(deserialized.id_token, None);
    }
}
