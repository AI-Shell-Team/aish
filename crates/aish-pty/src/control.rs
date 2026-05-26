use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// BackendControlEvent
// ---------------------------------------------------------------------------

/// Events sent over the control pipe (NDJSON protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BackendControlEvent {
    #[serde(rename = "session_ready")]
    SessionReady {
        shell_pid: i32,
        cwd: String,
        shlvl: i32,
    },

    #[serde(rename = "command_started")]
    CommandStarted {
        command_seq: Option<i32>,
        command: String,
        cwd: String,
    },

    #[serde(rename = "prompt_ready")]
    PromptReady {
        command_seq: Option<i32>,
        exit_code: i32,
        cwd: String,
        interrupted: bool,
    },

    #[serde(rename = "shell_exiting")]
    ShellExiting { exit_code: i32 },

    #[serde(rename = "command_output")]
    CommandOutput { data: String },
}

// ---------------------------------------------------------------------------
// NDJSON helpers
// ---------------------------------------------------------------------------

/// Encode a single event as one NDJSON line (JSON + newline).
pub fn encode_control_event(event: &BackendControlEvent) -> String {
    let mut json = serde_json::to_string(event).unwrap_or_default();
    json.push('\n');
    json
}

/// Incrementally parse NDJSON from raw byte chunks.
///
/// `buffer` accumulates partial data across calls.  Returns a list of
/// successfully decoded events.
pub fn decode_control_chunk(buffer: &mut String, chunk: &[u8]) -> Vec<BackendControlEvent> {
    let incoming = match std::str::from_utf8(chunk) {
        Ok(s) => s,
        Err(e) => {
            // Keep the valid prefix and stash the rest for next time.
            let valid_up_to = e.valid_up_to();
            if valid_up_to == 0 {
                return vec![];
            }
            std::str::from_utf8(&chunk[..valid_up_to]).unwrap_or("")
        }
    };

    buffer.push_str(incoming);

    let mut events = Vec::new();
    while let Some(pos) = buffer.find('\n') {
        let line: String = buffer.drain(..=pos).collect();
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<BackendControlEvent>(line) {
            Ok(evt) => events.push(evt),
            Err(e) => {
                tracing::warn!(line, error = %e, "failed to parse control event");
            }
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_session_ready() {
        let evt = BackendControlEvent::SessionReady {
            shell_pid: 0,
            cwd: String::new(),
            shlvl: 0,
        };
        let encoded = encode_control_event(&evt);
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, encoded.as_bytes());
        assert!(matches!(
            decoded.first(),
            Some(BackendControlEvent::SessionReady { .. })
        ));
    }

    #[test]
    fn roundtrip_command_started() {
        let evt = BackendControlEvent::CommandStarted {
            command_seq: None,
            command: "ls -la".to_string(),
            cwd: String::new(),
        };
        let encoded = encode_control_event(&evt);
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, encoded.as_bytes());
        match decoded.first() {
            Some(BackendControlEvent::CommandStarted { command, .. }) => {
                assert_eq!(command, "ls -la");
            }
            other => panic!("expected CommandStarted, got {other:?}"),
        }
    }

    #[test]
    fn incremental_parsing() {
        let evt1 = BackendControlEvent::SessionReady {
            shell_pid: 0,
            cwd: String::new(),
            shlvl: 0,
        };
        let evt2 = BackendControlEvent::PromptReady {
            command_seq: None,
            exit_code: 0,
            cwd: "/home".to_string(),
            interrupted: false,
        };
        let mut payload = encode_control_event(&evt1);
        payload.push_str(&encode_control_event(&evt2));

        // Split the payload in the middle of the second event.
        let mid = payload.len() / 2;
        let mut buf = String::new();
        let first = decode_control_chunk(&mut buf, &payload.as_bytes()[..mid]);
        assert_eq!(first.len(), 1);

        let second = decode_control_chunk(&mut buf, &payload.as_bytes()[mid..]);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn roundtrip_prompt_ready_with_payload() {
        let evt = BackendControlEvent::PromptReady {
            command_seq: Some(42),
            exit_code: 0,
            cwd: "/tmp".to_string(),
            interrupted: false,
        };
        let encoded = encode_control_event(&evt);
        assert!(encoded.contains("\"exit_code\":0"));
        assert!(encoded.contains("\"cwd\":\"/tmp\""));
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, encoded.as_bytes());
        match decoded.first() {
            Some(BackendControlEvent::PromptReady {
                command_seq,
                exit_code,
                cwd,
                interrupted,
            }) => {
                assert_eq!(*command_seq, Some(42));
                assert_eq!(*exit_code, 0);
                assert_eq!(cwd, "/tmp");
                assert!(!interrupted);
            }
            other => panic!("expected PromptReady, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_shell_exiting() {
        let evt = BackendControlEvent::ShellExiting { exit_code: 0 };
        let encoded = encode_control_event(&evt);
        assert!(encoded.contains("\"exit_code\":0"));
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, encoded.as_bytes());
        match decoded.first() {
            Some(BackendControlEvent::ShellExiting { exit_code }) => {
                assert_eq!(*exit_code, 0);
            }
            other => panic!("expected ShellExiting, got {other:?}"),
        }
    }

    #[test]
    fn decode_real_session_ready() {
        let json = "{\"version\":1,\"type\":\"session_ready\",\"ts\":1234,\"shell_pid\":100,\"cwd\":\"/home/user\",\"shlvl\":1}\n";
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, json.as_bytes());
        assert_eq!(decoded.len(), 1);
        match &decoded[0] {
            BackendControlEvent::SessionReady {
                shell_pid,
                cwd,
                shlvl,
            } => {
                assert_eq!(*shell_pid, 100);
                assert_eq!(cwd, "/home/user");
                assert_eq!(*shlvl, 1);
            }
            other => panic!("expected SessionReady, got {other:?}"),
        }
    }

    #[test]
    fn decode_real_prompt_ready() {
        let json = "{\"version\":1,\"type\":\"prompt_ready\",\"ts\":1236,\"command_seq\":5,\"exit_code\":0,\"cwd\":\"/home/user\",\"shlvl\":1,\"interrupted\":false}\n";
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, json.as_bytes());
        assert_eq!(decoded.len(), 1);
        match &decoded[0] {
            BackendControlEvent::PromptReady {
                command_seq,
                exit_code,
                cwd,
                interrupted,
            } => {
                assert_eq!(*command_seq, Some(5));
                assert_eq!(*exit_code, 0);
                assert_eq!(cwd, "/home/user");
                assert!(!interrupted);
            }
            other => panic!("expected PromptReady, got {other:?}"),
        }
    }

    #[test]
    fn decode_prompt_ready_null_seq() {
        let json = "{\"version\":1,\"type\":\"prompt_ready\",\"ts\":1,\"command_seq\":null,\"exit_code\":1,\"cwd\":\"/\",\"shlvl\":1,\"interrupted\":false}\n";
        let mut buf = String::new();
        let decoded = decode_control_chunk(&mut buf, json.as_bytes());
        match &decoded[0] {
            BackendControlEvent::PromptReady { command_seq, .. } => {
                assert_eq!(*command_seq, None);
            }
            other => panic!("expected PromptReady, got {other:?}"),
        }
    }
}
