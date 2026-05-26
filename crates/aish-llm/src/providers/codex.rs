//! OpenAI Codex provider adapter.
//!
//! Implements the Codex-specific Responses API with OAuth browser/device-code
//! login, token refresh, SSE stream parsing, and request/response conversion
//! between the OpenAI Chat Completions format and Codex's native format.

use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use super::types::{ProviderAdapter, ProviderAuthConfig, ProviderCapabilities, ProviderMetadata};
use crate::oauth::{login_with_browser, login_with_device_code, OAuthProviderSpec, OAuthTokens};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const CODEX_PROVIDER: &str = "openai-codex";
pub const CODEX_DEFAULT_MODEL: &str = "gpt-5.4";
pub const CODEX_DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_AUTH_ISSUER: &str = "https://auth.openai.com";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_DEFAULT_CALLBACK_PORT: u16 = 1455;
#[allow(dead_code)]
const CODEX_BROWSER_LOGIN_TIMEOUT_SECS: u64 = 300;
#[allow(dead_code)]
const CODEX_DEVICE_CODE_TIMEOUT_SECS: u64 = 900;
const CODEX_MAX_REQUEST_ATTEMPTS: u32 = 5;
const CODEX_REFRESH_LEEWAY_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CodexError {
    Auth(String),
    Request(String),
    Stream(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Http(String),
}

impl std::fmt::Display for CodexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodexError::Auth(s) => write!(f, "Auth error: {s}"),
            CodexError::Request(s) => write!(f, "Request error: {s}"),
            CodexError::Stream(s) => write!(f, "Stream error: {s}"),
            CodexError::Io(e) => write!(f, "IO error: {e}"),
            CodexError::Json(e) => write!(f, "JSON error: {e}"),
            CodexError::Http(s) => write!(f, "HTTP error: {s}"),
        }
    }
}

impl std::error::Error for CodexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CodexError::Io(e) => Some(e),
            CodexError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CodexError {
    fn from(e: std::io::Error) -> Self {
        CodexError::Io(e)
    }
}

impl From<serde_json::Error> for CodexError {
    fn from(e: serde_json::Error) -> Self {
        CodexError::Json(e)
    }
}

// ---------------------------------------------------------------------------
// Auth state types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodexAuthState {
    #[serde(skip_serializing)]
    pub auth_path: PathBuf,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: String,
    pub expires_at: Option<i64>,
}

impl CodexAuthState {
    /// Check if the token needs refresh (within leeway of expiry).
    pub fn needs_refresh(&self) -> bool {
        match self.expires_at {
            None => false,
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                now >= (exp - CODEX_REFRESH_LEEWAY_SECS)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// JWT helpers
// ---------------------------------------------------------------------------

/// Decode JWT payload claims (no signature verification).
fn decode_jwt_claims(token: &str) -> HashMap<String, Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return HashMap::new();
    }
    let payload = parts[1];
    // Base64url decode - standard base64 engine works with URL-safe alphabet.
    match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) {
        Ok(bytes) => serde_json::from_slice::<HashMap<String, Value>>(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Extract account_id from JWT claims (checks multiple claim paths).
fn extract_account_id(claims: &HashMap<String, Value>) -> Option<String> {
    coerce_str(claims.get("chatgpt_account_id"))
        .or_else(|| coerce_str(claims.get("account_id")))
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|v| v.as_object())
                .and_then(|obj| coerce_str(obj.get("chatgpt_account_id")))
        })
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|v| v.as_object())
                .and_then(|obj| coerce_str(obj.get("account_id")))
        })
}

fn coerce_str(val: Option<&Value>) -> Option<String> {
    val.and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn coerce_int(val: Option<&Value>) -> Option<i64> {
    val.and_then(|v| v.as_i64()).filter(|&v| v > 0)
}

// ---------------------------------------------------------------------------
// Auth path resolution
// ---------------------------------------------------------------------------

/// Resolve the auth.json path for Codex tokens.
pub fn resolve_codex_auth_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(env_path) = std::env::var("AISH_CODEX_AUTH_PATH") {
        return PathBuf::from(env_path);
    }
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        return PathBuf::from(codex_home).join("auth.json");
    }
    // Fallback to ~/.codex/auth.json
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".codex").join("auth.json")
}

// ---------------------------------------------------------------------------
// Token persistence
// ---------------------------------------------------------------------------

