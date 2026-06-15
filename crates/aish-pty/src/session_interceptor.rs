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
    /// Exit code of the last command (0 if not available or success).
    pub exit_code: i32,
}

/// Result returned by the AI callback containing an optional command to
/// inject into the remote shell and raw display text to be shown later
/// (after command output has been displayed).
pub struct AiResponse {
    /// Command to inject into the remote PTY. `None` when the AI only
    /// provides an explanation without a runnable command.
    pub command: Option<String>,
    /// Raw LLM response text for deferred display (markdown, will be
    /// rendered by the forwarding loop after command output).
    pub display_text: String,
    /// When Some(command), the forwarding loop should execute the command
    /// on the remote host, capture its output, and pass it to this
    /// followup callback for analysis.
    pub followup: Option<Box<FollowupCallback>>,
    /// When Some, the AI needs user input before continuing.  The
    /// forwarding loop displays the question, reads user input, sends
    /// the answer back via the channel, and waits for the next event.
    pub ask_user: Option<(AskUserRequest, AskUserChannel)>,
}

/// A question the AI wants to ask the user during an SSH session.
pub struct AskUserRequest {
    /// Interaction type: "text_input" or "choice_or_text".
    pub kind: String,
    /// The question to display.
    pub prompt: String,
    /// Predefined choices for "choice_or_text" mode.
    pub options: Option<Vec<AskUserOption>>,
    /// Optional title for the question.
    pub title: Option<String>,
    /// Default value (pre-selected).
    pub default: Option<String>,
    /// Whether the user can cancel/skip (default: true).
    pub allow_cancel: bool,
    /// Minimum length for text input (default: 0).
    pub min_length: usize,
}

/// One option in a choice_or_text ask_user interaction.
pub struct AskUserOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Answer from the user to an ask_user question.
pub enum AskUserAnswer {
    Response(String),
    Cancelled,
}

/// Result of a bash command executed via PTY (local or SSH).
#[derive(Debug, Clone)]
pub struct BashExecResult {
    /// The captured command output (cleaned of ANSI/prompt).
    pub output: String,
    /// If the output exceeded the offload threshold, this is the local path
    /// where the full output was written
    /// (e.g. "/tmp/aish-offload/{uuid}/stdout.raw").
    pub offload_path: Option<String>,
}

/// Event from the LLM thread to the forwarding loop.
pub enum AiEvent {
    /// The LLM wants to ask the user a question.
    AskUser(AskUserRequest),
    /// The LLM wants to execute a bash command on the remote host.
    BashExec {
        command: String,
        output_sender: std::sync::mpsc::Sender<BashExecResult>,
    },
    /// The LLM has finished processing. Payload is a fully processed AiResponse
    /// (with command, followup, etc. already populated).
    Done(Option<AiResponse>),
}

/// Channel pair for ask_user communication between the LLM thread and
/// the forwarding loop.
pub struct AskUserChannel {
    /// Send user's answer back to the LLM thread.
    pub answer_sender: std::sync::mpsc::Sender<AskUserAnswer>,
    /// Receive next event (another ask_user or done) from the LLM thread.
    pub event_receiver: std::sync::mpsc::Receiver<AiEvent>,
}

/// Second-stage callback invoked after the injected command finishes on
/// the remote host.  Receives the captured command output and an optional
/// remote offload path, streams the AI analysis to the terminal, and
/// optionally returns a new `AiResponse` to chain another command
/// execution (multi-round tool use).
pub type FollowupCallback = dyn Fn(&str, Option<&str>) -> Option<AiResponse> + Send + Sync;

/// AI callback type: receives an AiQuery and returns an optional AiResponse.
pub type AiCallback = dyn Fn(AiQuery) -> Option<AiResponse> + Send + Sync;

/// Callback for executing a command on the remote host and returning its output.
pub type RemoteExecFn = dyn FnMut(&str) -> String;

/// Callback invoked when `/status` is detected during an SSH/session.
/// Receives a closure that can execute commands on the remote host.
/// Returns the fully rendered status string to display.
pub type StatusCallback = dyn Fn(&mut RemoteExecFn) -> String + Send + Sync;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterceptorState {
    /// Normal passthrough — all stdin bytes go to PTY master.
    Passthrough,
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
    /// `/status` detected during session. Caller should run remote status scan.
    TriggerStatus,
}

