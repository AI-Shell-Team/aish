pub(crate) const DESCRIPTION: &str = "\
Execute one bash command in a PTY and return its output and exit status. The \
tool can pause for brief input only for commands its interactive detector \
recognizes (including sudo/su patterns and a limited list of terminal \
programs); arbitrary scripts that prompt may not be recognized. Terminal input \
may be echoed into captured output and returned to the model, so use this only \
for non-secret responses. Do not use Bash for prompts requesting passwords or \
other secrets; have the user run those commands in their own terminal. \
Persistent and full-screen sessions are unsupported. Use ONLY for running \
commands, builds, tests, and read-only discovery (find, ls, stat, ps). NEVER use \
bash to read, write, or edit files — use read_file to read, edit_file to modify, \
write_file to create; use grep/glob for search; use Python for structured \
scripts; use Agent (subagent_type=explore) for open-ended multi-round path/code \
search; use Agent (subagent_type=troubleshoot) for open-ended system/service \
diagnosis; use Agent to delegate other isolated sub-tasks.";

pub(crate) const PROMPT: &str = r#"Use this tool to run shell commands.

Usage:
- Explain non-trivial commands before running them.
- Use timeout only when a bounded runtime is expected.
- Do not retry commands the user rejected or cancelled.
- NEVER read or modify files through bash. Do not use redirections (>, >>),
  write-heredocs, `cat >>`, `echo >`, `printf >`, `sed -i`, `tee`, `dd`, or
  `patch` to alter files. Use read_file to read, edit_file to modify, and
  write_file to create — those tools snapshot changes for undo and anchor
  edits against stale content; bash file writes bypass all of that.
- Prefer read_file, grep, or glob for file content; use bash for one-shot
  commands or read-only discovery (find, ls, stat) when those tools fit better."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "Bash command to execute. Brief terminal input is available only when Bash recognizes the command as interactive; input may be echoed into output, so use only for non-secret responses. Do not use for commands requiring passwords or other secrets, or for persistent/full-screen sessions."
            },
            "timeout": {
                "type": "integer",
                "minimum": 1,
                "description": "Timeout in seconds. If omitted, the command is bounded by an internal default (3600s) and can be cancelled anytime."
            }
        },
        "required": ["command"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_discloses_safe_terminal_input_limits() {
        assert!(DESCRIPTION.contains("only for commands its interactive detector recognizes"));
        assert!(DESCRIPTION.contains("arbitrary scripts that prompt may not be recognized"));
        assert!(
            DESCRIPTION.contains("may be echoed into captured output and returned to the model")
        );
        assert!(DESCRIPTION.contains("use this only for non-secret responses"));
        assert!(DESCRIPTION.contains("Do not use Bash for prompts requesting passwords or"));
        assert!(DESCRIPTION.contains("Persistent and full-screen sessions are unsupported"));

        let command_schema = parameters();
        let command_description = command_schema["properties"]["command"]["description"]
            .as_str()
            .unwrap();
        assert!(command_description.contains("Bash recognizes the command as interactive"));
        assert!(command_description.contains("only for non-secret responses"));
        assert!(command_description.contains("commands requiring passwords or other secrets"));
    }
}
