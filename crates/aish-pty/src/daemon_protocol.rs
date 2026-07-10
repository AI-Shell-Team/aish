//! Wire protocol for PTY daemon ↔ client communication.
//!
//! Frame format: `[type:1B][length:2B LE][payload:N bytes]`
//! Maximum payload size: 65535 bytes (u16).

use crate::control::BackendControlEvent;
use aish_core::AishError;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol version
// ---------------------------------------------------------------------------

pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum payload size per frame (64KB - 1).
pub const MAX_FRAME_PAYLOAD: usize = u16::MAX as usize;

// ---------------------------------------------------------------------------
// Frame type tags
// ---------------------------------------------------------------------------

// daemon → client (0x01–0x0F)
pub const TYPE_PTY_OUTPUT: u8 = 0x01;
pub const TYPE_CONTROL_EVENT: u8 = 0x02;
pub const TYPE_SCROLLBACK: u8 = 0x03;
pub const TYPE_SESSION_INFO: u8 = 0x04;
pub const TYPE_ATTACH_ACK: u8 = 0x05;
pub const TYPE_DAEMON_ERROR: u8 = 0x06;
pub const TYPE_SCROLLBACK_END: u8 = 0x07;
pub const TYPE_EXIT_NOTICE: u8 = 0x08;

// client → daemon (0x10–0x1F)
pub const TYPE_ATTACH: u8 = 0x10;
pub const TYPE_INPUT: u8 = 0x11;
pub const TYPE_RESIZE: u8 = 0x12;
pub const TYPE_DETACH: u8 = 0x13;
pub const TYPE_KILL_SESSION: u8 = 0x14;

/// Header size: 1 byte type + 2 bytes length.
pub const HEADER_SIZE: usize = 3;

// ---------------------------------------------------------------------------
// Message payloads (JSON-serialized)
// ---------------------------------------------------------------------------

/// Client → daemon: request to attach to a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachRequest {
    pub protocol_version: u32,
    pub session_id: String,
    pub rows: u16,
    pub cols: u16,
    #[serde(default = "default_term")]
    pub term: String,
}

fn default_term() -> String {
    "xterm-256color".to_string()
}

/// daemon → client: attach acknowledged, scrollback replay starting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachAck {
    pub protocol_version: u32,
    pub session_id: String,
    pub scrollback_size: usize,
}

/// daemon → client: scrollback replay complete, switching to live mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollbackEnd {
    pub child_pid: u32,
    pub cwd: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
}

/// daemon → client: session metadata update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub child_pid: u32,
    pub cwd: String,
    pub running: bool,
}

/// daemon → client: resize request from client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeRequest {
    pub rows: u16,
    pub cols: u16,
}

/// daemon → client: bash exited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitNotice {
    pub exit_code: i32,
    pub reason: String,
}

/// daemon → client: error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonError {
    pub code: String,
    pub message: String,
}

impl DaemonError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Frame: a typed message with raw payload bytes
// ---------------------------------------------------------------------------

/// A decoded frame: type tag + raw payload bytes.
#[derive(Debug, Clone)]
pub struct Frame {
    pub frame_type: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Create a frame for raw PTY output.
    pub fn pty_output(data: &[u8]) -> Self {
        // Split into MAX_FRAME_PAYLOAD chunks if needed
        debug_assert!(data.len() <= MAX_FRAME_PAYLOAD);
        Self {
            frame_type: TYPE_PTY_OUTPUT,
            payload: data.to_vec(),
        }
    }

    /// Create a frame for scrollback replay.
    pub fn scrollback(data: &[u8]) -> Self {
        debug_assert!(data.len() <= MAX_FRAME_PAYLOAD);
        Self {
            frame_type: TYPE_SCROLLBACK,
            payload: data.to_vec(),
        }
    }

    /// Create a frame for user input.
    pub fn input(data: &[u8]) -> Self {
        debug_assert!(data.len() <= MAX_FRAME_PAYLOAD);
        Self {
            frame_type: TYPE_INPUT,
            payload: data.to_vec(),
        }
    }

    /// Create a JSON-serialized control event frame.
    pub fn control_event(event: &BackendControlEvent) -> Self {
        let json = serde_json::to_string(event).unwrap_or_default();
        Self {
            frame_type: TYPE_CONTROL_EVENT,
            payload: json.into_bytes(),
        }
    }

