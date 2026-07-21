/// Clean the LLM's raw output into a hint suffix suitable for `Hinter`.
///
/// Rules:
/// 1. Strip surrounding quotes (single or double) and triple backticks.
/// 2. Take only the first line (rustyline Hinter is single-line).
/// 3. Trim leading/trailing whitespace.
/// 4. If the model repeated the user's input as a prefix, strip that prefix.
/// 5. Return None when the result is empty.
/// 6. Never prepend a space — the model is responsible for including one
///    in its own output when the language needs it (English etc.). CJK
///    text does not use word separators, so a glued-on space reads as a
///    typo there.
pub fn sanitize_suffix(raw: &str, current_question: &str) -> Option<String> {
    let mut s = raw.trim();

    s = s.trim_start_matches("```").trim_end_matches("```");

    // Skip a leading language-identifier line like ```bash\n...
    if let Some(first_line) = s.split('\n').next() {
        if !first_line.contains(' ') && !first_line.is_empty() {
            if let Some(newline_pos) = s.find('\n') {
                s = &s[newline_pos + 1..];
            }
        }
    }

    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = &s[1..s.len() - 1];
    }

    let first_line = s.split('\n').next().unwrap_or("").trim();
    if first_line.is_empty() {
        return None;
    }

    let question_trimmed = current_question.trim();
    let stripped = if !question_trimmed.is_empty() && first_line.starts_with(question_trimmed) {
        &first_line[question_trimmed.len()..]
    } else {
        first_line
    }
    .trim();

    if stripped.is_empty() {
        return None;
    }

    // Cap the visible width so the ghost text stays a short hint, not a
    // paragraph. Models routinely ignore the "3-15 tokens" instruction in
    // the system prompt and return full sentences. We truncate at the last
    // natural boundary (punctuation / space) within the width budget.
    let capped = cap_suffix_width(stripped, 20);

    // Never prepend a space — the model includes one when the language
    // needs it. CJK doesn't use word separators, so a glued-on space reads
    // as a typo.
    Some(capped)
}

/// Truncate `suffix` so its visible terminal width ≤ `max_width`. If
/// truncation is needed, break at the last natural boundary (CJK/ASCII
/// punctuation or space) within the budget so the result reads naturally.
fn cap_suffix_width(suffix: &str, max_width: usize) -> String {
    if crate::prompt::term_width(suffix) <= max_width {
        return suffix.to_string();
    }
    // Walk char-by-char, tracking the position of the last natural break
    // point (punctuation or space) that still fits within max_width.
    let natural_breaks = "，。、；：！？,.;:!? \t—–-";
    let mut cum_width = 0usize;
    let mut last_break_end = None; // byte index just past the break char
    for (idx, ch) in suffix.char_indices() {
        let w = crate::prompt::term_char_width(ch);
        if cum_width + w > max_width {
            break;
        }
        cum_width += w;
        if natural_breaks.contains(ch) {
            // Position BEFORE the punctuation char so it's excluded from
            // the result (e.g. "形式输出，..." → "形式输出", not "形式输出，").
            last_break_end = Some(idx);
        }
    }
    match last_break_end {
        // Break at the last punctuation within budget.
        Some(end) => suffix[..end].trim_end().to_string(),
        // No punctuation found — hard-cut at the width boundary.
        None => {
            let mut out = String::new();
            let mut w = 0;
            for ch in suffix.chars() {
                let cw = crate::prompt::term_char_width(ch);
                if w + cw > max_width {
                    break;
                }
                w += cw;
                out.push(ch);
            }
            out
        }
    }
}

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

/// Abstraction over the LLM call used for inline completion.
/// Production wraps `aish_llm::LlmClient`; tests inject a fake.
#[async_trait]
pub trait CompletionProvider: Send + Sync {
    /// Return ONLY the suffix text to append. Empty string is a valid
    /// "no suggestion" answer. `Err(())` means any failure.
    async fn complete(&self, prompt: &str, max_tokens: u32) -> Result<String, ()>;

    /// Update the underlying model (for runtime `/model` switching).
    /// Default no-op; only `LlmCompletionProvider` implements it.
    fn update_model(&self, _model: &str) {}
}

/// Fake provider for tests and for the `AISH_INLINE_COMPLETION_FAUX` runtime hook.
pub struct FakeCompletionProvider {
    pub canned: String,
    pub fail: bool,
    pub calls: AtomicUsize,
}

impl FakeCompletionProvider {
    pub fn new(canned: impl Into<String>) -> Self {
        Self {
            canned: canned.into(),
            fail: false,
            calls: AtomicUsize::new(0),
        }
    }
    pub fn new_failing() -> Self {
        Self {
            canned: String::new(),
            fail: true,
            calls: AtomicUsize::new(0),
        }
    }
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CompletionProvider for FakeCompletionProvider {
    async fn complete(&self, _prompt: &str, _max_tokens: u32) -> Result<String, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(())
        } else {
            Ok(self.canned.clone())
        }
    }
}

use aish_llm::{ChatMessage, LlmResponse};

/// Production `CompletionProvider` backed by `aish_llm::LlmClient`.
pub struct LlmCompletionProvider {
    client: Arc<aish_llm::LlmClient>,
    /// Provider-specific extras (reasoning-toggle flags, JSON-mode marker)
    /// built once at construction and cloned per call. The two construction
    /// flags `disable_thinking` / `enforce_json` are not retained as bools
    /// because nothing reads them after this map is assembled.
    extras: serde_json::Map<String, serde_json::Value>,
}

impl LlmCompletionProvider {
    pub fn new(
        client: Arc<aish_llm::LlmClient>,
        disable_thinking: bool,
        enforce_json: bool,
    ) -> Self {
        Self {
            client,
            extras: build_extras(disable_thinking, enforce_json),
        }
    }

    /// Single-entry wrapper for `chat_completion_with_extras` so the call
    /// site doesn't repeat the 6 fixed arguments three times.
    async fn request(
        &self,
        messages: &[ChatMessage],
        max_tokens: u32,
        extras: serde_json::Map<String, serde_json::Value>,
    ) -> Result<LlmResponse, aish_core::AishError> {
        self.client
            .chat_completion_with_extras(messages, None, false, Some(0.2), Some(max_tokens), extras)
            .await
    }
}

/// Assemble the provider-extras map. Different reasoning-model gateways use
/// different field names to suppress thinking — we send all known variants
/// simultaneously. Providers that don't recognize a field ignore it.
fn build_extras(
    disable_thinking: bool,
    enforce_json: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut extras = serde_json::Map::new();
    if disable_thinking {
        // Anthropic / Claude extended-thinking toggle.
        extras.insert(
            "thinking".to_string(),
            serde_json::json!({"type": "disabled"}),
        );
        // Qwen3 / DeepSeek / vLLM direct flag.
        extras.insert("enable_thinking".to_string(), serde_json::json!(false));
        // Qwen3 via vLLM with template kwargs.
        extras.insert(
            "chat_template_kwargs".to_string(),
            serde_json::json!({"enable_thinking": false}),
        );
        // OpenAI o1-style gateways reject "minimal"; "low" is the most
        // aggressive reduction they accept.
        extras.insert("reasoning_effort".to_string(), serde_json::json!("low"));
    }
    if enforce_json {
        extras.insert(
            "response_format".to_string(),
            serde_json::json!({"type": "json_object"}),
        );
    }
    extras
}

