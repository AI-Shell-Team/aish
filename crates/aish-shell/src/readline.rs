use std::borrow::Cow;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rustyline::completion::{Completer, Pair};
use rustyline::config::Configurer;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::Helper;
use rustyline::{
    Cmd, CompletionType, ConditionalEventHandler, Config, Context, EditMode, Editor, Event,
    EventContext, EventHandler, KeyCode, KeyEvent, Modifiers, RepeatCount,
};

use aish_pty::readline_tab::clamp_pos;

use crate::autosuggest::AutoSuggest;
use crate::completion::CompletionEngine;

/// Slash commands with descriptions for popup completion.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show help information"),
    ("/model", "Show or switch AI model"),
    ("/setup", "Open setup wizard"),
    ("/plan", "Plan mode control"),
    ("/token", "Show token usage"),
    ("/resume", "Resume previous session"),
    ("/feedback", "Submit feedback"),
    ("/record", "Record terminal session (start/stop)"),
    ("/quit", "Exit AI Shell"),
    ("/doctor", "Run system diagnostics"),
    ("/diagnose", "Read-only diagnosis for last failed command"),
    ("/status", "Show system environment status"),
    ("/live_sessions", "List live PTY sessions"),
    ("/kill_live_sessions", "Kill live PTY session(s) by ID"),
    (
        "/audit",
        "Query audit log (who/when/what/AI suggestion/confirm)",
    ),
];

// ---------------------------------------------------------------------------
// Mode toggle key binding handler
// ---------------------------------------------------------------------------

/// Flag set by `ModeToggleHandler` when Shift+Tab or F2 is pressed.
static MODE_TOGGLE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Flag set by `CtrlOHandler` when Ctrl+O is pressed.
static CTRL_O_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Flag set by `SlashHandler` when `/` is pressed on an empty line.
static SLASH_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Max gap between Tab presses to count as "double Tab" (bash second-tab list).
const DOUBLE_TAB_MS: u128 = 800;

struct TabCompletionState {
    last_tab_at: Option<Instant>,
    pending_list: bool,
}

static TAB_STATE: Mutex<TabCompletionState> = Mutex::new(TabCompletionState {
    last_tab_at: None,
    pending_list: false,
});

thread_local! {
    static CURRENT_TERMINAL_SIZE: Cell<(u16, u16)> = const { Cell::new((80, 24)) };
    static CURRENT_PROMPT_WIDTH: Cell<usize> = const { Cell::new(0) };
}

/// Terminal size last seen by rustyline (cols, rows).
pub fn current_terminal_size() -> (u16, u16) {
    CURRENT_TERMINAL_SIZE.with(|s| s.get())
}

/// Visible width (terminal columns) of the prompt for the current `read_line`
/// call. Set just before each readline; read by `InlineCompleter` to detect
/// input wrap.
pub fn current_prompt_width() -> usize {
    CURRENT_PROMPT_WIDTH.with(|c| c.get())
}

pub(crate) fn set_current_prompt_width(width: usize) {
    CURRENT_PROMPT_WIDTH.with(|c| c.set(width));
}

fn refresh_terminal_size<H: Helper>(editor: &mut Editor<H, rustyline::history::DefaultHistory>) {
    if let Some((cols, rows)) = editor.dimensions() {
        CURRENT_TERMINAL_SIZE.with(|s| s.set((cols, rows)));
    }
}

/// Reset double-Tab tracking so a new prompt line starts clean.
pub fn clear_tab_completion_state() {
    if let Ok(mut state) = TAB_STATE.lock() {
        state.last_tab_at = None;
        state.pending_list = false;
    }
}

/// Intercept the second Tab to print a bash-style column list ourselves.
struct TabCompletionHandler {
    engine: Arc<CompletionEngine>,
}

