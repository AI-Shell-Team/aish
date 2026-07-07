//! Pure spawn outcome extraction (Seam C): map loop termination to status + text.

use super::tool_loop::LoopStatus;

/// Prefix prepended when max turns is reached (PRD §4.6).
pub const INCOMPLETE_PREFIX: &str = "[incomplete: max turns reached]\n";

/// Configuration for [`extract_spawn_outcome`].
#[derive(Debug, Clone)]
pub struct OutcomeConfig {
    pub incomplete_prefix: String,
}

impl Default for OutcomeConfig {
    fn default() -> Self {
        Self {
            incomplete_prefix: INCOMPLETE_PREFIX.to_string(),
        }
    }
}

/// Why the sub-agent loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationKind {
    /// Assistant responded with text and no tool calls (natural stop).
    NaturalStop { assistant_text: String },
    /// Turn budget exhausted while the assistant still had tool calls pending.
    MaxTurnsReached { last_assistant_text: String },
    /// Parent or sub-session cancel token fired.
    Cancelled,
    /// Unrecoverable LLM or runtime error.
    Fatal,
}

/// Status + text returned to the parent via [`super::spawn::SpawnResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOutcome {
    pub status: LoopStatus,
    pub text: String,
}

/// Map termination semantics to the parent-visible spawn outcome (PRD §4.6).
pub fn extract_spawn_outcome(kind: TerminationKind, config: &OutcomeConfig) -> SpawnOutcome {
    match kind {
        TerminationKind::NaturalStop { assistant_text } => SpawnOutcome {
            status: LoopStatus::Complete,
            text: assistant_text,
        },
        TerminationKind::MaxTurnsReached {
            last_assistant_text,
        } => {
            let text = if last_assistant_text.is_empty() {
                config.incomplete_prefix.clone()
            } else {
                format!("{}{last_assistant_text}", config.incomplete_prefix)
            };
            SpawnOutcome {
                status: LoopStatus::Incomplete,
                text,
            }
        }
        TerminationKind::Cancelled => SpawnOutcome {
            status: LoopStatus::Cancelled,
            text: String::new(),
        },
        TerminationKind::Fatal => SpawnOutcome {
            status: LoopStatus::Fatal,
            text: String::new(),
        },
    }
}

#[cfg(test)]
fn extract_spawn_outcome_from_messages(
    messages: &[crate::types::ChatMessage],
    config: &OutcomeConfig,
) -> Option<SpawnOutcome> {
    let last_assistant = messages.iter().rev().find(|m| m.role == "assistant")?;
    if last_assistant
        .tool_calls
        .as_ref()
        .is_some_and(|tc| !tc.is_empty())
    {
        return None;
    }
    Some(extract_spawn_outcome(
        TerminationKind::NaturalStop {
            assistant_text: last_assistant.text_content().unwrap_or("").to_string(),
        },
        config,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, MessageContent, ToolCall};

    fn config() -> OutcomeConfig {
        OutcomeConfig::default()
    }

    #[test]
    fn test_natural_stop_assistant_text_is_complete() {
        let outcome = extract_spawn_outcome(
            TerminationKind::NaturalStop {
                assistant_text: "nginx at /etc/nginx".to_string(),
            },
            &config(),
        );
        assert_eq!(outcome.status, LoopStatus::Complete);
        assert_eq!(outcome.text, "nginx at /etc/nginx");
    }

    #[test]
    fn test_max_turns_incomplete_with_prefix_and_last_text() {
        let outcome = extract_spawn_outcome(
            TerminationKind::MaxTurnsReached {
                last_assistant_text: "partial findings".to_string(),
            },
            &config(),
        );
        assert_eq!(outcome.status, LoopStatus::Incomplete);
        assert_eq!(
            outcome.text,
            "[incomplete: max turns reached]\npartial findings"
        );
    }

    #[test]
    fn test_max_turns_incomplete_prefix_only_when_no_last_text() {
        let outcome = extract_spawn_outcome(
            TerminationKind::MaxTurnsReached {
                last_assistant_text: String::new(),
            },
            &config(),
        );
        assert_eq!(outcome.status, LoopStatus::Incomplete);
        assert_eq!(outcome.text, "[incomplete: max turns reached]\n");
    }

    #[test]
    fn test_cancelled_maps_to_cancelled_status() {
        let outcome = extract_spawn_outcome(TerminationKind::Cancelled, &config());
        assert_eq!(outcome.status, LoopStatus::Cancelled);
        assert!(outcome.text.is_empty());
    }

    #[test]
    fn test_fatal_maps_to_fatal_status() {
        let outcome = extract_spawn_outcome(TerminationKind::Fatal, &config());
        assert_eq!(outcome.status, LoopStatus::Fatal);
        assert!(outcome.text.is_empty());
    }

    #[test]
    fn test_extract_from_messages_natural_stop() {
        let mut assistant = ChatMessage::assistant("done");
        assistant.content = Some(MessageContent::Text("done".to_string()));
        let messages = vec![ChatMessage::user("task"), assistant];

        let outcome =
            extract_spawn_outcome_from_messages(&messages, &config()).expect("natural stop");
        assert_eq!(outcome.status, LoopStatus::Complete);
        assert_eq!(outcome.text, "done");
    }

    #[test]
    fn test_extract_from_messages_returns_none_when_last_assistant_has_tool_calls() {
        let mut assistant = ChatMessage::assistant("");
        assistant.tool_calls = Some(vec![ToolCall {
            id: "c1".to_string(),
            name: "grep".to_string(),
            arguments: "{}".to_string(),
        }]);
        let messages = vec![ChatMessage::user("task"), assistant];

        assert!(extract_spawn_outcome_from_messages(&messages, &config()).is_none());
    }
}