#[async_trait]
impl CompletionProvider for LlmCompletionProvider {
    async fn complete(&self, prompt: &str, max_tokens: u32) -> Result<String, ()> {
        let system_prompt =
            INLINE_COMPLETION_SYSTEM_PROMPT.replace("{max_tokens}", &max_tokens.to_string());
        let messages = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(prompt.to_string()),
        ];

        // Try with extras (thinking suppression, JSON mode) first; if the
        // gateway rejects unknown fields (HTTP 400), retry bare.
        let resp = if self.extras.is_empty() {
            self.request(&messages, max_tokens, serde_json::Map::new())
                .await
        } else {
            match self
                .request(&messages, max_tokens, self.extras.clone())
                .await
            {
                Ok(r) => Ok(r),
                Err(e) => {
                    tracing::debug!(error = %e, "inline completion: extras rejected, retrying bare");
                    self.request(&messages, max_tokens, serde_json::Map::new())
                        .await
                }
            }
        };
        let resp = resp.map_err(|e| {
            tracing::debug!(error = %e, "inline completion: request error");
        })?;

        let LlmResponse::Json(value) = resp else {
            return Err(());
        };
        let (content, _reasoning, _tools, _usage) =
            aish_llm::streaming::StreamParser::parse_response(&value);

        // Simple: take content, extract JSON suffix. Ignore reasoning_content
        // entirely — the model's chain-of-thought is irrelevant to the ghost
        // text. If content is empty or has no JSON, there's nothing to show.
        Ok(content
            .as_deref()
            .and_then(extract_suffix_from_json)
            .unwrap_or_default())
    }

    fn update_model(&self, model: &str) {
        self.client.update_model(model);
    }
}

