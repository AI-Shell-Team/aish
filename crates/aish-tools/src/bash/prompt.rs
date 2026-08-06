pub(crate) const DESCRIPTION: &str = "\
Execute a bash command and return its output. Use ONLY for running commands, \
builds, tests, and read-only discovery (find, ls, stat, ps). NEVER use bash \
to read, write, or edit files — use read_file to read, edit_file to modify, \
write_file to create; use grep/glob for search; use Python for structured \
scripts; use Agent (subagent_type=explore) for open-ended multi-round \
path/code search; use Agent (subagent_type=troubleshoot) for open-ended \
system/service diagnosis; use Agent to delegate other isolated sub-tasks.";

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
                "description": "Bash command to execute."
            },
            "timeout": {
                "type": "integer",
                "minimum": 1,
                "description": "Timeout in seconds. If omitted, the command runs until completion or cancellation."
            }
        },
        "required": ["command"]
    })
}