impl ConditionalEventHandler for TabCompletionHandler {
    fn handle(
        &self,
        evt: &Event,
        _n_repeat: RepeatCount,
        _positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        let Event::KeySeq(keys) = evt else {
            return None;
        };
        let key = keys.first()?;
        if key.0 != KeyCode::Tab || key.1 != Modifiers::NONE {
            return None;
        }

        let line = ctx.line();
        let pos = clamp_pos(line, ctx.pos());
        let Some((_word_start, pairs)) = self.engine.complete(line, pos) else {
            if let Ok(mut state) = TAB_STATE.lock() {
                state.pending_list = false;
            }
            return None;
        };

        let now = Instant::now();
        let Ok(mut state) = TAB_STATE.lock() else {
            return None;
        };

        let consecutive = state
            .last_tab_at
            .is_some_and(|t| now.duration_since(t).as_millis() < DOUBLE_TAB_MS);
        if !consecutive {
            state.pending_list = false;
        }
        state.last_tab_at = Some(now);

        if pairs.len() <= 1 {
            state.pending_list = false;
            return None;
        }

        let show_list = consecutive && state.pending_list;
        if show_list {
            state.pending_list = false;
            drop(state);
            let _ = crate::completion_list::print_completion_list(&pairs);
            // Repaint: rustyline must redraw prompt+line; manual stdout redraw
            // overwrites the buffer when the aish prompt uses ANSI/control chars.
            return Some(Cmd::Repaint);
        }

        state.pending_list = true;
        None
    }
}

/// Event handler that sets a flag when Shift+Tab or F2 is pressed,
/// then returns `Cmd::Interrupt` to break out of `read_line`.
struct ModeToggleHandler;

impl ConditionalEventHandler for ModeToggleHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n_repeat: RepeatCount,
        _positive: bool,
        _ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        MODE_TOGGLE_REQUESTED.store(true, Ordering::SeqCst);
        Some(Cmd::Interrupt)
    }
}

/// Event handler that sets a flag when Ctrl+O is pressed,
/// then returns `Cmd::Interrupt` to break out of `read_line`.
struct CtrlOHandler;

impl ConditionalEventHandler for CtrlOHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n_repeat: RepeatCount,
        _positive: bool,
        _ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        CTRL_O_REQUESTED.store(true, Ordering::SeqCst);
        Some(Cmd::Interrupt)
    }
}

/// Event handler that sets a flag when `/` is typed on an empty line,
/// then returns `Cmd::Interrupt` to break out of `read_line` for the
/// slash command completion popup.
struct SlashHandler;

impl ConditionalEventHandler for SlashHandler {
    fn handle(
        &self,
        evt: &Event,
        _n_repeat: RepeatCount,
        _positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        // Only trigger for plain `/` key press (no modifiers)
        if let Event::KeySeq(keys) = evt {
            if let Some(key) = keys.first() {
                if key.1 == Modifiers::NONE && key.0 == KeyCode::Char('/') && ctx.line().is_empty()
                {
                    SLASH_REQUESTED.store(true, Ordering::SeqCst);
                    return Some(Cmd::Interrupt);
                }
            }
        }
        None
    }
}

/// Accept the inline AI completion ghost text (if any) when the user presses
/// Right Arrow or Ctrl+F while in AI mode with the cursor at end of line.
/// Falls through to default behavior otherwise.
struct AcceptInlineHintHandler {
    inline_ai: Arc<crate::inline_completion::InlineCompleter>,
}

impl ConditionalEventHandler for AcceptInlineHintHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n_repeat: RepeatCount,
        _positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        if ctx.pos() != ctx.line().len() {
            return None;
        }
        if !crate::input::is_ai_prompt_line(ctx.line()) {
            return None;
        }
        self.inline_ai
            .take_hint()
            .map(|suffix| Cmd::Insert(1, suffix))
    }
}

/// On Enter (or any line-submit key): clear the visible ghost text before
/// rustyline accepts the line. Our ghost is rendered directly via ANSI
/// escapes, so rustyline doesn't know to clear it during its own repaint.
/// Without this, the ghost would remain frozen on the submitted line.
struct ClearGhostOnSubmitHandler {
    inline_ai: Arc<crate::inline_completion::InlineCompleter>,
}

impl ConditionalEventHandler for ClearGhostOnSubmitHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n_repeat: RepeatCount,
        _positive: bool,
        _ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        // Take the hint BEFORE cancel clears the slot, so we know whether
        // a ghost was visible and needs `\x1b[K` to erase it.
        let had_hint = self.inline_ai.take_hint().is_some();
        // Cancel any in-flight prefetch (debounce / LLM call) and stop the
        // spinner. Without this, a prefetch task dispatched before Enter
        // would keep running — the spinner would re-appear mid-execution
        // of the submitted line, and any late LLM result would render a
        // stale ghost on the already-submitted line.
        self.inline_ai.cancel();
        if had_hint {
            eprint!("\x1b[K");
            use std::io::Write;
            let _ = std::io::stderr().flush();
        }
        None
    }
}

