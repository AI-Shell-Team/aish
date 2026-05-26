use std::sync::atomic::{AtomicBool, Ordering};

/// Simple cancellation token backed by an atomic bool.
pub struct CancelToken {
    cancelled: AtomicBool,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Identifies which output stream we are dealing with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamName {
    Stdout,
    Stderr,
}

impl std::fmt::Display for StreamName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamName::Stdout => write!(f, "stdout"),
            StreamName::Stderr => write!(f, "stderr"),
        }
    }
}

/// Source of a command submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    User,
    Backend,
}

impl std::fmt::Display for CommandSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandSource::User => write!(f, "user"),
            CommandSource::Backend => write!(f, "backend"),
        }
    }
}

/// Resolved completion state for a backend command.
#[derive(Debug, Clone)]
pub struct PtyCommandResult {
    pub command: String,
    pub exit_code: i32,
    pub source: CommandSource,
    pub command_seq: Option<i32>,
    pub interrupted: bool,
    pub allow_error_correction: bool,
}

/// Metadata for a command submitted to the backend shell.
#[derive(Debug, Clone)]
pub struct CommandSubmission {
    pub command: String,
    pub source: CommandSource,
    pub command_seq: Option<i32>,
    pub allow_error_correction: bool,
}

impl CommandSubmission {
    pub fn new(command: String, source: CommandSource, command_seq: Option<i32>) -> Self {
        let allow_error_correction =
            source == CommandSource::User && !Self::is_interactive_session_command(&command);
        Self {
            command,
            source,
            command_seq,
            allow_error_correction,
        }
    }

    /// Commands that maintain an interactive session where error correction
    /// should not be offered and Ctrl-C should be forwarded as a character.
    pub fn is_interactive_session_command(command: &str) -> bool {
        let first = command.split_whitespace().next().unwrap_or("");
        let basename = first.rsplit('/').next().unwrap_or(first);
        matches!(
            basename,
            "ssh" | "telnet" | "mosh" | "nc" | "netcat" | "ftp" | "sftp"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_source_display() {
        assert_eq!(format!("{}", CommandSource::User), "user");
        assert_eq!(format!("{}", CommandSource::Backend), "backend");
    }

    #[test]
    fn test_pty_command_result_fields() {
        let result = PtyCommandResult {
            command: "ls".to_string(),
            exit_code: 0,
            source: CommandSource::User,
            command_seq: None,
            interrupted: false,
            allow_error_correction: false,
        };
        assert_eq!(result.command, "ls");
        assert_eq!(result.exit_code, 0);
        assert!(!result.interrupted);
        assert!(!result.allow_error_correction);
    }

    #[test]
    fn test_command_submission_default_error_correction_user() {
        let sub = CommandSubmission::new("ls".to_string(), CommandSource::User, None);
        assert!(sub.allow_error_correction);
    }

    #[test]
    fn test_command_submission_default_error_correction_backend() {
        let sub = CommandSubmission::new("ls".to_string(), CommandSource::Backend, None);
        assert!(!sub.allow_error_correction);
    }

    #[test]
    fn test_is_interactive_session_command() {
        assert!(CommandSubmission::is_interactive_session_command(
            "ssh user@host"
        ));
        assert!(CommandSubmission::is_interactive_session_command(
            "telnet example.com"
        ));
        assert!(!CommandSubmission::is_interactive_session_command("ls -la"));
        assert!(!CommandSubmission::is_interactive_session_command(
            "vim file.txt"
        ));
    }
}
