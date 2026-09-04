//! Helpers for rendering sub-agent LLM events in the shell TUI (AC-11).

use aish_core::LlmEvent;

/// Parsed sub-agent metadata from an [`LlmEvent`] `data` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAgentContext {
    pub agent_type: String,
    pub depth: u32,
    pub spawn_id: String,
}

pub fn event_data_source(event: &LlmEvent) -> Option<&str> {
    event.data.get("source").and_then(|value| value.as_str())
}

pub fn sub_agent_context(event: &LlmEvent) -> Option<SubAgentContext> {
    if event_data_source(event) != Some("sub_agent") {
        return None;
    }
    Some(SubAgentContext {
        agent_type: event.data.get("agent_type")?.as_str()?.to_string(),
        depth: event.data.get("depth")?.as_u64()? as u32,
        spawn_id: event.data.get("spawn_id")?.as_str()?.to_string(),
    })
}

pub fn sub_agent_llm_event(event: &LlmEvent) -> bool {
    sub_agent_context(event).is_some()
}

/// Parent-session `Agent` tool call that spawns a sub-agent — hide its tool banner;
/// sub-agent indented events carry the visible progress instead.
pub fn is_parent_agent_spawn_tool_event(event: &LlmEvent) -> bool {
    if sub_agent_llm_event(event) {
        return false;
    }
    matches!(
        event.data.get("tool_name").and_then(|value| value.as_str()),
        Some("Agent")
    )
}

/// Branch indent for sub-agent status lines (`depth=1` → `  └─ `).
pub fn sub_agent_indent(depth: u32) -> String {
    match depth {
        0 => String::new(),
        1 => format!("  {} ", crate::theme::TREE_LAST),
        n => format!(
            "{}  {} ",
            "  ".repeat(n as usize - 1),
            crate::theme::TREE_LAST
        ),
    }
}

/// Child indent for tool/output lines under a branch (`depth=1` → four spaces).
pub fn sub_agent_child_indent(depth: u32) -> String {
    match depth {
        0 => "  ".to_string(),
        1 => "    ".to_string(),
        n => format!("{}    ", "  ".repeat(n as usize)),
    }
}

pub fn sub_agent_thinking_prefix(ctx: &SubAgentContext) -> String {
    format!("{}{} · ", sub_agent_indent(ctx.depth), ctx.agent_type)
}

pub fn format_sub_agent_thinking_line(ctx: &SubAgentContext, status: &str) -> String {
    format!(
        "{}{} · {}",
        sub_agent_indent(ctx.depth),
        ctx.agent_type,
        crate::theme::muted(status)
    )
}

pub fn format_sub_agent_tool_line(ctx: &SubAgentContext, name: &str, args_preview: &str) -> String {
    format!(
        "{}{} {} {}",
        sub_agent_child_indent(ctx.depth),
        crate::theme::accent(crate::theme::TOOL_PREFIX),
        crate::theme::accent(name),
        crate::theme::muted(&format!("({})", args_preview))
    )
}

