use std::io::Write;

use crate::output_buffer::OutputBuffer;

// ---------------------------------------------------------------------------
// Callback types
// ---------------------------------------------------------------------------

/// Input provided to the AI callback.
pub struct AiQuery {
    /// The user's question text (after the `;`/`；` prefix).
    pub question: String,
    /// Recent PTY output for context (error correction).
    pub recent_output: String,
}

/// Response returned by the AI callback.
pub struct AiResponse {
    /// Full AI response text to display.
    pub text: String,
    /// Suggested command to inject into remote shell, if any.
    pub command: Option<String>,
}

/// Callback function type: receives query, returns response.
/// Implemented by the shell layer using LLM.
pub type AiCallback = dyn Fn(AiQuery) -> Result<AiResponse, String> + Send + Sync;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterceptorState {
    /// Normal passthrough — all stdin bytes go to PTY master.
    Passthrough,
    /// Collecting AI input after detecting `;` at line start.
    AiInput,
    /// AI callback is running; buffer PTY output but don't display.
    AiProcessing,
}

/// Action returned after processing a stdin byte.
#[derive(Debug, PartialEq, Eq)]
pub enum StdinAction {
    /// Forward byte to PTY master.
    Forward,
    /// Byte was intercepted (AI mode); echo it locally to stdout.
    EchoLocally,
    /// AI input line is complete. Caller should invoke AI callback.
    TriggerAi(String),
}

// ---------------------------------------------------------------------------
// SessionInterceptor
// ---------------------------------------------------------------------------

pub struct SessionInterceptor {
    state: InterceptorState,
    at_line_start: bool,
    line_buffer: Vec<u8>,
    output_buffer: OutputBuffer,
    ai_callback: Option<Box<AiCallback>>,
}

impl SessionInterceptor {
    /// Create a new interceptor.
    /// `ai_callback` is None → interceptor is disabled (pure passthrough).
    /// `ai_callback` is Some → interceptor will intercept `;` input.
    pub fn new(ai_callback: Option<Box<AiCallback>>) -> Self {
        Self {
            state: InterceptorState::Passthrough,
            at_line_start: true,
            line_buffer: Vec::with_capacity(4096),
            output_buffer: OutputBuffer::new(8192),
            ai_callback,
        }
    }

    /// Feed a single byte from stdin. Returns the action the caller should take.
    pub fn feed_stdin(&mut self, byte: u8) -> StdinAction {
        match self.state {
            InterceptorState::Passthrough => {
                if self.at_line_start
                    && self.ai_callback.is_some()
                    && (byte == b';' || byte == 0xEF)
                {
                    self.line_buffer.clear();
                    self.line_buffer.push(byte);
                    self.state = InterceptorState::AiInput;
                    self.at_line_start = false;
                    return StdinAction::EchoLocally;
                }
                self.at_line_start = false;
                StdinAction::Forward
            }
            InterceptorState::AiInput => {
                self.line_buffer.push(byte);
                if byte == b'\r' || byte == b'\n' {
                    let line = String::from_utf8_lossy(&self.line_buffer).to_string();
                    let question = extract_ai_question(&line);
                    self.state = InterceptorState::AiProcessing;
                    self.line_buffer.clear();
                    StdinAction::TriggerAi(question)
                } else {
                    StdinAction::EchoLocally
                }
            }
            InterceptorState::AiProcessing => {
                StdinAction::EchoLocally
            }
        }
    }

    /// Feed PTY output data — track line starts and buffer for error correction.
    pub fn feed_pty_output(&mut self, data: &[u8]) {
        self.output_buffer.append(data);
        if data.contains(&b'\n') {
            self.at_line_start = true;
        }
    }

    /// Reset state to passthrough after AI processing completes.
    pub fn finish_ai(&mut self) {
        self.state = InterceptorState::Passthrough;
        self.at_line_start = true;
    }

    /// Whether AI is currently processing.
    pub fn is_ai_processing(&self) -> bool {
        self.state == InterceptorState::AiProcessing
    }

