//! `/mcp` command parsing and configuration edits.
//!
//! Three jobs:
//!
//! - [`parse_command`] turns the argv the shell already split into a
//!   [`McpCommand`]. It never touches the filesystem and never produces
//!   user-facing prose: errors are short technical strings the shell renders
//!   with its own locale.
//! - [`add_to_config`] / [`remove_from_config`] / [`set_enabled_in_config`] /
//!   [`set_trust_in_config`] apply one change to the `mcp:` section of an
//!   [`McpConfig`]. They are pure; writing the file is the shell's job.
//! - [`merge_into_document`] is the other half of that write: it folds a typed
//!   [`McpConfig`] back into the raw `config.yaml` document, so every key the
//!   typed model does not own survives an edit.
//!
//! The flag surface mirrors oh-my-pi's `/mcp add` quick-add: a name, transport
//! options, and either `--url` (HTTP) or `-- <command> [args...]` (stdio).

use std::collections::BTreeMap;

use aish_config::{McpConfig, McpServerConfig};

use crate::config::ExposeMode;
use crate::types::{SideEffects, TransportKind, TrustLevel};

/// Parsed `/mcp` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpCommand {
    /// `/mcp`, `/mcp status`, `/mcp list`: show every server.
    Status,
    /// `/mcp help`: show the command list.
    Help,
    /// `/mcp tools [server]`: list the tools of one server, or of all of them.
    Tools {
        server: Option<String>,
    },
    /// `/mcp tool <server> <tool>`: show one tool.
    Tool {
        server: String,
        tool: String,
    },
    /// `/mcp add ...`: quick-add, or the wizard when nothing was given.
    Add(AddArgs),
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    Trust {
        server: String,
        tool: String,
        level: TrustLevel,
    },
    Test {
        name: String,
    },
    Reload,
    Reconnect {
        name: String,
    },
    Unknown(String),
}

/// Arguments of `/mcp add`, filled from flags (quick-add) or by the wizard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddArgs {
    pub name: Option<String>,
    pub transport: Option<TransportKind>,
    /// `-- <cmd> [args...]`, or what the wizard collected.
    pub command: Vec<String>,
    pub url: Option<String>,
    /// `--token <t>` becomes an `Authorization: Bearer <t>` header when no
    /// `Authorization` header was given explicitly (`add_to_config` is the one
    /// place that builds it).
    pub token: Option<String>,
    pub trust: Option<TrustLevel>,
    pub side_effects: Option<SideEffects>,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
    /// `--expose gateway|tools`; `None` means the default `gateway` exposure.
    pub expose: Option<ExposeMode>,
    pub env: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}

/// Parse the argv of a `/mcp` invocation (without `/mcp` itself).
///
/// Subcommands are matched case-insensitively and `rm`/`list` are aliases. An
/// unknown subcommand is [`McpCommand::Unknown`] rather than an error, so the
/// shell can suggest the closest name; an unknown *flag* is an error, because
/// silently dropping a flag would configure the wrong server.
pub fn parse_command(args: &[&str]) -> Result<McpCommand, String> {
    let Some((subcommand, rest)) = args.split_first() else {
        // A bare `/mcp` is the status view.
        return Ok(McpCommand::Status);
    };
    match subcommand.to_ascii_lowercase().as_str() {
        "status" | "list" => Ok(McpCommand::Status),
        "help" => Ok(McpCommand::Help),
        "tools" => match rest {
            [] => Ok(McpCommand::Tools { server: None }),
            [server] => Ok(McpCommand::Tools {
                server: Some((*server).to_string()),
            }),
            _ => Err("`tools` takes at most one server name".to_string()),
        },
        "tool" => match rest {
            [server, tool] => Ok(McpCommand::Tool {
                server: (*server).to_string(),
                tool: (*tool).to_string(),
            }),
            _ => Err("`tool` requires a server name and a tool name".to_string()),
        },
        "add" => parse_add(rest).map(McpCommand::Add),
        "remove" | "rm" => single_name(rest, "remove").map(|name| McpCommand::Remove { name }),
        "enable" => single_name(rest, "enable").map(|name| McpCommand::Enable { name }),
        "disable" => single_name(rest, "disable").map(|name| McpCommand::Disable { name }),
        "trust" => match rest {
            [server, tool, level] => {
                let level = TrustLevel::parse(level).ok_or_else(|| {
                    format!("unknown trust level `{level}` (expected `ask`, `allow` or `block`)")
                })?;
                Ok(McpCommand::Trust {
                    server: (*server).to_string(),
                    tool: (*tool).to_string(),
                    level,
                })
            }
            _ => Err(
                "`trust` requires a server name, a tool name (`*` for the whole server) and a level"
                    .to_string(),
            ),
        },
        "test" => single_name(rest, "test").map(|name| McpCommand::Test { name }),
        "reload" => Ok(McpCommand::Reload),
        "reconnect" => single_name(rest, "reconnect").map(|name| McpCommand::Reconnect { name }),
        other => Ok(McpCommand::Unknown(other.to_string())),
    }
}

