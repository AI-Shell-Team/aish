//! Static text and JSON schema behind the MCP gateway tool.
//!
//! One gateway tool per MCP server exposes three actions (`list`, `describe`,
//! `call`) so the server's full tool catalogue is never injected into the
//! system prompt up front — the model discovers tools on demand. Everything in
//! this module is static English shown to the model verbatim; the gateway tool
//! owns the per-server dynamic text (see [`crate::tool`]).
//!
//! Only [`tool_list_prompt`] varies per server: it renders the live tool list
//! and status the gateway tool caches for its `prompt()`.

use crate::types::ToolSummary;

/// Short, static description of the gateway. `{server}` and `{transport}` are
/// placeholders the gateway tool substitutes with the concrete values; the
/// server name is what the tool name encodes, so it is called out here.
pub const DESCRIPTION: &str = "Gateway to one MCP server (\"{server}\" over {transport}). That server's tools are not listed in your prompt; use action \"list\" to discover them, \"describe\" to read one tool's schema, and \"call\" to run it.";

/// Static usage guidance: the recommended list → describe → call flow.
pub const PROMPT: &str = "\
This tool is the gateway to one MCP server. Its tools are NOT listed in your \
prompt, so discover them on demand:

1. action \"list\" — returns every tool the server exposes, one line each, with \
the side-effect scope of each tool.
2. action \"describe\" with \"tool\": <name> — returns that tool's full JSON \
input schema. Call this before invoking a tool whose arguments are not obvious.
3. action \"call\" with \"tool\": <name> and \"arguments\": {...} — invokes the \
tool. \"arguments\" must match the schema returned by \"describe\".

Work in that order: list first, describe a tool when its purpose or arguments \
are unclear, then call it. \"call\" returns the server's raw result, including \
errors the server reports. Calling a tool requires user approval unless the \
tool is read-only and the server is trusted; changing any argument requires a \
fresh approval.";

/// The three actions the gateway accepts, in the order the schema lists them.
pub const GATEWAY_ACTIONS: [&str; 3] = ["list", "describe", "call"];

/// Static JSON schema of the gateway's arguments.
pub fn gateway_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": GATEWAY_ACTIONS,
                "description": "list: show the server's tools; describe: show one tool's input schema; call: invoke a tool."
            },
            "tool": {
                "type": "string",
                "description": "Tool name returned by action:\"list\" (required for describe/call)"
            },
            "arguments": {
                "type": "object",
                "description": "Arguments for action:\"call\"",
                "additionalProperties": true
            }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// Per-tool description line budget inside the dynamic prompt.
const PROMPT_DESCRIPTION_CHARS: usize = 160;

/// Cap on one rendered tool name in the dynamic prompt. The name comes from
/// the server, so it is budgeted like every other server-supplied string: a
/// hostile server must not be able to enlarge the system prompt at will.
const PROMPT_TOOL_NAME_CHARS: usize = 64;

/// Cap on one rendered required-parameter name.
const PROMPT_REQUIRED_NAME_CHARS: usize = 64;

/// Cap on the whole joined `required` list rendered for one tool.
const PROMPT_REQUIRED_CHARS: usize = 160;

/// Cap on tool lines rendered into the dynamic prompt.
const PROMPT_MAX_REQUIRED_PARAMS: usize = 6;
const PROMPT_MAX_TOOL_LINES: usize = 200;

/// Cap on the server `instructions` text embedded in the dynamic prompt.
const PROMPT_INSTRUCTIONS_BYTES: usize = 2000;

/// Dynamic segment of the gateway's `prompt()`: the server's current status,
/// its tool catalogue and, when the server sent any, its `instructions`.
///
/// Everything here is server-supplied text that lands in the system prompt, so
/// every field is budgeted: `description` is collapsed to its first line and
/// truncated, tool names and required-parameter lists are capped too. One
/// verbose or openly hostile tool therefore cannot push the rest of the
/// catalogue — or unbounded text — into the prompt.
pub fn tool_list_prompt(
    server: &str,
    status_label: &str,
    tools: &[ToolSummary],
    instructions: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("MCP server \"{server}\" is {status_label}.\n"));

    if tools.is_empty() {
        out.push_str("No tools are available from this server yet.\n");
    } else {
        out.push_str(&format!("Tools ({}):\n", tools.len()));
        for tool in tools.iter().take(PROMPT_MAX_TOOL_LINES) {
            // Required parameters are inlined: a model that only sees tool names
            // guesses the arguments, and a server with required parameters then
            // answers with an input-validation error instead of a result.
            let required = if tool.required.is_empty() {
                String::new()
            } else {
                let names = tool
                    .required
                    .iter()
                    .take(PROMPT_MAX_REQUIRED_PARAMS)
                    .map(|name| cap_chars(name, PROMPT_REQUIRED_NAME_CHARS))
                    .collect::<Vec<_>>()
                    .join(", ");
                let names = cap_chars(&names, PROMPT_REQUIRED_CHARS);
                let more = tool
                    .required
                    .len()
                    .saturating_sub(PROMPT_MAX_REQUIRED_PARAMS);
                if more > 0 {
                    format!(" [required: {names}, +{more} more]")
                } else {
                    format!(" [required: {names}]")
                }
            };
            out.push_str(&format!(
                "- {}: {} ({}){}\n",
                cap_chars(&tool.name, PROMPT_TOOL_NAME_CHARS),
                summarize_description(tool.description.as_deref()),
                tool.side_effects.as_str(),
                required
            ));
        }
        if tools.len() > PROMPT_MAX_TOOL_LINES {
            out.push_str(&format!(
                "...and {} more tools\n",
                tools.len() - PROMPT_MAX_TOOL_LINES
            ));
        }
    }

    if let Some(instructions) = instructions {
        let trimmed = instructions.trim();
        if !trimmed.is_empty() {
            let (text, truncated) = crate::types::truncate_utf8(trimmed, PROMPT_INSTRUCTIONS_BYTES);
            out.push_str("\nServer instructions:\n");
            out.push_str(&text);
            if truncated {
                out.push_str("\n...(instructions truncated)");
            }
            out.push('\n');
        }
    }

    out
}