    /// Get the recent PTY output for error correction context.
    pub fn recent_output(&self, max_len: usize) -> String {
        let bytes = self.output_buffer.recent(max_len);
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Run the AI callback and return the response.
    pub fn call_ai(&self, question: String) -> Option<Result<AiResponse, String>> {
        self.ai_callback.as_ref().map(|cb| {
            let recent = self.recent_output(4000);
            cb(AiQuery {
                question,
                recent_output: recent,
            })
        })
    }

    /// Display the AI response on stdout and inject command into PTY master.
    /// Returns the command string if injected.
    pub fn display_response_and_inject(
        response: &AiResponse,
        pty_master_fd: std::os::fd::RawFd,
    ) -> Option<String> {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();

        let _ = handle.write_all(b"\r\x1b[2K");
        let _ = handle.write_all(b"\x1b[36m--- AI ---\x1b[0m\r\n");
        let _ = handle.write_all(response.text.as_bytes());
        let _ = handle.write_all(b"\r\n\x1b[36m----------\x1b[0m\r\n");
        let _ = handle.flush();

        if let Some(ref cmd) = response.command {
            let mut inject = cmd.as_bytes().to_vec();
            inject.push(b'\r');
            unsafe {
                libc::write(
                    pty_master_fd,
                    inject.as_ptr() as *const libc::c_void,
                    inject.len(),
                );
            }
            Some(cmd.clone())
        } else {
            None
        }
    }
}

/// Extract the question text after `;` or `；` prefix.
fn extract_ai_question(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.starts_with('；') {
        trimmed[3..].trim().to_string()
    } else if trimmed.starts_with(';') {
        trimmed[1..].trim().to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_callback() -> Box<AiCallback> {
        Box::new(|_q| {
            Ok(AiResponse {
                text: "test response".to_string(),
                command: Some("echo test".to_string()),
            })
        })
    }

    // ---- extract_ai_question tests ----

    #[test]
    fn test_extract_question_ascii_semicolon() {
        assert_eq!(extract_ai_question(";ip a"), "ip a");
    }

    #[test]
    fn test_extract_question_fullwidth_semicolon() {
        assert_eq!(extract_ai_question("；查看IP"), "查看IP");
    }

    #[test]
    fn test_extract_question_with_extra_spaces() {
        assert_eq!(extract_ai_question(";  ip a  "), "ip a");
    }

    #[test]
    fn test_extract_question_only_semicolon() {
        assert_eq!(extract_ai_question(";"), "");
    }

    // ---- State machine tests ----

    #[test]
    fn test_passthrough_forward_normal_bytes() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        assert_eq!(ic.feed_stdin(b'a'), StdinAction::Forward);
        assert_eq!(ic.feed_stdin(b'b'), StdinAction::Forward);
    }

    #[test]
    fn test_semicolon_at_line_start_triggers_ai() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        let action = ic.feed_stdin(b';');
        assert_eq!(action, StdinAction::EchoLocally);
        assert_eq!(ic.state, InterceptorState::AiInput);
    }

    #[test]
    fn test_semicolon_not_at_line_start_passthrough() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        ic.feed_stdin(b'a');
        let action = ic.feed_stdin(b';');
        assert_eq!(action, StdinAction::Forward);
    }

    #[test]
    fn test_ai_input_line_complete_on_enter() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        ic.feed_stdin(b';');
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::TriggerAi(_)));
        assert_eq!(ic.state, InterceptorState::AiProcessing);
    }

    #[test]
    fn test_ai_input_captures_question() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        ic.feed_stdin(b';');
        ic.feed_stdin(b'i');
        ic.feed_stdin(b'p');
        ic.feed_stdin(b' ');
        ic.feed_stdin(b'a');
        if let StdinAction::TriggerAi(q) = ic.feed_stdin(b'\r') {
            assert_eq!(q, "ip a");
        } else {
            panic!("expected TriggerAi");
        }
    }

    #[test]
    fn test_no_callback_means_pure_passthrough() {
        let mut ic = SessionInterceptor::new(None);
        assert_eq!(ic.feed_stdin(b';'), StdinAction::Forward);
    }

    #[test]
    fn test_pty_output_sets_line_start() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        ic.feed_stdin(b'a');
        ic.feed_pty_output(b"output\n");
        assert_eq!(ic.feed_stdin(b';'), StdinAction::EchoLocally);
    }

    #[test]
    fn test_finish_ai_resets_to_passthrough() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        ic.feed_stdin(b';');
        ic.feed_stdin(b'\r');
        assert!(ic.is_ai_processing());
        ic.finish_ai();
        assert_eq!(ic.state, InterceptorState::Passthrough);
        assert!(ic.at_line_start);
    }

    #[test]
    fn test_recent_output_captures_pty_data() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()));
        ic.feed_pty_output(b"hello ");
        ic.feed_pty_output(b"world\n");
        assert!(ic.recent_output(100).contains("hello world"));
    }
}