/// Persist Codex auth state to disk as JSON.
pub fn persist_codex_tokens(
    auth_path: &Path,
    access_token: &str,
    refresh_token: Option<&str>,
    account_id: &str,
    id_token: Option<&str>,
) -> Result<(), CodexError> {
    // Read existing payload or start fresh.
    let mut payload: Value = match std::fs::read_to_string(auth_path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };

    // Set auth mode.
    payload["auth_mode"] = Value::String("chatgpt".to_string());
    payload.as_object_mut().map(|o| o.remove("OPENAI_API_KEY"));

    // Set tokens.
    let tokens = payload
        .as_object_mut()
        .unwrap()
        .entry("tokens")
        .or_insert_with(|| serde_json::json!({}));
    tokens["access_token"] = Value::String(access_token.to_string());
    if let Some(rt) = refresh_token {
        tokens["refresh_token"] = Value::String(rt.to_string());
    }
    tokens["account_id"] = Value::String(account_id.to_string());
    if let Some(idt) = id_token {
        tokens["id_token"] = Value::String(idt.to_string());
    }

    // Add last_refresh timestamp (ISO 8601).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    payload["last_refresh"] = Value::String(format!("{now}"));

    // Write file.
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&payload)?;
    std::fs::write(auth_path, json)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Auth loading
// ---------------------------------------------------------------------------

