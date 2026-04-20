//! Free API key registration via the `aish_freekey_bin` standalone binary.
//!
//! The binary provides three commands:
//!   `fp`  — SHA256 device fingerprint
//!   `loc` — JSON `{ "location": "cn" | "overseas" }`
//!   `reg` — JSON registration result (key, base, model)

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use aish_core::AishError;

/// Sentinel value indicating the user chose to fall back to manual setup.
pub const FALLBACK_TO_MANUAL: &str = "FALLBACK_TO_MANUAL";

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Result of a free key registration attempt.
#[derive(Debug, Clone)]
pub struct FreeKeyResult {
    pub success: bool,
    pub api_key: String,
    pub api_base: String,
    pub model: String,
    pub already_registered: bool,
    pub error_message: Option<String>,
}

impl FreeKeyResult {
    fn from_json(val: &serde_json::Value) -> Self {
        Self {
            success: val
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            api_key: val
                .get("api_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            api_base: val
                .get("api_base")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            model: val
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            already_registered: val
                .get("already_registered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            error_message: val
                .get("error_message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

const BINARY_NAME: &str = "aish_freekey_bin";

/// Find the standalone `aish_freekey_bin` binary.
///
/// Searches in:
/// 1. `PATH` environment
/// 2. `~/.local/bin/`
/// 3. `/usr/local/bin/`
pub fn find_freekey_binary() -> Option<PathBuf> {
    // 1. Check PATH via `which`-like lookup
    if let Ok(path) = which::which(BINARY_NAME) {
        return Some(path);
    }

    // 2. Common installation locations
    let candidates: Vec<PathBuf> = vec![
        dirs::home_dir().map(|h| h.join(".local/bin").join(BINARY_NAME))?,
        PathBuf::from("/usr/local/bin").join(BINARY_NAME),
    ];

    candidates.into_iter().find(|candidate| candidate.exists())
}

/// Whether the free key module is available (binary found).
pub fn has_free_key_module() -> bool {
    find_freekey_binary().is_some()
}

// ---------------------------------------------------------------------------
// Binary invocation helpers
// ---------------------------------------------------------------------------

fn run_binary(binary: &Path, args: &[&str]) -> Result<String, AishError> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|e| AishError::Config(format!("Failed to run {}: {}", binary.display(), e)))?;

    if !output.status.success() {
        return Err(AishError::Config(format!(
            "{} exited with code {}",
            binary.display(),
            output.status.code().unwrap_or(-1)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_binary_json(binary: &Path, args: &[&str]) -> Result<serde_json::Value, AishError> {
    let stdout = run_binary(binary, args)?;
    if stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&stdout)
        .map_err(|e| AishError::Config(format!("Invalid JSON from {}: {}", binary.display(), e)))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a SHA256 device fingerprint via the binary.
pub fn generate_device_fingerprint() -> Result<String, AishError> {
    let binary = find_freekey_binary()
        .ok_or_else(|| AishError::Config("aish_freekey_bin not found".into()))?;
    run_binary(&binary, &["fp"])
}

/// Detect the user's geo location via the binary.
///
/// Returns `"cn"` or `"overseas"`.
pub fn detect_geo_location() -> String {
    match find_freekey_binary() {
        Some(binary) => {
            let result = run_binary_json(&binary, &["loc"]);
            match result {
                Ok(val) => val
                    .get("location")
                    .and_then(|v| v.as_str())
                    .unwrap_or("overseas")
                    .to_string(),
                Err(_) => detect_geo_location_fallback(),
            }
        }
        None => detect_geo_location_fallback(),
    }
}

/// Fallback geo detection via HTTP when the binary is unavailable.
fn detect_geo_location_fallback() -> String {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => return "overseas".to_string(),
    };

    match client
        .get("http://ip-api.com/line/?fields=countryCode")
        .send()
    {
        Ok(resp) => {
            let text = resp.text().unwrap_or_default().trim().to_uppercase();
            if text == "CN" {
                "cn".to_string()
            } else {
                "overseas".to_string()
            }
        }
        Err(_) => "overseas".to_string(),
    }
}

/// Register for a free API key via the binary.
pub fn register_free_key() -> Result<FreeKeyResult, AishError> {
    let binary = find_freekey_binary()
        .ok_or_else(|| AishError::Config("aish_freekey_bin not found".into()))?;
    let val = run_binary_json(&binary, &["reg"])?;

    if val.is_null() {
        return Err(AishError::Config(
            "Failed to communicate with registration service".into(),
        ));
    }

    Ok(FreeKeyResult::from_json(&val))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_geo_location_returns_string() {
        let location = detect_geo_location();
        assert!(location == "cn" || location == "overseas");
    }

    #[test]
    fn test_fallback_sentinel() {
        assert_eq!(FALLBACK_TO_MANUAL, "FALLBACK_TO_MANUAL");
    }

    #[test]
    fn test_free_key_result_from_json() {
        let json: serde_json::Value = serde_json::json!({
            "success": true,
            "api_key": "test-key",
            "api_base": "http://example.com/v1",
            "model": "gpt-4",
            "already_registered": false
        });
        let result = FreeKeyResult::from_json(&json);
        assert!(result.success);
        assert_eq!(result.api_key, "test-key");
        assert_eq!(result.api_base, "http://example.com/v1");
        assert_eq!(result.model, "gpt-4");
        assert!(!result.already_registered);
        assert!(result.error_message.is_none());
    }

    #[test]
    fn test_free_key_result_failure_json() {
        let json: serde_json::Value = serde_json::json!({
            "success": false,
            "error_message": "quota exhausted"
        });
        let result = FreeKeyResult::from_json(&json);
        assert!(!result.success);
        assert_eq!(result.error_message.as_deref(), Some("quota exhausted"));
    }
}