// ---------------------------------------------------------------------------
// SessionInterceptor
// ---------------------------------------------------------------------------

pub struct SessionInterceptor {
    state: InterceptorState,
    /// Shadow buffer of the current input line in Passthrough mode.
    /// Used for line-level AI trigger detection: when Enter is pressed,
    /// we check whether the accumulated line starts with `;` or `；`.
    line_shadow: Vec<u8>,
    /// Flag set when AI is triggered from line-level detection (the PTY
    /// already has the echoed text). The forwarding loop should send
    /// Ctrl+C to the PTY to cancel the line before invoking AI.
    cancel_pty_line: bool,
    output_buffer: OutputBuffer,
    ai_callback: Option<Box<AiCallback>>,
    status_callback: Option<Box<StatusCallback>>,
    /// Escape sequence tracker: when Some, we're consuming bytes of a
    /// terminal escape sequence (arrow keys, function keys, etc.) so they
    /// don't corrupt line_shadow.
    escape_seq: Option<EscSeqPhase>,
    /// Bracketed paste mode: when true, we're receiving pasted content
    /// (between ESC [ 200 ~ and ESC [ 201 ~). Pasted content should not
    /// be added to line_shadow to avoid false NL/AI triggers.
    in_bracketed_paste: bool,
    /// Buffer to collect CSI sequence parameters for bracketed paste detection.
    csi_params: Vec<u8>,
}

/// Phase of an escape sequence being consumed.
#[derive(Debug, Clone, Copy)]
enum EscSeqPhase {
    /// Received ESC (0x1B), waiting for the next byte.
    Start,
    /// Received ESC [ — consuming CSI parameter/intermediate bytes.
    Csi,
}

impl SessionInterceptor {
    /// Create a new interceptor.
    /// `ai_callback` is None -> interceptor is disabled (pure passthrough).
    /// `ai_callback` is Some -> interceptor will intercept `;` input.
    /// `status_callback` is Some -> interceptor will intercept `/status` input.
    pub fn new(
        ai_callback: Option<Box<AiCallback>>,
        status_callback: Option<Box<StatusCallback>>,
    ) -> Self {
        Self {
            state: InterceptorState::Passthrough,
            line_shadow: Vec::with_capacity(4096),
            cancel_pty_line: false,
            output_buffer: OutputBuffer::new(8192),
            ai_callback,
            status_callback,
            escape_seq: None,
            in_bracketed_paste: false,
            csi_params: Vec::with_capacity(16),
        }
    }

    /// Feed a single byte from stdin. Returns the action the caller should take.
    pub fn feed_stdin(&mut self, byte: u8) -> StdinAction {
        // Escape sequence tracking takes precedence over state machine.
        // Input methods (Chinese IME, etc.) and terminal keys (arrows, F-keys)
        // send multi-byte escape sequences starting with 0x1B.  Consuming them
        // here prevents them from corrupting line_shadow (Passthrough)
        // or cancelling the AI input prematurely (AiInput).
        if let Some(phase) = self.escape_seq.take() {
            return self.handle_escape_seq_byte(byte, phase);
        }

        match self.state {
            InterceptorState::Passthrough => {
                match byte {
                    b'\r' | b'\n' => {
                        // In bracketed paste mode, don't trigger AI/NL detection
                        // for pasted newlines — just forward them.
                        if self.in_bracketed_paste {
                            return StdinAction::Forward;
                        }
                        // End of line — check for /status first, then AI prefix.
                        if self.status_callback.is_some() && is_status_command(&self.line_shadow) {
                            self.line_shadow.clear();
                            self.cancel_pty_line = true;
                            self.state = InterceptorState::AiProcessing;
                            return StdinAction::TriggerStatus;
                        }
                        // Check whether the accumulated input starts with
                        // `;` or `；` to trigger AI.
                        if self.ai_callback.is_some() && starts_with_ai_prefix(&self.line_shadow) {
                            let line = String::from_utf8_lossy(&self.line_shadow).to_string();
                            let question = extract_ai_question(&line);
                            self.line_shadow.clear();
                            self.cancel_pty_line = true;
                            self.state = InterceptorState::AiProcessing;
                            return StdinAction::TriggerAi(question);
                        }
                        self.line_shadow.clear();
                    }
                    0x03 => {
                        // Ctrl+C — discard current shadow line
                        self.line_shadow.clear();
                    }
                    0x7F | 0x08 => {
                        // Backspace — pop last UTF-8 character from shadow
                        pop_last_utf8_char(&mut self.line_shadow);
                    }
                    0x15 => {
                        // Ctrl+U — clear shadow line
                        self.line_shadow.clear();
                    }
                    0x1B => {
                        // Start of escape sequence — don't add to shadow
                        self.escape_seq = Some(EscSeqPhase::Start);
                    }
                    0x04 => {
                        // Ctrl+D — don't add to shadow
                    }
                    _ => {
                        // Regular character — add to shadow unless in bracketed paste
                        if !self.in_bracketed_paste {
                            self.line_shadow.push(byte);
                        }
                    }
                }
                StdinAction::Forward
            }
            InterceptorState::AiProcessing => StdinAction::EchoLocally,
        }
    }

