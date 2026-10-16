//! Integration tests for the `/mcp` shell command surface.
//!
//! The interactive shell needs a PTY, so these tests cover the pieces the
//! command is built from: the `mcp:` resolution the command reads at startup,
//! the gateway-tool registration premise (`app.rs` registers one gateway tool
//! per enabled server, keyed by [`aish_mcp::gateway_tool_name`]), and the i18n
//! keys the command renders in every locale.

use aish_config::{McpConfig, McpServerConfig};

/// A `stdio` config whose command points at a path that does not exist. The
/// resolver accepts it; the connection fails later at runtime.
fn config_with_missing_command() -> McpConfig {
    let mut config = McpConfig::default();
    config.servers.push(McpServerConfig {
        name: "broken".to_string(),
        transport: "stdio".to_string(),
        command: Some("/nonexistent/aish-mcp-test-server".to_string()),
        ..McpServerConfig::default()
    });
    config
}

#[test]
fn empty_mcp_section_is_inactive_and_warns_nothing() {
    // `/mcp` on this config prints `shell.mcp.no_servers`.
    let (settings, warnings) = aish_mcp::resolve_settings(&McpConfig::default());
    assert!(settings.is_inactive());
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn server_with_missing_command_is_not_inactive() {
    // A nonexistent command must not block startup: resolution succeeds, so
    // the shell registers the gateway tool and `/mcp status` later shows a
    // connecting/failed line.
    let (settings, _warnings) = aish_mcp::resolve_settings(&config_with_missing_command());
    assert!(!settings.is_inactive());
    assert_eq!(settings.enabled_servers().count(), 1);

    // Startup registration premise: one gateway tool per enabled server.
    let manager = aish_mcp::McpManager::new(settings);
    let names: Vec<String> = aish_mcp::gateway_tools(&manager)
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert_eq!(names, vec![aish_mcp::gateway_tool_name("broken")]);
}

#[test]
fn mcp_i18n_keys_exist_in_every_locale() {
    const LOCALES: &[&str] = &["en-US", "zh-CN", "de-DE", "es-ES", "fr-FR", "ja-JP"];
    const KEYS: &[&str] = &[
        "shell.slash.mcp",
        "shell.mcp.no_servers",
        "shell.mcp.disabled_hint",
        "shell.mcp.config_warning",
        "shell.mcp.status_line",
        "shell.mcp.endpoint",
        "shell.mcp.failure",
        "shell.mcp.tool_line",
        "shell.mcp.tools_header",
        "shell.mcp.server_connecting",
        "shell.mcp.unknown_server",
        "shell.mcp.no_tools",
        "shell.mcp.summary",
        "shell.mcp.help",
        "shell.mcp.runtime_unavailable",
        "shell.mcp.reload_header",
        "shell.mcp.reload_done",
        "shell.mcp.reconnect_started",
        "shell.mcp.reconnect_done",
        "shell.mcp.reconnect_failed",
        "shell.mcp.confirm_message",
        "shell.mcp.blocked_message",
        "shell.mcp.alternative_hint",
        "shell.mcp.invalid_action",
        "shell.mcp.tool_required",
        "shell.mcp.reason_server",
        "shell.mcp.reason_side_effects",
        "shell.mcp.reason_arguments",
        "shell.mcp.reason_trust",
        "shell.mcp.call_failed",
        "shell.mcp.needs_async",
        "shell.mcp.help_hint",
        "shell.mcp.unknown_subcommand",
        "shell.mcp.missing_server_arg",
        "shell.mcp.config_write_failed",
        "shell.mcp.menu_title",
        "shell.mcp.menu_question",
        "shell.mcp.menu_add",
        "shell.mcp.menu_tools",
        "shell.mcp.menu_tools_list",
        "shell.mcp.menu_test",
        "shell.mcp.menu_trust",
        "shell.mcp.menu_toggle",
        "shell.mcp.menu_reconnect",
        "shell.mcp.menu_reload",
        "shell.mcp.menu_remove",
        "shell.mcp.menu_help",
        "shell.mcp.menu_remove_confirm_title",
        "shell.mcp.menu_remove_confirm_question",
        "shell.mcp.menu_remove_confirm_no",
        "shell.mcp.menu_remove_confirm_yes",
        "shell.mcp.pick_server_title",
        "shell.mcp.pick_server_question",
        "shell.mcp.pick_tool_title",
        "shell.mcp.pick_tool_question",
        "shell.mcp.pick_tool_any",
        "shell.mcp.pick_level_title",
        "shell.mcp.pick_level_question",
        "shell.mcp.menu_cancelled",
        "shell.mcp.tool_detail_header",
        "shell.mcp.tool_detail_description",
        "shell.mcp.tool_detail_side_effects",
        "shell.mcp.tool_detail_trust",
        "shell.mcp.tool_detail_schema",
        "shell.mcp.tool_unknown",
        "shell.mcp.verdict_allow",
        "shell.mcp.verdict_confirm",
        "shell.mcp.verdict_block",
        "shell.mcp.add.name_prompt",
        "shell.mcp.add.transport_title",
        "shell.mcp.add.transport_question",
        "shell.mcp.add.command_prompt",
        "shell.mcp.add.args_prompt",
        "shell.mcp.add.url_prompt",
        "shell.mcp.add.token_prompt",
        "shell.mcp.add.trust_title",
        "shell.mcp.add.trust_question",
        "shell.mcp.add.expose_title",
        "shell.mcp.add.expose_question",
        "shell.mcp.add.side_effects_title",
        "shell.mcp.add.side_effects_question",
        "shell.mcp.add.confirm_title",
        "shell.mcp.add.confirm_summary",
        "shell.mcp.add.confirm_yes",
        "shell.mcp.add.confirm_no",
        "shell.mcp.add_cancelled",
        "shell.mcp.added",
        "shell.mcp.add_failed",
        "shell.mcp.removed",
        "shell.mcp.remove_failed",
        "shell.mcp.enabled",
        "shell.mcp.disabled",
        "shell.mcp.already_enabled",
        "shell.mcp.already_disabled",
        "shell.mcp.toggle_failed",
        "shell.mcp.trust_updated",
        "shell.mcp.trust_failed",
        "shell.mcp.test_ok",
        "shell.mcp.test_tool_line",
        "shell.mcp.test_failed",
        "shell.mcp.test_tip_not_found",
        "shell.mcp.test_tip_refused",
        "shell.mcp.test_tip_timeout",
        "shell.mcp.test_tip_auth",
    ];
    for locale in LOCALES {
        aish_i18n::set_locale(locale);
        for key in KEYS {
            assert_ne!(
                aish_i18n::t(key),
                *key,
                "missing i18n key {key} in locale {locale}"
            );
        }
    }
}

#[test]
fn mcp_placeholders_substitute_in_every_locale() {
    const LOCALES: &[&str] = &["en-US", "zh-CN", "de-DE", "es-ES", "fr-FR", "ja-JP"];
    for locale in LOCALES {
        aish_i18n::set_locale(locale);
        for (key, placeholders) in [
            ("shell.mcp.config_warning", &["warning"][..]),
            (
                "shell.mcp.status_line",
                &[
                    "name",
                    "transport",
                    "status",
                    "tools",
                    "trust",
                    "side_effects",
                ][..],
            ),
            ("shell.mcp.endpoint", &["endpoint"][..]),
            ("shell.mcp.failure", &["reason"][..]),
            (
                "shell.mcp.tool_line",
                &["name", "side_effects", "description"][..],
            ),
            ("shell.mcp.tools_header", &["server", "count"][..]),
            ("shell.mcp.unknown_server", &["server"][..]),
            ("shell.mcp.no_tools", &["server"][..]),
            ("shell.mcp.summary", &["connected", "total", "tools"][..]),
            ("shell.mcp.reconnect_started", &["server"][..]),
            ("shell.mcp.reconnect_done", &["server"][..]),
            ("shell.mcp.reconnect_failed", &["server", "reason"][..]),
            ("shell.mcp.unknown_subcommand", &["sub"][..]),
            ("shell.mcp.missing_server_arg", &["sub"][..]),
            ("shell.mcp.config_write_failed", &["error"][..]),
            ("shell.mcp.tool_detail_header", &["server", "tool"][..]),
            ("shell.mcp.tool_detail_description", &["description"][..]),
            (
                "shell.mcp.tool_detail_side_effects",
                &["declared", "reported", "effective"][..],
            ),
            ("shell.mcp.tool_detail_trust", &["trust", "verdict"][..]),
            ("shell.mcp.tool_detail_schema", &["schema"][..]),
            ("shell.mcp.tool_unknown", &["server", "tool"][..]),
            ("shell.mcp.add.confirm_summary", &["summary"][..]),
            ("shell.mcp.added", &["server", "transport", "status"][..]),
            ("shell.mcp.add_failed", &["error"][..]),
            ("shell.mcp.removed", &["server"][..]),
            ("shell.mcp.remove_failed", &["error"][..]),
            ("shell.mcp.enabled", &["server", "status"][..]),
            ("shell.mcp.disabled", &["server"][..]),
            ("shell.mcp.already_enabled", &["server"][..]),
            ("shell.mcp.already_disabled", &["server"][..]),
            ("shell.mcp.toggle_failed", &["error"][..]),
            ("shell.mcp.trust_updated", &["server", "tool", "level"][..]),
            ("shell.mcp.trust_failed", &["error"][..]),
            ("shell.mcp.test_ok", &["server", "version", "tools"][..]),
            ("shell.mcp.test_tool_line", &["name", "description"][..]),
            ("shell.mcp.test_failed", &["server", "error"][..]),
            ("shell.mcp.menu_remove_confirm_question", &["server"][..]),
        ] {
            let mut args = std::collections::HashMap::new();
            for placeholder in placeholders {
                args.insert((*placeholder).to_string(), format!("<{placeholder}>"));
            }
            let text = aish_i18n::t_with_args(key, &args);
            assert!(
                !text.contains('{'),
                "unsubstituted placeholder in {key} for {locale}: {text}"
            );
        }
    }
}

#[test]
fn command_surface_edits_the_config_the_shell_persists() {
    // The shell dispatches `/mcp ...` through `aish_mcp::parse_command` and
    // then applies exactly one pure edit. Walk the same sequence — quick-add,
    // enable/disable, trust, remove — and check the config each step produces:
    // that is the state `persist_mcp_config` writes back to config.yaml.
    let mut config = McpConfig::default();

    // `/mcp add fs --side-effects read --trust allow -- npx -y server /tmp`
    let add = aish_mcp::parse_command(&[
        "add",
        "fs",
        "--side-effects",
        "read",
        "--trust",
        "allow",
        "--",
        "npx",
        "-y",
        "@modelcontextprotocol/server-filesystem",
        "/tmp",
    ])
    .expect("add parses");
    match add {
        aish_mcp::McpCommand::Add(args) => {
            aish_mcp::add_to_config(&mut config, &args).expect("add applies");
        }
        other => panic!("expected Add, got {other:?}"),
    }
    assert_eq!(config.servers.len(), 1);
    assert_eq!(config.servers[0].name, "fs");
    assert_eq!(config.servers[0].transport, "stdio");
    assert_eq!(config.servers[0].command.as_deref(), Some("npx"));
    assert_eq!(
        config.servers[0].args.join(" "),
        "-y @modelcontextprotocol/server-filesystem /tmp"
    );
    assert!(config.servers[0].enabled);

    // The added entry resolves into exactly one gateway tool, and the trust
    // verdict `/mcp tool` prints matches the declared read-only scope.
    let (settings, _) = aish_mcp::resolve_settings(&config);
    let server = settings.server("fs").expect("resolved");
    assert_eq!(aish_mcp::gateway_tool_name("fs"), "mcp__fs");
    let effective = aish_mcp::combine_side_effects(server.side_effects, None);
    assert_eq!(effective, aish_mcp::SideEffects::Read);
    assert_eq!(
        aish_mcp::evaluate_call_trust(server.effective_trust("any_tool"), effective),
        aish_mcp::TrustDecision::Allow
    );

    // `/mcp disable fs` then `/mcp enable fs`.
    aish_mcp::set_enabled_in_config(&mut config, "fs", false).expect("disable applies");
    assert!(!config.servers[0].enabled);
    assert_eq!(aish_mcp::server_names(&config), vec!["fs".to_string()]);
    aish_mcp::set_enabled_in_config(&mut config, "fs", true).expect("enable applies");
    assert!(config.servers[0].enabled);

    // `/mcp trust fs * block`, then a per-tool override.
    aish_mcp::set_trust_in_config(&mut config, "fs", "*", aish_mcp::TrustLevel::Block)
        .expect("server trust applies");
    aish_mcp::set_trust_in_config(&mut config, "fs", "read_file", aish_mcp::TrustLevel::Allow)
        .expect("tool trust applies");
    assert_eq!(config.servers[0].trust, "block");
    assert_eq!(
        config.servers[0].tools.get("read_file").map(String::as_str),
        Some("allow")
    );

    // `/mcp tool` reports the per-tool override, not the server default.
    let (settings, _) = aish_mcp::resolve_settings(&config);
    let server = settings.server("fs").expect("resolved");
    assert_eq!(
        server.effective_trust("read_file"),
        aish_mcp::TrustLevel::Allow
    );
    assert_eq!(server.effective_trust("other"), aish_mcp::TrustLevel::Block);

    // `/mcp remove fs`, and the errors the shell renders for an unknown name.
    aish_mcp::remove_from_config(&mut config, "fs").expect("remove applies");
    assert!(config.servers.is_empty());
    assert!(aish_mcp::remove_from_config(&mut config, "fs").is_err());
    assert!(aish_mcp::set_enabled_in_config(&mut config, "fs", true).is_err());
    assert!(
        aish_mcp::set_trust_in_config(&mut config, "fs", "*", aish_mcp::TrustLevel::Ask).is_err()
    );
}
