use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

use aish_pty::readline_tab::{
    clamp_pos, is_path_like_token, to_replacement_pairs, word_start_at, TAB_PROBE_TIMEOUT,
};

use crate::autosuggest::AutoSuggest;

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
];

/// Timeout for PTY completion queries.
const COMPLETION_TIMEOUT: Duration = Duration::from_millis(1200);

// ---------------------------------------------------------------------------
// Mode toggle key binding handler
// ---------------------------------------------------------------------------

/// Flag set by `ModeToggleHandler` when Shift+Tab or F2 is pressed.
static MODE_TOGGLE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Flag set by `CtrlOHandler` when Ctrl+O is pressed.
static CTRL_O_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Flag set by `SlashHandler` when `/` is pressed on an empty line.
static SLASH_REQUESTED: AtomicBool = AtomicBool::new(false);

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

/// Shell readline helper: tab completion delegates to PTY bash.
struct ShellHelper {
    /// Shared reference to the persistent PTY session.
    pty: Arc<Mutex<aish_pty::PersistentPty>>,
    /// Shared autosuggest engine.
    autosuggest: Arc<Mutex<AutoSuggest>>,
}

impl ShellHelper {
    fn new(pty: Arc<Mutex<aish_pty::PersistentPty>>, autosuggest: Arc<Mutex<AutoSuggest>>) -> Self {
        Self { pty, autosuggest }
    }

    /// Drop candidates that would not change the current word (rustyline re-applies
    /// single-candidate replacements even when the word is already complete).
    fn filter_extending_candidates(
        line: &str,
        pos: usize,
        word_start: usize,
        candidates: Vec<aish_pty::CompletionCandidate>,
    ) -> Vec<Pair> {
        let current = line.get(word_start..pos).unwrap_or("");
        candidates
            .into_iter()
            .filter(|c| c.replacement != current)
            .map(|c| Pair {
                display: c.display,
                replacement: c.replacement,
            })
            .collect()
    }

    fn complete_via_pty(&self, line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
        let pos = clamp_pos(line, pos);
        let mut pty = self.pty.lock().ok()?;
        if !pty.is_running() {
            return None;
        }

        if !is_path_like_token(line.get(word_start_at(line, pos)..pos).unwrap_or("")) {
            if let Some(tab) = pty.forward_readline_tab(line, pos, TAB_PROBE_TIMEOUT) {
                let (word_start, raw_pairs) = to_replacement_pairs(&tab, line, pos);
                if !raw_pairs.is_empty() {
                    return Some((
                        word_start,
                        raw_pairs
                            .into_iter()
                            .map(|(display, replacement)| Pair {
                                display,
                                replacement,
                            })
                            .collect(),
                    ));
                }
            }
        }

        pty.query_completions(line, pos, COMPLETION_TIMEOUT)
            .ok()
            .filter(|resp| !resp.candidates.is_empty())
            .map(|resp| {
                (
                    resp.word_start,
                    Self::filter_extending_candidates(line, pos, resp.word_start, resp.candidates),
                )
            })
    }
}

impl Helper for ShellHelper {}

impl Highlighter for ShellHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[38;5;242m{}\x1b[0m", hint))
    }
}

impl Hinter for ShellHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        // Only hint at the end of the line
        if pos == 0 || pos < line.len() {
            return None;
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
        let before = &line[..pos];

        // Skip completion for AI queries
        if before.starts_with(';') || before.starts_with('\u{ff1b}') {
            return Ok((0, Vec::new()));
        }

        // Empty input: no completion (bash semantics)
        if before.trim().is_empty() {
            return Ok((0, Vec::new()));
        }

        if let Some((word_start, pairs)) = self.complete_via_pty(line, pos) {
            return Ok((word_start, pairs));
        }

        // PTY unavailable — no fallback (bash is the single source of truth).
        Ok((0, Vec::new()))
    }
}

/// Wrapper around rustyline Editor with shell-friendly configuration.
pub struct ShellReadline {
    editor: Editor<ShellHelper, rustyline::history::DefaultHistory>,
    /// Shared autosuggest engine so both Hinter and external callers can add
    /// suggestions without needing Editor::helper_mut().
    autosuggest: Arc<Mutex<AutoSuggest>>,
}

impl ShellReadline {
    pub fn new(pty: Arc<Mutex<aish_pty::PersistentPty>>) -> rustyline::Result<Self> {
        let autosuggest = Arc::new(Mutex::new(AutoSuggest::new(5000)));

        let builder = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .auto_add_history(true);
        let config = builder.history_ignore_dups(true)?.build();

        let mut editor = Editor::with_config(config)?;
        editor.set_helper(Some(ShellHelper::new(pty, autosuggest.clone())));
        editor.set_max_history_size(500)?;

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

        Ok(Self {
            editor,
            autosuggest,
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
    fn test_filter_extending_candidates_drops_unchanged() {
        let candidates = vec![aish_pty::CompletionCandidate {
            display: "usr/".into(),
            replacement: "/usr/".into(),
        }];
        let pairs = ShellHelper::filter_extending_candidates("/usr/", 5, 0, candidates);
        assert!(pairs.is_empty(), "unchanged replacement should be filtered");
    }

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
    }
}