pub fn indent_sub_agent_output_block(ctx: &SubAgentContext, block: &str) -> String {
    let prefix = sub_agent_child_indent(ctx.depth);
    block
        .lines()
        .map(|line| format!("{}{}", prefix, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Icon + tool name + status shared by all completion-row variants.
fn format_tool_end_body(name: &str, ok: bool) -> String {
    let status = if ok { "done" } else { "failed" };
    let icon = if ok {
        crate::theme::success(crate::theme::ICON_SUCCESS)
    } else {
        crate::theme::error(crate::theme::ICON_ERROR)
    };
    format!(
        "{} {} {}",
        icon,
        crate::theme::accent(name),
        crate::theme::dim(status)
    )
}

/// Format a tool completion line for the main session.
///
/// Mirrors the start line (`╭─ ❯ tool (args)`) so start/end rows pair by
/// tool name instead of showing a bare `done`/`failed`.
pub fn format_main_tool_end_line(name: &str, ok: bool) -> String {
    format!(
        "{} {}",
        crate::theme::dim(crate::theme::TOOL_BOX_BOT),
        format_tool_end_body(name, ok)
    )
}

/// Format a tool completion line for a sub-agent event.
///
/// Keeps the child indent used by the sub-agent start line so Start/End
/// pair deterministically under the agent branch.
pub fn format_sub_agent_tool_end_line(ctx: &SubAgentContext, name: &str, ok: bool) -> String {
    format!(
        "{}{}",
        sub_agent_child_indent(ctx.depth),
        format_tool_end_body(name, ok)
    )
}

/// Format the completion line for a `ToolExecutionEnd` event.
///
/// Dispatches on sub-agent context so completion rows preserve the same
/// indentation as their start rows.
pub fn format_tool_end_line(event: &LlmEvent, name: &str, ok: bool) -> String {
    match sub_agent_context(event) {
        Some(ctx) => format_sub_agent_tool_end_line(&ctx, name, ok),
        None => format_main_tool_end_line(name, ok),
    }
}

pub fn print_sub_agent_generation_start(event: &LlmEvent) {
    if let Some(ctx) = sub_agent_context(event) {
        let status = aish_i18n::t("shell.sub_agent.thinking");
        let line = format_sub_agent_thinking_line(&ctx, &status);
        println!("{}", crate::theme::muted(&line));
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// Prefix for [`crate::animation::SubAgentThinkingAnimation`].
pub fn sub_agent_thinking_animation_prefix(event: &LlmEvent) -> Option<String> {
    sub_agent_context(event).map(|ctx| sub_agent_thinking_prefix(&ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use aish_core::LlmEventType;

    fn sub_agent_event(event_type: LlmEventType, extra: serde_json::Value) -> LlmEvent {
        let mut data = serde_json::json!({
            "source": "sub_agent",
            "agent_type": "explore",
            "depth": 1,
            "spawn_id": "550e8400-e29b-41d4-a716-446655440000",
        });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                data[k] = v.clone();
            }
        }
        LlmEvent {
            event_type,
            data,
            timestamp: 0.0,
            metadata: None,
        }
    }

    #[test]
    fn sub_agent_context_parses_metadata_fields() {
        let event = sub_agent_event(LlmEventType::ToolExecutionStart, serde_json::json!({}));
        let ctx = sub_agent_context(&event).expect("sub-agent context");
        assert_eq!(ctx.agent_type, "explore");
        assert_eq!(ctx.depth, 1);
        assert_eq!(ctx.spawn_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn sub_agent_context_rejects_other_sources() {
        let event = LlmEvent {
            event_type: LlmEventType::ToolExecutionStart,
            data: serde_json::json!({"source": "react_agent"}),
            timestamp: 0.0,
            metadata: None,
        };
        assert!(sub_agent_context(&event).is_none());
    }

    #[test]
    fn is_parent_agent_spawn_tool_event_detects_parent_only() {
        let parent = LlmEvent {
            event_type: LlmEventType::ToolExecutionStart,
            data: serde_json::json!({"tool_name": "Agent", "tool_args": {}}),
            timestamp: 0.0,
            metadata: None,
        };
        assert!(is_parent_agent_spawn_tool_event(&parent));

        let sub = sub_agent_event(
            LlmEventType::ToolExecutionStart,
            serde_json::json!({"tool_name": "glob"}),
        );
        assert!(!is_parent_agent_spawn_tool_event(&sub));
    }

    #[test]
    fn main_tool_end_line_pairs_with_start_line() {
        let ok = format_main_tool_end_line("glob", true);
        assert!(ok.contains("╰─"));
        assert!(ok.contains(theme::ICON_SUCCESS));
        assert!(ok.contains("glob"));
        assert!(ok.contains("done"));
        assert!(!ok.contains("failed"));

        let failed = format_main_tool_end_line("bash", false);
        assert!(failed.contains(theme::ICON_ERROR));
        assert!(failed.contains("bash"));
        assert!(failed.contains("failed"));
        assert!(!failed.contains("done"));
    }

    #[test]
    fn sub_agent_tool_end_line_preserves_child_indent() {
        let ctx = SubAgentContext {
            agent_type: "explore".into(),
            depth: 1,
            spawn_id: "id".into(),
        };
        let line = format_sub_agent_tool_end_line(&ctx, "grep", true);
        // Child indent (4 spaces) matches the sub-agent start line.
        assert!(line.starts_with("    "));
        assert!(!line.contains("└─"));
        assert!(line.contains(theme::ICON_SUCCESS));
        assert!(line.contains("grep"));
        assert!(line.contains("done"));
    }

    #[test]
    fn format_tool_end_line_dispatches_on_sub_agent_context() {
        let main = LlmEvent {
            event_type: LlmEventType::ToolExecutionEnd,
            data: serde_json::json!({"tool_name": "glob"}),
            timestamp: 0.0,
            metadata: None,
        };
        let line = format_tool_end_line(&main, "glob", true);
        assert!(line.contains("╰─"));
        assert!(line.contains("glob"));

        let sub = sub_agent_event(
            LlmEventType::ToolExecutionEnd,
            serde_json::json!({"tool_name": "glob"}),
        );
        let line = format_tool_end_line(&sub, "glob", true);
        assert!(line.starts_with("    "));
        assert!(line.contains("glob"));
        assert!(!line.contains("╰─"));
    }

    #[test]
    fn sub_agent_thinking_prefix_includes_branch_and_agent_type() {
        let ctx = SubAgentContext {
            agent_type: "explore".into(),
            depth: 1,
            spawn_id: "id".into(),
        };
        assert_eq!(sub_agent_thinking_prefix(&ctx), "  └─ explore · ");
    }

    #[test]
    fn format_sub_agent_tool_line_uses_child_indent_without_agent_prefix() {
        let ctx = SubAgentContext {
            agent_type: "plan".into(),
            depth: 1,
            spawn_id: "id".into(),
        };
        let line = format_sub_agent_tool_line(&ctx, "grep", "pattern=x");
        assert!(line.starts_with("    "));
        assert!(line.contains("❯"));
        assert!(line.contains("grep"));
        assert!(!line.contains("plan"));
        assert!(line.contains("pattern=x"));
    }

    #[test]
    fn indent_sub_agent_output_block_uses_child_indent() {
        let ctx = SubAgentContext {
            agent_type: "explore".into(),
            depth: 1,
            spawn_id: "id".into(),
        };
        let indented = indent_sub_agent_output_block(&ctx, "line1\nline2");
        assert!(indented.starts_with("    line1"));
        assert!(indented.contains("\n    line2"));
        assert!(!indented.contains("└─"));
    }
}
