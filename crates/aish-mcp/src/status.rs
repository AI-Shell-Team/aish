//! Rendering for `/mcp`, `/mcp tools <server>` and the `/status` summary line.
//!
//! Everything here comes from [`ServerSnapshot`]s, which contain no
//! credentials: `endpoint` is the transport's redacted label, and env/header
//! values never reach the manager in the first place. The three entry points
//! are pure functions of the manager so the command layer can print them
//! anywhere.
//!
//! Layout rule: the renderer adds no indentation of its own. The locale
//! templates carry the leading spaces of the continuation lines (`endpoint`,
//! `failure`, `tool_line`), so a translator can restyle a block without
//! touching Rust.

use std::collections::HashMap;

use crate::manager::McpManager;
use crate::types::ServerStatus;

/// Longest tool description `/mcp tools` prints before it truncates.
const MAX_DESCRIPTION_CHARS: usize = 120;

/// `/mcp`: one block per configured server, plus a hint when there is nothing
/// to show.
pub fn render_status(manager: &McpManager) -> Vec<String> {
    let settings = manager.settings();
    // The switch is checked first: "MCP is off" and "MCP is on but no server is
    // configured" are different situations and get different hints.
    if !settings.enabled {
        return vec![aish_i18n::t("shell.mcp.disabled_hint")];
    }
    if settings.servers.is_empty() {
        return vec![aish_i18n::t("shell.mcp.no_servers")];
    }

    // Rendered from the configuration, not from the live registry: a server the
    // user just disabled (or one that failed before connecting) must still be
    // visible as `disabled`/`connecting` instead of silently disappearing.
    let snapshots = manager.snapshots();
    let mut lines = Vec::new();
    for server in &settings.servers {
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.name == server.name);
        let status = match snapshot {
            Some(snapshot) => snapshot.status.label().to_string(),
            None if server.enabled => "connecting".to_string(),
            None => "disabled".to_string(),
        };
        let tools = snapshot.map(|snapshot| snapshot.tools.len()).unwrap_or(0);
        let endpoint = snapshot
            .map(|snapshot| snapshot.endpoint.clone())
            .unwrap_or_else(|| server.endpoint_label());

        lines.push(aish_i18n::t_with_args(
            "shell.mcp.status_line",
            &args(&[
                ("name", server.name.clone()),
                ("transport", server.transport.as_str().to_string()),
                ("status", status),
                ("tools", tools.to_string()),
                ("trust", server.trust.as_str().to_string()),
                ("side_effects", server.side_effects.as_str().to_string()),
            ]),
        ));
        lines.push(aish_i18n::t_with_args(
            "shell.mcp.endpoint",
            &args(&[("endpoint", endpoint)]),
        ));
        if let Some(ServerStatus::Failed { reason }) = snapshot.map(|snapshot| &snapshot.status) {
            lines.push(aish_i18n::t_with_args(
                "shell.mcp.failure",
                &args(&[("reason", reason.clone())]),
            ));
        }
    }
    lines
}

