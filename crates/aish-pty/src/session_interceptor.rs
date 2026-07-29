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
    /// Line blocked by InputGuard; do NOT forward to PTY.
    Blocked(String),
    /// Line needs user confirmation (Confirm verdict).  Caller should
    /// display the warning, read y/N, and either re-inject `line` into
    /// the PTY or cancel.
    NeedConfirm { reason: String, line: Vec<u8> },
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
    /// True between a Tab keystroke and the next non-Tab stdin byte.
    /// During this window, printable bytes arriving via feed_pty_output
    /// are accumulated as the bash completion result.
    awaiting_completion: bool,
    /// Printable bytes captured from PTY output while awaiting_completion.
    /// Merged into line_shadow on the next non-Tab stdin byte so that
    /// InputGuard sees the fully-completed line on Enter.
    pending_completion: Vec<u8>,
    /// Shadow buffer used ONLY by InputGuard. Unlike `line_shadow`, this
    /// buffer accumulates bracketed-paste bytes too, so a destructive
    /// command pasted into readline still gets screened when the user
    /// subsequently presses Enter. AI/NL/status triggers continue to
    /// consult `line_shadow` (which excludes pasted bytes) so paste
    /// cannot synthesize an AI trigger.
    guard_shadow: Vec<u8>,
    /// InputGuard for pre-screening dangerous commands during sessions.
    input_guard: aish_security::input_guard::InputGuard,
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
    /// `input_guard_enabled` comes from the live security policy
    /// (`security_policy.yaml` / `/setting`) so local and PTY screening
    /// share the same toggle without rebuilding the rule set.
    pub fn new(
        ai_callback: Option<Box<AiCallback>>,
        status_callback: Option<Box<StatusCallback>>,
        input_guard_enabled: bool,
    ) -> Self {
        // Constructing an InputGuard recompiles 16+ regexes and re-reads
        // security_policy.yaml from disk. Cache the policy-built guard so
        // repeated SessionInterceptor construction (one per PTY command)
        // only pays the cost of cloning pre-compiled rules — Regex::clone
        // is Arc-based, so it's cheap.
        static BASE_GUARD: std::sync::OnceLock<aish_security::input_guard::InputGuard> =
            std::sync::OnceLock::new();
        let mut input_guard = BASE_GUARD
            .get_or_init(|| {
                let policy = aish_security::policy::load_policy(None);
                aish_security::input_guard::InputGuard::from_policy(&policy)
            })
            .clone();
        input_guard.set_enabled(input_guard_enabled);
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
            awaiting_completion: false,
            pending_completion: Vec::with_capacity(256),
            guard_shadow: Vec::with_capacity(4096),
            input_guard,
        }
    }

    /// Feed a single byte from stdin. Returns the action the caller should take.
    pub fn feed_stdin(&mut self, byte: u8) -> StdinAction {
        // Commit any pending Tab completion before processing this byte.
        // Completion chars arrived via PTY output since the last Tab;
        // merge them into line_shadow here so the next byte (especially
        // Enter) sees the fully-completed line. Skip on Tab itself so
        // consecutive Tabs reset the awaiting window (Tab handler does
        // its own commit before re-entering awaiting mode).
        if self.awaiting_completion && byte != 0x09 {
            self.commit_pending_completion();
        }

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
                        // In bracketed paste mode, don't trigger AI/NL
                        // detection for pasted newlines. But pasted \r/\n
                        // must STILL go through InputGuard — otherwise
                        // pasting `rm -rf /\nls\n` would let bash execute
                        // the destructive line as soon as the embedded \n
                        // is forwarded, before the user ever presses Enter.
                        // The check below covers both paste and non-paste
                        // cases; the only paste-specific behavior we keep
                        // is skipping the AI/status prefix scan (pasted
                        // `;` must not trigger AI mode).
                        if !self.in_bracketed_paste {
                            // End of line — check for /status first, then AI prefix.
                            if self.status_callback.is_some()
                                && is_status_command(&self.line_shadow)
                            {
                                self.line_shadow.clear();
                                self.guard_shadow.clear();
                                self.cancel_pty_line = true;
                                self.state = InterceptorState::AiProcessing;
                                return StdinAction::TriggerStatus;
                            }
                            // Check whether the accumulated input starts with
                            // `;` or `；` to trigger AI.
                            if self.ai_callback.is_some()
                                && starts_with_ai_prefix(&self.line_shadow)
                            {
                                let line = String::from_utf8_lossy(&self.line_shadow).to_string();
                                let question = extract_ai_question(&line);
                                self.line_shadow.clear();
                                self.guard_shadow.clear();
                                self.cancel_pty_line = true;
                                self.state = InterceptorState::AiProcessing;
                                return StdinAction::TriggerAi(question);
                            }
                        }
                        // InputGuard: check for dangerous commands before
                        // forwarding to PTY. Use guard_shadow (which includes
                        // bracketed-paste bytes) so a pasted destructive
                        // command is screened both on Enter AND on embedded
                        // \r/\n inside a paste.
                        //
                        // NOTE: history recall (↑/↓) and other readline
                        // escape sequences mutate bash's line in ways we
                        // cannot reconstruct from the byte stream alone.
                        // We deliberately do NOT fail-closed on this —
                        // doing so made every ↑+Enter require confirmation
                        // and broke normal shell use. A proper fix needs
                        // PTY-echo reconstruction (read the line bash
                        // actually rendered back from PTY output before
                        // screening), tracked as future work.
                        {
                            let line = String::from_utf8_lossy(&self.guard_shadow).to_string();
                            let verdict = self.input_guard.check(
                                &line,
                                aish_security::input_guard::InputContext::ShellCommand,
                            );
                            match &verdict {
                                aish_security::input_guard::InputVerdict::Block { .. } => {
                                    self.cancel_pty_line = true;
                                    self.line_shadow.clear();
                                    self.guard_shadow.clear();
                                    return StdinAction::Blocked(verdict.format_display());
                                }
                                aish_security::input_guard::InputVerdict::Confirm { .. }
                                | aish_security::input_guard::InputVerdict::Unknown { .. } => {
                                    // N5: reinject guard_shadow (not line_shadow)
                                    // on approval — line_shadow excludes
                                    // bracketed-paste bytes, so a confirmed
                                    // paste command would otherwise reinject
                                    // an empty/truncated line.
                                    let saved = self.guard_shadow.clone();
                                    self.cancel_pty_line = true;
                                    self.line_shadow.clear();
                                    self.guard_shadow.clear();
                                    return StdinAction::NeedConfirm {
                                        reason: verdict.format_display(),
                                        line: saved,
                                    };
                                }
                                aish_security::input_guard::InputVerdict::Allow => {}
                            }
                        }
                        self.line_shadow.clear();
                        self.guard_shadow.clear();
                    }
                    0x03 => {
                        // Ctrl+C — discard current shadow line and any
                        // pending Tab completion.  Without clearing pending
                        // here, stale completion bytes from a previous Tab
                        // would leak into the next command's line_shadow
                        // (e.g. Tab → Ctrl+C → "ls" + Enter → shadow="sswdls").
                        self.line_shadow.clear();
                        self.guard_shadow.clear();
                        self.pending_completion.clear();
                        self.awaiting_completion = false;
                    }
                    0x7F | 0x08 => {
                        // Backspace — pop last UTF-8 character from shadow.
                        // Also abandon pending Tab completion: the user is
                        // manually editing, so any PTY-output completion
                        // captured so far may not match the new line state.
                        pop_last_utf8_char(&mut self.line_shadow);
                        pop_last_utf8_char(&mut self.guard_shadow);
                        self.pending_completion.clear();
                        self.awaiting_completion = false;
                    }
                    0x15 => {
                        // Ctrl+U — clear shadow line and pending completion.
                        self.line_shadow.clear();
                        self.guard_shadow.clear();
                        self.pending_completion.clear();
                        self.awaiting_completion = false;
                    }
                    0x1B => {
                        // Start of escape sequence — don't add to shadow.
                        // Arrow keys, Home/End/Delete, F-keys, bracketed
                        // paste markers, etc. all begin with ESC. The
                        // sequence is consumed byte-by-byte in
                        // handle_escape_seq_byte; only bracketed-paste
                        // payload bytes feed into guard_shadow.
                        self.escape_seq = Some(EscSeqPhase::Start);
                    }
                    0x04 => {
                        // Ctrl+D — don't add to shadow
                    }
                    0x09 => {
                        // Tab — triggers remote bash completion.  Commit
                        // any pending completion from a previous Tab
                        // (handles consecutive Tabs), then enter awaiting
                        // mode.  The completion result arrives via PTY
                        // output and is captured by feed_pty_output.
                        if self.awaiting_completion {
                            self.commit_pending_completion();
                        }
                        self.awaiting_completion = true;
                    }
                    _ => {
                        // Regular character — add to shadow unless in
                        // bracketed paste. guard_shadow always tracks the
                        // byte so InputGuard sees pasted content on Enter.
                        if !self.in_bracketed_paste {
                            self.line_shadow.push(byte);
                        }
                        self.guard_shadow.push(byte);
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
                            // Bracketed paste start. Following bytes are
                            // paste payload; they accumulate into
                            // guard_shadow directly.
                            self.in_bracketed_paste = true;
                        } else if params == "201" {
                            // Bracketed paste end. guard_shadow now holds
                            // the full pasted line snapshot.
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

    /// Merge pending Tab completion into line_shadow and exit awaiting
    /// mode. Called when the next non-Tab stdin byte arrives (so Enter
    /// sees the fully-completed line) and on consecutive Tabs (so the
    /// first Tab's completion is committed before re-entering awaiting).
    fn commit_pending_completion(&mut self) {
        self.line_shadow.extend_from_slice(&self.pending_completion);
        self.guard_shadow
            .extend_from_slice(&self.pending_completion);
        self.pending_completion.clear();
        self.awaiting_completion = false;
    }

    /// Feed PTY output data — buffer for error correction context.
    /// Also captures Tab completion characters when awaiting_completion
    /// is set (between a Tab keystroke and the next non-Tab stdin byte).
    pub fn feed_pty_output(&mut self, data: &[u8]) {
        // bash completion behavior:
        //   - Single candidate: appends printable chars in-place (no
        //     CR/LF/ESC sequences). This is the common case we handle.
        //   - Multiple candidates: prints each on its own line then
        //     redraws the prompt + current input. Output contains CR/LF.
        //   - No completion (ambiguous/empty): bash emits a bell (0x07).
        //
        // Heuristic: if data contains any control char (<0x20 or 0x7f),
        // treat as multi-candidate or no-completion and abandon pending
        // (we can't reliably parse multi-candidate redraws). Otherwise
        // collect printable chars as the completed text.
        if self.awaiting_completion {
            let has_control = data.iter().any(|&b| b < 0x20 || b == 0x7f);
            if has_control {
                self.pending_completion.clear();
                self.awaiting_completion = false;
            } else {
                for &b in data {
                    if b.is_ascii_graphic() || b == b' ' {
                        self.pending_completion.push(b);
                    }
                }
            }
        }
        self.output_buffer.append(data);
    }

    /// Reset state to passthrough after AI processing completes.
    pub fn finish_ai(&mut self) {
        self.state = InterceptorState::Passthrough;
        self.line_shadow.clear();
        self.guard_shadow.clear();
        self.cancel_pty_line = false;
        self.in_bracketed_paste = false;
        self.awaiting_completion = false;
        self.pending_completion.clear();
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

    /// Screen a command that is about to be injected into the PTY by a
    /// non-stdin path (e.g. AI `response.command`, BashExec tool). This
    /// bypasses the feed_stdin state machine — the caller has already
    /// decided to inject and just wants the InputGuard verdict so it can
    /// gate the write to master_fd.
    pub fn screen_command(&self, command: &str) -> aish_security::input_guard::InputVerdict {
        self.input_guard.check(
            command,
            aish_security::input_guard::InputContext::ShellCommand,
        )
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
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        assert_eq!(ic.feed_stdin(b'a'), StdinAction::Forward);
        assert_eq!(ic.feed_stdin(b'b'), StdinAction::Forward);
    }

    #[test]
    fn test_semicolon_triggers_ai_on_enter() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        // `;` alone is Forward; AI triggers when Enter is pressed
        assert_eq!(ic.feed_stdin(b';'), StdinAction::Forward);
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::TriggerAi(_)));
        assert_eq!(ic.state, InterceptorState::AiProcessing);
        assert!(ic.take_cancel_pty_line());
    }

    #[test]
    fn test_semicolon_midline_does_not_trigger_ai() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        // pwd; → starts with 'p', not ';' → Enter should be Forward
        ic.feed_stdin(b'p');
        ic.feed_stdin(b'w');
        ic.feed_stdin(b'd');
        ic.feed_stdin(b';');
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_ai_input_captures_question() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
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
        let mut ic = SessionInterceptor::new(None, None, true);
        assert_eq!(ic.feed_stdin(b';'), StdinAction::Forward);
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_fullwidth_semicolon_triggers_ai() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
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
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
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
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        ic.feed_stdin(b'l');
        ic.feed_stdin(b's');
        ic.feed_stdin(0x7F); // backspace removes 's'
        ic.feed_stdin(b';'); // now shadow is "l;" — starts with 'l', not ';'
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_ctrl_u_clears_shadow() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        ic.feed_stdin(b'a');
        ic.feed_stdin(b'b');
        ic.feed_stdin(0x15); // Ctrl+U clears shadow
        ic.feed_stdin(b';'); // now shadow is ";" — triggers AI
        assert!(matches!(ic.feed_stdin(b'\r'), StdinAction::TriggerAi(_)));
    }

    #[test]
    fn test_escape_sequence_not_in_shadow() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
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
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        assert!(!ic.take_cancel_pty_line());
        ic.feed_stdin(b';');
        ic.feed_stdin(b'\r');
        // Flag is set but we need to call take_cancel_pty_line
        // (normally done by forwarding loop, not in this order)
    }

    #[test]
    fn test_finish_ai_resets_to_passthrough() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
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
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        ic.feed_pty_output(b"hello ");
        ic.feed_pty_output(b"world\n");
        assert!(ic.recent_output(100).contains("hello world"));
    }

    #[test]
    fn test_call_ai_returns_command() {
        let mut ic = SessionInterceptor::new(Some(noop_callback()), None, true);
        ic.feed_stdin(b';');
        ic.feed_stdin(b'\r');
        let resp = ic.call_ai("test".to_string(), 0, None);
        assert!(resp.is_some());
        let r = resp.unwrap();
        assert_eq!(r.command, Some("echo test".to_string()));
    }

    #[test]
    fn test_call_ai_returns_none() {
        let ic = SessionInterceptor::new(Some(noop_callback_no_cmd()), None, true);
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

    // ---- InputGuard Blocked tests ----

    #[test]
    fn test_blocked_destructive_rm_in_session() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"rm -rf /" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::Blocked(_)));
        assert!(ic.take_cancel_pty_line());
    }

    #[test]
    fn test_blocked_dd_device_in_session() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"dd if=/dev/zero of=/dev/sda" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::Blocked(_)));
    }

    #[test]
    fn test_blocked_fork_bomb_in_session() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b":(){ :|:& };:" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::Blocked(_)));
    }

    #[test]
    fn test_safe_command_forwarded_in_session() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"ls -la" {
            ic.feed_stdin(*b);
        }
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_blocked_clears_shadow_after_block() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"rm -rf /etc" {
            ic.feed_stdin(*b);
        }
        let _ = ic.feed_stdin(b'\r');
        // Shadow should be cleared — next safe line should forward
        for b in b"ls" {
            ic.feed_stdin(*b);
        }
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_blocked_does_not_fire_for_confirm_rules() {
        // sudo is a Confirm rule — should produce NeedConfirm, not Forward
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"sudo ls" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::NeedConfirm { .. }));
    }

    // ---- InputGuard NeedConfirm tests ----

    #[test]
    fn test_confirm_sudo_in_session() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"sudo ls" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        match action {
            StdinAction::NeedConfirm { reason, line } => {
                assert!(reason.contains("sudo"));
                assert_eq!(line, b"sudo ls");
            }
            _ => panic!("expected NeedConfirm, got {:?}", action),
        }
        assert!(ic.take_cancel_pty_line());
    }

    /// Helper: feed the bracketed-paste start sequence ESC[200~
    fn feed_paste_start(ic: &mut SessionInterceptor) {
        for &b in b"\x1b[200~" {
            ic.feed_stdin(b);
        }
    }

    /// Helper: feed the bracketed-paste end sequence ESC[201~
    fn feed_paste_end(ic: &mut SessionInterceptor) {
        for &b in b"\x1b[201~" {
            ic.feed_stdin(b);
        }
    }

    #[test]
    fn bracketed_paste_destructive_command_is_screened_on_enter() {
        // Regression: pasted bytes used to bypass InputGuard because they
        // weren't added to line_shadow. After the guard_shadow fix, the
        // Enter following a paste must still trigger Block for a pasted
        // `rm -rf /`.
        let mut ic = SessionInterceptor::new(None, None, true);
        feed_paste_start(&mut ic);
        for &b in b"rm -rf /" {
            ic.feed_stdin(b);
        }
        feed_paste_end(&mut ic);
        let action = ic.feed_stdin(b'\r');
        assert!(
            matches!(action, StdinAction::Blocked(_)),
            "pasted destructive command must be screened on Enter, got {:?}",
            action
        );
    }

    #[test]
    fn bracketed_paste_does_not_synthesize_ai_trigger() {
        // Pasted `;` at start of line used to be ignored by line_shadow,
        // and that behavior is correct — paste must NOT trigger AI.
        // Verify guard_shadow accumulation doesn't break this.
        let mut ic = SessionInterceptor::new(None, None, true);
        feed_paste_start(&mut ic);
        for &b in b"; what is the meaning of life" {
            ic.feed_stdin(b);
        }
        feed_paste_end(&mut ic);
        let action = ic.feed_stdin(b'\r');
        assert!(
            matches!(action, StdinAction::Forward),
            "pasted AI-prefix content must not trigger AI, got {:?}",
            action
        );
    }

    #[test]
    fn bracketed_paste_embedded_newline_screens_destructive_line() {
        // Regression: pasting `rm -rf /\nls\n` used to forward the
        // embedded \n to bash, which executed `rm -rf /` immediately —
        // before the user ever pressed Enter. After the C4 fix, every
        // \r/\n inside a paste must also pass through InputGuard.
        let mut ic = SessionInterceptor::new(None, None, true);
        feed_paste_start(&mut ic);
        let mut blocked = false;
        for &b in b"rm -rf /etc\nls\n" {
            let action = ic.feed_stdin(b);
            if !matches!(action, StdinAction::Forward) {
                blocked = true;
                break;
            }
        }
        assert!(
            blocked,
            "embedded \\n inside bracketed paste with destructive content must be screened"
        );
    }

    #[test]
    fn bracketed_paste_embedded_newline_allows_safe_multiline() {
        // Counter-test: safe multi-line paste must still Forward every
        // byte (including embedded \n) so bash can execute each line.
        let mut ic = SessionInterceptor::new(None, None, true);
        feed_paste_start(&mut ic);
        for &b in b"ls\npwd\n" {
            let action = ic.feed_stdin(b);
            assert!(
                matches!(action, StdinAction::Forward),
                "safe paste byte {:?} should Forward, got {:?}",
                b as char,
                action
            );
        }
        feed_paste_end(&mut ic);
    }

    #[test]
    fn test_confirm_kill_in_session() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"kill -9 1234" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::NeedConfirm { .. }));
    }

    #[test]
    fn test_confirm_clears_shadow() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"sudo ls" {
            ic.feed_stdin(*b);
        }
        let _ = ic.feed_stdin(b'\r');
        // Shadow cleared — next safe line should forward
        for b in b"pwd" {
            ic.feed_stdin(*b);
        }
        assert_eq!(ic.feed_stdin(b'\r'), StdinAction::Forward);
    }

    #[test]
    fn test_block_takes_priority_over_confirm() {
        // sudo rm -rf / should be Blocked, not NeedConfirm
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"sudo rm -rf /" {
            ic.feed_stdin(*b);
        }
        let action = ic.feed_stdin(b'\r');
        assert!(matches!(action, StdinAction::Blocked(_)));
    }

    // ---- Tab completion capture ----

    #[test]
    fn tab_completion_single_candidate_merged_into_shadow() {
        // User types "rm -rf /etc/pa" + Tab + Enter.
        // PTY outputs "sswd" between Tab and Enter (bash completes
        // "passwd" — "pa" was already typed, so the appended part is "sswd").
        // After capture, line_shadow should be "rm -rf /etc/passwd" → Block.
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"rm -rf /etc/pa" {
            ic.feed_stdin(*b);
        }
        ic.feed_stdin(0x09); // Tab
        ic.feed_pty_output(b"sswd");
        let action = ic.feed_stdin(b'\r'); // Enter
        assert!(
            matches!(action, StdinAction::Blocked(_)),
            "Tab-completed destructive command must be blocked"
        );
    }

    #[test]
    fn tab_completion_multiple_candidates_abandoned() {
        // User types "ls /etc/" + Tab; bash shows multiple candidates
        // (output contains newlines). We abandon pending — line_shadow
        // stays as the original "ls /etc/" which doesn't match any
        // rule, so Allow/Forward.
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"ls /etc/" {
            ic.feed_stdin(*b);
        }
        ic.feed_stdin(0x09); // Tab
        ic.feed_pty_output(b"\npasswd  profile\n[root@host ~]# ls /etc/");
        let action = ic.feed_stdin(b'\r');
        assert!(
            matches!(action, StdinAction::Forward),
            "multi-candidate completion abandoned; original line forwarded"
        );
    }

    #[test]
    fn consecutive_tabs_commit_first_completion() {
        // First Tab → awaiting; PTY delivers "sswd"; second Tab should
        // commit "sswd" into shadow then start a new awaiting window.
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"rm -rf /etc/pa" {
            ic.feed_stdin(*b);
        }
        ic.feed_stdin(0x09); // Tab 1
        ic.feed_pty_output(b"sswd");
        ic.feed_stdin(0x09); // Tab 2 — commits "sswd", starts new awaiting
        assert_eq!(
            String::from_utf8_lossy(&ic.line_shadow),
            "rm -rf /etc/passwd"
        );
    }

    #[test]
    fn ctrl_c_after_tab_clears_pending_completion() {
        // Regression: pending completion used to leak across commands.
        // Repro: type "rm -rf /etc/pa" + Tab (PTY appends "sswd"), then
        // Ctrl+C, then "ls" + Enter. Without the abort-key fix, the
        // next non-Tab byte merges stale "sswd" into the new line.
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"rm -rf /etc/pa" {
            ic.feed_stdin(*b);
        }
        ic.feed_stdin(0x09); // Tab
        ic.feed_pty_output(b"sswd");
        assert!(ic.awaiting_completion);
        assert!(!ic.pending_completion.is_empty());

        // Ctrl+C should clear both the shadow and pending completion.
        ic.feed_stdin(0x03);
        assert!(!ic.awaiting_completion);
        assert!(ic.pending_completion.is_empty());
        assert!(ic.line_shadow.is_empty());

        // Type "ls" — pending should NOT carry over, so shadow is just "ls".
        for b in b"ls" {
            ic.feed_stdin(*b);
        }
        assert_eq!(&ic.line_shadow, b"ls");
        let action = ic.feed_stdin(b'\r');
        assert_eq!(
            std::mem::discriminant(&action),
            std::mem::discriminant(&crate::StdinAction::Forward)
        );
    }

    #[test]
    fn ctrl_u_after_tab_clears_pending_completion() {
        let mut ic = SessionInterceptor::new(None, None, true);
        ic.feed_stdin(0x09);
        ic.feed_pty_output(b"abc");
        ic.feed_stdin(0x15); // Ctrl+U
        assert!(!ic.awaiting_completion);
        assert!(ic.pending_completion.is_empty());
    }

    #[test]
    fn backspace_after_tab_clears_pending_completion() {
        let mut ic = SessionInterceptor::new(None, None, true);
        for b in b"rm -rf /etc/pa" {
            ic.feed_stdin(*b);
        }
        ic.feed_stdin(0x09);
        ic.feed_pty_output(b"sswd");
        ic.feed_stdin(0x7F); // Backspace
        assert!(!ic.awaiting_completion);
        assert!(ic.pending_completion.is_empty());
    }

    #[test]
    fn finish_ai_resets_completion_state() {
        let mut ic = SessionInterceptor::new(None, None, true);
        ic.feed_stdin(0x09); // Tab
        ic.feed_pty_output(b"abc");
        assert!(ic.awaiting_completion);
        assert!(!ic.pending_completion.is_empty());
        ic.finish_ai();
        assert!(!ic.awaiting_completion);
        assert!(ic.pending_completion.is_empty());
    }
}