/// Try to extract the `suffix` field from a model response that should be a
/// strict JSON object wrapped in a ```json code fence. Tolerates a wide
/// variety of malformations: missing fence, prose-wrapped JSON, double-braced
/// `{{...}}`, single-quoted values, etc.
///
/// Three layers: fence extraction → shared `{`×`}` brute-force scan
/// (`ai_handler::extract_json_object_from_text`) → regex-extract the suffix
/// value as last line of defense.
fn extract_suffix_from_json(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Layer 1: code-fence extraction. The prompt asks for ```json fence.
    use regex::Regex;
    static FENCE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let fence_re = FENCE_RE
        .get_or_init(|| Regex::new(r"(?s)```(?:json)?\s*\n?(.*?)```").expect("fence regex"));
    for caps in fence_re.captures_iter(trimmed) {
        let block = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(block) {
            if let Some(s) = suffix_of_value(&value) {
                return Some(s);
            }
        }
    }

    // Layer 2: brute-force `{` × `}` scan via the shared helper.
    if let Some(value) = crate::ai_handler::extract_json_object_from_text(trimmed) {
        if let Some(s) = suffix_of_value(&value) {
            return Some(s);
        }
    }

    // Layer 3: regex extract the suffix value directly. For models that
    // return `{{"suffix": "..."}}` or otherwise broken JSON, this is the
    // last line of defense.
    static SUFFIX_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = SUFFIX_RE
        .get_or_init(|| Regex::new(r#""suffix"\s*:\s*"((?:[^"\\]|\\.)*)""#).expect("suffix regex"));
    let caps = re.captures(trimmed)?;
    let raw = caps.get(1)?.as_str();
    // Single-pass unescape: process \\, \", \n, \t left-to-right so that
    // a literal "\\n" (backslash + n) is NOT mistaken for a newline.
    // The previous chained-replace approach first collapsed "\\\\" → "\\"
    // and then mis-read the result as "\n" → newline.
    let unescaped = unescape_json_suffix(raw);
    if unescaped.is_empty() {
        None
    } else {
        Some(unescaped)
    }
}

/// Decode JSON string escapes in a single left-to-right pass. Handles
/// `\\"`, `\\\\`, `\\n`, `\\t`; any unrecognized escape is passed through
/// literally (backslash + char).
fn unescape_json_suffix(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Pull the non-empty `suffix` field out of a parsed JSON value.
fn suffix_of_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("suffix")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

const INLINE_COMPLETION_SYSTEM_PROMPT: &str = "\
Predict the short text to append to the user's partial AI-shell instruction.\n\
Respond with ONLY a raw JSON object — no markdown, no ```fence```, no text before or after.\n\
\n\
Format: {\"suffix\": \"<text>\"}\n\
\n\
Rules:\n\
- suffix is appended directly to the user's input. Do NOT repeat what they already typed.\n\
- Stop at the first natural boundary. Keep it SHORT — a few words, ~10 chars max.\n\
- Match the user's language. English: prefix a space. CJK: never prefix a space.\n\
- If truly unpredictable, return {\"suffix\": \"\"}.\n\
\n\
Examples (input → output):\n\
查看当前系统的 → {\"suffix\": \"CPU使用率\"}\n\
检查哪些端口 → {\"suffix\": \"正在监听\"}\n\
分析 → {\"suffix\": \"磁盘占用\"}\n\
how do I find → {\"suffix\": \" large files\"}\n\
监控 → {\"suffix\": \"CPU温度\"}\n\
\n\
Output ONLY the JSON. No code fence. No explanation. Max {max_tokens} tokens.\n\
";

/// Inspect the runtime environment and decide which provider to use.
/// Honors `AISH_INLINE_COMPLETION_FAUX` for end-to-end tests:
///   - `txt:<string>` → FakeCompletionProvider returning `<string>`
///   - `err:<anything>` → FakeCompletionProvider that always fails
///   - unset          → real LlmCompletionProvider
pub fn build_default_provider(
    client: Arc<aish_llm::LlmClient>,
    disable_thinking: bool,
    enforce_json: bool,
) -> Arc<dyn CompletionProvider> {
    if let Ok(spec) = std::env::var("AISH_INLINE_COMPLETION_FAUX") {
        if let Some(canned) = spec.strip_prefix("txt:") {
            return Arc::new(FakeCompletionProvider::new(canned));
        }
        if spec.starts_with("err:") {
            return Arc::new(FakeCompletionProvider::new_failing());
        }
    }
    Arc::new(LlmCompletionProvider::new(
        client,
        disable_thinking,
        enforce_json,
    ))
}

use std::sync::Mutex;
use std::time::Duration;

use aish_config::InlineCompletionConfig;
use aish_llm::CancellationToken;

use crate::autosuggest::AutoSuggest;
use crate::input::extract_ai_question;

/// Single-cell spinner that replaces ONLY the `◆` icon at column 1 of the
/// prompt while an inline-completion LLM call is in flight. `aish` and the
/// rest of the badge stay exactly where rustyline rendered them — the
/// spinner never rewrites them, so nothing shifts regardless of how the
/// terminal measures `◆`'s width.
///
/// Each frame is a single accent-colored Braille glyph from
/// `theme::SPINNER_STATUS` (8-frame "⣾⣽⣻⢿⡿⣟⣯⣷" cycle), written to
/// column 1. On CJK terminals that render `◆` as 2 columns, the terminal
/// auto-clears the second column of the `◆` cell when the narrow spinner
/// glyph overwrites its first — `aish` never moves.
///
/// Writes go directly to stderr via `\x1b7` (save cursor) + optional
/// `\x1b[<N>A` (move up N lines, navigating to the prompt line when input
/// wraps) + `\x1b[1G` (move to column 1) + frame + `\x1b8` (restore cursor).
/// `lines_up` is recomputed at each tick from current prompt width + input
/// width + terminal cols so it stays correct as the user keeps typing. The
/// up-sequence is omitted entirely when `lines_up == 0` because ANSI
/// `CSI 0 A` is interpreted as `CSI 1 A` by most terminals (parameter 0
/// falls back to the default of 1) — emitting it would push the spinner
/// onto the previous line.
///
/// All geometry values are stored as atomics rather than read from the
/// `thread_local` globals in `readline.rs` (`CURRENT_PROMPT_WIDTH` /
/// `CURRENT_TERMINAL_SIZE`). Those thread-locals are set on the main
/// (readline) thread, but this spinner task runs on a tokio **worker**
/// thread (the runtime is multi-threaded — see `app.rs`), where the
/// thread-locals fall back to their defaults (`0` and `(80, 24)`) and
/// produce a wrong `lines_up` when input wraps.
struct PromptSpinner {
    token: Mutex<Option<Arc<CancellationToken>>>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Visible width of the current input line. Updated by
    /// `InlineCompleter::hint` on every keystroke.
    current_input_width: AtomicUsize,
    /// Visible width of the prompt for the current `read_line` call.
    /// Captured from the main thread on every `hint()`.
    prompt_width: AtomicUsize,
    /// Terminal column count, captured alongside `prompt_width`.
    terminal_cols: AtomicUsize,
}

impl PromptSpinner {
    fn new() -> Self {
        Self {
            token: Mutex::new(None),
            handle: Mutex::new(None),
            current_input_width: AtomicUsize::new(0),
            prompt_width: AtomicUsize::new(0),
            terminal_cols: AtomicUsize::new(80),
        }
    }

    fn set_input_width(&self, width: usize) {
        self.current_input_width.store(width, Ordering::SeqCst);
    }

    /// Capture prompt width and terminal column count from the main thread.
    /// Must be called on every `hint()` (which runs on the readline thread
    /// where these values are current) so the spinner task — running on a
    /// tokio worker thread — can compute `lines_up` correctly.
    fn set_layout(&self, prompt_width: usize, cols: u16) {
        self.prompt_width.store(prompt_width, Ordering::SeqCst);
        self.terminal_cols.store(cols as usize, Ordering::SeqCst);
    }

    /// How many lines above the cursor's current row the prompt starts on.
    /// 0 when the input fits on one line; >0 when it wraps. Computed from
    /// `current_prompt_width + current_input_width` divided by terminal cols.
    fn lines_up(&self) -> u16 {
        let cols = self.terminal_cols.load(Ordering::SeqCst);
        if cols == 0 {
            return 0;
        }
        let prompt_w = self.prompt_width.load(Ordering::SeqCst);
        let input_w = self.current_input_width.load(Ordering::SeqCst);
        let total = prompt_w + input_w;
        let lines = total.div_ceil(cols);
        lines.saturating_sub(1).min(u16::MAX as usize) as u16
    }

    /// Remaining columns on the current cursor line that ghost text may
    /// occupy without wrapping to the next line. Returns 0 when the cursor
    /// sits at the last column (any suffix would wrap). Computed from the
    /// captured prompt_width + input_width and terminal_cols.
    fn available_ghost_width(&self) -> usize {
        let cols = self.terminal_cols.load(Ordering::SeqCst);
        if cols == 0 {
            return 0;
        }
        let prompt_w = self.prompt_width.load(Ordering::SeqCst);
        let input_w = self.current_input_width.load(Ordering::SeqCst);
        let used = (prompt_w + input_w) % cols;
        cols.saturating_sub(used)
    }

    /// Build the cursor-up prefix. Empty when `lines_up == 0` because
    /// `CSI 0 A` is treated as `CSI 1 A` by most terminals.
    fn up_prefix(lines_up: u16) -> String {
        if lines_up == 0 {
            String::new()
        } else {
            format!("\x1b[{}A", lines_up)
        }
    }

    fn start(self: &Arc<Self>, runtime: &tokio::runtime::Handle) {
        self.stop_internal();

        // Skip the spinner when the cursor sits at an exact line boundary
        // (prompt_width + input_width is a multiple of cols). At the
        // boundary, the terminal leaves the cursor either at "pending
        // wrap" (last column of the current line) or auto-wraps it to the
        // next line — and we cannot tell which from width alone. Guessing
        // wrong makes lines_up off-by-one, so the animation (and the
        // stop() restore that writes ◆ aish) lands on the wrong row,
        // overwriting the wrapped input instead of the prompt badge.
        // Skipping start() also makes stop() a no-op (no token set), so
        // the prompt's original ◆ aish stays untouched. The ghost text
        // still renders normally — only the spinner animation is skipped.
        let cols = self.terminal_cols.load(Ordering::SeqCst);
        let total = self.prompt_width.load(Ordering::SeqCst)
            + self.current_input_width.load(Ordering::SeqCst);
        if cols > 0 && total > 0 && total.is_multiple_of(cols) {
            tracing::debug!(
                total,
                cols,
                "spinner: skipping — cursor at exact line boundary (ambiguous position)"
            );
            return;
        }

        let tok = Arc::new(CancellationToken::new());
        *self.token.lock().unwrap() = Some(tok.clone());

        let me = self.clone();
        let handle = runtime.spawn(async move {
            let frames = spinner_frames();
            let mut idx = 0usize;
            loop {
                if tok.is_cancelled() {
                    return;
                }
                {
                    let mut stderr = std::io::stderr().lock();
                    if tok.is_cancelled() {
                        return;
                    }
                    let up = Self::up_prefix(me.lines_up());
                    let _ = write!(
                        stderr,
                        "\x1b7{}\x1b[1G{}\x1b8",
                        up,
                        frames[idx % frames.len()]
                    );
                }
                idx = idx.wrapping_add(1);
                let mut elapsed_ms: u64 = 0;
                while elapsed_ms < 80 {
                    if tok.is_cancelled() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    elapsed_ms += 20;
                }
            }
        });
        *self.handle.lock().unwrap() = Some(handle);
    }

    /// Cancel current task and synchronously write the restore sequence.
    /// No-op if no spinner was started. Idempotent.
    ///
    /// Only the `◆` icon cell is rewritten — the rest of the badge
    /// (`aish`, path, etc.) stays exactly where rustyline rendered it, so
    /// nothing shifts regardless of how the terminal measures the width
    /// of `◆` (ambiguous-width glyph — 1 col on some terminals, 2 on CJK).
    fn stop(&self) {
        let prev = self.token.lock().unwrap().take();
        let Some(tok) = prev else {
            return;
        };
        tok.cancel();
        self.handle.lock().unwrap().take();
        let up = Self::up_prefix(self.lines_up());
        let mut stderr = std::io::stderr().lock();
        let icon = crate::theme::accent(crate::theme::MODE_ICON);
        let _ = write!(stderr, "\x1b7{}\x1b[1G{}\x1b8", up, icon);
    }

    /// Cancel current task without writing restore (used internally by
    /// `start()` so the new spinner task's first frame is the next visible
    /// write, not our restore).
    fn stop_internal(&self) {
        if let Some(tok) = self.token.lock().unwrap().take() {
            tok.cancel();
        }
        self.handle.lock().unwrap().take();
    }
}

/// Spinner frame sequence: one Braille glyph per frame (accent-colored).
///
/// The spinner replaces ONLY the `◆` icon cell (column 1). The rest of
/// the badge (`aish`, path, prompt symbol) is left untouched — rustyline
/// already rendered it, and rewriting it would risk shifting `aish` when
/// the terminal's measurement of `◆` (an East-Asian-Width Ambiguous
/// glyph) disagrees with our `term_char_width` estimate.
///
/// Because each frame is a single 1-column glyph written to column 1,
/// there is no width-mismatch with the badge and no trailing-cell
/// residue to clean up. On CJK terminals that render `◆` as 2 columns,
/// the terminal automatically clears the second column of the `◆` cell
/// when the narrow spinner glyph overwrites its first column — the
/// visible gap is absorbed by the space that already follows `◆` in the
/// badge, so `aish` never moves.
fn spinner_frames() -> &'static [String] {
    static FRAMES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
        // The single-cell spinner design hinges on MODE_ICON and every
        // SPINNER_STATUS glyph being exactly ONE terminal cell. A multi-char
        // MODE_ICON (or a multi-char spinner glyph) would write multiple
        // cells and silently reintroduce the aish-shift regression.
        debug_assert!(
            crate::theme::MODE_ICON.chars().count() == 1,
            "MODE_ICON must be exactly one character; multi-cell icons break \
             the single-cell spinner design (got {:?})",
            crate::theme::MODE_ICON
        );
        debug_assert!(
            crate::theme::SPINNER_STATUS
                .iter()
                .all(|s| s.chars().count() == 1),
            "every SPINNER_STATUS glyph must be exactly one character"
        );
        crate::theme::SPINNER_STATUS
            .iter()
            .map(|&ch| crate::theme::accent(ch))
            .collect()
    });
    &FRAMES
}
/// RAII guard: stops the spinner on drop. Used in `prefetch()` to guarantee
/// the restore sequence fires on every return path (cancel, timeout, error,
/// sanitize None, success). No-op if `start()` was never called.
struct SpinnerGuard<'a>(&'a PromptSpinner);
impl Drop for SpinnerGuard<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