/// Which inline-AI handler to attach for a given key.
enum HandlerKind {
    AcceptInline,
    ClearGhost,
}

/// Bind multiple (key, handler-kind) pairs to the same `InlineCompleter`.
/// Exists because rustyline's `EventHandler` is not `Clone`, so each key
/// needs its own handler instance — this helper hides the repetition.
fn bind_conditional_handlers(
    editor: &mut Editor<ShellHelper, rustyline::history::DefaultHistory>,
    bindings: &[(KeyEvent, HandlerKind)],
    ai: &Arc<crate::inline_completion::InlineCompleter>,
) {
    for (key, kind) in bindings {
        let handler: Box<dyn ConditionalEventHandler> = match kind {
            HandlerKind::AcceptInline => Box::new(AcceptInlineHintHandler {
                inline_ai: Arc::clone(ai),
            }),
            HandlerKind::ClearGhost => Box::new(ClearGhostOnSubmitHandler {
                inline_ai: Arc::clone(ai),
            }),
        };
        editor.bind_sequence(*key, EventHandler::Conditional(handler));
    }
}

/// Shell readline helper: tab completion via CompletionEngine.
struct ShellHelper {
    engine: Arc<CompletionEngine>,
    autosuggest: Arc<Mutex<AutoSuggest>>,
    inline_ai: Option<Arc<crate::inline_completion::InlineCompleter>>,
}

impl ShellHelper {
    fn new(
        engine: Arc<CompletionEngine>,
        autosuggest: Arc<Mutex<AutoSuggest>>,
        inline_ai: Option<Arc<crate::inline_completion::InlineCompleter>>,
    ) -> Self {
        Self {
            engine,
            autosuggest,
            inline_ai,
        }
    }
}

impl Helper for ShellHelper {}

impl Highlighter for ShellHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(crate::theme::dim(hint))
    }
}

impl Hinter for ShellHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        // Cursor not at end-of-line: just bail. Don't cancel the in-flight
        // prefetch — a cursor move alone isn't an input change, and the
        // Right-Arrow accept handler already guards against stale hints via
        // its own `pos == line.len()` check, so no need to abort the request.
        if pos == 0 || pos < line.len() {
            return None;
        }

        let is_ai = crate::input::is_ai_prompt_line(line);
        if let Some(ai) = &self.inline_ai {
            if is_ai {
                ai.hint(line);
                return None;
            }
            ai.cancel();
        }

        let trimmed = line.trim_start();
        let guard = self.autosuggest.lock().unwrap();
        guard.suggest(trimmed).and_then(|s| {
            let start = line.len();
            if start < s.len() && s.is_char_boundary(start) {
                Some(s[start..].to_string())
            } else {
                None
            }
        })
    }
}

impl Validator for ShellHelper {}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let pos = clamp_pos(line, pos);
        let Some((start, pairs)) = self.engine.complete(line, pos) else {
            return Ok((0, Vec::new()));
        };

        // If our Tab handler already armed a list, block rustyline's own column listing
        // on the matching second Tab (handler normally Interrupts before we get here).
        if pairs.len() > 1 {
            if let Ok(state) = TAB_STATE.lock() {
                if state.pending_list {
                    if let Some(last) = state.last_tab_at {
                        let elapsed = last.elapsed().as_millis();
                        if (100..DOUBLE_TAB_MS).contains(&elapsed) {
                            return Ok((start, Vec::new()));
                        }
                    }
                }
            }
        }

        Ok((start, pairs))
    }
}

/// Wrapper around rustyline Editor with shell-friendly configuration.
pub struct ShellReadline {
    editor: Editor<ShellHelper, rustyline::history::DefaultHistory>,
    /// Shared autosuggest engine so both Hinter and external callers can add
    /// suggestions without needing Editor::helper_mut().
    autosuggest: Arc<Mutex<AutoSuggest>>,
    /// Inline AI completer, kept here so we can cancel any in-flight
    /// spinner / prefetch on every readline entry. The submit keys
    /// (Enter/Ctrl+J/Ctrl+M/Ctrl+C) also clear it via bound handlers,
    /// but this guard covers every return path (Interrupted, error, EOF)
    /// so a spinner can never outlive its `read_line` call.
    inline_ai: Option<Arc<crate::inline_completion::InlineCompleter>>,
}