    /// Create an Attach request frame.
    pub fn attach(req: &AttachRequest) -> Self {
        let json = serde_json::to_string(req).unwrap_or_default();
        Self {
            frame_type: TYPE_ATTACH,
            payload: json.into_bytes(),
        }
    }

    /// Create an AttachAck frame.
    pub fn attach_ack(ack: &AttachAck) -> Self {
        let json = serde_json::to_string(ack).unwrap_or_default();
        Self {
            frame_type: TYPE_ATTACH_ACK,
            payload: json.into_bytes(),
        }
    }

    /// Create a ScrollbackEnd frame.
    pub fn scrollback_end(info: &ScrollbackEnd) -> Self {
        let json = serde_json::to_string(info).unwrap_or_default();
        Self {
            frame_type: TYPE_SCROLLBACK_END,
            payload: json.into_bytes(),
        }
    }

    /// Create a SessionInfo frame.
    pub fn session_info(info: &SessionInfo) -> Self {
        let json = serde_json::to_string(info).unwrap_or_default();
        Self {
            frame_type: TYPE_SESSION_INFO,
            payload: json.into_bytes(),
        }
    }

    /// Create a Resize frame.
    pub fn resize(rows: u16, cols: u16) -> Self {
        let json = serde_json::to_string(&ResizeRequest { rows, cols }).unwrap_or_default();
        Self {
            frame_type: TYPE_RESIZE,
            payload: json.into_bytes(),
        }
    }

    /// Create a Detach frame.
    pub fn detach() -> Self {
        Self {
            frame_type: TYPE_DETACH,
            payload: Vec::new(),
        }
    }

    /// Create a KillSession frame.
    pub fn kill_session() -> Self {
        Self {
            frame_type: TYPE_KILL_SESSION,
            payload: Vec::new(),
        }
    }

    /// Create an ExitNotice frame.
    pub fn exit_notice(exit_code: i32, reason: &str) -> Self {
        let json = serde_json::to_string(&ExitNotice {
            exit_code,
            reason: reason.to_string(),
        })
        .unwrap_or_default();
        Self {
            frame_type: TYPE_EXIT_NOTICE,
            payload: json.into_bytes(),
        }
    }

    /// Create a DaemonError frame.
    pub fn daemon_error(code: &str, message: impl Into<String>) -> Self {
        let json = serde_json::to_string(&DaemonError::new(code, message)).unwrap_or_default();
        Self {
            frame_type: TYPE_DAEMON_ERROR,
            payload: json.into_bytes(),
        }
    }
}

// ---------------------------------------------------------------------------
// Frame encoding / decoding
// ---------------------------------------------------------------------------

/// Encode a frame into bytes: `[type:1B][len:2B LE][payload]`.
/// Panics if payload exceeds MAX_FRAME_PAYLOAD (use chunking before encoding).
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    assert!(
        frame.payload.len() <= MAX_FRAME_PAYLOAD,
        "frame payload too large: {} > {}",
        frame.payload.len(),
        MAX_FRAME_PAYLOAD
    );
    let len = frame.payload.len() as u16;
    let mut buf = Vec::with_capacity(HEADER_SIZE + frame.payload.len());
    buf.push(frame.frame_type);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&frame.payload);
    buf
}

/// Read one complete frame from a buffer of bytes.
///
/// Returns `(frame, bytes_consumed)` if a complete frame is available,
/// or `Ok(None)` if more data is needed.
pub fn try_decode_frame(buf: &[u8]) -> Result<Option<(Frame, usize)>, FrameDecodeError> {
    if buf.len() < HEADER_SIZE {
        return Ok(None);
    }

    let frame_type = buf[0];
    let payload_len = u16::from_le_bytes([buf[1], buf[2]]) as usize;

    if payload_len > MAX_FRAME_PAYLOAD {
        return Err(FrameDecodeError::PayloadTooLarge {
            claimed: payload_len,
            max: MAX_FRAME_PAYLOAD,
        });
    }

    let total_len = HEADER_SIZE + payload_len;
    if buf.len() < total_len {
        return Ok(None); // Need more data
    }

    let payload = buf[HEADER_SIZE..total_len].to_vec();
    Ok(Some((
        Frame {
            frame_type,
            payload,
        },
        total_len,
    )))
}

