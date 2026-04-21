use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::Helper;
use rustyline::{
    Cmd, CompletionType, ConditionalEventHandler, Config, Context, EditMode, Editor, Event,
    EventContext, EventHandler, KeyCode, KeyEvent, Modifiers, RepeatCount,
};

use crate::autosuggest::AutoSuggest;

/// Built-in command names for completion.
const BUILTINS: &[&str] = &[
    "cd", "pwd", "export", "unset", "pushd", "popd", "dirs", "history", "help", "clear", "exit",
    "quit",
];

/// Special commands starting with /.
const SPECIALS: &[&str] = &["/model", "/setup"];

/// Timeout for PTY completion queries.
const COMPLETION_TIMEOUT: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Mode toggle key binding handler
// ---------------------------------------------------------------------------

/// Flag set by `ModeToggleHandler` when Shift+Tab or F2 is pressed.
static MODE_TOGGLE_REQUESTED: AtomicBool = AtomicBool::new(false);

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

/// Shell command completer that delegates to the persistent PTY bash process
/// for full bash-completion support (git add, systemctl status, etc.).
struct ShellHelper {
    file_completer: FilenameCompleter,
    /// Shared reference to the persistent PTY session.
    pty: Arc<Mutex<aish_pty::PersistentPty>>,
    /// Shared autosuggest engine.
    autosuggest: Arc<Mutex<AutoSuggest>>,
}

impl ShellHelper {
    fn new(pty: Arc<Mutex<aish_pty::PersistentPty>>, autosuggest: Arc<Mutex<AutoSuggest>>) -> Self {
        Self {
            file_completer: FilenameCompleter::new(),
            pty,
            autosuggest,
        }
    }

    /// Query the PTY bash for completions using `__aish_query_completions`.
    /// Returns a list of completion candidates on success, or an empty vec on
    /// any error (timeout, PTY not running, etc.).
    fn query_pty_completions(&self, line: &str, pos: usize) -> Vec<String> {
        let mut pty = match self.pty.lock() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };

        // Skip if PTY is not running.
        if !pty.is_running() {
            return Vec::new();
        }

        // Build the completion query command.
        let escaped_line = aish_pty::shell_quote_escape(line);
        let cmd = format!("__aish_query_completions {} {}", escaped_line, pos);

        match pty.execute_command(&cmd, COMPLETION_TIMEOUT) {
            Ok((output, _exit_code)) => output
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Helper for ShellHelper {}

impl Highlighter for ShellHelper {}

impl Hinter for ShellHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        // Only hint at the end of the line
        if pos == 0 || pos < line.len() {
            return None;
        }
        let guard = self.autosuggest.lock().unwrap();
        guard.suggest(line).map(|s| s[line.len()..].to_string())
    }
}

impl Validator for ShellHelper {}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let before = &line[..pos];
        let word_start = before.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let word = &before[word_start..];

        // Skip completion for AI queries
        if before.starts_with(';') || before.starts_with('\u{ff1b}') {
            return Ok((0, Vec::new()));
        }

        // At the start of the line: mix builtins/specials with PTY results
        if !before.contains(' ') {
            let mut candidates: Vec<Pair> = Vec::new();

            // Builtin commands (handled by Rust shell, not PTY)
            for cmd in BUILTINS {
                if cmd.starts_with(word) {
                    candidates.push(Pair {
                        display: cmd.to_string(),
                        replacement: format!("{} ", cmd),
                    });
                }
            }

            // Special commands
            for cmd in SPECIALS {
                if cmd.starts_with(word) {
                    candidates.push(Pair {
                        display: cmd.to_string(),
                        replacement: format!("{} ", cmd),
                    });
                }
            }

            // AI prefix
            if ";".starts_with(word) || "\u{ff1b}".starts_with(word) {
                candidates.push(Pair {
                    display: "; <question>".to_string(),
                    replacement: "; ".to_string(),
                });
            }

            // Query PTY for all other command completions
            if !word.is_empty() {
                let pty_results = self.query_pty_completions(line, pos);
                let builtin_set: Vec<&str> = BUILTINS.to_vec();
                for candidate in &pty_results {
                    // Skip duplicates already covered by BUILTINS/SPECIALS
                    if builtin_set.contains(&candidate.as_str()) {
                        continue;
                    }
                    candidates.push(Pair {
                        display: candidate.clone(),
                        replacement: format!("{} ", candidate),
                    });
                }
            }

            if !candidates.is_empty() {
                return Ok((word_start, candidates));
            }

            // Fall through to file completion for path-like tokens
            if word.starts_with("./")
                || word.starts_with("../")
                || word.starts_with("/")
                || word.starts_with("~/")
            {
                return self.file_completer.complete(line, pos, ctx);
            }

            return Ok((word_start, candidates));
        }

        // After a command: delegate entirely to PTY for context-aware completion
        let pty_results = self.query_pty_completions(line, pos);
        if !pty_results.is_empty() {
            let candidates: Vec<Pair> = pty_results
                .into_iter()
                .map(|c| {
                    // Append space for non-directory completions
                    let replacement = if c.ends_with('/') {
                        c.clone()
                    } else {
                        format!("{} ", c)
                    };
                    Pair {
                        display: c,
                        replacement,
                    }
                })
                .collect();
            return Ok((word_start, candidates));
        }

        // Fallback: file completion
        self.file_completer.complete(line, pos, ctx)
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
        let autosuggest = Arc::new(Mutex::new(AutoSuggest::new(1000)));

        let builder = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .auto_add_history(true);
        let config = builder.history_ignore_dups(true)?.build();

        let mut editor = Editor::with_config(config)?;
        editor.set_helper(Some(ShellHelper::new(pty, autosuggest.clone())));

        // Bind Shift+Tab (BackTab) and F2 for mode toggle
        editor.bind_sequence(
            KeyEvent(KeyCode::BackTab, Modifiers::NONE),
            EventHandler::Conditional(Box::new(ModeToggleHandler)),
        );
        editor.bind_sequence(
            KeyEvent(KeyCode::F(2), Modifiers::NONE),
            EventHandler::Conditional(Box::new(ModeToggleHandler)),
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

    /// Read a line with the given prompt.
    /// Returns None on EOF (Ctrl-D).
    /// Supports backslash continuation: lines ending with `\` read
    /// additional lines with a `> ` prompt.
    pub fn read_line(&mut self, prompt: &str) -> rustyline::Result<Option<String>> {
        // Invalidate command cache so newly-installed commands are discovered.
        if let Some(helper) = self.editor.helper() {
            helper.invalidate_cache();
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

    /// Load history from a file (best-effort).
    pub fn load_history(&mut self, path: &std::path::Path) {
        let _ = self.editor.load_history(path);
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
    fn test_builtins_contains_cd() {
        assert!(BUILTINS.contains(&"cd"));
        assert!(BUILTINS.contains(&"exit"));
    }

    #[test]
    fn test_specials_format() {
        for cmd in SPECIALS {
            assert!(cmd.starts_with('/'), "special must start with /: {}", cmd);
        }
    }
}
