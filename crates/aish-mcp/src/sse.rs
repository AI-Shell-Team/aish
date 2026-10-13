//! Incremental Server-Sent Events decoding.
//!
//! Ported from oh-my-pi `packages/utils/src/stream.ts` (`readSseEvents`): the
//! decoder is fed raw body bytes as they arrive and yields one [`SseEvent`]
//! per blank-line dispatch. Line terminators may be split across chunks, so a
//! chunk ending in `\r` keeps that byte buffered until the next chunk proves
//! whether it began a CRLF pair. LF, CRLF and a lone CR all terminate a line.
//!
//! Field semantics follow the SSE spec (and the reference implementation):
//! `data:` lines accumulate joined by `\n`, one leading space after the colon
//! is stripped, comment lines (starting with `:`) are ignored, `id:` values
//! containing NUL are refused and `retry:` only accepts a plain decimal
//! integer. An event carrying only control fields (`id:`/`retry:`) is still
//! dispatched, with an empty `data`, so reconnecting transports keep their
//! cursor and the server-requested retry interval.

/// One dispatched SSE event.
///
/// `data` is empty when the event carried no `data:` field at all; the
/// distinction between "absent" and `data:` with an empty value is not
/// observable through this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` field, when the event carried one.
    pub event: Option<String>,
    /// `data:` fields joined with `\n`.
    pub data: String,
    /// `id:` field, when present and free of NUL bytes.
    pub id: Option<String>,
    /// `retry:` field in milliseconds, when present and a plain integer.
    pub retry_ms: Option<u64>,
}

/// Accumulator for the fields of the event currently being read.
#[derive(Default)]
struct EventState {
    event: Option<String>,
    /// `None` until the first `data:` field is seen, so an empty `data:` is
    /// distinguishable from the absence of the field while accumulating.
    data: Option<String>,
    id: Option<String>,
    retry_ms: Option<u64>,
}

impl EventState {
    /// Fold one decoded wire line into the pending event. Returns the event
    /// when the line terminated it (an empty line), `None` otherwise.
    fn push_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.take_event();
        }
        // Comment line (`: heartbeat`): wire diagnostics only, never parsed.
        if line.starts_with(':') {
            return None;
        }
        let (field, mut value) = match line.split_once(':') {
            Some((field, value)) => (field, value),
            None => (line, ""),
        };
        if let Some(stripped) = value.strip_prefix(' ') {
            value = stripped;
        }
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            // The spec refuses ids containing NUL; other bytes are kept as-is.
            "id" if !value.contains('\0') => self.id = Some(value.to_string()),
            "retry" if is_plain_integer(value) => self.retry_ms = value.parse().ok(),
            _ => {}
        }
        None
    }

    /// Emit the pending event, if any, and reset for the next one.
    fn take_event(&mut self) -> Option<SseEvent> {
        if self.event.is_none()
            && self.data.is_none()
            && self.id.is_none()
            && self.retry_ms.is_none()
        {
            return None;
        }
        let event = SseEvent {
            event: self.event.take(),
            data: self.data.take().unwrap_or_default(),
            id: self.id.take(),
            retry_ms: self.retry_ms.take(),
        };
        Some(event)
    }
}