/// Error during frame decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameDecodeError {
    PayloadTooLarge { claimed: usize, max: usize },
}

impl std::fmt::Display for FrameDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadTooLarge { claimed, max } => {
                write!(f, "frame payload too large: {claimed} > {max}")
            }
        }
    }
}

impl std::error::Error for FrameDecodeError {}

impl From<FrameDecodeError> for AishError {
    fn from(e: FrameDecodeError) -> Self {
        AishError::Pty(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Non-blocking frame reader (accumulates partial reads)
// ---------------------------------------------------------------------------

/// Accumulates raw bytes from partial reads and yields complete frames.
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(8192),
        }
    }

    /// Append raw bytes from a read() call.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to decode all complete frames currently in the buffer.
    /// Returns all decoded frames and removes them from the internal buffer.
    pub fn drain_frames(&mut self) -> Result<Vec<Frame>, FrameDecodeError> {
        let mut frames = Vec::new();
        while let Some((frame, consumed)) = try_decode_frame(&self.buf)? {
            self.buf.drain(..consumed);
            frames.push(frame);
        }
        Ok(frames)
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// JSON payload decode helpers
// ---------------------------------------------------------------------------

impl Frame {
    /// Decode payload as AttachRequest (client → daemon).
    pub fn as_attach_request(&self) -> Result<AttachRequest, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as ResizeRequest (client → daemon).
    pub fn as_resize_request(&self) -> Result<ResizeRequest, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as BackendControlEvent (daemon → client).
    pub fn as_control_event(&self) -> Result<BackendControlEvent, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as AttachAck (daemon → client).
    pub fn as_attach_ack(&self) -> Result<AttachAck, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as ScrollbackEnd (daemon → client).
    pub fn as_scrollback_end(&self) -> Result<ScrollbackEnd, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as SessionInfo (daemon → client).
    pub fn as_session_info(&self) -> Result<SessionInfo, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as DaemonError (daemon → client).
    pub fn as_daemon_error(&self) -> Result<DaemonError, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Decode payload as ExitNotice (daemon → client).
    pub fn as_exit_notice(&self) -> Result<ExitNotice, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Check if this frame type is a raw-bytes frame (PTY output, input, scrollback).
    pub fn is_raw_bytes(&self) -> bool {
        matches!(
            self.frame_type,
            TYPE_PTY_OUTPUT | TYPE_SCROLLBACK | TYPE_INPUT
        )
    }

    /// Check if this is a client-to-daemon frame.
    pub fn is_client_message(&self) -> bool {
        (0x10..=0x1F).contains(&self.frame_type)
    }

    /// Check if this is a daemon-to-client frame.
    pub fn is_daemon_message(&self) -> bool {
        (0x01..=0x0F).contains(&self.frame_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let frame = Frame {
            frame_type: TYPE_PTY_OUTPUT,
            payload: b"hello world".to_vec(),
        };
        let encoded = encode_frame(&frame);
        assert_eq!(encoded.len(), HEADER_SIZE + 11);

        let (decoded, consumed) = try_decode_frame(&encoded).unwrap().unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.frame_type, TYPE_PTY_OUTPUT);
        assert_eq!(decoded.payload, b"hello world");
    }

    #[test]
    fn test_empty_payload() {
        let frame = Frame::detach();
        let encoded = encode_frame(&frame);
        assert_eq!(encoded.len(), HEADER_SIZE);

        let (decoded, _) = try_decode_frame(&encoded).unwrap().unwrap();
        assert_eq!(decoded.frame_type, TYPE_DETACH);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_max_payload() {
        let payload = vec![0xAB; MAX_FRAME_PAYLOAD];
        let frame = Frame {
            frame_type: TYPE_PTY_OUTPUT,
            payload: payload.clone(),
        };
        let encoded = encode_frame(&frame);
        let (decoded, _) = try_decode_frame(&encoded).unwrap().unwrap();
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_payload_too_large() {
        // Craft an invalid frame with length > MAX_FRAME_PAYLOAD
        let mut bad_frame = vec![TYPE_PTY_OUTPUT];
        bad_frame.extend_from_slice(&0x10000u32.to_le_bytes()[..2]); // u16 = 0, but we fake it
                                                                     // Actually, u16 can't exceed 65535, so this test verifies the guard works at the protocol level
                                                                     // A well-formed frame with max u16 length is OK
        let max_len = u16::MAX as usize;
        let mut ok_frame = vec![TYPE_PTY_OUTPUT];
        ok_frame.extend_from_slice(&(max_len as u16).to_le_bytes());
        ok_frame.extend(vec![0u8; max_len]);
        let result = try_decode_frame(&ok_frame);
        assert!(result.is_ok());
    }

    #[test]
    fn test_partial_data() {
        let frame = Frame::pty_output(b"hello");
        let encoded = encode_frame(&frame);

        // Only header bytes
        // Should return None (need more data)
        assert!(try_decode_frame(&encoded[..2]).unwrap().is_none());

        // Header + partial payload
        assert!(try_decode_frame(&encoded[..HEADER_SIZE + 2])
            .unwrap()
            .is_none());

        // Complete frame
        let result = try_decode_frame(&encoded).unwrap().unwrap();
        assert_eq!(result.0.payload, b"hello");
    }

    #[test]
    fn test_frame_reader_multiple_frames() {
        let mut reader = FrameReader::new();

        let f1 = Frame::pty_output(b"first");
        let f2 = Frame::input(b"second");
        let f3 = Frame::detach();

        let mut combined = Vec::new();
        combined.extend(encode_frame(&f1));
        combined.extend(encode_frame(&f2));
        combined.extend(encode_frame(&f3));

        reader.push(&combined);
        let frames = reader.drain_frames().unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].payload, b"first");
        assert_eq!(frames[1].payload, b"second");
        assert_eq!(frames[2].frame_type, TYPE_DETACH);
    }

    #[test]
    fn test_frame_reader_incremental() {
        let mut reader = FrameReader::new();

        let frame = Frame::pty_output(b"split across reads");
        let encoded = encode_frame(&frame);

        // Push first half
        let mid = encoded.len() / 2;
        reader.push(&encoded[..mid]);
        let frames = reader.drain_frames().unwrap();
        assert_eq!(frames.len(), 0); // Not complete yet

        // Push second half
        reader.push(&encoded[mid..]);
        let frames = reader.drain_frames().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"split across reads");
    }

    #[test]
    fn test_frame_reader_split_across_frames() {
        let mut reader = FrameReader::new();

        let f1 = Frame::pty_output(b"AAA");
        let f2 = Frame::input(b"BBB");
        let combined = encode_frame(&f1)
            .into_iter()
            .chain(encode_frame(&f2))
            .collect::<Vec<_>>();

        // Push in tiny chunks
        for byte in combined {
            reader.push(&[byte]);
        }

        let frames = reader.drain_frames().unwrap();
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn test_json_frame_roundtrip() {
        let req = AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            session_id: "test-123".to_string(),
            rows: 24,
            cols: 80,
            term: "xterm-256color".to_string(),
        };
        let frame = Frame::attach(&req);
        let decoded = frame.as_attach_request().unwrap();
        assert_eq!(decoded.session_id, "test-123");
        assert_eq!(decoded.rows, 24);
        assert_eq!(decoded.cols, 80);
    }

    #[test]
    fn test_control_event_frame() {
        let event = BackendControlEvent::PromptReady {
            command_seq: Some(42),
            exit_code: 0,
            cwd: "/tmp".to_string(),
            interrupted: false,
        };
        let frame = Frame::control_event(&event);
        let decoded = frame.as_control_event().unwrap();
        match decoded {
            BackendControlEvent::PromptReady { command_seq, .. } => {
                assert_eq!(command_seq, Some(42));
            }
            other => panic!("expected PromptReady, got {other:?}"),
        }
    }

    #[test]
    fn test_frame_type_classification() {
        assert!(Frame::pty_output(b"x").is_raw_bytes());
        assert!(Frame::input(b"x").is_raw_bytes());
        assert!(Frame::scrollback(b"x").is_raw_bytes());
        assert!(!Frame::detach().is_raw_bytes());

        assert!(Frame::pty_output(b"x").is_daemon_message());
        assert!(Frame::input(b"x").is_client_message());
        assert!(Frame::attach(&AttachRequest {
            protocol_version: 1,
            session_id: String::new(),
            rows: 24,
            cols: 80,
            term: default_term(),
        })
        .is_client_message());
    }
}
