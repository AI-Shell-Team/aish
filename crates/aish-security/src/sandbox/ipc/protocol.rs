use serde::{Deserialize, Serialize};

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::types::{SandboxLimits, SandboxResult, SandboxRunRequest};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SandboxIpcRequest {
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) repo_root: std::path::PathBuf,
    pub(crate) client_pid: u32,
    pub(crate) timeout_s: f64,
}

impl From<SandboxRunRequest> for SandboxIpcRequest {
    fn from(value: SandboxRunRequest) -> Self {
        Self {
            id: value.id,
            command: value.command,
            cwd: value.cwd,
            repo_root: value.repo_root,
            client_pid: value.client_pid,
            timeout_s: value.timeout_s,
        }
    }
}

impl SandboxIpcRequest {
    pub(crate) fn into_run_request(self, limits: SandboxLimits) -> SandboxRunRequest {
        SandboxRunRequest {
            id: self.id,
            command: self.command,
            cwd: self.cwd,
            repo_root: self.repo_root,
            client_pid: self.client_pid,
            timeout_s: limits.clamp_timeout_s(self.timeout_s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SandboxIpcSuccessResponse {
    pub(crate) id: String,
    pub(crate) ok: bool,
    pub(crate) result: SandboxResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SandboxIpcFailureResponse {
    pub(crate) id: String,
    pub(crate) ok: bool,
    pub(crate) reason: SandboxReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSandboxIpcResponse {
    id: Option<String>,
    ok: Option<bool>,
    result: Option<SandboxResult>,
    reason: Option<String>,
    error: Option<String>,
}

pub(crate) fn encode_request_line(
    request: &SandboxIpcRequest,
    limits: SandboxLimits,
) -> Result<Vec<u8>, SandboxError> {
    encode_line(
        request,
        limits.request_bytes,
        SandboxReason::RequestTooLarge,
    )
}

pub(crate) fn decode_request_line(
    raw: &[u8],
    limits: SandboxLimits,
) -> Result<SandboxRunRequest, SandboxError> {
    if raw.len() > limits.request_bytes {
        return Err(SandboxError::from_reason(SandboxReason::RequestTooLarge));
    }

    let line = first_line(raw);
    if line.is_empty() {
        return Err(SandboxError::with_details(
            SandboxReason::BadRequest,
            "empty_request",
        ));
    }

    let request: SandboxIpcRequest = serde_json::from_slice(line)
        .map_err(|_| SandboxError::with_details(SandboxReason::BadRequest, "invalid_json"))?;

    if request.id.is_empty() {
        return Err(SandboxError::with_details(
            SandboxReason::BadRequest,
            "missing_id",
        ));
    }

    Ok(request.into_run_request(limits))
}

pub(crate) fn encode_success_response_line(
    id: &str,
    result: &SandboxResult,
    limits: SandboxLimits,
) -> Result<Vec<u8>, SandboxError> {
    let response = SandboxIpcSuccessResponse {
        id: id.to_string(),
        ok: true,
        result: result.clone(),
    };
    encode_line(
        &response,
        limits.response_bytes,
        SandboxReason::SandboxIpcProtocolError,
    )
}

pub(crate) fn encode_failure_response_line(
    id: &str,
    reason: SandboxReason,
    error: Option<&str>,
    limits: SandboxLimits,
) -> Result<Vec<u8>, SandboxError> {
    let response = SandboxIpcFailureResponse {
        id: id.to_string(),
        ok: false,
        reason,
        error: error.map(str::to_owned),
    };
    encode_line(
        &response,
        limits.response_bytes,
        SandboxReason::SandboxIpcProtocolError,
    )
}

pub(crate) fn decode_response_line(
    raw: &[u8],
    expected_id: &str,
    limits: SandboxLimits,
) -> Result<SandboxResult, SandboxError> {
    if raw.len() > limits.response_bytes {
        return Err(SandboxError::with_details(
            SandboxReason::SandboxIpcProtocolError,
            "response_too_large",
        ));
    }

    let line = first_line(raw);
    if line.is_empty() {
        return Err(SandboxError::with_details(
            SandboxReason::SandboxIpcProtocolError,
            "empty_response",
        ));
    }

    let response: RawSandboxIpcResponse = serde_json::from_slice(line).map_err(|_| {
        SandboxError::with_details(SandboxReason::SandboxIpcProtocolError, "invalid_json")
    })?;

    let response_id = response.id.as_deref().ok_or_else(|| {
        SandboxError::with_details(SandboxReason::SandboxIpcProtocolError, "missing_id")
    })?;
    if response_id != expected_id {
        return Err(SandboxError::with_details(
            SandboxReason::SandboxIpcProtocolError,
            "id_mismatch",
        ));
    }

    match response.ok {
        Some(true) => response.result.ok_or_else(|| {
            SandboxError::with_details(SandboxReason::SandboxIpcProtocolError, "missing_result")
        }),
        Some(false) => {
            let raw_reason = response.reason.as_deref().ok_or_else(|| {
                SandboxError::with_details(SandboxReason::SandboxIpcProtocolError, "missing_reason")
            })?;

            let reason =
                SandboxReason::from_wire(raw_reason).unwrap_or(SandboxReason::SandboxIpcFailed);
            let details = response.error.unwrap_or_else(|| raw_reason.to_string());
            Err(SandboxError::with_details(reason, details))
        }
        None => Err(SandboxError::with_details(
            SandboxReason::SandboxIpcProtocolError,
            "missing_ok",
        )),
    }
}

fn encode_line<T: Serialize>(
    value: &T,
    limit: usize,
    too_large_reason: SandboxReason,
) -> Result<Vec<u8>, SandboxError> {
    let mut raw = serde_json::to_vec(value).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxIpcProtocolError, error.to_string())
    })?;
    raw.push(b'\n');
    if raw.len() > limit {
        return Err(SandboxError::with_details(
            too_large_reason,
            format!("encoded_message_too_large:{}", raw.len()),
        ));
    }
    Ok(raw)
}

fn first_line(raw: &[u8]) -> &[u8] {
    raw.split(|byte| *byte == b'\n').next().unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{
        decode_request_line, decode_response_line, encode_failure_response_line,
        encode_request_line, encode_success_response_line, SandboxIpcRequest,
    };
    use crate::sandbox::error::SandboxReason;
    use crate::sandbox::types::{FsChange, FsChangeKind, SandboxLimits, SandboxResult};

    fn sample_request() -> SandboxIpcRequest {
        SandboxIpcRequest {
            id: "req-1".to_string(),
            command: "rm -rf /etc/aish/123".to_string(),
            cwd: PathBuf::from("/tmp/repo"),
            repo_root: PathBuf::from("/"),
            client_pid: 12345,
            timeout_s: 999.0,
        }
    }

    #[test]
    fn request_round_trip_clamps_timeout() {
        let limits = SandboxLimits::default();
        let raw = encode_request_line(&sample_request(), limits).unwrap();
        let decoded = decode_request_line(&raw, limits).unwrap();

        assert_eq!(decoded.id, "req-1");
        assert_eq!(decoded.timeout_s, 300.0);
    }

    #[test]
    fn encode_request_rejects_message_larger_than_limit() {
        let mut request = sample_request();
        request.command = "x".repeat(SandboxLimits::DEFAULT_REQUEST_BYTES);

        let error = encode_request_line(&request, SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::RequestTooLarge);
    }

    #[test]
    fn decode_request_rejects_invalid_json() {
        let error = decode_request_line(b"{bad json}\n", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::BadRequest);
        assert_eq!(error.details(), Some("invalid_json"));
    }

    #[test]
    fn decode_response_success_parses_typed_result() {
        let result = SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            changes: vec![FsChange {
                path: "/etc/aish/123".to_string(),
                kind: FsChangeKind::Deleted,
                detail: None,
            }],
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        };

        let raw = encode_success_response_line("req-1", &result, SandboxLimits::default()).unwrap();
        let decoded = decode_response_line(&raw, "req-1", SandboxLimits::default()).unwrap();
        assert_eq!(decoded, result);
    }

    #[test]
    fn decode_response_failure_maps_reason_and_details() {
        let raw = encode_failure_response_line(
            "req-1",
            SandboxReason::SandboxIpcFailed,
            Some("daemon_down"),
            SandboxLimits::default(),
        )
        .unwrap();

        let error = decode_response_line(&raw, "req-1", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcFailed);
        assert_eq!(error.details(), Some("daemon_down"));
    }

    #[test]
    fn decode_response_rejects_invalid_json() {
        let error =
            decode_response_line(b"{bad json}\n", "req-1", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcProtocolError);
        assert_eq!(error.details(), Some("invalid_json"));
    }

    #[test]
    fn decode_response_rejects_id_mismatch() {
        let raw = json!({
            "id": "req-2",
            "ok": true,
            "result": {
                "exit_code": 0,
                "stdout": "",
                "stderr": "",
                "changes": []
            }
        });
        let mut raw = serde_json::to_vec(&raw).unwrap();
        raw.push(b'\n');

        let error = decode_response_line(&raw, "req-1", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcProtocolError);
        assert_eq!(error.details(), Some("id_mismatch"));
    }

    #[test]
    fn decode_response_rejects_missing_result() {
        let raw = json!({
            "id": "req-1",
            "ok": true
        });
        let mut raw = serde_json::to_vec(&raw).unwrap();
        raw.push(b'\n');

        let error = decode_response_line(&raw, "req-1", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcProtocolError);
        assert_eq!(error.details(), Some("missing_result"));
    }

    #[test]
    fn decode_response_rejects_oversized_message() {
        let raw = vec![b'x'; SandboxLimits::DEFAULT_RESPONSE_BYTES + 1];
        let error = decode_response_line(&raw, "req-1", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcProtocolError);
        assert_eq!(error.details(), Some("response_too_large"));
    }

    #[test]
    fn decode_response_unknown_failure_reason_falls_back_to_ipc_failed() {
        let mut raw = serde_json::to_vec(&json!({
            "id": "req-1",
            "ok": false,
            "reason": "non_standard_reason",
            "error": "something happened"
        }))
        .unwrap();
        raw.push(b'\n');

        let error = decode_response_line(&raw, "req-1", SandboxLimits::default()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcFailed);
        assert_eq!(error.details(), Some("something happened"));
    }

    #[test]
    fn encode_failure_response_keeps_reason_on_wire() {
        let raw = encode_failure_response_line(
            "req-1",
            SandboxReason::CommandNotFound,
            None,
            SandboxLimits::default(),
        )
        .unwrap();

        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("\"reason\":\"command_not_found\""));
    }
}
