use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// Commands that take another command as their first argument.
/// After these commands, tab completion suggests commands instead of files.
const COMMAND_TAKING_COMMANDS: &[&str] = &[
    "sudo", "doas", "pkexec", "nice", "nohup", "ionice", "taskset", "strace", "ltrace", "perf",
    "timeout", "xargs", "exec", "chroot", "unshare", "setsid", "env", "bash", "sh", "zsh",
    "fish", "run0", "time", "coproc",
];

/// Commands that only accept directory paths as arguments.
const DIRECTORY_ONLY_COMMANDS: &[&str] = &["cd", "pushd", "popd"];

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

/// Shell command completer using bash `compgen` for command discovery and
/// rustyline's `FilenameCompleter` for path completion.  Also provides
/// history-based autosuggestions via Hinter.
struct ShellHelper {
    file_completer: FilenameCompleter,
    /// Lazily-populated set of all commands from `bash -c 'compgen -c'`.
    /// Invalidated each prompt so newly-installed commands are picked up.
    command_cache: RefCell<Option<HashSet<String>>>,
    /// Shared autosuggest engine (Arc<Mutex> so it can be mutated from
    /// ShellReadline without needing helper_mut).
    autosuggest: Arc<Mutex<AutoSuggest>>,
}

impl ShellHelper {
    fn new(autosuggest: Arc<Mutex<AutoSuggest>>) -> Self {
        Self {
            file_completer: FilenameCompleter::new(),
            command_cache: RefCell::new(None),
            autosuggest,
        }
    }

    /// Return a reference to the cached command set, populating it first if needed.
    fn cached_commands(&self) -> std::cell::Ref<'_, HashSet<String>> {
        if self.command_cache.borrow().is_none() {
            *self.command_cache.borrow_mut() = Some(bash_compgen_commands());
        }
        std::cell::Ref::map(self.command_cache.borrow(), |opt| {
            opt.as_ref().unwrap()
        })
    }

    /// Invalidate the command cache so it is refreshed on the next tab press.
    fn invalidate_cache(&self) {
        *self.command_cache.borrow_mut() = None;
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

        // At the start of the line: complete commands
        if !before.contains(' ') {
            let mut candidates: Vec<Pair> = Vec::new();

            // Builtin commands
            for cmd in BUILTINS {
                if cmd.starts_with(word) {
                    candidates.push(Pair {
                        display: cmd.to_string(),
                        replacement: cmd.to_string(),
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

            // All commands from bash compgen (includes PATH executables,
            // aliases, functions, builtins)
            if !word.is_empty() {
                let commands = self.cached_commands();
                for cmd in &*commands {
                    if cmd.starts_with(word) && !BUILTINS.contains(&cmd.as_str()) {
                        candidates.push(Pair {
                            display: cmd.clone(),
                            replacement: format!("{} ", cmd),
                        });
                    }
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

        // After a command: context-aware completion
        let tokens: Vec<&str> = before.split_whitespace().collect();
        if let Some(&command) = tokens.first() {
            // Skip flag arguments
            if word.starts_with('-') {
                return Ok((0, Vec::new()));
            }
            // Directory-only commands: complete just directories
            if DIRECTORY_ONLY_COMMANDS.contains(&command) {
                return self.file_completer.complete(line, pos, ctx);
            }
            // Commands that take another command as first argument (sudo, etc.)
            if COMMAND_TAKING_COMMANDS.contains(&command) {
                let mut candidates: Vec<Pair> = Vec::new();
                if !word.is_empty() {
                    let commands = self.cached_commands();
                    for cmd in &*commands {
                        if cmd.starts_with(word) {
                            candidates.push(Pair {
                                display: cmd.clone(),
                                replacement: format!("{} ", cmd),
                            });
                        }
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
        }

        // Default: file completion
        self.file_completer.complete(line, pos, ctx)
    }
}

/// Query bash for all available commands using `compgen -c`.
/// Returns aliases, functions, builtins, and PATH executables.
fn bash_compgen_commands() -> HashSet<String> {
    let output = match std::process::Command::new("bash")
        .args(&["-c", "compgen -c"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return HashSet::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Wrapper around rustyline Editor with shell-friendly configuration.
pub struct ShellReadline {
    editor: Editor<ShellHelper, rustyline::history::DefaultHistory>,
    /// Shared autosuggest engine so both Hinter and external callers can add
    /// suggestions without needing Editor::helper_mut().
    autosuggest: Arc<Mutex<AutoSuggest>>,
}

impl ShellReadline {
    pub fn new() -> rustyline::Result<Self> {
        let autosuggest = Arc::new(Mutex::new(AutoSuggest::new(1000)));

        let builder = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .auto_add_history(true);
        let config = builder.history_ignore_dups(true)?.build();

        let mut editor = Editor::with_config(config)?;
        editor.set_helper(Some(ShellHelper::new(autosuggest.clone())));

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
    fn test_bash_compgen_commands_non_empty() {
        let cmds = bash_compgen_commands();
        assert!(!cmds.is_empty(), "compgen -c should return commands");
        // ls and cat are extremely common, at least one should exist
        assert!(cmds.contains("ls") || cmds.contains("cat") || cmds.contains("echo"));
    }

    #[test]
    fn test_command_taking_commands_contains_sudo() {
        assert!(
            COMMAND_TAKING_COMMANDS.contains(&"sudo"),
            "sudo must be in COMMAND_TAKING_COMMANDS"
        );
    }

    #[test]
    fn test_directory_commands_recognized() {
        let dir_cmds: &[&str] = &["cd", "pushd", "popd"];
        for cmd in dir_cmds {
            assert!(DIRECTORY_ONLY_COMMANDS.contains(cmd));
        }
    }
}