impl ShellReadline {
    pub fn new(
        pty: Arc<Mutex<aish_pty::PersistentPty>>,
        autosuggest: Arc<Mutex<AutoSuggest>>,
        inline_ai: Option<Arc<crate::inline_completion::InlineCompleter>>,
    ) -> rustyline::Result<Self> {
        let engine = Arc::new(CompletionEngine::new(pty));

        let builder = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .auto_add_history(true);
        let config = builder.history_ignore_dups(true)?.build();

        let mut editor = Editor::with_config(config)?;
        editor.set_helper(Some(ShellHelper::new(
            engine.clone(),
            autosuggest.clone(),
            inline_ai.clone(),
        )));
        editor.set_max_history_size(500)?;

        editor.bind_sequence(
            KeyEvent(KeyCode::Tab, Modifiers::NONE),
            EventHandler::Conditional(Box::new(TabCompletionHandler {
                engine: engine.clone(),
            })),
        );

        // Bind Shift+Tab (BackTab) and F2 for mode toggle
        editor.bind_sequence(
            KeyEvent(KeyCode::BackTab, Modifiers::NONE),
            EventHandler::Conditional(Box::new(ModeToggleHandler)),
        );
        editor.bind_sequence(
            KeyEvent(KeyCode::F(2), Modifiers::NONE),
            EventHandler::Conditional(Box::new(ModeToggleHandler)),
        );

        // Bind Ctrl+O for expand/collapsed output browsing
        editor.bind_sequence(
            KeyEvent(KeyCode::Char('O'), Modifiers::CTRL),
            EventHandler::Conditional(Box::new(CtrlOHandler)),
        );

        // Bind `/` on empty line for slash command completion popup
        editor.bind_sequence(
            KeyEvent(KeyCode::Char('/'), Modifiers::NONE),
            EventHandler::Conditional(Box::new(SlashHandler)),
        );

        // Bind Right Arrow and Ctrl+F to accept the inline AI ghost, and
        // bind the line-submit keys to clear it before rustyline repaints.
        // rustyline's `EventHandler` is not `Clone`, so each binding needs
        // its own handler instance — the helpers below hide that.
        if let Some(ref ai) = inline_ai {
            bind_conditional_handlers(
                &mut editor,
                &[
                    (
                        KeyEvent(KeyCode::Right, Modifiers::NONE),
                        HandlerKind::AcceptInline,
                    ),
                    (
                        KeyEvent(KeyCode::Char('f'), Modifiers::CTRL),
                        HandlerKind::AcceptInline,
                    ),
                    (
                        KeyEvent(KeyCode::Enter, Modifiers::NONE),
                        HandlerKind::ClearGhost,
                    ),
                    (
                        KeyEvent(KeyCode::Enter, Modifiers::SHIFT),
                        HandlerKind::ClearGhost,
                    ),
                    (
                        KeyEvent(KeyCode::Char('j'), Modifiers::CTRL),
                        HandlerKind::ClearGhost,
                    ),
                    (
                        KeyEvent(KeyCode::Char('m'), Modifiers::CTRL),
                        HandlerKind::ClearGhost,
                    ),
                    // Ctrl+C: cancel the spinner / clear the ghost BEFORE
                    // rustyline raises `Interrupted`. Without this the
                    // animation keeps running because nothing calls cancel()
                    // on the Interrupted path.
                    (
                        KeyEvent(KeyCode::Char('c'), Modifiers::CTRL),
                        HandlerKind::ClearGhost,
                    ),
                ],
                ai,
            );
        }

        Ok(Self {
            editor,
            autosuggest,
            inline_ai,
        })
    }

    /// Check whether a mode toggle key (Shift+Tab or F2) triggered the
    /// last `Interrupted` error. The flag is consumed on read.
    pub fn was_mode_toggle_requested(&self) -> bool {
        MODE_TOGGLE_REQUESTED.swap(false, Ordering::SeqCst)
    }

    /// Check whether Ctrl+O triggered the last `Interrupted` error.
    /// The flag is consumed on read.
    pub fn was_ctrl_o_requested(&self) -> bool {
        CTRL_O_REQUESTED.swap(false, Ordering::SeqCst)
    }