/// True when `value` is a non-empty run of ASCII digits. Guards `retry:` against
/// signs, spaces and non-decimal junk; overflow falls out of `parse`.
fn is_plain_integer(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Longest single SSE line the decoder keeps.
///
/// The transports already cap a whole response, but the decoder is also fed by
/// the fire-and-forget drain of a notification reply whose stream has no
/// response cap of its own: one peer that never terminates a line must not grow
/// this buffer without bound. An overlong line is dropped and reported as
/// malformed, which the caller treats like any other decode failure.
pub const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;

/// Incremental SSE decoder over a byte stream.
#[derive(Default)]
pub struct SseDecoder {
    /// Bytes received but not yet terminated by a line ending.
    buffer: Vec<u8>,
    state: EventState,
    /// Set when an overlong line was dropped; the current event is discarded
    /// rather than delivered half-decoded.
    overflowed: bool,
}

impl SseDecoder {
    /// Create an empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the stream carried a line longer than [`MAX_SSE_LINE_BYTES`].
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Feed the next chunk of body bytes and return every event it completed.
    ///
    /// A trailing `\r` is held back: the following chunk may turn it into a
    /// CRLF pair, which is one terminator and not two line breaks.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        // Complete lines are drained first: the cap applies to one *line*, so a
        // chunk carrying many well-formed events must not be mistaken for an
        // overlong one.
        let events = self.drain_lines(false);
        if self.buffer.len() > MAX_SSE_LINE_BYTES {
            self.buffer.clear();
            self.overflowed = true;
            self.state = EventState::default();
        }
        events
    }

    /// Flush the end of the stream: a trailing line without a terminator is
    /// treated as complete, and the last event is dispatched even when the
    /// stream never sent its terminating blank line.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = self.drain_lines(true);
        if !self.buffer.is_empty() {
            let tail = std::mem::take(&mut self.buffer);
            let line = String::from_utf8_lossy(&tail);
            if let Some(event) = self.state.push_line(&line) {
                events.push(event);
            }
        }
        if let Some(event) = self.state.take_event() {
            events.push(event);
        }
        events
    }

    /// Split complete lines off the buffer and fold them into events.
    ///
    /// `at_end` relaxes the CRLF look-ahead so a trailing lone `\r` terminates
    /// the final line instead of waiting for a byte that will never arrive.
    fn drain_lines(&mut self, at_end: bool) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mut start = 0;
        let length = self.buffer.len();
        while start < length {
            let terminator = self.buffer[start..length]
                .iter()
                .position(|byte| *byte == b'\n' || *byte == b'\r')
                .map(|offset| start + offset);
            let Some(terminator) = terminator else {
                break;
            };
            let mut next = terminator + 1;
            if self.buffer[terminator] == b'\r' {
                if next == length {
                    if !at_end {
                        // The paired `\n` may start the next chunk.
                        break;
                    }
                } else if self.buffer[next] == b'\n' {
                    next += 1;
                }
            }
            // A terminated line is still a line: enforce the cap here too, or a
            // peer could send one over-long line per chunk and never hit the
            // buffer check in `push`.
            if terminator - start > MAX_SSE_LINE_BYTES {
                self.overflowed = true;
                self.state = EventState::default();
                start = next;
                continue;
            }
            // Line endings are ASCII, so they can never cut a UTF-8 sequence.
            let line = String::from_utf8_lossy(&self.buffer[start..terminator]);
            if let Some(event) = self.state.push_line(&line) {
                events.push(event);
            }
            start = next;
        }
        if start > 0 {
            self.buffer.drain(..start);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(data: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_string(),
            id: None,
            retry_ms: None,
        }
    }

    #[test]
    fn dispatches_on_blank_line() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: hello\n\n");
        assert_eq!(events, vec![event("hello")]);
        assert!(decoder.push(b"").is_empty());
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn splits_events_across_chunks() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"ev").is_empty());
        assert!(decoder.push(b"ent: message\nda").is_empty());
        assert!(decoder.push(b"ta: {\"a\":").is_empty());
        let events = decoder.push(b"1}\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("message".to_string()),
                data: "{\"a\":1}".to_string(),
                id: None,
                retry_ms: None,
            }]
        );
    }

    #[test]
    fn crlf_terminators_and_split_pair() {
        let mut decoder = SseDecoder::new();
        // The CR of the CRLF pair ends the first chunk.
        assert!(decoder.push(b"data: one\r").is_empty());
        let events = decoder.push(b"\n\r\ndata: two\r\n\r\n");
        assert_eq!(events, vec![event("one"), event("two")]);
    }

    #[test]
    fn lone_cr_terminates_lines() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: one\r\rdata: two\r\rdata: three\n\n");
        assert_eq!(events, vec![event("one"), event("two"), event("three")]);
    }

    #[test]
    fn joins_multiple_data_lines() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: line one\ndata: line two\ndata:\n\n");
        assert_eq!(events, vec![event("line one\nline two\n")]);
    }

    #[test]
    fn data_without_space_keeps_value() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data:x\n\n");
        assert_eq!(events, vec![event("x")]);
    }

    #[test]
    fn data_without_colon_is_empty() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data\n\n");
        assert_eq!(events, vec![event("")]);
    }

    #[test]
    fn ignores_comments_and_unknown_fields() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b": keep-alive\nfoo: bar\ndata: value\n\n");
        assert_eq!(events, vec![event("value")]);
    }

    #[test]
    fn parses_id_and_retry() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"id: 42\nretry: 1500\ndata: x\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "x".to_string(),
                id: Some("42".to_string()),
                retry_ms: Some(1500),
            }]
        );
    }

    #[test]
    fn rejects_nul_id_and_non_numeric_retry() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"id: a\0b\nretry: 15s\ndata: x\n\n");
        assert_eq!(events, vec![event("x")]);
    }

    #[test]
    fn control_only_event_is_dispatched() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"retry: 3000\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: String::new(),
                id: None,
                retry_ms: Some(3000),
            }]
        );
    }

    #[test]
    fn blank_lines_without_fields_emit_nothing() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"\n\r\n\r").is_empty());
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn finish_flushes_unterminated_trailing_event() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"event: ping\ndata: {}").is_empty());
        let events = decoder.finish();
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("ping".to_string()),
                data: "{}".to_string(),
                id: None,
                retry_ms: None,
            }]
        );
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn finish_flushes_trailing_cr_and_terminated_event() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"data: x\r").is_empty());
        assert_eq!(decoder.finish(), vec![event("x")]);
    }

    #[test]
    fn decodes_multibyte_utf8_split_across_chunks() {
        let mut decoder = SseDecoder::new();
        let mut bytes = b"data: ".to_vec();
        bytes.extend_from_slice("héllo".as_bytes());
        assert!(decoder.push(&bytes[..bytes.len() - 1]).is_empty());
        let events = decoder.push(&bytes[bytes.len() - 1..]);
        assert!(events.is_empty());
        assert_eq!(decoder.finish(), vec![event("héllo")]);
    }

    /// A peer that never terminates a line must not grow the decoder without
    /// bound; the overlong line is dropped and the stream is flagged.
    #[test]
    fn drops_a_line_longer_than_the_cap() {
        let mut decoder = SseDecoder::new();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..((MAX_SSE_LINE_BYTES / chunk.len()) + 2) {
            assert!(decoder.push(&chunk).is_empty());
            assert!(
                decoder.buffer.len() <= MAX_SSE_LINE_BYTES,
                "the line buffer must stay capped, got {}",
                decoder.buffer.len()
            );
        }
        assert!(decoder.overflowed());

        // A fresh line after the overflow is decoded normally.
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(&chunk).is_empty());
        assert!(!decoder.overflowed());
        assert_eq!(decoder.push(b"\ndata: ok\n\n"), vec![event("ok")]);
    }

    /// A single over-long line is flagged even when it *is* terminated, so a
    /// peer cannot bypass the cap by always sending the newline.
    #[test]
    fn drops_a_terminated_line_longer_than_the_cap() {
        let mut decoder = SseDecoder::new();
        let mut chunk = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
        chunk.push(b'\n');
        let events = decoder.push(&chunk);
        assert!(decoder.overflowed());
        assert!(events.is_empty());
        assert!(
            decoder.buffer.len() <= MAX_SSE_LINE_BYTES,
            "the overlong line must not be retained"
        );
    }

    /// The cap applies to one *line*: a chunk carrying many well-formed events
    /// is larger than the cap but must be decoded, not discarded.
    #[test]
    fn decodes_a_chunk_larger_than_the_cap_when_its_lines_are_short() {
        let mut decoder = SseDecoder::new();
        let mut chunk = Vec::new();
        let mut expected = 0usize;
        while chunk.len() <= MAX_SSE_LINE_BYTES {
            chunk.extend_from_slice(format!("data: e{expected}\n\n").as_bytes());
            expected += 1;
        }
        let events = decoder.push(&chunk);
        assert!(
            !decoder.overflowed(),
            "complete short lines are not a cap hit"
        );
        assert_eq!(events.len(), expected);
        assert_eq!(events[0], event("e0"));
        assert_eq!(events[expected - 1], event(&format!("e{}", expected - 1)));
    }
}