    /// Handle a byte in the middle of an escape sequence.
    /// For Passthrough: forward all bytes without updating state flags.
    /// For AiInput: silently consume the sequence (don't cancel).
    fn handle_escape_seq_byte(&mut self, byte: u8, phase: EscSeqPhase) -> StdinAction {
        match phase {
            EscSeqPhase::Start => match byte {
                b'[' => {
                    // CSI sequence — consume parameter/intermediate bytes
                    self.escape_seq = Some(EscSeqPhase::Csi);
                    self.csi_params.clear();
                }
                // Two-byte escape (ESC O, ESC (, etc.) — consume final byte
                _ => {
                    // Sequence complete
                    self.escape_seq = None;
                }
            },
            EscSeqPhase::Csi => {
                if (0x40..=0x7E).contains(&byte) {
                    // Final byte — sequence complete
                    // Check for bracketed paste: ESC [ 200 ~ or ESC [ 201 ~
                    if byte == b'~' {
                        let params: String = String::from_utf8_lossy(&self.csi_params).to_string();
                        if params == "200" {
                            // Bracketed paste start
                            self.in_bracketed_paste = true;
                        } else if params == "201" {
                            // Bracketed paste end
                            self.in_bracketed_paste = false;
                        }
                    }
                    self.escape_seq = None;
                }
                // Otherwise still consuming parameters (0x30-0x3F) or
                // intermediate bytes (0x20-0x2F).
                else {
                    // Collect parameter bytes for bracketed paste detection
                    if byte.is_ascii_digit() || byte == b';' {
                        self.csi_params.push(byte);
                    }
                    self.escape_seq = Some(EscSeqPhase::Csi);
                }
            }
        }
        match self.state {
            InterceptorState::Passthrough => StdinAction::Forward,
            _ => StdinAction::EchoLocally,
        }
    }

    /// Feed PTY output data — buffer for error correction context.
    pub fn feed_pty_output(&mut self, data: &[u8]) {
        self.output_buffer.append(data);
    }

    /// Reset state to passthrough after AI processing completes.
    pub fn finish_ai(&mut self) {
        self.state = InterceptorState::Passthrough;
        self.line_shadow.clear();
        self.cancel_pty_line = false;
        self.in_bracketed_paste = false;
    }

    /// Whether AI is currently processing.
    pub fn is_ai_processing(&self) -> bool {
        self.state == InterceptorState::AiProcessing
    }

    /// Run the AI callback. The callback returns an AiResponse containing
    /// an optional command and display text (or None on error).
    pub fn call_ai(
        &self,
        question: String,
        exit_code: i32,
        secret_vault: Option<&std::sync::Arc<std::sync::Mutex<aish_security::secret::SecretVault>>>,
    ) -> Option<AiResponse> {
        self.ai_callback.as_ref().and_then(|cb| {
            let mut recent = self.recent_output(4000);
            if let Some(vault) = secret_vault {
                let (redacted, _) = vault.lock().unwrap().redact_output(&recent);
                recent = redacted;
            }
            cb(AiQuery {
                question,
                recent_output: recent,
                exit_code,
            })
        })
    }

