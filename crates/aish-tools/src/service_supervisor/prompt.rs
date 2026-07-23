pub(crate) const DESCRIPTION: &str = "\
Manage long-running background services: start a detached process, query status, stop \
(SIGTERM then SIGKILL), tail logs, or restart. State is persisted per service under \
~/.local/share/aish/services/<name>.{json,log}. Use this to supervise daemons, dev servers, \
and other processes that should outlive the current shell session.";

pub(crate) const PROMPT: &str = r#"Supervise background services (long-running processes detached from this shell).

Usage:
- `action=start`: requires `command` (executable). Spawns the process detached in its own
  process group with stdout/stderr appended to the service log and stdin set to /dev/null.
  Records the pid and, if `ready_log` and/or `ready_port` are supplied, polls readiness up to
  `ready_timeout` seconds (default 15) before returning.
- `action=status`: reports running/ready, pid, uptime, and restart_count by probing the
  recorded pid with kill(pid, 0).
- `action=stop`: SIGTERM, waits up to ~3s, then SIGKILL if still alive.
- `action=logs`: returns the last `log_lines` lines (default 50) of the service log.
- `action=restart`: stop then start; bumps restart_count.

Notes:
- `command` is the executable path/name; `args` is its argument array.
- Readiness is satisfied only when ALL configured checks pass (regex match on recent log
  output AND/OR TCP connect to ready_port succeeds). With no checks configured, start returns
  optimistically ready once the process has spawned.
- Service names are sanitized to path-safe characters; avoid relying on others."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["start", "status", "stop", "logs", "restart"],
                "description": "Operation to perform on the named service."
            },
            "name": {
                "type": "string",
                "description": "Unique service name. Used as the state/log file basename."
            },
            "command": {
                "type": "string",
                "description": "Executable to run. Required for `start` when no prior state exists; updates stored config when provided."
            },
            "args": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Arguments passed to the executable."
            },
            "cwd": {
                "type": "string",
                "description": "Working directory for the spawned process."
            },
            "ready_log": {
                "type": "string",
                "description": "Regex; a match against recent service log output marks the service ready."
            },
            "ready_port": {
                "type": "integer",
                "minimum": 0,
                "maximum": 65535,
                "description": "TCP port on 127.0.0.1; an accepted connection marks the service ready."
            },
            "restart_policy": {
                "type": "string",
                "enum": ["no", "on-failure", "always"],
                "default": "no",
                "description": "Stored policy hint. `no` (default) does not auto-restart; auto-restart monitoring is not currently enforced."
            },
            "log_lines": {
                "type": "integer",
                "minimum": 1,
                "default": 50,
                "description": "Number of trailing log lines returned by `logs` (default 50)."
            },
            "ready_timeout": {
                "type": "integer",
                "minimum": 1,
                "default": 15,
                "description": "Max seconds to wait for readiness checks on `start` (default 15)."
            }
        },
        "required": ["action", "name"]
    })
}
