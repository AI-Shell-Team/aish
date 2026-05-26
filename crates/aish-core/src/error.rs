use thiserror::Error;

/// Unified error type for all aish crates.
#[derive(Debug, Error)]
pub enum AishError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("PTY error: {0}")]
    Pty(String),

    #[error("security error: {0}")]
    Security(String),

    #[error("skill error: {0}")]
    Skill(String),

    #[error("memory error: {0}")]
    Memory(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("i18n error: {0}")]
    I18n(String),

    #[error("shell error: {0}")]
    Shell(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error("operation timed out")]
    Timeout,
}

impl From<serde_json::Error> for AishError {
    fn from(err: serde_json::Error) -> Self {
        AishError::Parse(err.to_string())
    }
}

/// Convenience alias used across all aish crates.
pub type Result<T> = std::result::Result<T, AishError>;