    /// Check whether `/` on an empty line triggered the last `Interrupted` error.
    /// The flag is consumed on read.
    pub fn was_slash_requested(&self) -> bool {
        SLASH_REQUESTED.swap(false, Ordering::SeqCst)
    }

    /// Read a line with initial text pre-filled, letting the user edit and
    /// submit. Returns `None` on EOF (Ctrl-D).
    pub fn read_line_with_initial(
        &mut self,
        prompt: &str,
        initial: (&str, &str),
    ) -> rustyline::Result<Option<String>> {
        clear_tab_completion_state();
        refresh_terminal_size(&mut self.editor);
        set_current_prompt_width(crate::prompt::strip_ansi_len(prompt));
        // Cancel any spinner / prefetch left over from a previous readline
        // (e.g. after Ctrl+C, an error, or a mode-toggle Interrupted).
        if let Some(ai) = &self.inline_ai {
            ai.cancel();
        }
        match self.editor.readline_with_initial(prompt, initial) {
            Ok(line) => Ok(Some(line)),
            Err(rustyline::error::ReadlineError::Eof) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Read a line with the given prompt.
    /// Returns None on EOF (Ctrl-D).
    /// Supports backslash continuation: lines ending with `\` read
    /// additional lines with a `> ` prompt.
    pub fn read_line(&mut self, prompt: &str) -> rustyline::Result<Option<String>> {
        clear_tab_completion_state();
        refresh_terminal_size(&mut self.editor);
        set_current_prompt_width(crate::prompt::strip_ansi_len(prompt));
        // Cancel any spinner / prefetch left over from a previous readline
        // (e.g. after Ctrl+C, an error, or a mode-toggle Interrupted).
        if let Some(ai) = &self.inline_ai {
            ai.cancel();
        }
        let line = match self.editor.readline(prompt) {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Eof) => return Ok(None),
            Err(e) => return Err(e),
        };

        // Handle multiline continuation (trailing backslash)
        if !line.ends_with('\\') {
            return Ok(Some(line));
        }

        let mut result = line;
        result.truncate(result.len() - 1); // remove trailing backslash

        loop {
            match self.editor.readline("> ") {
                Ok(next) => {
                    if next.is_empty() {
                        break;
                    }
                    let has_continuation = next.ends_with('\\');
                    let trimmed = if has_continuation {
                        let mut s = next;
                        s.truncate(s.len() - 1);
                        s
                    } else {
                        next
                    };
                    result.push(' ');
                    result.push_str(&trimmed);
                    if !has_continuation {
                        break;
                    }
                }
                Err(rustyline::error::ReadlineError::Eof) => return Ok(None),
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    // Ctrl-C during continuation cancels multiline
                    return Ok(Some(result));
                }
                Err(e) => return Err(e),
            }
        }

        Ok(Some(result))
    }

    /// Add a line to the history and autosuggest.
    pub fn add_history_entry(&mut self, line: &str) {
        let _ = self.editor.add_history_entry(line);
        self.autosuggest.lock().unwrap().add(line);
    }

    /// Add a command to the autosuggest engine without adding to history.
    /// Useful for pre-loading history entries so they appear as hints.
    pub fn add_suggestion(&self, command: &str) {
        self.autosuggest.lock().unwrap().add(command);
    }

    /// Update the inline-completion model (called on `/model` switch).
    pub fn update_inline_model(&self, model: &str) {
        if let Some(ai) = &self.inline_ai {
            ai.update_model(model);
        }
    }

    /// Load history from a file (best-effort).
    /// Also populates the autosuggest engine so hints appear immediately.
    pub fn load_history(&mut self, path: &std::path::Path) {
        if self.editor.load_history(path).is_ok() {
            let history = self.editor.history();
            let mut guard = self.autosuggest.lock().unwrap();
            for entry in history.iter() {
                guard.add(entry);
            }
        }
    }

    /// Save history to a file (best-effort).
    pub fn save_history(&mut self, path: &std::path::Path) {
        let _ = self.editor.save_history(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slash_commands_format() {
        for (cmd, desc) in SLASH_COMMANDS {
            assert!(
                cmd.starts_with('/'),
                "slash command must start with /: {}",
                cmd
            );
            assert!(
                !desc.is_empty(),
                "description must not be empty for {}",
                cmd
            );
        }
        assert_eq!(SLASH_COMMANDS.len(), 15);
    }
}
