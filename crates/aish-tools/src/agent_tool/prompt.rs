//! Prompt and schema for the `Agent` tool.

pub const DESCRIPTION: &str = "\
Spawn a synchronous sub-agent to handle an isolated sub-task. Only the final conclusion \
is returned to the parent session; intermediate tool output stays in the sub-session.";

pub const ROUTING_SECTION: &str = "\
## Routing: planning vs plan mode vs sub-agents

| User intent | Tool |
|-------------|------|
| Plan/runbook/advice only, no files, no approval flow | Agent(subagent_type=plan) |
| Multi-step work needing an approvable plan file (`.aish/plans/`) before execution | enter_plan_mode |
| Open-ended read-only search / facts across paths or code | Agent(subagent_type=explore) |
| Open-ended system/service/network/performance diagnosis | Agent(subagent_type=troubleshoot) |
| Focused sub-task needing parent tools (including writes) | Agent(subagent_type=general-purpose) |

Do NOT use enter_plan_mode when the user only wants a textual plan or runbook.
Do NOT use Agent(plan) when the user explicitly wants a saved plan artifact reviewed in plan mode.
Do NOT use Agent(explore) for host/system diagnosis — prefer Agent(troubleshoot).";

pub const WHEN_NOT_SECTION: &str = "\
## When NOT to use the Agent tool

- Known file path to read → read_file (not Agent)
- Single targeted grep or glob with a clear pattern → grep or glob (not Agent)
- One shell command the user asked to run → bash (not Agent)
- Tasks unrelated to the built-in subagent descriptions above

Prefer Agent(subagent_type=explore) over many grep/glob/read_file rounds in this session when \
investigation is open-ended path/code search. Prefer Agent(subagent_type=troubleshoot) when \
the user asks why the system, a service, network, disk, or performance is unhealthy.";

pub const USAGE_SECTION: &str = "\
## Usage notes

- Include a short description (3-5 words) summarizing what the sub-agent will do.
- In `prompt`, always state: goal, scope (paths or directories), thoroughness (quick | medium | \
thorough), and whether the sub-agent must stay read-only. Default to quick or medium unless the \
user asked for exhaustive coverage; do not expand scope to \"everywhere\" on your own.
- When the sub-agent finishes, only its final conclusion is returned here; summarize for the user if needed.
- Launch multiple agents in one turn when their tasks are independent.
- If you delegate research to a sub-agent, do not duplicate the same searches in this session.";

pub fn parameters(subagent_types: &[String]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "description": {
                "type": "string",
                "description": "3-5 word task summary for UI display"
            },
            "prompt": {
                "type": "string",
                "description": "Task brief for the sub-agent: goal, scope (paths/directories), thoroughness (quick | medium | thorough), and read-only vs may-modify constraints"
            },
            "subagent_type": {
                "type": "string",
                "enum": subagent_types,
                "description": "Built-in sub-agent type to spawn"
            }
        },
        "required": ["description", "prompt", "subagent_type"]
    })
}