/// Load Codex auth state from disk.
pub fn load_codex_auth(auth_path: Option<&Path>) -> Result<CodexAuthState, CodexError> {
    let path = resolve_codex_auth_path(auth_path);
    if !path.exists() {
        return Err(CodexError::Auth(format!(
            "Codex auth file not found: {}",
            path.display()
        )));
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|e| CodexError::Auth(format!("Failed to read auth file: {e}")))?;
    let payload: Value = serde_json::from_str(&data)
        .map_err(|e| CodexError::Auth(format!("Failed to parse auth file: {e}")))?;

    let tokens = payload
        .get("tokens")
        .ok_or_else(|| CodexError::Auth("Missing 'tokens' in auth file".to_string()))?;

    let access_token = coerce_str(tokens.get("access_token"))
        .ok_or_else(|| CodexError::Auth("Missing access_token".to_string()))?;
    let refresh_token = coerce_str(tokens.get("refresh_token"));
    let account_id_from_tokens = coerce_str(tokens.get("account_id"));

    // Extract account_id from JWT claims if not in tokens.
    let id_token_claims = tokens
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(decode_jwt_claims)
        .unwrap_or_default();
    let access_claims = decode_jwt_claims(&access_token);

    let account_id = account_id_from_tokens
        .or_else(|| extract_account_id(&id_token_claims))
        .or_else(|| extract_account_id(&access_claims))
        .ok_or_else(|| CodexError::Auth("Missing account_id".to_string()))?;

    let expires_at =
        coerce_int(access_claims.get("exp")).or_else(|| coerce_int(id_token_claims.get("exp")));

    Ok(CodexAuthState {
        auth_path: path,
        access_token,
        refresh_token,
        account_id,
        expires_at,
    })
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

/// Refresh the Codex access token using the refresh token.
pub async fn refresh_codex_auth(auth: &CodexAuthState) -> Result<CodexAuthState, CodexError> {
    let refresh_token = auth
        .refresh_token
        .as_deref()
        .ok_or_else(|| CodexError::Auth("No refresh token available".to_string()))?;

    let client = reqwest::Client::new();
    let resp = client
        .post(CODEX_REFRESH_URL)
        .json(&serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| CodexError::Request(format!("Refresh request failed: {e}")))?;

    if resp.status().as_u16() == 401 {
        return Err(CodexError::Auth(
            "Session no longer valid. Re-run auth setup.".to_string(),
        ));
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(CodexError::Request(format!(
            "Refresh failed: {status} {text}"
        )));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| CodexError::Request(format!("Invalid refresh response: {e}")))?;

    let new_access = coerce_str(body.get("access_token"))
        .ok_or_else(|| CodexError::Auth("No access_token in refresh response".to_string()))?;
    let new_refresh = coerce_str(body.get("refresh_token")).or(auth.refresh_token.clone());

    let id_token_claims = body
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(decode_jwt_claims)
        .unwrap_or_default();
    let access_claims = decode_jwt_claims(&new_access);

    let account_id = extract_account_id(&id_token_claims)
        .or_else(|| extract_account_id(&access_claims))
        .unwrap_or_else(|| auth.account_id.clone());
    let expires_at = coerce_int(access_claims.get("exp"));

    persist_codex_tokens(
        &auth.auth_path,
        &new_access,
        new_refresh.as_deref(),
        &account_id,
        body.get("id_token").and_then(|v| v.as_str()),
    )?;

    Ok(CodexAuthState {
        auth_path: auth.auth_path.clone(),
        access_token: new_access,
        refresh_token: new_refresh,
        account_id,
        expires_at,
    })
}

// ---------------------------------------------------------------------------
// OAuth provider spec
// ---------------------------------------------------------------------------

/// Build the Codex-specific OAuth provider spec.
pub fn codex_oauth_provider() -> OAuthProviderSpec {
    OAuthProviderSpec {
        provider_id: CODEX_PROVIDER.to_string(),
        display_name: "OpenAI Codex".to_string(),
        client_id: CODEX_CLIENT_ID.to_string(),
        scope: CODEX_OAUTH_SCOPE.to_string(),
        authorize_url: CODEX_AUTHORIZE_URL.to_string(),
        token_url: CODEX_REFRESH_URL.to_string(),
        device_authorization_url: Some(format!(
            "{}/api/accounts/deviceauth/usercode",
            CODEX_AUTH_ISSUER
        )),
        extra_query_params: vec![
            ("id_token_add_organizations".to_string(), "true".to_string()),
            ("codex_cli_simplified_flow".to_string(), "true".to_string()),
            ("originator".to_string(), CODEX_ORIGINATOR.to_string()),
        ],
        http_timeout_secs: 30,
        device_poll_interval_secs: 5,
    }
}

// ---------------------------------------------------------------------------
// Browser login
// ---------------------------------------------------------------------------

/// Login via browser OAuth flow.
pub fn login_codex_browser(
    auth_path: Option<&Path>,
    open_browser: bool,
) -> Result<CodexAuthState, CodexError> {
    let path = resolve_codex_auth_path(auth_path);
    let provider = codex_oauth_provider();
    let tokens = login_with_browser(&provider, CODEX_DEFAULT_CALLBACK_PORT, open_browser)
        .map_err(|e| CodexError::Auth(e))?;

    let account_id = extract_account_id_from_tokens(&tokens)?;
    persist_codex_tokens(
        &path,
        &tokens.access_token,
        tokens.refresh_token.as_deref(),
        &account_id,
        tokens.id_token.as_deref(),
    )?;

    let access_claims = decode_jwt_claims(&tokens.access_token);
    let expires_at = coerce_int(access_claims.get("exp"));

    Ok(CodexAuthState {
        auth_path: path,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id,
        expires_at,
    })
}

/// Login via device code flow.
pub fn login_codex_device_code(auth_path: Option<&Path>) -> Result<CodexAuthState, CodexError> {
    let path = resolve_codex_auth_path(auth_path);
    let provider = codex_oauth_provider();
    let tokens = login_with_device_code(&provider).map_err(|e| CodexError::Auth(e))?;

    let account_id = extract_account_id_from_tokens(&tokens)?;
    persist_codex_tokens(
        &path,
        &tokens.access_token,
        tokens.refresh_token.as_deref(),
        &account_id,
        tokens.id_token.as_deref(),
    )?;

    let access_claims = decode_jwt_claims(&tokens.access_token);
    let expires_at = coerce_int(access_claims.get("exp"));

    Ok(CodexAuthState {
        auth_path: path,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id,
        expires_at,
    })
}

fn extract_account_id_from_tokens(tokens: &OAuthTokens) -> Result<String, CodexError> {
    let id_claims = tokens
        .id_token
        .as_deref()
        .map(decode_jwt_claims)
        .unwrap_or_default();
    let access_claims = decode_jwt_claims(&tokens.access_token);
    extract_account_id(&id_claims)
        .or_else(|| extract_account_id(&access_claims))
        .ok_or_else(|| CodexError::Auth("Missing account_id in tokens".to_string()))
}

// ---------------------------------------------------------------------------
// Request builder
// ---------------------------------------------------------------------------

/// Build request body for the Codex Responses API.
pub fn build_codex_request(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    tool_choice: &str,
) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input_items: Vec<Value> = Vec::new();

    for msg in messages {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        match role.as_str() {
            "system" => {
                if let Some(content) = coerce_message_text(msg.get("content")) {
                    instructions.push(content);
                }
            }
            "user" => {
                if let Some(content) = coerce_message_text(msg.get("content")) {
                    input_items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": content}],
                    }));
                }
            }
            "assistant" => {
                if let Some(content) = coerce_message_text(msg.get("content")) {
                    input_items.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": content}],
                    }));
                }
                // Convert tool_calls.
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let function = tc.get("function").unwrap_or(&Value::Null);
                        let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let arguments = function
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        if !name.is_empty() && !call_id.is_empty() {
                            input_items.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": name,
                                "arguments": arguments,
                            }));
                        }
                    }
                }
            }
            "tool" => {
                let call_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !call_id.is_empty() {
                    let content = coerce_message_text(msg.get("content")).unwrap_or_default();
                    input_items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content,
                    }));
                }
            }
            _ => {}
        }
    }

    let instructions_str = instructions
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");

    let converted_tools = convert_tools_for_codex(tools.unwrap_or(&[]));

    serde_json::json!({
        "model": strip_codex_prefix(model),
        "instructions": instructions_str,
        "input": input_items,
        "tools": converted_tools,
        "tool_choice": tool_choice,
        "parallel_tool_calls": true,
        "store": false,
        "stream": true,
        "include": [],
    })
}