/// Cap `text` at `max_chars` characters, cutting on a character boundary and
/// appending a visible marker when anything was dropped.
///
/// Every server-supplied string rendered into the prompt goes through this, so
/// callers get a `max_chars` (plus the 3-byte marker) ceiling per field.
fn cap_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}

/// First non-empty line of a tool description, truncated to one prompt line.
fn summarize_description(description: Option<&str>) -> String {
    let line = description
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(no description)");
    let first_line = line.replace(['\r', '\n'], " ");
    cap_chars(&first_line, PROMPT_DESCRIPTION_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SideEffects;

    fn summary(name: &str, required: &[&str]) -> ToolSummary {
        ToolSummary {
            name: name.to_string(),
            description: Some("Does the thing".to_string()),
            side_effects: SideEffects::Read,
            required: required.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    /// A model that only sees tool names guesses arguments; the prompt must
    /// carry the required parameter names, or a server with required parameters
    /// answers with an input-validation error instead of a result (found live
    /// with the official git server).
    #[test]
    fn prompt_inlines_required_parameters() {
        let text = tool_list_prompt(
            "git",
            "connected",
            &[
                summary("git_status", &["repo_path"]),
                summary("git_log", &[]),
            ],
            None,
        );
        assert!(
            text.contains("git_status: Does the thing (read) [required: repo_path]"),
            "{text}"
        );
        assert!(
            text.contains("- git_log: Does the thing (read)\n"),
            "{text}"
        );
    }

    #[test]
    fn prompt_caps_the_required_parameter_list() {
        let many: Vec<&str> = vec!["a", "b", "c", "d", "e", "f", "g", "h"];
        let text = tool_list_prompt("s", "connected", &[summary("t", &many)], None);
        assert!(
            text.contains("[required: a, b, c, d, e, f, +2 more]"),
            "{text}"
        );
    }

    /// Tool names come from the server and the rendered catalogue is appended
    /// to the system prompt on every request, so a hostile server must not be
    /// able to enlarge the prompt through its names.
    #[test]
    fn prompt_caps_hostile_tool_names() {
        let hostile = format!("tool0_{}", "n".repeat(1_048_576));
        let text = tool_list_prompt("evil", "connected", &[summary(&hostile, &[])], None);
        assert!(text.len() < 1_000, "{}", text.len());
        assert!(text.contains("tool0_nnn"), "{text}");
        assert!(text.contains("..."), "{text}");
        assert!(!text.contains(&"n".repeat(200)), "{text}");
    }

    #[test]
    fn prompt_caps_hostile_required_parameter_names() {
        let hostile = "p".repeat(1_048_576);
        let text = tool_list_prompt("evil", "connected", &[summary("t", &[&hostile])], None);
        assert!(text.len() < 1_000, "{}", text.len());
        assert!(text.contains("[required: pppp"), "{text}");
        assert!(text.contains("..."), "{text}");
        assert!(!text.contains(&"p".repeat(200)), "{text}");
    }

    #[test]
    fn prompt_caps_hostile_required_parameter_lists() {
        let names: Vec<String> = (0..200)
            .map(|index| format!("param{index}_{}", "q".repeat(4_096)))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let text = tool_list_prompt("evil", "connected", &[summary("t", &refs)], None);
        assert!(text.len() < 1_000, "{}", text.len());
        assert!(text.contains("[required: param0_qqqq"), "{text}");
        assert!(text.contains("+194 more]"), "{text}");
        assert!(text.contains("..."), "{text}");
        assert!(!text.contains(&"q".repeat(200)), "{text}");
    }

    /// 200 hostile tools plus hostile instructions must still render a prompt
    /// of a predictable, small size: the caps above hold per line, so the
    /// catalogue as a whole cannot be unbounded.
    #[test]
    fn prompt_stays_bounded_with_many_hostile_tools() {
        let tools: Vec<ToolSummary> = (0..200)
            .map(|index| ToolSummary {
                name: format!("{index}_{}", "n".repeat(4_096)),
                description: Some("d".repeat(4_096)),
                side_effects: SideEffects::Read,
                required: vec!["r".repeat(4_096); 200],
            })
            .collect();
        let text = tool_list_prompt("evil", "connected", &tools, Some(&"i".repeat(1_048_576)));
        assert!(text.len() < 100_000, "{}", text.len());
        assert!(text.contains("..."), "{text}");
        assert!(text.contains("...(instructions truncated)"), "{text}");
    }
}