/// Parse `add` arguments.
///
/// Both flag spellings work (`--timeout 5000` and `--timeout=5000`) and
/// everything after `--` is the child command line.
fn parse_add(args: &[&str]) -> Result<AddArgs, String> {
    let mut parsed = AddArgs::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index];
        index += 1;

        if arg == "--" {
            parsed.command = args[index..]
                .iter()
                .map(|value| (*value).to_string())
                .collect();
            break;
        }
        let Some(option) = arg.strip_prefix("--") else {
            if parsed.name.is_none() {
                parsed.name = Some(arg.to_string());
                continue;
            }
            return Err(format!("unexpected argument: {arg}"));
        };

        let (flag, inline) = match option.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (option, None),
        };
        match flag {
            "transport" => {
                let value = take_value(args, &mut index, flag, inline)?;
                parsed.transport = Some(match value.to_ascii_lowercase().as_str() {
                    "stdio" => TransportKind::Stdio,
                    "http" => TransportKind::Http,
                    other => {
                        return Err(format!(
                            "unknown transport `{other}` (expected `stdio` or `http`)"
                        ))
                    }
                });
            }
            "trust" => {
                let value = take_value(args, &mut index, flag, inline)?;
                parsed.trust = Some(TrustLevel::parse(&value).ok_or_else(|| {
                    format!("unknown trust level `{value}` (expected `ask`, `allow` or `block`)")
                })?);
            }
            "side-effects" => {
                let value = take_value(args, &mut index, flag, inline)?;
                parsed.side_effects = Some(SideEffects::parse(&value).ok_or_else(|| {
                    format!(
                        "unknown side effects `{value}` (expected `read`, `network`, `write` or `unknown`)"
                    )
                })?);
            }
            "timeout" => {
                let value = take_value(args, &mut index, flag, inline)?;
                parsed.timeout_ms = Some(
                    value
                        .parse()
                        .map_err(|_| format!("`--timeout` expects milliseconds, got `{value}`"))?,
                );
            }
            "max-output" => {
                let value = take_value(args, &mut index, flag, inline)?;
                parsed.max_output_bytes = Some(value.parse().map_err(|_| {
                    format!("`--max-output` expects a number of bytes, got `{value}`")
                })?);
            }
            "expose" => {
                let value = take_value(args, &mut index, flag, inline)?;
                parsed.expose = Some(match value.trim().to_ascii_lowercase().as_str() {
                    "gateway" => ExposeMode::Gateway,
                    "tools" => ExposeMode::Tools,
                    _ => {
                        return Err(format!(
                            "invalid --expose value: {value} (expected gateway or tools)"
                        ))
                    }
                });
            }
            "env" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let (key, value) = split_pair(&value, "--env")?;
                parsed.env.push((key.to_string(), value.to_string()));
            }
            "header" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let (key, value) = split_pair(&value, "--header")?;
                parsed.headers.push((key.to_string(), value.to_string()));
            }
            "url" => {
                parsed.url = Some(take_value(args, &mut index, flag, inline)?);
            }
            "token" => {
                // Recorded only; `add_to_config` materializes the
                // `Authorization` header, so `--token t --header
                // authorization=...` cannot end up with two of them.
                parsed.token = Some(take_value(args, &mut index, flag, inline)?);
            }
            other => return Err(format!("unknown option: --{other}")),
        }
    }

    if parsed.url.is_some() && !parsed.command.is_empty() {
        return Err("`--url` and `-- <command>` are mutually exclusive".to_string());
    }
    if let Some(url) = parsed.url.as_mut() {
        let trimmed = url.trim().to_string();
        *url = if trimmed.is_empty() || trimmed.contains("://") {
            trimmed
        } else {
            format!("https://{trimmed}")
        };
    }
    // An empty name means the user asked for the wizard, not for a server
    // literally named "".
    if parsed.name.as_deref().map(str::trim).map(str::is_empty) == Some(true) {
        parsed.name = None;
    }
    Ok(parsed)
}

/// One server name, exactly.
fn single_name(args: &[&str], subcommand: &str) -> Result<String, String> {
    match args {
        [name] if !name.trim().is_empty() => Ok((*name).to_string()),
        [] | [_] => Err(format!("`{subcommand}` requires a server name")),
        _ => Err(format!("`{subcommand}` takes exactly one server name")),
    }
}

/// The value of a flag, either `--flag=value` or the next argument.
fn take_value(
    args: &[&str],
    index: &mut usize,
    flag: &str,
    inline: Option<&str>,
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value.to_string());
    }
    match args.get(*index) {
        Some(value) => {
            *index += 1;
            Ok((*value).to_string())
        }
        None => Err(format!("`--{flag}` requires a value")),
    }
}

/// Split a `KEY=VALUE` flag payload.
fn split_pair<'a>(value: &'a str, flag: &str) -> Result<(&'a str, &'a str), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| format!("`{flag}` expects KEY=VALUE, got `{value}`"))?;
    if key.trim().is_empty() {
        return Err(format!("`{flag}` expects KEY=VALUE, got an empty key"));
    }
    Ok((key, value))
}

/// Add one server to the `mcp.servers` list and return the entry as written.
///
/// The transport defaults to `http` when a URL was given and to `stdio`
/// otherwise. A `stdio` server needs a command, an `http` server needs a URL,
/// and the name must be non-empty (the caller reports that as it likes).
pub fn add_to_config(config: &mut McpConfig, args: &AddArgs) -> Result<McpServerConfig, String> {
    let name = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "a server name is required".to_string())?
        .to_string();
    if config.servers.iter().any(|server| server.name == name) {
        return Err(format!("a server named `{name}` is already configured"));
    }

    let transport = args.transport.unwrap_or(if args.url.is_some() {
        TransportKind::Http
    } else {
        TransportKind::Stdio
    });
    match transport {
        TransportKind::Stdio => {
            if args.command.is_empty() {
                return Err(format!(
                    "the stdio transport of `{name}` needs a command (`-- <command> [args...]`) or `--url`"
                ));
            }
            if args.url.is_some() {
                return Err("`--url` and `-- <command>` are mutually exclusive".to_string());
            }
        }
        TransportKind::Http => {
            if args
                .url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .is_none()
            {
                return Err(format!(
                    "the http transport of `{name}` needs `--url <url>`"
                ));
            }
            if !args.command.is_empty() {
                return Err("`--url` and `-- <command>` are mutually exclusive".to_string());
            }
        }
    }

    let mut headers: BTreeMap<String, String> = args.headers.iter().cloned().collect();
    // The single place an `Authorization` header is built: from `--token`, or
    // from a wizard that filled `token` without building the header. An
    // explicitly configured `Authorization` header (any casing) always wins, and
    // is never duplicated — `parse_add` deliberately keeps the token out of
    // `headers`.
    if let Some(token) = args
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        if !headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization"))
        {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
    }

    let server = McpServerConfig {
        name,
        transport: transport.as_str().to_string(),
        enabled: true,
        command: args.command.first().cloned(),
        args: args.command.iter().skip(1).cloned().collect(),
        env: args.env.iter().cloned().collect(),
        cwd: None,
        url: args.url.clone(),
        headers,
        request_timeout_ms: args.timeout_ms,
        // An empty trust/side-effects value means "not configured": the
        // resolver turns it into `ask`/`unknown`, which is the safe default.
        trust: args
            .trust
            .map(|level| level.as_str().to_string())
            .unwrap_or_default(),
        side_effects: args
            .side_effects
            .map(|effects| effects.as_str().to_string())
            .unwrap_or_default(),
        tools: BTreeMap::new(),
        max_output_bytes: args.max_output_bytes,
        // Empty means the default `gateway` exposure.
        expose: args
            .expose
            .map(|mode| mode.as_str().to_string())
            .unwrap_or_default(),
    };
    config.servers.push(server.clone());
    Ok(server)
}

