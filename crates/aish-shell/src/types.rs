/// Classification of user input intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputIntent {
    /// Empty line, no action needed.
    Empty,
    /// AI question (starts with `;` or full-width semicolon).
    Ai,
    /// Help command.
    Help,
    /// Operator command (reserved for future use).
    OperatorCommand,
    /// Special command like /model, /setup.
    SpecialCommand,
    /// Built-in shell command (cd, pwd, export, etc.).
    BuiltinCommand,
    /// Script call (.aish script).
    ScriptCall,
    /// External command to be executed.
    Command,
}

/// How a command should be routed for execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandCategory {
    /// State-modifying builtins (cd, pushd, popd, export, etc.)
    BuiltinStateModify,
    /// Informational builtins (help, history, clear)
    BuiltinInfo,
    /// Commands requiring PTY for interactive input (su, sudo)
    PtyRequired,
    /// Commands intercepted with confirmation (exit, quit)
    Rejected,
    /// All other commands → PTY execution
    External,
}

/// Mutable shell state tracked across the REPL loop.
#[derive(Debug, Clone)]
pub struct ShellState {
    pub cwd: String,
    pub prev_cwd: Option<String>,
    pub dir_stack: Vec<String>,
    pub env_vars: std::collections::HashMap<String, String>,
    pub should_exit: bool,
    pub history: Vec<String>,
    /// Last executed command (for error correction).
    pub last_command: Option<String>,
    /// Exit code of last executed command.
    pub last_exit_code: i32,
    /// Whether error correction is available for the last command.
    pub can_correct_error: bool,
    /// Captured output (stdout+stderr combined) from last executed command.
    pub last_output: String,
    /// Pre-approved AI commands that skip future confirmation dialogs.
    pub approved_ai_commands: std::collections::HashSet<String>,
}

impl ShellState {
    pub fn new() -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".to_string());
        let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
        Self {
            cwd,
            prev_cwd: None,
            dir_stack: Vec::new(),
            env_vars,
            should_exit: false,
            history: Vec::new(),
            last_command: None,
            last_exit_code: 0,
            can_correct_error: false,
            last_output: String::new(),
            approved_ai_commands: std::collections::HashSet::new(),
        }
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}