struct State {
    /// Currently in-flight task: `(input, cancel_token)`. None when idle.
    /// The input doubles as the "last dispatched" marker — if a new hint()
    /// call arrives with the same input, we skip re-dispatching.
    pending: Option<(String, Arc<CancellationToken>)>,
    /// Latest suggestion awaiting acceptance. Written by the prefetch task
    /// after LLM success; consumed (taken) by the accept key handler.
    hint: Option<String>,
}

pub struct InlineCompleter {
    state: Arc<Mutex<State>>,
    provider: Arc<dyn CompletionProvider>,
    history: Arc<Mutex<AutoSuggest>>,
    config: InlineCompletionConfig,
    runtime: tokio::runtime::Handle,
    spinner: Arc<PromptSpinner>,
}

impl InlineCompleter {
    pub fn new(
        provider: Arc<dyn CompletionProvider>,
        history: Arc<Mutex<AutoSuggest>>,
        config: InlineCompletionConfig,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        // Clamp nonsensical config values to safe minima.
        let mut config = config;
        if config.max_tokens == 0 {
            tracing::warn!("inline_completion.max_tokens was 0, clamping to 1");
            config.max_tokens = 1;
        }
        if config.debounce_ms < 50 {
            tracing::warn!(
                "inline_completion.debounce_ms={} < 50, clamping to 50",
                config.debounce_ms
            );
            config.debounce_ms = 50;
        }
        Arc::new(Self {
            state: Arc::new(Mutex::new(State {
                pending: None,
                hint: None,
            })),
            provider,
            history,
            config,
            runtime,
            spinner: Arc::new(PromptSpinner::new()),
        })
    }

    /// Synchronous, called by `Hinter::hint()` through an Arc. Triggers a
    /// prefetch when the input is new and long enough. The ghost is rendered
    /// directly via ANSI escapes from the prefetch task (so it appears after
    /// the LLM returns, without requiring another keystroke). Acceptance is
    /// handled by the `AcceptInlineHintHandler` key binding.
    pub fn hint(self: &Arc<Self>, line: &str) {
        if !crate::input::is_ai_prompt_line(line) {
            return;
        }

        // Capture terminal geometry HERE (on the readline/main thread) and
        // stash it into atomics, because the spinner task runs on a tokio
        // worker thread where the thread_local CURRENT_PROMPT_WIDTH /
        // CURRENT_TERMINAL_SIZE are unavailable (they'd default to 0 / 80
        // and produce a wrong lines_up when input wraps).
        let (cols, _rows) = crate::readline::current_terminal_size();
        self.spinner
            .set_layout(crate::readline::current_prompt_width(), cols);
        // Track input width so the spinner can compute lines_up at write
        // time and navigate to the prompt's actual line when input wraps.
        self.spinner
            .set_input_width(crate::prompt::term_width(line));

        let question = extract_ai_question(line);
        let char_count = question.trim().chars().count();
        if char_count < self.config.min_input_chars {
            return;
        }

        let mut state = self.state.lock().unwrap();
        // Already dispatched for this exact input — don't fire again.
        // Crucially, do NOT clear state.hint here: a prior prefetch may have
        // already filled the slot and if we wipe it the user's accept key
        // (Right/Ctrl+F) finds nothing.
        if state.pending.as_ref().is_some_and(|(inp, _)| inp == line) {
            return;
        }
        // Input changed — any pending hint is now stale.
        state.hint = None;
        // Cancel any in-flight task. Stop the spinner immediately so the
        // prompt snaps back to `◆ aish` on this keypress — the cancelled
        // prefetch task will exit on its own within ~20ms.
        if let Some((_, tok)) = state.pending.take() {
            tok.cancel();
            self.spinner.stop();
        }
        let tok = Arc::new(CancellationToken::new());
        state.pending = Some((line.to_string(), tok.clone()));
        drop(state);
        tracing::debug!(input = %line, "inline hint: dispatched prefetch");

        let me = self.clone();
        let input_owned = line.to_string();
        self.runtime.spawn(async move {
            me.prefetch(input_owned, tok).await;
        });
    }

    /// Take the current hint, if any (clears the slot). Called by
    /// `AcceptInlineHintHandler` when the user presses Right/Ctrl+F.
    pub fn take_hint(&self) -> Option<String> {
        self.state.lock().unwrap().hint.take()
    }

    /// Update the model used for inline completion (called on `/model` switch).
    pub fn update_model(&self, model: &str) {
        self.provider.update_model(model);
    }

    /// Cancel any in-flight prefetch, clear the hint slot, and stop the
    /// spinner. Idempotent — `spinner.stop()` is a no-op when no spinner is
    /// running, so callers can invoke this unconditionally on submit /
    /// mode-change / drop without tracking whether a task is in flight.
    pub fn cancel(&self) {
        let cancelled = {
            let mut state = self.state.lock().unwrap();
            let tok = state.pending.take().map(|(_, t)| t);
            state.hint = None;
            tok
        };
        if let Some(tok) = cancelled {
            tok.cancel();
        }
        self.spinner.stop();
    }

    async fn prefetch(self: Arc<Self>, input: String, token: Arc<CancellationToken>) {
        // 1) Debounce with cancellation via polling (CancellationToken is
        //    not an async future; we poll is_cancelled() every 20ms).
        let mut elapsed_ms: u64 = 0;
        let step_ms: u64 = 20;
        while elapsed_ms < self.config.debounce_ms {
            if token.is_cancelled() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(step_ms)).await;
            elapsed_ms += step_ms;
        }
        if token.is_cancelled() {
            return;
        }
        tracing::debug!(input = %input, "prefetch: debounce complete");

        // The spinner runs from "LLM call starts" until "result applied"
        // (success, error, cancel, or timeout). `SpinnerGuard` ensures the
        // restore sequence fires on every return path.
        self.spinner.start(&self.runtime);
        let spinner_guard = SpinnerGuard(&self.spinner);

