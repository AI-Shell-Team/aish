pub(crate) const DESCRIPTION: &str = "\
Execute one bash command in a PTY and return its output and exit status. For \
commands the tool recognizes as interactive (such as sudo or su prompts), it \
can pause for brief terminal input and continue when the user responds. Keep \
passwords and other secrets in terminal input; never ask for them in chat or \
put them in command text. This does not support persistent or full-screen \
sessions. Use ONLY for running commands, builds, tests, and read-only discovery \
(find, ls, stat, ps). NEVER use bash to read, write, or edit files — use \
read_file to read, edit_file to modify, write_file to create; use grep/glob for \
search; use Python for structured scripts; use Agent (subagent_type=explore) \
for open-ended multi-round path/code search; use Agent \
(subagent_type=troubleshoot) for open-ended system/service diagnosis; use Agent \
to delegate other isolated sub-tasks.";

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
                "description": "Bash command to execute. Commands recognized as interactive (such as sudo or su prompts) may pause for brief terminal input; persistent or full-screen sessions are unsupported."
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
    fn description_discloses_bounded_terminal_input() {
        assert!(DESCRIPTION.contains("commands the tool recognizes as interactive"));
        assert!(DESCRIPTION.contains("pause for brief terminal input"));
        assert!(DESCRIPTION.contains("never ask for them in chat"));
        assert!(DESCRIPTION.contains("persistent or full-screen sessions"));

        let command_description = parameters()["properties"]["command"]["description"]
            .as_str()
            .unwrap();
        assert!(command_description.contains("brief terminal input"));
        assert!(command_description.contains("full-screen sessions are unsupported"));
    }
}