fn coerce_message_text(val: Option<&Value>) -> Option<String> {
    match val {
        None => None,
        Some(Value::String(s)) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Some(Value::Array(arr)) => {
            let chunks: Vec<String> = arr
                .iter()
                .filter_map(|item| match item {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(obj) => obj
                        .get("text")
                        .or_else(|| obj.get("content"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect();
            if chunks.is_empty() {
                None
            } else {
                Some(chunks.join("\n"))
            }
        }
        _ => None,
    }
}

fn convert_tools_for_codex(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
                return None;
            }
            let function = tool.get("function")?;
            let name = function.get("name").and_then(|v| v.as_str())?;
            let description = function
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let parameters = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
            Some(serde_json::json!({
                "type": "function",
                "name": name,
                "description": description,
                "parameters": parameters,
            }))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Response converter
// ---------------------------------------------------------------------------

/// Convert a Codex Responses API payload to OpenAI Chat Completion format.
pub fn convert_codex_response(payload: &Value) -> Value {
    let output = payload
        .get("output")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut content_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for item in &output {
        let item_type = item
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        match item_type.as_str() {
            "message" => {
                if let Some(text) = extract_response_text(item.get("content")) {
                    content_parts.push(text);
                }
            }
            "function_call" => {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let arguments = item.get("arguments").cloned().unwrap_or(Value::Null);
                if !name.is_empty() && !call_id.is_empty() {
                    let args_str = match &arguments {
                        Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    tool_calls.push(serde_json::json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args_str,
                        },
                    }));
                }
            }
            _ => {}
        }
    }

    let content = content_parts.join("\n");
    let finish_reason = if tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };

    let mut message = serde_json::json!({
        "role": "assistant",
        "content": content,
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    serde_json::json!({
        "choices": [{
            "message": message,
            "finish_reason": finish_reason,
        }]
    })
}

fn extract_response_text(content: Option<&Value>) -> Option<String> {
    let arr = content.and_then(|v| v.as_array())?;
    let chunks: Vec<String> = arr
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(item_type, "output_text" | "input_text") {
                item.get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// SSE stream collection
// ---------------------------------------------------------------------------

/// Collect SSE events from a Codex streaming response into a single payload.
pub fn collect_codex_stream(events: &[(String, Value)]) -> Result<Value, CodexError> {
    let mut output_items: Vec<Value> = Vec::new();
    let mut text_deltas: Vec<String> = Vec::new();
    let mut response_id: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut completed = false;

    for (event_type, payload) in events {
        match event_type.as_str() {
            "response.output_item.done" => {
                if let Some(item) = payload.get("item").cloned() {
                    output_items.push(item);
                }
            }
            "response.output_text.delta" => {
                if let Some(delta) = payload.get("delta").and_then(|v| v.as_str()) {
                    text_deltas.push(delta.to_string());
                }
            }
            "response.failed" => {
                return Err(CodexError::Stream(extract_stream_failure(payload)));
            }
            "response.incomplete" => {
                let reason = payload
                    .get("response")
                    .and_then(|v| v.get("incomplete_details"))
                    .and_then(|v| v.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(CodexError::Stream(format!("Incomplete response: {reason}")));
            }
            "response.completed" => {
                if let Some(resp) = payload.get("response") {
                    response_id = resp
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or(response_id);
                    usage = resp.get("usage").cloned();
                    if output_items.is_empty() {
                        if let Some(output) = resp.get("output").and_then(|v| v.as_array()) {
                            output_items =
                                output.iter().filter(|v| v.is_object()).cloned().collect();
                        }
                    }
                }
                completed = true;
                break;
            }
            _ => {}
        }
    }

    if !completed {
        return Err(CodexError::Stream(
            "Stream ended before response.completed".to_string(),
        ));
    }

    // Fallback: if no output items but we have text deltas.
    if output_items.is_empty() && !text_deltas.is_empty() {
        output_items.push(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text_deltas.join("")}],
        }));
    }

    let mut result = serde_json::json!({"output": output_items});
    if let Some(id) = response_id {
        result["id"] = Value::String(id);
    }
    if let Some(u) = usage {
        result["usage"] = u;
    }
    Ok(result)
}

fn extract_stream_failure(payload: &Value) -> String {
    payload
        .get("response")
        .and_then(|v| v.get("error"))
        .and_then(|v| v.get("message").or_else(|| v.get("code")))
        .and_then(|v| v.as_str())
        .unwrap_or("Stream failed")
        .to_string()
}

// ---------------------------------------------------------------------------
// Request headers
// ---------------------------------------------------------------------------

fn build_headers(auth: &CodexAuthState) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(val) = format!("Bearer {}", auth.access_token).parse() {
        headers.insert(reqwest::header::AUTHORIZATION, val);
    }
    if let Ok(val) = auth.account_id.parse() {
        headers.insert("ChatGPT-Account-ID", val);
    }
    if let Ok(val) = "application/json".parse() {
        headers.insert(reqwest::header::CONTENT_TYPE, val);
    }
    let ua = format!(
        "{}/0.0.0 (aish; Rust; {} {})",
        CODEX_ORIGINATOR,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if let Ok(val) = ua.parse() {
        headers.insert(reqwest::header::USER_AGENT, val);
    }
    if let Ok(val) = CODEX_ORIGINATOR.parse() {
        headers.insert("originator", val);
    }
    headers
}

// ---------------------------------------------------------------------------
// Chat completion
// ---------------------------------------------------------------------------

/// Create a chat completion using the Codex Responses API.
pub async fn create_codex_chat_completion(
    model: &str,
    messages: &[Value],
    tools: Option<&[Value]>,
    tool_choice: &str,
    api_base: Option<&str>,
    auth_path: Option<&Path>,
    timeout_secs: u64,
) -> Result<Value, CodexError> {
    let base_url = resolve_codex_base_url(api_base);
    let url = format!("{}/responses", base_url);

    let request_body = build_codex_request(model, messages, tools, tool_choice);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| CodexError::Request(format!("Failed to build HTTP client: {e}")))?;

    let mut auth = load_codex_auth(auth_path)?;

    // Auto-refresh if needed.
    if auth.needs_refresh() {
        debug!("Refreshing Codex auth token");
        auth = refresh_codex_auth(&auth).await?;
    }

    let mut auth_refresh_attempted = false;

    for attempt in 0..CODEX_MAX_REQUEST_ATTEMPTS {
        debug!(attempt, "Sending Codex request");
        let result = client
            .post(&url)
            .headers(build_headers(&auth))
            .json(&request_body)
            .send()
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status();

                // Try refresh on 401.
                if status.as_u16() == 401 && auth.refresh_token.is_some() && !auth_refresh_attempted
                {
                    auth_refresh_attempted = true;
                    match refresh_codex_auth(&auth).await {
                        Ok(new_auth) => auth = new_auth,
                        Err(e) => warn!("Auth refresh failed: {e}"),
                    }
                    continue;
                }

                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(CodexError::Http(format!("{status} {text}")));
                }

                // Parse response.
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_lowercase();

                if content_type.contains("text/event-stream") || content_type.is_empty() {
                    // SSE streaming response — collect all chunks.
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| CodexError::Stream(format!("Failed to read stream: {e}")))?;
                    let events = parse_sse_text(&text);
                    let payload = collect_codex_stream(&events)?;
                    return Ok(convert_codex_response(&payload));
                } else {
                    // JSON response.
                    let body: Value = resp
                        .json()
                        .await
                        .map_err(|e| CodexError::Request(format!("Invalid JSON response: {e}")))?;
                    return Ok(convert_codex_response(&body));
                }
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                warn!(attempt, "Transport error: {e}");
                if attempt + 1 >= CODEX_MAX_REQUEST_ATTEMPTS {
                    return Err(CodexError::Request(format!(
                        "Failed after {CODEX_MAX_REQUEST_ATTEMPTS} attempts: {e}"
                    )));
                }
                continue;
            }
            Err(e) => {
                return Err(CodexError::Request(format!("Request failed: {e}")));
            }
        }
    }

    Err(CodexError::Request(
        "Failed after all retry attempts".to_string(),
    ))
}