        // 2) Snapshot context synchronously.
        let history_lines = {
            let g = self.history.lock().unwrap();
            g.recent_n(self.config.context_lines)
        };
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        let question = extract_ai_question(&input);

        let prompt = build_prompt(&question, &cwd, &history_lines);
        tracing::debug!("prefetch: firing LLM call");

        // Hard cap from config (default 15s). The underlying LlmClient sets
        // a 120s timeout which is far too long for inline completion; users
        // on slow gateways can bump `timeout_secs` in their config.
        let max_tokens = self.config.max_tokens;
        let started = std::time::Instant::now();
        let hard_cap = Duration::from_secs(self.config.timeout_secs.max(1));
        let llm_fut = self.provider.complete(&prompt, max_tokens);
        tokio::pin!(llm_fut);
        let poll_step: u64 = 50;
        let result: Result<String, ()> = loop {
            if token.is_cancelled() {
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "prefetch: cancelled during LLM poll"
                );
                return;
            }
            if started.elapsed() > hard_cap {
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    cap_secs = hard_cap.as_secs(),
                    "prefetch: timed out"
                );
                return;
            }
            tokio::select! {
                r = &mut llm_fut => break r,
                _ = tokio::time::sleep(Duration::from_millis(poll_step)) => {}
            }
        };
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "prefetch: LLM call finished"
        );

        let raw = match result {
            Ok(s) => s,
            Err(_) => {
                tracing::debug!("prefetch: provider error");
                return;
            }
        };
        tracing::debug!(raw = %raw, "prefetch: raw response");

        let suffix = match sanitize_suffix(&raw, &question) {
            Some(s) => s,
            None => {
                tracing::debug!("prefetch: sanitize returned None");
                return;
            }
        };
        tracing::debug!(suffix = %suffix, "prefetch: sanitized");

        // Cap the suffix to the remaining columns on the current cursor
        // line so the ghost text doesn't wrap mid-word. But when even the
        // first character can't fit (e.g., 1 col left but a CJK char needs
        // 2), show the full suffix anyway — a wrapping suggestion is more
        // useful than no suggestion at all. The render_ghost DECSC/DECRC
        // pair keeps the cursor position correct regardless of wrapping.
        let avail = self.spinner.available_ghost_width();
        let suffix = if crate::prompt::term_width(&suffix) > avail {
            let capped = cap_suffix_width(&suffix, avail);
            if capped.is_empty() {
                tracing::debug!(
                    avail_cols = avail,
                    "prefetch: no room for first char on current line, \
                     showing full suffix (will wrap)"
                );
                suffix
            } else {
                tracing::debug!(
                    avail_cols = avail,
                    capped = %capped,
                    "prefetch: truncated suffix to fit current line"
                );
                capped
            }
        } else {
            suffix
        };

        // Stop the spinner BEFORE rendering the ghost text. stop() computes
        // lines_up() from the input width (without the ghost), and its
        // DECSC saves the cursor position to navigate back to the prompt
        // badge row. If the restore ran AFTER render_ghost and the ghost
        // wrapped the cursor onto a new line, the saved position would be
        // wrong and ◆ aish would land on the wrapped input row. Dropping
        // the guard here ensures stop() runs while the cursor is still at
        // the input end. On early-return paths (cancel/timeout/error above)
        // the guard still drops automatically.
        drop(spinner_guard);

        // 3) Freshness check + render ghost + write hint slot. Render first
        //    so the user sees the suggestion before the slot is readable;
        //    a quick Right-Arrow that hits an empty slot falls through to
        //    default cursor-move behavior, which is the safer failure mode.
        let mut state = self.state.lock().unwrap();
        let fresh =
            !token.is_cancelled() && state.pending.as_ref().is_some_and(|(inp, _)| inp == &input);
        if fresh {
            render_ghost(&suffix);
            state.hint = Some(suffix);
            tracing::debug!("prefetch: ghost rendered + slot filled");
        } else {
            tracing::debug!(
                cancelled = token.is_cancelled(),
                "prefetch: stale, discarding"
            );
        }
    }
}

/// Render the ghost suggestion directly to stderr in ANSI 256-color gray
/// (242), then move the cursor back so it sits BEFORE the ghost — the same
/// position rustyline's Hinter would put it. `\x1b[K` clears any longer
/// prior ghost first so a shorter new suffix fully overwrites it.
fn render_ghost(suffix: &str) {
    use std::io::Write;
    let width = crate::prompt::term_width(suffix);
    if width == 0 {
        return;
    }
    let mut stderr = std::io::stderr().lock();
    // DECSC (\x1b7) saves the cursor before writing the ghost; DECRC
    // (\x1b8) restores it afterward. This is critical when the suffix
    // wraps across lines: the previous approach used \x1b[<N>D to move
    // the cursor back, but that sequence can only move left within the
    // current row. A wrapped ghost stranded the cursor on the wrong line,
    // which in turn made the spinner's ◆ aish restore (via SpinnerGuard)
    // write to the wrong row.
    let _ = write!(stderr, "\x1b7\x1b[K{}\x1b8", crate::theme::dim(suffix));
    let _ = stderr.flush();
}