    /// Check and clear the cancel_pty_line flag.
    /// Returns true when AI was triggered from line-level detection and
    /// the PTY has an echoed input line that needs to be cancelled.
    pub fn take_cancel_pty_line(&mut self) -> bool {
        std::mem::replace(&mut self.cancel_pty_line, false)
    }

    /// Get the recent PTY output for error correction context.
    pub fn recent_output(&self, max_len: usize) -> String {
        let bytes = self.output_buffer.recent(max_len);
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Whether a status_callback is configured.
    pub fn has_status_callback(&self) -> bool {
        self.status_callback.is_some()
    }

    /// Invoke the status callback with the given remote-exec function.
    /// Panics if no status_callback is set.
    pub fn invoke_status_callback(&self, exec_fn: &mut RemoteExecFn) -> String {
        let cb = self
            .status_callback
            .as_ref()
            .expect("invoke_status_callback called without status_callback");
        cb(exec_fn)
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

/// Check whether a byte buffer starts with the ASCII semicolon `;` or the
/// fullwidth semicolon `；` (UTF-8: 0xEF 0xBC 0x9B).
fn starts_with_ai_prefix(line: &[u8]) -> bool {
    line.first() == Some(&b';') || line.starts_with(&[0xEF, 0xBC, 0x9B])
}

/// Check whether the line is exactly `/status` (ignoring leading/trailing whitespace).
fn is_status_command(line: &[u8]) -> bool {
    let s = match std::str::from_utf8(line) {
        Ok(s) => s.trim(),
        Err(_) => return false,
    };
    s == "/status"
}

/// Pop the last complete UTF-8 character from a byte buffer.
pub fn pop_last_utf8_char(buf: &mut Vec<u8>) {
    // Pop trailing continuation bytes (0x80..0xBF), then the leader byte
    while buf.last().is_some_and(|b| b & 0xC0 == 0x80) {
        buf.pop();
    }
    buf.pop();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_callback() -> Box<AiCallback> {
        Box::new(|_q| {
            Some(AiResponse {
                command: Some("echo test".to_string()),
                display_text: String::new(),
                followup: None,
                ask_user: None,
            })
        })
    }

    fn noop_callback_no_cmd() -> Box<AiCallback> {
        Box::new(|_q| None)
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
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        assert_eq!(ic.feed_stdin(b'a'), StdinAction::Forward);
        assert_eq!(ic.feed_stdin(b'b'), StdinAction::Forward);
    }

    #[test]
    fn test_semicolon_triggers_ai_on_enter() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        // `;` alone is Forward; AI triggers when Enter is pressed
        assert_eq!(ic.feed_stdin(b';'), StdinAction::Forward);
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::TriggerAi(_)));
        assert_eq!(ic.state, InterceptorState::AiProcessing);
        assert!(ic.take_cancel_pty_line());
    }