/// Drop one server from the configuration.
pub fn remove_from_config(config: &mut McpConfig, name: &str) -> Result<(), String> {
    let before = config.servers.len();
    config.servers.retain(|server| server.name != name);
    if config.servers.len() == before {
        return Err(format!("no MCP server named `{name}` is configured"));
    }
    Ok(())
}

/// Flip one server's `enabled` flag; every other parameter is untouched.
pub fn set_enabled_in_config(
    config: &mut McpConfig,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let Some(server) = config.servers.iter_mut().find(|server| server.name == name) else {
        return Err(format!("no MCP server named `{name}` is configured"));
    };
    server.enabled = enabled;
    Ok(())
}

/// Set the trust level of one tool, or of the whole server when `tool` is `*`.
pub fn set_trust_in_config(
    config: &mut McpConfig,
    server: &str,
    tool: &str,
    level: TrustLevel,
) -> Result<(), String> {
    let tool = tool.trim();
    if tool.is_empty() {
        return Err("the tool name must not be empty".to_string());
    }
    let Some(entry) = config.servers.iter_mut().find(|entry| entry.name == server) else {
        return Err(format!("no MCP server named `{server}` is configured"));
    };
    if tool == crate::types::ANY_TOOL {
        entry.trust = level.as_str().to_string();
    } else {
        entry
            .tools
            .insert(tool.to_string(), level.as_str().to_string());
    }
    Ok(())
}