fn build_prompt(question: &str, cwd: &str, history_lines: &[String]) -> String {
    let history_block = if history_lines.is_empty() {
        "(none)".to_string()
    } else {
        history_lines
            .iter()
            .map(|h| format!("  {}", h))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Current working directory: {cwd}\n\n\
         Recent shell commands (most recent first):\n\
         {history_block}\n\n\
         Current partial input (the part after \";\"):\n\
         {question}\n\n\
         Return only the suffix to append:"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_suffix_parses_clean_json() {
        assert_eq!(
            extract_suffix_from_json(r#"{"suffix": "文件"}"#),
            Some("文件".to_string())
        );
    }

    #[test]
    fn extract_suffix_parses_json_wrapped_in_prose() {
        assert_eq!(
            extract_suffix_from_json(r#"Sure, here: {"suffix": "files"}"#),
            Some("files".to_string())
        );
    }

    #[test]
    fn extract_suffix_strips_markdown_fences() {
        assert_eq!(
            extract_suffix_from_json("```json\n{\"suffix\": \"abc\"}\n```"),
            Some("abc".to_string())
        );
    }

    /// Happy path for the new prompt format: a single ```json fence with
    /// the JSON inside. Mirrors what the prompt asks the model to return.
    #[test]
    fn extract_suffix_from_fenced_block() {
        let resp = "```json\n{\"suffix\": \"地址是什么\"}\n```";
        assert_eq!(
            extract_suffix_from_json(resp),
            Some("地址是什么".to_string())
        );
    }

    /// Model adds prose around the fence. Layer 1 still finds it.
    #[test]
    fn extract_suffix_from_fenced_block_with_prose() {
        let resp = "Sure, here's the completion:\n```json\n{\"suffix\": \"文件\"}\n```\nDone.";
        assert_eq!(extract_suffix_from_json(resp), Some("文件".to_string()));
    }

    /// Regression: some models return the JSON example wrapped in extra
    /// braces (e.g. `{{"suffix": "..."}}`), which is not valid JSON. The
    /// regex fallback should still extract the suffix value.
    #[test]
    fn extract_suffix_handles_double_braces() {
        assert_eq!(
            extract_suffix_from_json(r#"{{"suffix": "地址是什么"}}"#),
            Some("地址是什么".to_string())
        );
    }

    /// Regression: model wraps the JSON in prose + extra braces.
    #[test]
    fn extract_suffix_handles_prose_with_double_braces() {
        assert_eq!(
            extract_suffix_from_json(r#"Sure! Here you go: {{"suffix": "文件"}}. Done."#),
            Some("文件".to_string())
        );
    }

    #[test]
    fn extract_suffix_unescapes_basic_escapes() {
        assert_eq!(
            extract_suffix_from_json(r#"{"suffix": "hello \"world\""}"#),
            Some("hello \"world\"".to_string())
        );
    }

    /// Regression: a literal backslash-n in the JSON (`\\n` — two chars)
    /// must NOT be turned into an actual newline. The old chained-replace
    /// approach first collapsed `\\` → `\`, then mis-read the result as
    /// `\n` → newline. The single-pass scanner avoids this.
    #[test]
    fn extract_suffix_preserves_literal_backslash_n() {
        // JSON: "foo\\nbar" → should decode to "foo\nbar" (literal \ + n)
        assert_eq!(
            extract_suffix_from_json(r#"{"suffix": "foo\\nbar"}"#),
            Some("foo\\nbar".to_string())
        );
        // But a real JSON newline (\n) should still decode to a newline.
        assert_eq!(
            extract_suffix_from_json("{\"suffix\": \"a\\nb\"}"),
            Some("a\nb".to_string())
        );
    }

    #[test]
    fn extract_suffix_returns_none_for_missing_field() {
        assert_eq!(extract_suffix_from_json(r#"{"foo": "bar"}"#), None);
    }

    #[test]
    fn extract_suffix_returns_none_for_empty_input() {
        assert_eq!(extract_suffix_from_json(""), None);
        assert_eq!(extract_suffix_from_json("   "), None);
    }

    #[test]
    fn extract_suffix_returns_none_for_invalid_json() {
        assert_eq!(extract_suffix_from_json("not json at all"), None);
    }

    #[test]
    fn sanitize_passes_through_clean_text() {
        assert_eq!(
            sanitize_suffix("list all files", "how do I"),
            Some("list all files".to_string())
        );
    }

    #[test]
    fn sanitize_strips_surrounding_quotes() {
        assert_eq!(
            sanitize_suffix("\"list files\"", "how do I"),
            Some("list files".to_string())
        );
    }

    #[test]
    fn sanitize_strips_code_fences() {
        assert_eq!(
            sanitize_suffix("```bash\nlist files\n```", "how do I"),
            Some("list files".to_string())
        );
    }

    #[test]
    fn sanitize_truncates_at_first_newline() {
        assert_eq!(
            sanitize_suffix("list files\nmore stuff", "how do I"),
            Some("list files".to_string())
        );
    }

    #[test]
    fn sanitize_strips_repeated_prefix() {
        // Model echoed back the user's partial input.
        assert_eq!(
            sanitize_suffix("how do I list files", "how do I"),
            Some("list files".to_string())
        );
    }

    #[test]
    fn sanitize_returns_none_for_empty() {
        assert_eq!(sanitize_suffix("", "how do I"), None);
    }

    #[test]
    fn sanitize_returns_none_for_only_quotes() {
        assert_eq!(sanitize_suffix("\"\"", "how do I"), None);
    }

    #[test]
    fn sanitize_returns_none_for_only_whitespace() {
        assert_eq!(sanitize_suffix("   \n  ", "how do I"), None);
    }

    #[test]
    fn sanitize_trims_leading_whitespace() {
        // We never prepend a space — the model is responsible for including
        // one in its output when the language needs it.
        assert_eq!(
            sanitize_suffix("   list files", "how do I"),
            Some("list files".to_string())
        );
    }

    #[test]
    fn sanitize_no_leading_space_for_cjk_input() {
        // CJK input never wants a leading space glued onto the suffix.
        assert_eq!(
            sanitize_suffix("系统IP地址", "如何查看"),
            Some("系统IP地址".to_string())
        );
    }

    #[test]
    fn sanitize_no_leading_space_when_question_ends_with_space() {
        assert_eq!(
            sanitize_suffix("list files", "how do I "),
            Some("list files".to_string())
        );
    }

    #[test]
    fn cap_suffix_short_text_unchanged() {
        assert_eq!(cap_suffix_width("形式输出", 20), "形式输出");
        assert_eq!(cap_suffix_width("list files", 20), "list files");
    }

    #[test]
    fn cap_suffix_long_cjk_truncates_at_punctuation() {
        // 16 CJK chars = 32 cols > 20. Break at the ，(comma) at col 10.
        let long = "形式输出，包含所有接口信息";
        let capped = cap_suffix_width(long, 20);
        assert_eq!(capped, "形式输出");
    }

    #[test]
    fn cap_suffix_long_ascii_truncates_at_space() {
        let long = "processes consuming the most memory and CPU";
        let capped = cap_suffix_width(long, 20);
        // Break at the last space within 20 cols.
        assert!(capped.chars().count() <= 20);
        assert!(!capped.is_empty());
    }

    #[test]
    fn cap_suffix_no_punctuation_hard_cuts() {
        // No punctuation within budget → hard cut at width boundary.
        let long = "abcdefghijklmnopqrstuvwxyz";
        let capped = cap_suffix_width(long, 10);
        assert_eq!(capped, "abcdefghij");
    }

    #[test]
    fn cap_suffix_empty_input() {
        assert_eq!(cap_suffix_width("", 20), "");
    }

    #[tokio::test]
    async fn fake_provider_returns_canned_value() {
        let p = FakeCompletionProvider::new("list files");
        let r = p.complete("anything", 32).await.unwrap();
        assert_eq!(r, "list files");
        assert_eq!(p.call_count(), 1);
    }

    #[tokio::test]
    async fn fake_provider_failure_propagates() {
        let p = FakeCompletionProvider::new_failing();
        assert!(p.complete("x", 32).await.is_err());
    }
}

#[cfg(test)]
mod completer_tests {
    use super::*;
    use crate::autosuggest::AutoSuggest;
    use aish_config::InlineCompletionConfig;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::runtime::Runtime;

    fn mk_completer(
        provider: Arc<dyn CompletionProvider>,
        config: InlineCompletionConfig,
    ) -> (Arc<InlineCompleter>, Arc<Mutex<AutoSuggest>>, Runtime) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let history = Arc::new(Mutex::new(AutoSuggest::new(100)));
        let completer =
            InlineCompleter::new(provider, history.clone(), config, rt.handle().clone());
        (completer, history, rt)
    }

    fn fast_config() -> InlineCompletionConfig {
        InlineCompletionConfig {
            enabled: true,
            debounce_ms: 10,
            context_lines: 3,
            max_tokens: 32,
            min_input_chars: 3,
            disable_thinking: false,
            enforce_json: false,
            timeout_secs: 15,
        }
    }

    /// Strip ANSI CSI sequences (SGR color codes etc.) from `s`, leaving
    /// only the visible content characters. Used by tests to compare what
    /// the terminal actually renders, not the escape-laden raw output.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                              // Consume the rest of the CSI sequence up to the final byte
                              // (any ASCII alphabetic, e.g. 'm' for SGR, 'G' for CHA).
                for ci in chars.by_ref() {
                    if ci.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn hint_no_op_for_too_short_input() {
        let provider = Arc::new(FakeCompletionProvider::new("list files"));
        let (c, _h, _rt) = mk_completer(provider, fast_config());
        // After stripping the `;` prefix, "h" is 1 char < min_input_chars.
        c.hint("; h");
        assert_eq!(c.take_hint(), None);
    }

    #[test]
    fn hint_eventually_populates_slot_after_debounce() {
        let provider = Arc::new(FakeCompletionProvider::new("list files"));
        let (c, _h, rt) = mk_completer(provider, fast_config());

        // hint() spawns the prefetch task on the runtime and returns
        // immediately. The ghost is rendered directly via ANSI escapes,
        // not via Hinter display, so the suffix is observable only via
        // take_hint() — the same path the accept key handler uses.
        c.hint("; how do I");

        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });

        // Do NOT call hint() again here — every hint() call clears the
        // slot on the assumption that input may have changed.
        let suffix = c.take_hint();
        assert_eq!(suffix, Some("list files".to_string()));
    }

    #[test]
    fn hint_clears_slot_when_input_diverges() {
        let provider = Arc::new(FakeCompletionProvider::new("list files"));
        let (c, _h, rt) = mk_completer(provider, fast_config());

        c.hint("; how do I");
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });

        // Different input — hint() clears the slot before dispatching.
        c.hint("; list");
        assert_eq!(c.take_hint(), None);
    }

    /// Regression: ghost text must NOT wrap to the next line. When the
    /// current input line is nearly full, the suffix is truncated to fit
    /// the remaining columns instead of being rendered as-is.
    #[test]
    fn hint_truncates_suffix_to_fit_remaining_cols() {
        let provider = Arc::new(FakeCompletionProvider::new("list files"));
        let (c, _h, rt) = mk_completer(provider, fast_config());

        // cols defaults to 80 in tests; bump prompt_width so "; abc"
        // (5 cols) leaves only 1 col on the line (74 + 5 = 79 → avail = 1).
        crate::readline::set_current_prompt_width(74);
        c.hint("; abc");
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });
        crate::readline::set_current_prompt_width(0);

        // "list files" (10 cols) truncated to 1 col → "l".
        assert_eq!(c.take_hint(), Some("l".to_string()));
    }

    /// Regression: when the remaining columns can't fit even the first
    /// character of the suffix (e.g. CJK on a 1-col slot), the full suffix
    /// is shown anyway (wrapping to the next line) rather than being
    /// suppressed. A wrapping suggestion is more useful than no suggestion.
    #[test]
    fn hint_shows_full_suffix_when_no_room_for_first_char() {
        let provider = Arc::new(FakeCompletionProvider::new("中文建议"));
        let (c, _h, rt) = mk_completer(provider, fast_config());

        // avail = 1 col, but every CJK char is 2 cols → cap returns "".
        // The full suffix "中文建议" should still be stored as the hint.
        crate::readline::set_current_prompt_width(74);
        c.hint("; abc");
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });
        crate::readline::set_current_prompt_width(0);

        assert_eq!(c.take_hint(), Some("中文建议".to_string()));
    }

    #[test]
    fn cancel_clears_state() {
        let provider = Arc::new(FakeCompletionProvider::new("list files"));
        let (c, _h, rt) = mk_completer(provider, fast_config());

        c.hint("; how do I");
        c.cancel();
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });

        // After cancel + settling, the slot must not have been populated
        // for the cancelled dispatch.
        c.hint("; how do I");
        assert_eq!(c.take_hint(), None);
    }

    #[test]
    fn cancel_is_idempotent() {
        let provider = Arc::new(FakeCompletionProvider::new("x"));
        let (c, _h, _rt) = mk_completer(provider, fast_config());
        c.cancel();
        c.cancel();
        c.cancel();
    }

    #[test]
    fn non_ai_input_does_not_dispatch() {
        let provider = Arc::new(FakeCompletionProvider::new("list files"));
        let p = provider.clone();
        let (c, _h, rt) = mk_completer(provider, fast_config());

        // Plain command — InlineCompleter itself never sees this in
        // production (ShellHelper::hint branches first), but the call must
        // be safe.
        c.hint("ls -la");
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });
        assert_eq!(p.call_count(), 0);
    }

    #[test]
    fn cancellation_aborts_in_flight_llm_call() {
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Slow {
            started: Arc<AtomicUsize>,
            finished: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl CompletionProvider for Slow {
            async fn complete(&self, _: &str, _: u32) -> Result<String, ()> {
                self.started.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(5)).await;
                self.finished.fetch_add(1, Ordering::SeqCst);
                Ok("should not see me".to_string())
            }
        }
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn CompletionProvider> = Arc::new(Slow {
            started: started.clone(),
            finished: finished.clone(),
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let history = Arc::new(Mutex::new(AutoSuggest::new(100)));
        let config = InlineCompletionConfig {
            enabled: true,
            debounce_ms: 10,
            context_lines: 3,
            max_tokens: 32,
            min_input_chars: 3,
            disable_thinking: false,
            enforce_json: false,
            timeout_secs: 15,
        };
        let c = InlineCompleter::new(provider, history, config, rt.handle().clone());
        c.hint("; how do I");
        // Wait for debounce (10ms) + some extra time for the LLM call to start
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        // Now cancel while the LLM call is in-flight (sleeping for 5 seconds)
        c.cancel();
        rt.block_on(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
        });
        // No panic, no hang; slot stays empty.
        c.hint("; how do I");
        assert_eq!(c.take_hint(), None);
        // Prove the future was started but never completed (cancelled mid-sleep).
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        drop(c);
    }

    /// Regression for the thread_local bug: the spinner task runs on a tokio
    /// worker thread, where `CURRENT_PROMPT_WIDTH` / `CURRENT_TERMINAL_SIZE`
    /// thread-locals are unavailable (default to 0 / 80). `lines_up` must
    /// read the atomic values captured by `set_layout` instead, so it stays
    /// correct regardless of which thread it runs on.
    #[test]
    fn spinner_lines_up_uses_captured_layout() {
        let s = PromptSpinner::new();

        // prompt 50 cols + input 40 cols = 90 > 80 → wraps → 1 line up.
        s.set_layout(50, 80);
        s.set_input_width(40);
        assert_eq!(s.lines_up(), 1);

        // total exactly 80 → fits → 0 lines up.
        s.set_input_width(30);
        assert_eq!(s.lines_up(), 0);

        // Wraps to 3 lines: 50 + 130 = 180 / 80 = 3 → 2 lines up.
        s.set_layout(50, 80);
        s.set_input_width(130);
        assert_eq!(s.lines_up(), 2);

        // CJK: prompt_width and input_width are visible-cell counts, so a
        // wide-char-heavy input still computes correctly.
        s.set_layout(10, 30);
        s.set_input_width(25); // 35 / 30 = 2 lines
        assert_eq!(s.lines_up(), 1);
    }

    /// Regression for the wrap bug: when the input nearly fills the
    /// terminal width, `available_ghost_width` must report only the
    /// remaining columns on the current cursor line so the caller can
    /// truncate or skip the ghost instead of letting it wrap.
    #[test]
    fn spinner_available_ghost_width_matches_remaining_cols() {
        let s = PromptSpinner::new();

        // 80-col terminal, prompt 50 + input 20 = 70 → 10 cols remain.
        s.set_layout(50, 80);
        s.set_input_width(20);
        assert_eq!(s.available_ghost_width(), 10);

        // Input fills the line exactly: 50 + 30 = 80, cursor wrapped to
        // col 0 of line 2 → full line (80) available there.
        s.set_input_width(30);
        assert_eq!(s.available_ghost_width(), 80);

        // Input wraps past one full line: 50 + 90 = 140, 140 % 80 = 60
        // → cursor at col 60 of line 2 → 20 cols remain.
        s.set_input_width(90);
        assert_eq!(s.available_ghost_width(), 20);

        // Cursor at the very last column: 50 + 29 = 79 → 1 col remain.
        s.set_input_width(29);
        assert_eq!(s.available_ghost_width(), 1);

        // cols == 0 (uninitialized / weird terminal) → no room reported.
        s.set_layout(50, 0);
        assert_eq!(s.available_ghost_width(), 0);
    }

    /// Regression: when prompt_width + input_width is an exact multiple of
    /// cols, the cursor position is ambiguous (pending-wrap vs actually
    /// wrapped). The spinner must NOT start in this state — guessing wrong
    /// makes the animation and the stop() restore land on the wrong row.
    #[test]
    fn spinner_skips_start_at_exact_line_boundary() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let s = Arc::new(PromptSpinner::new());

        // total = 80 = 1 × 80 → exact boundary → skip.
        s.set_layout(50, 80);
        s.set_input_width(30);
        s.clone().start(rt.handle());
        assert!(
            s.token.lock().unwrap().is_none(),
            "spinner should not start when total is an exact multiple of cols"
        );

        // total = 160 = 2 × 80 → exact boundary at 2 lines → skip.
        s.set_layout(50, 80);
        s.set_input_width(110);
        s.clone().start(rt.handle());
        assert!(
            s.token.lock().unwrap().is_none(),
            "spinner should not start at multi-line exact boundary"
        );
    }

    /// Counterpart: the spinner DOES start when total is not at a boundary.
    #[test]
    fn spinner_starts_when_not_at_boundary() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let s = Arc::new(PromptSpinner::new());

        // total = 79 < 80 → single line, not at boundary → start.
        s.set_layout(50, 80);
        s.set_input_width(29);
        s.clone().start(rt.handle());
        assert!(
            s.token.lock().unwrap().is_some(),
            "spinner should start when input fits on one line"
        );

        // Clean up: stop the spawned task.
        s.stop_internal();

        // total = 90, 90 % 80 = 10 ≠ 0 → wrapped but not at boundary → start.
        s.set_layout(50, 80);
        s.set_input_width(40);
        s.clone().start(rt.handle());
        assert!(
            s.token.lock().unwrap().is_some(),
            "spinner should start when wrapped but not at exact boundary"
        );
        s.stop_internal();
    }

    /// Core invariant of the new spinner design: each frame is a SINGLE
    /// glyph that overwrites ONLY the `◆` cell at column 1. Because
    /// nothing else is rewritten, `aish` can never shift — regardless of
    /// how the terminal measures `◆`'s width (the root cause of the old
    /// `◆ aishh` bug and the "aish shifts by one column" regression).
    ///
    /// The assertion strips SGR color codes and verifies the remaining
    /// content is EXACTLY one Braille glyph — not just that one is
    /// present. The old buggy code (`accent("⣾ aish")`) also contains
    /// exactly one Braille glyph but has trailing ` aish`; this test
    /// catches that because it asserts no other characters follow.
    #[test]
    fn spinner_frame_is_single_glyph() {
        let frames = spinner_frames();
        assert!(!frames.is_empty(), "spinner must have frames");
        for (i, frame) in frames.iter().enumerate() {
            let stripped = strip_sgr(frame);
            let mut chars = stripped.chars();
            let glyph = chars.next().unwrap_or_else(|| {
                panic!("frame {i} is empty after SGR strip (full frame {frame:?})")
            });
            assert!(
                ('\u{2800}'..='\u{28FF}').contains(&glyph),
                "frame {i} must start with a Braille glyph, got {glyph:?} \
                 (stripped {stripped:?}, full frame {frame:?})"
            );
            assert_eq!(
                chars.next(),
                None,
                "frame {i} must be EXACTLY one cell after SGR strip, got \
                 {stripped:?}; any extra cell would shift `aish` on terminals \
                 whose `◆` width differs from our estimate"
            );
        }
    }

    /// The restore written by `stop()` must also be exactly `MODE_ICON` —
    /// a single glyph that overwrites ONLY the cell the spinner took over.
    /// A multi-cell restore (the old `◆ aish` approach) is what left the
    /// spinner's trailing `h` on screen (`◆ aishh` bug).
    ///
    /// Strips SGR codes and asserts the content equals MODE_ICON exactly,
    /// so a regression to `accent(format!("{} aish", MODE_ICON))` (which
    /// would pass a mere "contains one ◆" check) is caught.
    #[test]
    fn stop_restore_is_single_icon_glyph() {
        // Reconstruct what `stop()` writes: theme::accent(MODE_ICON).
        let restore = crate::theme::accent(crate::theme::MODE_ICON);
        let stripped = strip_sgr(&restore);
        assert_eq!(
            stripped,
            crate::theme::MODE_ICON,
            "stop() restore must be exactly MODE_ICON after SGR strip, got \
             {stripped:?} (full {restore:?}); a multi-cell restore leaves the \
             spinner's trailing `h` on screen (`◆ aishh` bug)"
        );
        assert_eq!(
            crate::theme::MODE_ICON.chars().count(),
            1,
            "MODE_ICON must be a single character for the single-cell spinner \
             design to work (got {:?})",
            crate::theme::MODE_ICON
        );
    }

    /// End-to-end character-buffer model driven by REAL production output:
    /// the spinner frame and the restore glyph are taken from
    /// `spinner_frames()[0]` and `theme::accent(MODE_ICON)` respectively,
    /// stripped of SGR codes, and applied to a hand-rolled screen row. The
    /// test verifies that cells beyond index 0 are NEVER touched — this is
    /// what guarantees `aish` never moves.
    #[test]
    fn spinner_and_restore_touch_only_icon_cell() {
        // Drive the model with REAL production output, not hardcoded glyphs.
        let spinner_content = strip_sgr(&spinner_frames()[0]);
        let restore_content = strip_sgr(&crate::theme::accent(crate::theme::MODE_ICON));

        // Both must be exactly one character for the single-cell design.
        assert_eq!(
            spinner_content.chars().count(),
            1,
            "spinner frame must be one cell: {spinner_content:?}"
        );
        assert_eq!(
            restore_content.chars().count(),
            1,
            "restore must be one cell: {restore_content:?}"
        );

        let spinner_glyph: char = spinner_content.chars().next().unwrap();
        let restore_glyph: char = restore_content.chars().next().unwrap();

        // Seed: the prompt row as rustyline rendered it.
        let icon_char = crate::theme::MODE_ICON.chars().next().unwrap();
        let seed: Vec<char> = std::iter::once(icon_char)
            .chain(" aish ~/path ➜".chars())
            .collect();

        // Spinner overwrites ONLY cell 0.
        let mut after_spinner = seed.clone();
        after_spinner[0] = spinner_glyph;
        // Cells 1+ must be byte-identical to seed — no shift.
        assert_eq!(
            &after_spinner[1..],
            &seed[1..],
            "spinner must not touch any cell beyond the icon"
        );
        let spinner_rendered: String = after_spinner.iter().collect();
        // `aish` did NOT move — it's still at the same offset.
        assert!(
            spinner_rendered.contains(" aish "),
            "spinner must not shift `aish`: got {spinner_rendered:?}"
        );

        // stop() restores ONLY cell 0.
        let mut after_restore = after_spinner;
        after_restore[0] = restore_glyph;
        // Fully restored to original seed — byte-identical.
        assert_eq!(
            after_restore, seed,
            "prompt must be byte-identical after spinner + restore"
        );
        let restored: String = after_restore.iter().collect();
        // Regression markers must NOT appear.
        assert!(!restored.contains("aishh"), "no `aishh` residue");
    }
}