/// `/mcp tools <server>`: the cached catalogue. `None` means the server is not
/// configured at all.
pub fn render_server_tools(manager: &McpManager, server: &str) -> Option<Vec<String>> {
    let settings = manager.settings().server(server).cloned()?;

    // A server that is configured but has no runtime entry yet (disabled, or
    // still on its way up) has no catalogue to show.
    let Some(snapshot) = manager
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.name == server)
    else {
        return Some(vec![if settings.enabled {
            aish_i18n::t_with_args(
                "shell.mcp.server_connecting",
                &args(&[("server", server.to_string())]),
            )
        } else {
            aish_i18n::t("shell.mcp.disabled_hint")
        }]);
    };

    // A server that has not finished connecting has no catalogue yet. Saying
    // "exposes no tools" would be wrong — and it hid the real state of slow
    // stdio servers (an `npx` cold start can take tens of seconds) — so the
    // connection state is reported instead of a tool list.
    match &snapshot.status {
        ServerStatus::Connected { .. } => {}
        ServerStatus::Connecting => {
            return Some(vec![aish_i18n::t_with_args(
                "shell.mcp.server_connecting",
                &args(&[("server", snapshot.name.clone())]),
            )]);
        }
        ServerStatus::Failed { reason } => {
            return Some(vec![aish_i18n::t_with_args(
                "shell.mcp.failure",
                &args(&[("reason", reason.clone())]),
            )]);
        }
        ServerStatus::Disabled => {
            return Some(vec![aish_i18n::t("shell.mcp.disabled_hint")]);
        }
    }

    let mut lines = vec![aish_i18n::t_with_args(
        "shell.mcp.tools_header",
        &args(&[
            ("server", snapshot.name.clone()),
            ("count", snapshot.tools.len().to_string()),
        ]),
    )];

    if snapshot.tools.is_empty() {
        lines.push(aish_i18n::t_with_args(
            "shell.mcp.no_tools",
            &args(&[("server", snapshot.name.clone())]),
        ));
        return Some(lines);
    }
    for tool in &snapshot.tools {
        lines.push(aish_i18n::t_with_args(
            "shell.mcp.tool_line",
            &args(&[
                ("name", tool.name.clone()),
                ("side_effects", tool.side_effects.as_str().to_string()),
                ("description", describe(tool.description.as_deref())),
            ]),
        ));
    }
    Some(lines)
}

/// `/status`: one summary line, or `None` when there is no MCP work to report.
///
/// `total` counts every configured server, matching what [`render_status`]
/// lists; `tools` counts only the catalogues that are actually live.
pub fn render_summary_line(manager: &McpManager) -> Option<String> {
    let settings = manager.settings();
    if !settings.enabled || settings.servers.is_empty() {
        return None;
    }
    let snapshots = manager.snapshots();
    let connected = snapshots
        .iter()
        .filter(|snapshot| matches!(snapshot.status, ServerStatus::Connected { .. }))
        .count();
    let tools: usize = snapshots
        .iter()
        .filter(|snapshot| matches!(snapshot.status, ServerStatus::Connected { .. }))
        .map(|snapshot| snapshot.tools.len())
        .sum();
    Some(aish_i18n::t_with_args(
        "shell.mcp.summary",
        &args(&[
            ("connected", connected.to_string()),
            // Count what the user configured, so a disabled server still shows
            // up as part of the total.
            ("total", settings.servers.len().to_string()),
            ("tools", tools.to_string()),
        ]),
    ))
}