    #[test]
    fn test_semicolon_midline_does_not_trigger_ai() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        // pwd; → starts with 'p', not ';' → Enter should be Forward
        ic.feed_stdin(b'p');
        ic.feed_stdin(b'w');
        ic.feed_stdin(b'd');
        ic.feed_stdin(b';');
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_ai_input_captures_question() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
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
        let mut ic = SessionInterceptor::new(None, None);
        assert_eq!(ic.feed_stdin(b';'), StdinAction::Forward);
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_fullwidth_semicolon_triggers_ai() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        // ； = 0xEF 0xBC 0x9B
        ic.feed_stdin(0xEF);
        ic.feed_stdin(0xBC);
        ic.feed_stdin(0x9B);
        ic.feed_stdin(b'h');
        ic.feed_stdin(b'i');
        if let StdinAction::TriggerAi(q) = ic.feed_stdin(b'\r') {
            assert_eq!(q, "hi");
        } else {
            panic!("expected TriggerAi");
        }
    }

    #[test]
    fn test_ctrl_c_clears_shadow() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        ic.feed_stdin(b';');
        ic.feed_stdin(b'h');
        ic.feed_stdin(b'i');
        assert_eq!(ic.feed_stdin(0x03), StdinAction::Forward);
        // shadow was cleared — new line with ; should trigger AI
        ic.feed_stdin(b';');
        assert!(matches!(ic.feed_stdin(b'\r'), StdinAction::TriggerAi(_)));
    }

    #[test]
    fn test_backspace_pops_from_shadow() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        ic.feed_stdin(b'l');
        ic.feed_stdin(b's');
        ic.feed_stdin(0x7F); // backspace removes 's'
        ic.feed_stdin(b';'); // now shadow is "l;" — starts with 'l', not ';'
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_ctrl_u_clears_shadow() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        ic.feed_stdin(b'a');
        ic.feed_stdin(b'b');
        ic.feed_stdin(0x15); // Ctrl+U clears shadow
        ic.feed_stdin(b';'); // now shadow is ";" — triggers AI
        assert!(matches!(ic.feed_stdin(b'\r'), StdinAction::TriggerAi(_)));
    }

    #[test]
    fn test_escape_sequence_not_in_shadow() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        // Simulate arrow key: ESC [ A
        ic.feed_stdin(b';');
        ic.feed_stdin(0x1B); // ESC
        ic.feed_stdin(b'['); // CSI
        ic.feed_stdin(b'A'); // final byte (up arrow)
                             // shadow is just ";" — triggers AI
        assert!(matches!(ic.feed_stdin(b'\r'), StdinAction::TriggerAi(_)));
    }

    #[test]
    fn test_cancel_pty_line_flag() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        assert!(!ic.take_cancel_pty_line());
        ic.feed_stdin(b';');
        ic.feed_stdin(b'\r');
        // Flag is set but we need to call take_cancel_pty_line
        // (normally done by forwarding loop, not in this order)
    }

    #[test]
    fn test_finish_ai_resets_to_passthrough() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        ic.feed_stdin(b';');
        ic.feed_stdin(b'\r');
        assert!(ic.is_ai_processing());
        ic.finish_ai();
        assert_eq!(ic.state, InterceptorState::Passthrough);
        // After finish_ai, a new ; + Enter should trigger again
        ic.feed_stdin(b';');
        assert!(matches!(ic.feed_stdin(b'\r'), StdinAction::TriggerAi(_)));
    }

    #[test]
    fn test_recent_output_captures_pty_data() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        ic.feed_pty_output(b"hello ");
        ic.feed_pty_output(b"world\n");
        assert!(ic.recent_output(100).contains("hello world"));
    }

    #[test]
    fn test_call_ai_returns_command() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None);
        ic.feed_stdin(b';');
        ic.feed_stdin(b'\r');
        let resp = ic.call_ai("test".to_string(), 0, None);
        assert!(resp.is_some());
        let r = resp.unwrap();
        assert_eq!(r.command, Some("echo test".to_string()));
    }

    #[test]
    fn test_call_ai_returns_none() {
        let ic = SessionInterceptor::new(Some(noop_callback_no_cmd()), None);
        let cmd = ic.call_ai("test".to_string(), 0, None);
        assert!(cmd.is_none());
    }

    // ---- Helper function tests ----

    #[test]
    fn test_starts_with_ai_prefix_ascii() {
        assert!(starts_with_ai_prefix(b";hello"));
        assert!(starts_with_ai_prefix(b";"));
    }

    #[test]
    fn test_starts_with_ai_prefix_fullwidth() {
        assert!(starts_with_ai_prefix(&[0xEF, 0xBC, 0x9B, b'h', b'i']));
        assert!(starts_with_ai_prefix(&[0xEF, 0xBC, 0x9B]));
    }

    #[test]
    fn test_starts_with_ai_prefix_negative() {
        assert!(!starts_with_ai_prefix(b"hello"));
        assert!(!starts_with_ai_prefix(b"ls;pwd"));
        assert!(!starts_with_ai_prefix(b""));
        // Incomplete fullwidth semicolon (just first byte)
        assert!(!starts_with_ai_prefix(&[0xEF]));
    }

    #[test]
    fn test_pop_last_utf8_char_ascii() {
        let mut buf = vec![b'a', b'b', b'c'];
        pop_last_utf8_char(&mut buf);
        assert_eq!(buf, b"ab");
    }

    #[test]
    fn test_pop_last_utf8_char_cjk() {
        // ；= 0xEF 0xBC 0x9B
        let mut buf = vec![b'x', 0xEF, 0xBC, 0x9B];
        pop_last_utf8_char(&mut buf);
        assert_eq!(buf, b"x");
    }
}