/// Parse SSE text into a list of (event_type, payload) tuples.
fn parse_sse_text(text: &str) -> Vec<(String, Value)> {
    let mut events = Vec::new();
    let mut event_type: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(parsed) = parse_sse_event(event_type.take(), &data_lines) {
                events.push(parsed);
            }
            data_lines.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some((field, value)) = line.split_once(':') {
            let value = value.trim_start_matches(' ');
            if field == "event" {
                event_type = Some(value.to_string());
            } else if field == "data" {
                data_lines.push(value.to_string());
            }
        }
    }

    if let Some(parsed) = parse_sse_event(event_type, &data_lines) {
        events.push(parsed);
    }

    events
}

fn parse_sse_event(event_type: Option<String>, data_lines: &[String]) -> Option<(String, Value)> {
    if data_lines.is_empty() {
        return None;
    }
    let data = data_lines.join("\n").trim().to_string();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let payload: Value = serde_json::from_str(&data).ok()?;
    if !payload.is_object() {
        return None;
    }
    let resolved_type = payload
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(event_type)?;
    Some((resolved_type, payload))
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Check if a model name indicates Codex usage.
pub fn is_codex_model(model: &str) -> bool {
    model
        .trim()
        .to_lowercase()
        .starts_with(&format!("{}/", CODEX_PROVIDER))
}

/// Strip the "openai-codex/" prefix from a model name.
pub fn strip_codex_prefix(model: &str) -> &str {
    if is_codex_model(model) {
        model
            .split_once('/')
            .map(|(_, rest)| rest.trim())
            .unwrap_or(model)
    } else {
        model.trim()
    }
}

/// Resolve the Codex API base URL.
pub fn resolve_codex_base_url(api_base: Option<&str>) -> String {
    let trimmed = api_base
        .map(|s| s.trim().trim_end_matches('/'))
        .unwrap_or("");
    if trimmed.is_empty() {
        return CODEX_DEFAULT_BASE_URL.to_string();
    }
    // Simple URL parsing without the url crate.
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let host = without_scheme.split('/').next().unwrap_or("");
    let host_lower = host.to_lowercase();
    if host_lower == "chatgpt.com" || host_lower == "chat.openai.com" {
        let scheme = if trimmed.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        return format!("{scheme}://{host}/backend-api/codex");
    }
    CODEX_DEFAULT_BASE_URL.to_string()
}

// ---------------------------------------------------------------------------
// Provider adapter
// ---------------------------------------------------------------------------

/// Codex provider adapter for the provider registry.
pub struct CodexProviderAdapter;

impl ProviderAdapter for CodexProviderAdapter {
    fn provider_id(&self) -> &str {
        CODEX_PROVIDER
    }

    fn display_name(&self) -> &str {
        "OpenAI Codex"
    }

    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            provider_id: CODEX_PROVIDER.to_string(),
            display_name: "OpenAI Codex".to_string(),
            dashboard_url: Some("https://codex.ai/settings".to_string()),
            api_key_env_var: "AISH_CODEX_AUTH_PATH".to_string(),
            supports_streaming: true,
            supports_tools: true,
            uses_custom_client: true,
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_streaming: true,
            supports_tools: true,
            should_trim_messages: false,
        }
    }

    fn matches_model(&self, model: &str) -> bool {
        is_codex_model(model)
    }

    fn matches_api_base(&self, api_base: &str) -> bool {
        api_base.contains("chatgpt.com") || api_base.contains("chat.openai.com")
    }

    fn auth_config(&self) -> Option<ProviderAuthConfig> {
        Some(ProviderAuthConfig {
            auth_path_config_key: "codex_auth_path".to_string(),
            default_model: CODEX_DEFAULT_MODEL.to_string(),
            supported_flows: vec![
                "browser".to_string(),
                "device-code".to_string(),
                "codex-cli".to_string(),
            ],
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn test_is_codex_model() {
        assert!(is_codex_model("openai-codex/gpt-5.4"));
        assert!(is_codex_model("OpenAI-Codex/gpt-4"));
        assert!(!is_codex_model("gpt-4o"));
        assert!(!is_codex_model("openai/gpt-4o"));
    }

    #[test]
    fn test_strip_codex_prefix() {
        assert_eq!(strip_codex_prefix("openai-codex/gpt-5.4"), "gpt-5.4");
        assert_eq!(strip_codex_prefix("gpt-4o"), "gpt-4o");
        assert_eq!(strip_codex_prefix("openai-codex/codex-mini"), "codex-mini");
    }

    #[test]
    fn test_resolve_codex_base_url_default() {
        assert_eq!(
            resolve_codex_base_url(None),
            "https://chatgpt.com/backend-api/codex"
        );
        assert_eq!(
            resolve_codex_base_url(Some("")),
            "https://chatgpt.com/backend-api/codex"
        );
    }

    #[test]
    fn test_resolve_codex_base_url_chatgpt() {
        assert_eq!(
            resolve_codex_base_url(Some("https://chatgpt.com")),
            "https://chatgpt.com/backend-api/codex"
        );
    }

    #[test]
    fn test_resolve_codex_base_url_non_chatgpt() {
        // Non-chatgpt hosts fall back to default.
        assert_eq!(
            resolve_codex_base_url(Some("https://api.example.com")),
            "https://chatgpt.com/backend-api/codex"
        );
    }

    #[test]
    fn test_decode_jwt_claims() {
        // Create a simple JWT (header.payload.signature) with base64url payload.
        let payload = br#"{"sub":"user123","exp":1700000000}"#;
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        let token = format!("header.{}.signature", encoded);
        let claims = decode_jwt_claims(&token);
        assert_eq!(claims.get("sub").and_then(|v| v.as_str()), Some("user123"));
        assert_eq!(claims.get("exp").and_then(|v| v.as_i64()), Some(1700000000));
    }

    #[test]
    fn test_decode_jwt_claims_invalid() {
        let claims = decode_jwt_claims("not-a-jwt");
        assert!(claims.is_empty());
        let claims = decode_jwt_claims("");
        assert!(claims.is_empty());
    }

    #[test]
    fn test_extract_account_id() {
        let mut claims = HashMap::new();
        claims.insert(
            "chatgpt_account_id".to_string(),
            Value::String("acct-123".to_string()),
        );
        assert_eq!(extract_account_id(&claims), Some("acct-123".to_string()));
    }

    #[test]
    fn test_extract_account_id_nested() {
        let mut auth_obj = serde_json::Map::new();
        auth_obj.insert(
            "account_id".to_string(),
            Value::String("nested-acct".to_string()),
        );
        let mut claims = HashMap::new();
        claims.insert(
            "https://api.openai.com/auth".to_string(),
            Value::Object(auth_obj),
        );
        assert_eq!(extract_account_id(&claims), Some("nested-acct".to_string()));
    }

    #[test]
    fn test_build_codex_request_system() {
        let messages = vec![serde_json::json!({
            "role": "system",
            "content": "You are helpful."
        })];
        let req = build_codex_request("gpt-5.4", &messages, None, "auto");
        assert_eq!(req["instructions"], "You are helpful.");
        assert_eq!(req["model"], "gpt-5.4");
    }

    #[test]
    fn test_build_codex_request_conversation() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi there"}),
        ];
        let req = build_codex_request("gpt-5.4", &messages, None, "auto");
        let input = req["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn test_build_codex_request_tool_calls() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "run ls"}),
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": "{\"command\":\"ls\"}"
                    }
                }]
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_123",
                "content": "file1.txt\nfile2.txt"
            }),
        ];
        let req = build_codex_request("gpt-5.4", &messages, None, "auto");
        let input = req["input"].as_array().unwrap();
        // user message + function_call + function_call_output
        assert_eq!(input.len(), 3);
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
    }

    #[test]
    fn test_build_codex_request_tools() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run command",
                "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}
            }
        })];
        let req = build_codex_request("gpt-5.4", &[], Some(&tools), "auto");
        let converted = req["tools"].as_array().unwrap();
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["name"], "bash");
    }

    #[test]
    fn test_convert_codex_response_text() {
        let payload = serde_json::json!({
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Hello!"}]
                }
            ]
        });
        let result = convert_codex_response(&payload);
        let msg = &result["choices"][0]["message"];
        assert_eq!(msg["content"], "Hello!");
        assert_eq!(msg["role"], "assistant");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_convert_codex_response_tool_calls() {
        let payload = serde_json::json!({
            "output": [
                {
                    "type": "function_call",
                    "call_id": "call_abc",
                    "name": "bash",
                    "arguments": "{\"command\":\"ls\"}"
                }
            ]
        });
        let result = convert_codex_response(&payload);
        let msg = &result["choices"][0]["message"];
        let tool_calls = msg["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["function"]["name"], "bash");
        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn test_collect_codex_stream_completed() {
        let events = vec![
            (
                "response.output_text.delta".to_string(),
                serde_json::json!({"delta": "Hello"}),
            ),
            (
                "response.output_text.delta".to_string(),
                serde_json::json!({"delta": " World"}),
            ),
            (
                "response.completed".to_string(),
                serde_json::json!({
                    "response": {
                        "id": "resp_123",
                        "output": [
                            {
                                "type": "message",
                                "content": [{"type": "output_text", "text": "Hello World"}]
                            }
                        ],
                        "usage": {"input_tokens": 10, "output_tokens": 5}
                    }
                }),
            ),
        ];
        let result = collect_codex_stream(&events).unwrap();
        assert_eq!(result["id"], "resp_123");
        assert!(result.get("usage").is_some());
    }

    #[test]
    fn test_collect_codex_stream_failed() {
        let events = vec![(
            "response.failed".to_string(),
            serde_json::json!({
                "response": {
                    "error": {"message": "Rate limited"}
                }
            }),
        )];
        let result = collect_codex_stream(&events);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Rate limited"));
    }

    #[test]
    fn test_collect_codex_stream_incomplete() {
        let events = vec![(
            "response.incomplete".to_string(),
            serde_json::json!({
                "response": {
                    "incomplete_details": {"reason": "max_tokens"}
                }
            }),
        )];
        let result = collect_codex_stream(&events);
        assert!(result.is_err());
    }

    #[test]
    fn test_collect_codex_stream_not_completed() {
        let events: Vec<(String, Value)> = vec![(
            "response.output_item.done".to_string(),
            serde_json::json!({"item": {"type": "message"}}),
        )];
        let result = collect_codex_stream(&events);
        assert!(matches!(result, Err(CodexError::Stream(_))));
    }

    #[test]
    fn test_parse_sse_text() {
        let text = "event: response.output_text.delta\ndata: {\"delta\":\"Hi\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\"}}\n\n";
        let events = parse_sse_text(text);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "response.output_text.delta");
        assert_eq!(events[1].0, "response.completed");
    }

    #[test]
    fn test_parse_sse_text_done_marker() {
        let text = "event: response.completed\ndata: [DONE]\n\n";
        let events = parse_sse_text(text);
        assert!(events.is_empty());
    }

    #[test]
    fn test_codex_provider_adapter() {
        let adapter = CodexProviderAdapter;
        assert_eq!(adapter.provider_id(), "openai-codex");
        assert_eq!(adapter.display_name(), "OpenAI Codex");
        assert!(adapter.matches_model("openai-codex/gpt-5.4"));
        assert!(!adapter.matches_model("gpt-4o"));
        assert!(adapter.matches_api_base("https://chatgpt.com"));
        assert!(!adapter.matches_api_base("https://api.openai.com"));

        let meta = adapter.metadata();
        assert!(meta.uses_custom_client);
        assert!(meta.dashboard_url.is_some());

        let auth = adapter.auth_config().unwrap();
        assert_eq!(auth.default_model, "gpt-5.4");
        assert!(auth.supported_flows.contains(&"browser".to_string()));
    }

    #[test]
    fn test_needs_refresh() {
        let auth = CodexAuthState {
            auth_path: PathBuf::from("/tmp/test"),
            access_token: "test".to_string(),
            refresh_token: None,
            account_id: "acct".to_string(),
            expires_at: Some(1), // expired
        };
        assert!(auth.needs_refresh());

        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let auth_future = CodexAuthState {
            auth_path: PathBuf::from("/tmp/test"),
            access_token: "test".to_string(),
            refresh_token: None,
            account_id: "acct".to_string(),
            expires_at: Some(far_future),
        };
        assert!(!auth_future.needs_refresh());
    }
}