/// Build the placeholder map for one `t_with_args` call.
fn args(pairs: &[(&str, String)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

/// A missing description renders as an empty string; a long one is truncated on
/// a character boundary so multi-byte text cannot be split.
fn describe(description: Option<&str>) -> String {
    let Some(text) = description.map(str::trim).filter(|text| !text.is_empty()) else {
        return String::new();
    };
    if text.chars().count() <= MAX_DESCRIPTION_CHARS {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(MAX_DESCRIPTION_CHARS).collect();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::config::{McpSettings, ServerSettings};
    use crate::types::{
        McpToolDefinition, SideEffects, ToolAnnotations, TransportKind, TrustLevel,
        MCP_PROTOCOL_VERSION,
    };

    fn settings(name: &str) -> ServerSettings {
        ServerSettings {
            name: name.to_string(),
            transport: TransportKind::Stdio,
            enabled: true,
            command: Some("fixture".to_string()),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            request_timeout: Some(Duration::from_secs(10)),
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Read,
            tool_trust: BTreeMap::new(),
            max_output_bytes: 64 * 1024,
        }
    }

    fn tool(name: &str, description: &str, read_only: Option<bool>) -> McpToolDefinition {
        McpToolDefinition {
            name: name.to_string(),
            description: Some(description.to_string()),
            input_schema: None,
            annotations: ToolAnnotations {
                read_only_hint: read_only,
                ..Default::default()
            },
        }
    }

    fn manager_with(servers: Vec<ServerSettings>) -> std::sync::Arc<McpManager> {
        McpManager::new(McpSettings {
            enabled: true,
            request_timeout: Some(Duration::from_secs(10)),
            max_output_bytes: 64 * 1024,
            servers,
        })
    }

    /// A connected server with a two-tool catalogue.
    fn connected(manager: &McpManager, name: &str) {
        manager.force_state(
            name,
            ServerStatus::Connected {
                protocol_version: MCP_PROTOCOL_VERSION.to_string(),
                tool_count: 2,
            },
            Some(vec![
                tool("echo", "Echo the input text back", Some(true)),
                tool("write", "Write a file to disk", None),
            ]),
        );
    }

    #[test]
    fn a_manager_without_servers_renders_the_no_servers_hint() {
        let manager = manager_with(Vec::new());
        assert_eq!(
            render_status(&manager),
            vec![aish_i18n::t("shell.mcp.no_servers")]
        );
        assert_eq!(render_summary_line(&manager), None);
        assert_eq!(render_server_tools(&manager, "anything"), None);
    }

    #[test]
    fn a_disabled_section_renders_the_disabled_hint() {
        let manager = McpManager::new(McpSettings {
            enabled: false,
            request_timeout: Some(Duration::from_secs(10)),
            max_output_bytes: 64 * 1024,
            servers: vec![settings("alpha")],
        });
        assert_eq!(
            render_status(&manager),
            vec![aish_i18n::t("shell.mcp.disabled_hint")]
        );
        assert_eq!(render_summary_line(&manager), None);
        assert_eq!(render_server_tools(&manager, "ghost"), None);
        // A configured server is still addressable while the section is off.
        assert!(render_server_tools(&manager, "alpha").is_some());
    }

    #[test]
    fn a_status_block_lists_each_server_with_its_failure() {
        let manager = manager_with(vec![settings("alpha"), settings("beta")]);
        connected(&manager, "alpha");
        manager.force_state(
            "beta",
            ServerStatus::Failed {
                reason: "connect connect: refused".to_string(),
            },
            None,
        );

        let lines = render_status(&manager);
        assert_eq!(
            lines.len(),
            5,
            "two lines for alpha, three for beta: {lines:?}"
        );
        assert_eq!(
            lines[0],
            aish_i18n::t_with_args(
                "shell.mcp.status_line",
                &args(&[
                    ("name", "alpha".to_string()),
                    ("transport", "stdio".to_string()),
                    ("status", "connected".to_string()),
                    ("tools", "2".to_string()),
                    ("trust", "ask".to_string()),
                    ("side_effects", "read".to_string()),
                ])
            )
        );
        assert_eq!(
            lines[1],
            aish_i18n::t_with_args(
                "shell.mcp.endpoint",
                &args(&[("endpoint", "fixture".to_string())])
            )
        );
        assert_eq!(
            lines[2],
            aish_i18n::t_with_args(
                "shell.mcp.status_line",
                &args(&[
                    ("name", "beta".to_string()),
                    ("transport", "stdio".to_string()),
                    ("status", "failed".to_string()),
                    ("tools", "0".to_string()),
                    ("trust", "ask".to_string()),
                    ("side_effects", "read".to_string()),
                ])
            )
        );
        assert_eq!(
            lines[3],
            aish_i18n::t_with_args(
                "shell.mcp.endpoint",
                &args(&[("endpoint", "fixture".to_string())])
            )
        );
        assert_eq!(
            lines[4],
            aish_i18n::t_with_args(
                "shell.mcp.failure",
                &args(&[("reason", "connect connect: refused".to_string())])
            )
        );
        // Nothing in the block may carry a credential; the endpoint label is
        // the redacted command line only.
        assert!(!lines.join("\n").contains("TOKEN"), "{lines:?}");
    }

    #[test]
    fn tool_lines_render_the_catalogue() {
        let manager = manager_with(vec![settings("alpha")]);
        connected(&manager, "alpha");

        let lines = render_server_tools(&manager, "alpha").expect("configured server");
        assert_eq!(
            lines[0],
            aish_i18n::t_with_args(
                "shell.mcp.tools_header",
                &args(&[("server", "alpha".to_string()), ("count", "2".to_string())])
            )
        );
        assert_eq!(
            lines[1],
            aish_i18n::t_with_args(
                "shell.mcp.tool_line",
                &args(&[
                    ("name", "echo".to_string()),
                    ("side_effects", "read".to_string()),
                    ("description", "Echo the input text back".to_string()),
                ])
            )
        );
        assert_eq!(
            lines[2],
            aish_i18n::t_with_args(
                "shell.mcp.tool_line",
                &args(&[
                    ("name", "write".to_string()),
                    ("side_effects", "read".to_string()),
                    ("description", "Write a file to disk".to_string()),
                ])
            )
        );

        // A *connected* server with no catalogue says so; an unknown one is None.
        let empty = manager_with(vec![settings("alpha")]);
        empty.force_state(
            "alpha",
            ServerStatus::Connected {
                protocol_version: MCP_PROTOCOL_VERSION.to_string(),
                tool_count: 0,
            },
            Some(Vec::new()),
        );
        let lines = render_server_tools(&empty, "alpha").expect("configured server");
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1],
            aish_i18n::t_with_args(
                "shell.mcp.no_tools",
                &args(&[("server", "alpha".to_string())])
            )
        );
        assert!(render_server_tools(&empty, "ghost").is_none());
    }

    /// A server that has not finished connecting must not be reported as
    /// "exposes no tools": real stdio servers can take tens of seconds to come
    /// up (an `npx` cold start), and the connection state is what the user
    /// needs to see.
    #[test]
    fn an_unconnected_server_reports_its_state_instead_of_an_empty_catalogue() {
        let manager = manager_with(vec![settings("alpha")]);
        manager.force_state("alpha", ServerStatus::Connecting, None);
        assert_eq!(
            render_server_tools(&manager, "alpha"),
            Some(vec![aish_i18n::t_with_args(
                "shell.mcp.server_connecting",
                &args(&[("server", "alpha".to_string())])
            )])
        );

        manager.force_state(
            "alpha",
            ServerStatus::Failed {
                reason: "connect connect: failed to spawn `npx`".to_string(),
            },
            None,
        );
        let failed = render_server_tools(&manager, "alpha").expect("configured server");
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains("failed to spawn"), "{failed:?}");

        manager.force_state("alpha", ServerStatus::Disabled, None);
        assert_eq!(
            render_server_tools(&manager, "alpha"),
            Some(vec![aish_i18n::t("shell.mcp.disabled_hint")])
        );
    }

    #[test]
    fn descriptions_are_truncated_at_a_character_boundary() {
        assert_eq!(describe(None), "");
        assert_eq!(describe(Some("   ")), "");
        assert_eq!(describe(Some("  Echo  ")), "Echo");

        let long = "a".repeat(200);
        let described = describe(Some(&long));
        assert_eq!(described.chars().count(), MAX_DESCRIPTION_CHARS + 3);
        assert!(described.ends_with("..."));

        // Multi-byte text must not be cut inside a character.
        let multibyte = "汉".repeat(200);
        let described = describe(Some(&multibyte));
        assert_eq!(described.chars().count(), MAX_DESCRIPTION_CHARS + 3);
        assert!(described.starts_with('汉'));
    }

    #[test]
    fn the_summary_counts_connected_servers_and_live_tools() {
        let manager = manager_with(vec![settings("alpha"), settings("beta")]);
        connected(&manager, "alpha");
        manager.force_state(
            "beta",
            ServerStatus::Failed {
                reason: "connect connect: refused".to_string(),
            },
            None,
        );

        assert_eq!(
            render_summary_line(&manager),
            Some(aish_i18n::t_with_args(
                "shell.mcp.summary",
                &args(&[
                    ("connected", "1".to_string()),
                    ("total", "2".to_string()),
                    ("tools", "2".to_string()),
                ])
            ))
        );

        // A server that is configured but idle contributes no tools.
        let idle = manager_with(vec![settings("alpha")]);
        assert_eq!(
            render_summary_line(&idle),
            Some(aish_i18n::t_with_args(
                "shell.mcp.summary",
                &args(&[
                    ("connected", "0".to_string()),
                    ("total", "1".to_string()),
                    ("tools", "0".to_string()),
                ])
            ))
        );
    }
}