/// Fold a typed [`McpConfig`] into a raw `config.yaml` document, in place.
///
/// The shell owns the file I/O (it hands the document to
/// `aish_config::ConfigLoader::update_raw`), so this function only edits the
/// `serde_yaml` tree — and it edits the `mcp` section *surgically* instead of
/// replacing it:
///
/// - the top-level `mcp` keys this editor owns (`enabled`, `request_timeout_ms`)
///   are set from `config`;
/// - `mcp.servers` is merged entry by entry, joined on `name`: our known fields
///   are written into the entry that already has that name — which keeps its
///   key order and every unknown key inside it (a hand-added `startup_grace_ms`,
///   a note, ...) — entries whose name is no longer configured are dropped
///   (that is what `remove` means), and servers that were not in the document
///   are appended in configuration order;
/// - every other key under `mcp:` and everything outside `mcp:` is left
///   untouched, key order included. That covers user extensions such as
///   `my_notes` and the fields of [`McpConfig`] this editor never changes
///   (`max_output_bytes`), so a hand-edited value cannot be clobbered by an
///   unrelated `/mcp` command.
///
/// A `null` document, or a `null` `mcp:` value, is normalized to an empty
/// mapping. A document whose root (or whose `mcp`) is not a mapping is rejected
/// rather than clobbered: a hand-broken config file must never be silently
/// replaced by a partial one.
///
/// The merge is deliberately derived from the *typed* config alone: fields the
/// shell changed in memory win, everything else comes from the file. It does
/// not read or write anything, so the caller stays responsible for the
/// (unlocked) read-modify-write window described on
/// `aish_config::ConfigLoader::update_raw`.
pub fn merge_into_document(
    document: &mut serde_yaml::Value,
    config: &McpConfig,
) -> Result<(), String> {
    if document.is_null() {
        *document = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }

    let mcp_key = serde_yaml::Value::String("mcp".to_string());
    let servers_key = serde_yaml::Value::String("servers".to_string());

    let root = document
        .as_mapping_mut()
        .ok_or_else(|| "the config document is not a mapping".to_string())?;
    if !root.contains_key(&mcp_key) {
        root.insert(
            mcp_key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    let mcp_value = root.get_mut(&mcp_key).expect("`mcp` was just inserted");
    if mcp_value.is_null() {
        *mcp_value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let mcp = mcp_value
        .as_mapping_mut()
        .ok_or_else(|| "`mcp` in the config document is not a mapping".to_string())?;

    // Only the `mcp` keys this editor owns are written: `enabled` and
    // `request_timeout_ms` here, `servers` merged below. Any other key under
    // `mcp:` — a user extension, or a field this shell never changes such as
    // `max_output_bytes` — keeps whatever the file has. `Mapping::insert` keeps
    // the slot of a key that is already present, so the file's own key order
    // survives the update.
    mcp.insert(
        serde_yaml::Value::String("enabled".to_string()),
        serde_yaml::Value::Bool(config.enabled),
    );
    mcp.insert(
        serde_yaml::Value::String("request_timeout_ms".to_string()),
        serde_yaml::Value::Number(config.request_timeout_ms.into()),
    );

    let mut typed_servers: Vec<(String, serde_yaml::Value)> =
        Vec::with_capacity(config.servers.len());
    for server in &config.servers {
        typed_servers.push((server.name.clone(), server_to_value(server)?));
    }

    // The configured list, not the file, decides which servers exist and in
    // which order; each configured server is joined with its existing entry so
    // its unknown keys survive.
    let existing = mcp
        .get(&servers_key)
        .and_then(serde_yaml::Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let mut merged: Vec<serde_yaml::Value> = Vec::with_capacity(typed_servers.len());
    let mut written = vec![false; typed_servers.len()];
    for entry in &existing {
        // A server entry is joined on `name`; an entry without one can only
        // belong to the (unusable) empty-name server, and is joined to it.
        let name = entry
            .get("name")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or_default();
        let Some(index) = typed_servers.iter().position(|(typed, _)| typed == name) else {
            // No longer configured: drop the entry, unknown keys included.
            continue;
        };
        if written[index] {
            // The file lists the same name twice; keep its first entry only.
            continue;
        }
        written[index] = true;
        merged.push(merge_server_entry(entry, &typed_servers[index].1));
    }
    for (index, (_, value)) in typed_servers.iter().enumerate() {
        if !written[index] {
            merged.push(value.clone());
        }
    }
    mcp.insert(servers_key, serde_yaml::Value::Sequence(merged));

    Ok(())
}

/// Encode one typed server as the mapping written into `mcp.servers`.
fn server_to_value(server: &McpServerConfig) -> Result<serde_yaml::Value, String> {
    serde_yaml::to_value(server)
        .map_err(|error| format!("failed to encode the mcp server `{}`: {error}", server.name))
}

/// Overlay the typed fields of one server onto its existing YAML entry.
///
/// The existing mapping is kept — and therefore its key order — and only the
/// keys the typed model owns are replaced (appended when the entry did not have
/// them yet). Keys the typed model does not know (`startup_grace_ms`, notes,
/// ...) stay untouched and in place. An entry that is not a mapping cannot carry
/// unknown keys worth keeping and is replaced by the typed one.
fn merge_server_entry(
    existing: &serde_yaml::Value,
    typed: &serde_yaml::Value,
) -> serde_yaml::Value {
    let Some(mut merged) = existing.as_mapping().cloned() else {
        // Not a mapping: there are no unknown keys to keep, so the typed entry
        // replaces it.
        return typed.clone();
    };
    let Some(typed) = typed.as_mapping() else {
        return serde_yaml::Value::Mapping(merged);
    };
    for (key, value) in typed {
        merged.insert(key.clone(), value.clone());
    }
    serde_yaml::Value::Mapping(merged)
}

/// Every configured server name, in configuration order (for completion).
pub fn server_names(config: &McpConfig) -> Vec<String> {
    config
        .servers
        .iter()
        .map(|server| server.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> McpCommand {
        parse_command(args).expect("the command must parse")
    }

    fn parse_error(args: &[&str]) -> String {
        parse_command(args).expect_err("the command must be rejected")
    }

    fn add_args(args: &[&str]) -> AddArgs {
        match parse_command(args).expect("the command must parse") {
            McpCommand::Add(args) => args,
            other => panic!("expected `add`, got {other:?}"),
        }
    }

    fn blank_config() -> McpConfig {
        McpConfig {
            enabled: true,
            request_timeout_ms: 30_000,
            max_output_bytes: 4096,
            servers: Vec::new(),
        }
    }

    fn stdio_args(name: &str) -> AddArgs {
        AddArgs {
            name: Some(name.to_string()),
            command: vec!["srv".to_string(), "--flag".to_string()],
            ..AddArgs::default()
        }
    }

    #[test]
    fn parses_every_subcommand() {
        assert_eq!(parse(&[]), McpCommand::Status);
        assert_eq!(parse(&["status"]), McpCommand::Status);
        assert_eq!(parse(&["list"]), McpCommand::Status);
        assert_eq!(parse(&["STATUS"]), McpCommand::Status);
        assert_eq!(parse(&["help"]), McpCommand::Help);
        assert_eq!(parse(&["tools"]), McpCommand::Tools { server: None });
        assert_eq!(
            parse(&["tools", "fs"]),
            McpCommand::Tools {
                server: Some("fs".to_string())
            }
        );
        assert_eq!(
            parse(&["tool", "fs", "read_file"]),
            McpCommand::Tool {
                server: "fs".to_string(),
                tool: "read_file".to_string(),
            }
        );
        assert_eq!(
            parse(&["remove", "fs"]),
            McpCommand::Remove {
                name: "fs".to_string()
            }
        );
        assert_eq!(
            parse(&["rm", "fs"]),
            McpCommand::Remove {
                name: "fs".to_string()
            }
        );
        assert_eq!(
            parse(&["enable", "fs"]),
            McpCommand::Enable {
                name: "fs".to_string()
            }
        );
        assert_eq!(
            parse(&["disable", "fs"]),
            McpCommand::Disable {
                name: "fs".to_string()
            }
        );
        assert_eq!(
            parse(&["test", "fs"]),
            McpCommand::Test {
                name: "fs".to_string()
            }
        );
        assert_eq!(parse(&["reload"]), McpCommand::Reload);
        assert_eq!(
            parse(&["reconnect", "fs"]),
            McpCommand::Reconnect {
                name: "fs".to_string()
            }
        );
        assert_eq!(
            parse(&["trust", "fs", "*", "allow"]),
            McpCommand::Trust {
                server: "fs".to_string(),
                tool: "*".to_string(),
                level: TrustLevel::Allow,
            }
        );
        assert_eq!(
            parse(&["trust", "fs", "write_file", "block"]),
            McpCommand::Trust {
                server: "fs".to_string(),
                tool: "write_file".to_string(),
                level: TrustLevel::Block,
            }
        );
        assert_eq!(
            parse(&["nonsense"]),
            McpCommand::Unknown("nonsense".to_string())
        );
    }

    #[test]
    fn rejects_missing_arguments_and_unknown_options() {
        assert!(parse_error(&["tool", "fs"]).contains("requires a server name and a tool name"));
        assert!(parse_error(&["tools", "a", "b"]).contains("at most one server name"));
        assert!(parse_error(&["remove"]).contains("requires a server name"));
        assert!(parse_error(&["rm", "a", "b"]).contains("exactly one server name"));
        assert!(parse_error(&["enable"]).contains("requires a server name"));
        assert!(parse_error(&["disable"]).contains("requires a server name"));
        assert!(parse_error(&["test"]).contains("requires a server name"));
        assert!(parse_error(&["reconnect"]).contains("requires a server name"));
        assert!(parse_error(&["trust", "fs", "*"]).contains("requires a server name"));
        assert!(parse_error(&["trust", "fs", "*", "maybe"]).contains("unknown trust level"));
        assert_eq!(parse_error(&["add", "--foo"]), "unknown option: --foo");
        assert!(parse_error(&["add", "--url"]).contains("requires a value"));
        assert!(parse_error(&["add", "--timeout", "fast"]).contains("milliseconds"));
        assert!(parse_error(&["add", "--max-output", "-1"]).contains("number of bytes"));
        assert!(parse_error(&["add", "--expose", "both"]).contains("invalid --expose value: both"));
        assert!(parse_error(&["add", "--transport", "sse"]).contains("unknown transport"));
        assert!(
            parse_error(&["add", "--side-effects", "everything"]).contains("unknown side effects")
        );
        assert!(parse_error(&["add", "--env", "TOKEN"]).contains("expects KEY=VALUE"));
        assert!(parse_error(&["add", "--header", "=1"]).contains("empty key"));
        assert!(parse_error(&["add", "fs", "extra"]).contains("unexpected argument: extra"));
    }

    #[test]
    fn parses_a_stdio_quick_add() {
        let args = add_args(&[
            "add",
            "fs",
            "--transport",
            "stdio",
            "--trust",
            "allow",
            "--side-effects",
            "read",
            "--timeout",
            "1500",
            "--max-output",
            "2048",
            "--env",
            "TOKEN=abc",
            "--header",
            "X-Trace=1",
            "--",
            "npx",
            "-y",
            "@modelcontextprotocol/server-filesystem",
            "/tmp",
        ]);

        assert_eq!(args.name.as_deref(), Some("fs"));
        assert_eq!(args.transport, Some(TransportKind::Stdio));
        assert_eq!(args.trust, Some(TrustLevel::Allow));
        assert_eq!(args.side_effects, Some(SideEffects::Read));
        assert_eq!(args.timeout_ms, Some(1500));
        assert_eq!(args.max_output_bytes, Some(2048));
        assert_eq!(args.env, vec![("TOKEN".to_string(), "abc".to_string())]);
        assert_eq!(args.headers, vec![("X-Trace".to_string(), "1".to_string())]);
        assert_eq!(
            args.command,
            vec![
                "npx".to_string(),
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                "/tmp".to_string()
            ]
        );
        assert_eq!(args.url, None);
        assert_eq!(args.token, None);
    }

    #[test]
    fn expose_flag_parses_and_reaches_the_config() {
        // `--expose` is what closes the quick-add gap over the `expose` field.
        for (value, expected) in [
            ("gateway", ExposeMode::Gateway),
            ("tools", ExposeMode::Tools),
        ] {
            let args = add_args(&["add", "fs", "--expose", value, "--", "srv"]);
            assert_eq!(args.expose, Some(expected));
            let mut config = blank_config();
            let server = add_to_config(&mut config, &args).expect("add");
            assert_eq!(server.expose, expected.as_str());
        }

        let error = parse_error(&["add", "fs", "--expose", "eager"]);
        assert_eq!(
            error,
            "invalid --expose value: eager (expected gateway or tools)"
        );

        // Without the flag the field stays empty, which the resolver reads as
        // the default `gateway` exposure.
        let args = add_args(&["add", "fs", "--", "srv"]);
        assert_eq!(args.expose, None);
        let mut config = blank_config();
        let server = add_to_config(&mut config, &args).expect("add");
        assert_eq!(server.expose, "");
    }

    #[test]
    fn parses_an_http_quick_add_and_carries_the_token() {
        let args = add_args(&[
            "add",
            "github",
            "--url=api.example.com/mcp",
            "--token",
            "s3cret",
            "--transport=http",
        ]);

        assert_eq!(args.name.as_deref(), Some("github"));
        assert_eq!(args.url.as_deref(), Some("https://api.example.com/mcp"));
        assert_eq!(args.transport, Some(TransportKind::Http));
        assert_eq!(args.token.as_deref(), Some("s3cret"));
        assert!(
            args.headers.is_empty(),
            "`--token` must stay a token; `add_to_config` builds the header once: {:?}",
            args.headers
        );
        assert!(args.command.is_empty());
    }

    #[test]
    fn a_token_never_duplicates_an_explicit_authorization_header() {
        // Regression: `parse_add` used to push its own `Authorization` header
        // *and* keep the token, so input mixing `--token` with an explicitly
        // spelled (any casing) authorization header persisted two of them, or
        // let the token shadow the user's real header.
        for argv in [
            vec![
                "add",
                "srv",
                "--url",
                "https://x/mcp",
                "--token",
                "t",
                "--header",
                "authorization=Basic abc",
            ],
            vec![
                "add",
                "srv",
                "--url",
                "https://x/mcp",
                "--header",
                "AUTHORIZATION=Basic abc",
                "--token",
                "t",
            ],
        ] {
            let args = add_args(&argv);
            assert_eq!(args.token.as_deref(), Some("t"));
            assert_eq!(
                args.headers.len(),
                1,
                "the token must not pre-build a header: {:?}",
                args.headers
            );
            assert!(
                args.headers[0].0.eq_ignore_ascii_case("authorization"),
                "the user's own header must be the only one: {:?}",
                args.headers
            );

            let mut config = blank_config();
            let server = add_to_config(&mut config, &args).expect("add");
            assert_eq!(
                server.headers.len(),
                1,
                "exactly one header must be persisted: {:?}",
                server.headers
            );
            let (key, value) = server.headers.iter().next().expect("the explicit header");
            assert!(key.eq_ignore_ascii_case("authorization"), "{key}");
            assert_eq!(value, "Basic abc", "the explicit header wins over --token");
        }
    }

    #[test]
    fn a_bare_add_is_the_wizard() {
        assert_eq!(add_args(&["add"]), AddArgs::default());
        // An empty name is not a name.
        assert_eq!(add_args(&["add", "  "]), AddArgs::default());
    }

    #[test]
    fn an_absolute_url_keeps_its_scheme_and_url_excludes_a_command() {
        let args = add_args(&["add", "--url", "http://127.0.0.1:8931/mcp"]);
        assert_eq!(args.url.as_deref(), Some("http://127.0.0.1:8931/mcp"));
        assert_eq!(args.name, None);

        let error = parse_error(&["add", "fs", "--url", "https://x/mcp", "--", "npx", "server"]);
        assert!(error.contains("mutually exclusive"), "{error}");
    }

    #[test]
    fn add_writes_a_stdio_server_and_refuses_a_duplicate() {
        let mut config = blank_config();
        let server = add_to_config(&mut config, &stdio_args("fs")).expect("add");
        assert_eq!(server.name, "fs");
        assert_eq!(server.transport, "stdio");
        assert_eq!(server.command.as_deref(), Some("srv"));
        assert_eq!(server.args, vec!["--flag".to_string()]);
        assert_eq!(server.url, None);
        assert_eq!(server.trust, "");
        assert_eq!(server.side_effects, "");
        assert!(server.enabled);
        assert_eq!(config.servers.len(), 1);

        let error = add_to_config(&mut config, &stdio_args("fs")).expect_err("duplicate");
        assert!(error.contains("already configured"), "{error}");
        assert_eq!(config.servers.len(), 1, "a duplicate must not be appended");
    }

    #[test]
    fn add_infers_the_transport_and_validates_its_requirements() {
        let mut config = blank_config();
        let server = add_to_config(
            &mut config,
            &AddArgs {
                name: Some("gh".to_string()),
                url: Some("https://example.com/mcp".to_string()),
                trust: Some(TrustLevel::Ask),
                side_effects: Some(SideEffects::Network),
                ..AddArgs::default()
            },
        )
        .expect("add");
        assert_eq!(server.transport, "http");
        assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(server.command, None);
        assert_eq!(server.trust, "ask");
        assert_eq!(server.side_effects, "network");

        let http_without_url = add_to_config(
            &mut config,
            &AddArgs {
                name: Some("nourl".to_string()),
                transport: Some(TransportKind::Http),
                ..AddArgs::default()
            },
        )
        .expect_err("http without a url");
        assert!(
            http_without_url.contains("needs `--url"),
            "{http_without_url}"
        );

        let unnamed = add_to_config(
            &mut config,
            &AddArgs {
                transport: Some(TransportKind::Stdio),
                command: vec!["srv".to_string()],
                ..AddArgs::default()
            },
        )
        .expect_err("an unnamed server");
        assert!(unnamed.contains("server name"), "{unnamed}");

        let stdio_without_command = add_to_config(
            &mut config,
            &AddArgs {
                name: Some("bad".to_string()),
                ..AddArgs::default()
            },
        )
        .expect_err("stdio without a command");
        assert!(
            stdio_without_command.contains("needs a command"),
            "{stdio_without_command}"
        );

        let both = add_to_config(
            &mut config,
            &AddArgs {
                name: Some("both".to_string()),
                url: Some("https://example.com/mcp".to_string()),
                command: vec!["srv".to_string()],
                ..AddArgs::default()
            },
        )
        .expect_err("url and command");
        assert!(both.contains("mutually exclusive"), "{both}");

        assert_eq!(config.servers.len(), 1, "only the valid entry was written");
    }

    #[test]
    fn add_carries_a_token_into_the_authorization_header() {
        let mut config = blank_config();
        let server = add_to_config(
            &mut config,
            &AddArgs {
                name: Some("wizard".to_string()),
                url: Some("https://example.com/mcp".to_string()),
                token: Some("s3cret".to_string()),
                ..AddArgs::default()
            },
        )
        .expect("add");
        assert_eq!(
            server.headers.get("Authorization").map(String::as_str),
            Some("Bearer s3cret")
        );

        // A header the user already wrote wins over the token.
        let mut explicit = blank_config();
        let server = add_to_config(
            &mut explicit,
            &AddArgs {
                name: Some("explicit".to_string()),
                url: Some("https://example.com/mcp".to_string()),
                token: Some("s3cret".to_string()),
                headers: vec![("authorization".to_string(), "Basic abc".to_string())],
                ..AddArgs::default()
            },
        )
        .expect("add");
        assert_eq!(server.headers.len(), 1);
        assert_eq!(
            server.headers.get("authorization").map(String::as_str),
            Some("Basic abc")
        );
    }

    #[test]
    fn remove_and_enable_only_touch_an_existing_entry() {
        let mut config = blank_config();
        add_to_config(&mut config, &stdio_args("fs")).expect("add");

        set_enabled_in_config(&mut config, "fs", false).expect("disable");
        assert!(!config.servers[0].enabled);
        set_enabled_in_config(&mut config, "fs", false).expect("disable again is idempotent");
        assert!(!config.servers[0].enabled);
        set_enabled_in_config(&mut config, "fs", true).expect("enable");
        assert!(config.servers[0].enabled);

        let missing =
            set_enabled_in_config(&mut config, "ghost", true).expect_err("unknown server");
        assert!(missing.contains("no MCP server named `ghost`"), "{missing}");
        let missing = remove_from_config(&mut config, "ghost").expect_err("unknown server");
        assert!(missing.contains("no MCP server named `ghost`"), "{missing}");

        remove_from_config(&mut config, "fs").expect("remove");
        assert!(config.servers.is_empty());
        let missing = remove_from_config(&mut config, "fs").expect_err("removed twice");
        assert!(missing.contains("no MCP server named `fs`"), "{missing}");
    }

    #[test]
    fn trust_edits_the_server_or_one_tool() {
        let mut config = blank_config();
        add_to_config(&mut config, &stdio_args("fs")).expect("add");

        set_trust_in_config(&mut config, "fs", "*", TrustLevel::Allow).expect("server trust");
        assert_eq!(config.servers[0].trust, "allow");
        assert!(config.servers[0].tools.is_empty());

        set_trust_in_config(&mut config, "fs", "write_file", TrustLevel::Block)
            .expect("tool trust");
        assert_eq!(
            config.servers[0]
                .tools
                .get("write_file")
                .map(String::as_str),
            Some("block")
        );
        assert_eq!(config.servers[0].trust, "allow", "the server level stays");

        set_trust_in_config(&mut config, "fs", "write_file", TrustLevel::Block)
            .expect("the same level again is idempotent");
        assert_eq!(config.servers[0].tools.len(), 1);

        let missing =
            set_trust_in_config(&mut config, "ghost", "*", TrustLevel::Ask).expect_err("unknown");
        assert!(missing.contains("no MCP server named `ghost`"), "{missing}");
        let empty =
            set_trust_in_config(&mut config, "fs", "", TrustLevel::Ask).expect_err("empty tool");
        assert!(empty.contains("tool name"), "{empty}");

        assert_eq!(server_names(&config), vec!["fs".to_string()]);
    }

    /// A raw `config.yaml` document with unknown keys at every level: an
    /// extension key inside `mcp:`, a hand-added key on each of the two
    /// servers, and a section outside `mcp:` that must stay byte-identical.
    const RAW_DOCUMENT: &str = "\
model: glm-4.7
mcp:
  enabled: true
  request_timeout_ms: 30000
  my_notes: hand-written note
  servers:
  - name: fs
    transport: stdio
    command: npx
    startup_grace_ms: 1500
  - name: gh
    transport: http
    url: https://example.com/mcp
    note: leave me alone
other_section:
  weight: 3
";

    fn raw_document() -> serde_yaml::Value {
        serde_yaml::from_str(RAW_DOCUMENT).expect("the fixture must parse")
    }

    /// The typed config as the shell holds it in memory: the `mcp:` section,
    /// deserialized (unknown keys are ignored, exactly like `ConfigModel`).
    fn typed_from(document: &serde_yaml::Value) -> McpConfig {
        serde_yaml::from_value(
            document
                .get("mcp")
                .cloned()
                .expect("the fixture must have an `mcp` section"),
        )
        .expect("the fixture must hold an mcp config")
    }

    fn mcp_mapping(document: &serde_yaml::Value) -> &serde_yaml::Mapping {
        document
            .get("mcp")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("`mcp` is a mapping")
    }

    fn root_mapping(document: &serde_yaml::Value) -> &serde_yaml::Mapping {
        document
            .as_mapping()
            .expect("the document root is a mapping")
    }

    fn mapping_keys(value: &serde_yaml::Mapping) -> Vec<String> {
        value
            .keys()
            .map(|key| key.as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn server_names_in(document: &serde_yaml::Value) -> Vec<String> {
        mcp_mapping(document)
            .get("servers")
            .and_then(serde_yaml::Value::as_sequence)
            .expect("`servers` is a sequence")
            .iter()
            .map(|entry| {
                entry
                    .get("name")
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    fn server_in(document: &serde_yaml::Value, name: &str) -> serde_yaml::Value {
        mcp_mapping(document)
            .get("servers")
            .and_then(serde_yaml::Value::as_sequence)
            .expect("`servers` is a sequence")
            .iter()
            .find(|entry| entry.get("name").and_then(serde_yaml::Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("server `{name}` is missing from the document"))
            .clone()
    }

    fn field<'a>(entry: &'a serde_yaml::Value, key: &str) -> &'a serde_yaml::Value {
        entry
            .get(key)
            .unwrap_or_else(|| panic!("key `{key}` is missing"))
    }

    #[test]
    fn disable_keeps_unknown_keys_and_key_order() {
        // `/mcp disable fs` must rewrite the file without touching a single key
        // the typed model does not own: `mcp.my_notes`, the hand-added
        // `startup_grace_ms` on `fs`, the `note` on the untouched server `gh`,
        // and everything outside `mcp:`.
        let mut document = raw_document();
        let mut config = typed_from(&document);
        set_enabled_in_config(&mut config, "fs", false).expect("disable");

        merge_into_document(&mut document, &config).expect("merge");

        assert_eq!(
            mcp_mapping(&document)
                .get("my_notes")
                .and_then(serde_yaml::Value::as_str),
            Some("hand-written note"),
            "an unknown key inside `mcp:` must survive"
        );
        assert_eq!(
            mcp_mapping(&document)
                .get("enabled")
                .and_then(serde_yaml::Value::as_bool),
            Some(true),
            "disabling one server must not flip the master switch"
        );
        assert_eq!(
            server_in(&document, "fs")
                .get("startup_grace_ms")
                .and_then(serde_yaml::Value::as_u64),
            Some(1500),
            "an unknown key inside an edited server entry must survive"
        );
        assert_eq!(
            server_in(&document, "fs")
                .get("enabled")
                .and_then(serde_yaml::Value::as_bool),
            Some(false),
            "the edit itself must land"
        );
        let gh = server_in(&document, "gh");
        assert_eq!(
            gh.get("note").and_then(serde_yaml::Value::as_str),
            Some("leave me alone"),
            "a server that was not edited keeps its unknown key"
        );
        assert_eq!(field(&gh, "url").as_str(), Some("https://example.com/mcp"));

        // Key order: untouched parts keep theirs, edited entries keep theirs
        // too (the typed keys that already existed are overwritten in place).
        assert_eq!(
            mapping_keys(root_mapping(&document)),
            vec!["model", "mcp", "other_section"]
        );
        assert_eq!(
            mapping_keys(mcp_mapping(&document)),
            vec!["enabled", "request_timeout_ms", "my_notes", "servers"]
        );
        let fs_keys = mapping_keys(
            server_in(&document, "fs")
                .as_mapping()
                .expect("a server entry is a mapping"),
        );
        assert_eq!(
            fs_keys[..4].to_vec(),
            vec!["name", "transport", "command", "startup_grace_ms"],
            "the existing keys keep their order in an edited entry; got {fs_keys:?}"
        );

        // The keys outside `mcp:` are byte-identical after re-serialization.
        let text = serde_yaml::to_string(&document).expect("serialize");
        assert!(
            text.contains("model: glm-4.7\n"),
            "the root document must survive; got:\n{text}"
        );
        assert!(
            text.contains("other_section:\n  weight: 3\n"),
            "keys outside `mcp:` must stay byte-identical; got:\n{text}"
        );

        // And the document still loads as a typed config with the change.
        let reloaded = typed_from(&document);
        assert!(!reloaded.servers[0].enabled);
        assert_eq!(reloaded.servers[1].name, "gh");
    }

    #[test]
    fn trust_remove_and_add_round_trip_keeps_every_unknown_key() {
        let mut document = raw_document();
        let mut config = typed_from(&document);

        // `trust gh search allow`: a new key inside an entry that has unknown
        // keys of its own.
        set_trust_in_config(&mut config, "gh", "search", TrustLevel::Allow).expect("trust");
        merge_into_document(&mut document, &config).expect("merge");
        let gh = server_in(&document, "gh");
        assert_eq!(
            field(field(&gh, "tools"), "search").as_str(),
            Some("allow"),
            "the trust edit must land"
        );
        assert_eq!(
            gh.get("note").and_then(serde_yaml::Value::as_str),
            Some("leave me alone")
        );
        assert_eq!(
            server_in(&document, "fs")
                .get("startup_grace_ms")
                .and_then(serde_yaml::Value::as_u64),
            Some(1500)
        );
        assert_eq!(
            mcp_mapping(&document)
                .get("my_notes")
                .and_then(serde_yaml::Value::as_str),
            Some("hand-written note")
        );

        // `remove fs` + `add sqlite`: the removed entry is gone, the survivor
        // (unknown keys included) is intact, the new server is appended.
        remove_from_config(&mut config, "fs").expect("remove");
        add_to_config(&mut config, &stdio_args("sqlite")).expect("add");
        merge_into_document(&mut document, &config).expect("merge");

        assert_eq!(server_names_in(&document), vec!["gh", "sqlite"]);
        assert!(
            !serde_yaml::to_string(&document)
                .expect("serialize")
                .contains("startup_grace_ms"),
            "the removed server's unknown key must go with it"
        );
        let gh = server_in(&document, "gh");
        assert_eq!(
            gh.get("note").and_then(serde_yaml::Value::as_str),
            Some("leave me alone")
        );
        assert_eq!(
            field(field(&gh, "tools"), "search").as_str(),
            Some("allow"),
            "the earlier trust edit survives the later ones"
        );
        assert_eq!(
            field(&server_in(&document, "sqlite"), "command").as_str(),
            Some("srv"),
            "a newly added server is appended with its fields"
        );
        assert_eq!(
            mcp_mapping(&document)
                .get("my_notes")
                .and_then(serde_yaml::Value::as_str),
            Some("hand-written note"),
            "each edit must preserve the mcp-level extension key"
        );
        assert_eq!(
            mapping_keys(mcp_mapping(&document)),
            vec!["enabled", "request_timeout_ms", "my_notes", "servers"]
        );

        let reloaded = typed_from(&document);
        assert_eq!(reloaded.servers.len(), 2);
        assert_eq!(reloaded.servers[1].name, "sqlite");
    }

    #[test]
    fn merge_updates_the_switches_and_appends_new_servers() {
        let mut document = raw_document();
        let mut config = typed_from(&document);
        config.enabled = false;
        config.request_timeout_ms = 1234;
        add_to_config(&mut config, &stdio_args("added")).expect("add");

        merge_into_document(&mut document, &config).expect("merge");

        assert_eq!(
            mcp_mapping(&document)
                .get("enabled")
                .and_then(serde_yaml::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            mcp_mapping(&document)
                .get("request_timeout_ms")
                .and_then(serde_yaml::Value::as_u64),
            Some(1234)
        );
        assert_eq!(
            server_names_in(&document),
            vec!["fs", "gh", "added"],
            "existing order is kept and genuinely new entries are appended"
        );
    }

    #[test]
    fn merge_creates_or_normalizes_the_mcp_section() {
        let mut config = blank_config();
        add_to_config(&mut config, &stdio_args("fs")).expect("add");

        // No `mcp:` key at all: it is appended, the rest of the file is kept.
        let mut document: serde_yaml::Value =
            serde_yaml::from_str("model: glm-4.7\nother: 1\n").expect("parse");
        merge_into_document(&mut document, &config).expect("merge");
        assert_eq!(server_names_in(&document), vec!["fs"]);
        assert_eq!(
            mapping_keys(root_mapping(&document)),
            vec!["model", "other", "mcp"],
            "an absent section is appended, existing keys keep their order"
        );

        // `mcp:` with no value parses as null and is normalized, not rejected.
        let mut document: serde_yaml::Value = serde_yaml::from_str("mcp:\n").expect("parse");
        merge_into_document(&mut document, &config).expect("merge");
        assert_eq!(server_names_in(&document), vec!["fs"]);
        assert_eq!(
            mcp_mapping(&document)
                .get("servers")
                .and_then(serde_yaml::Value::as_sequence)
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn the_shells_write_path_preserves_unknown_keys_on_disk() {
        // End-to-end over the exact call the shell makes: `update_raw` handing
        // the document to `merge_into_document`, then the file read back.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, RAW_DOCUMENT).expect("write the fixture");

        let mut config = typed_from(&raw_document());
        set_enabled_in_config(&mut config, "gh", false).expect("disable");
        add_to_config(&mut config, &stdio_args("sqlite")).expect("add");

        aish_config::ConfigLoader::update_raw(&path, |document| {
            merge_into_document(document, &config).map_err(aish_core::AishError::Config)
        })
        .expect("write");

        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("my_notes: hand-written note"), "{text}");
        assert!(text.contains("startup_grace_ms: 1500"), "{text}");
        assert!(text.contains("note: leave me alone"), "{text}");
        assert!(text.contains("other_section:"), "{text}");
        assert!(text.contains("sqlite"), "{text}");

        // The typed config still loads, with the edit applied.
        let loaded = aish_config::ConfigLoader::load(Some(&path)).expect("reload");
        let names: Vec<String> = loaded
            .mcp
            .servers
            .iter()
            .map(|server| server.name.clone())
            .collect();
        assert_eq!(names, vec!["fs", "gh", "sqlite"]);
        assert!(!loaded.mcp.servers[1].enabled);
        assert_eq!(
            loaded.mcp.servers[0].max_output_bytes, None,
            "an unmodelled field keeps the file's value (absent here)"
        );
    }

    #[test]
    fn merge_refuses_to_clobber_a_non_mapping_document() {
        let mut document: serde_yaml::Value = serde_yaml::from_str("- a\n- b\n").expect("parse");
        let error = merge_into_document(&mut document, &blank_config()).expect_err("not a mapping");
        assert!(error.contains("not a mapping"), "{error}");
        assert!(
            document.is_sequence(),
            "a rejected merge must leave the document alone"
        );

        let mut document: serde_yaml::Value = serde_yaml::from_str("mcp: [1, 2]\n").expect("parse");
        let error = merge_into_document(&mut document, &blank_config()).expect_err("bad mcp");
        assert!(error.contains("`mcp`"), "{error}");
    }
}
