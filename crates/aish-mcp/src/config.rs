//! Resolution of the `mcp:` config section into runtime settings.
//!
//! The config structs live in [`aish_config`]; this module validates them,
//! expands `${VAR}` references in `env`/`headers`, applies defaults and
//! produces the settings the transports and the manager consume. Nothing here
//! touches the network: validation failures become warnings so one broken
//! server cannot stop the shell from starting.

use std::collections::BTreeMap;
use std::time::Duration;

use aish_config::{McpConfig, McpServerConfig};

use crate::types::{
    SideEffects, TransportKind, TrustLevel, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_REQUEST_TIMEOUT_MS,
};

/// How one server's tools are presented to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposeMode {
    /// One `mcp__<server>` tool; the model discovers tools on demand. This is
    /// the default because it keeps a server's whole catalogue out of the
    /// prompt (see issue #564).
    Gateway,
    /// The gateway tool *plus* one tool per remote tool
    /// (`mcp__<server>_<tool>`) carrying its real schema, once the catalogue is
    /// known. Costs prompt space, saves the model a discovery round trip.
    Tools,
}

impl ExposeMode {
    /// Parse the `expose` field of one server entry. This is the only parser
    /// for the field; the caller (the shell, which owns the exposure switch)
    /// decides what to do with an unknown value.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gateway" | "" => Some(Self::Gateway),
            "tools" | "direct" | "eager" => Some(Self::Tools),
            _ => None,
        }
    }

    /// Canonical string written to the config `expose` field; the inverse of
    /// [`ExposeMode::parse`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Tools => "tools",
        }
    }
}

/// Resolved settings of one server.
#[derive(Debug, Clone)]
pub struct ServerSettings {
    /// Server name, as configured.
    pub name: String,
    /// Transport kind: stdio runs a child process, http speaks Streamable
    /// HTTP.
    pub transport: TransportKind,
    /// Whether the server should be connected.
    pub enabled: bool,
    /// Child command for the stdio transport.
    pub command: Option<String>,
    /// Arguments passed after the child command.
    pub args: Vec<String>,
    /// Extra environment variables overlaid on the inherited allowlist (see
    /// [`ServerSettings::child_env`]).
    pub env: BTreeMap<String, String>,
    /// Working directory of the child process, when configured.
    pub cwd: Option<String>,
    /// Endpoint of an `http` server.
    pub url: Option<String>,
    /// HTTP headers sent with every request.
    pub headers: BTreeMap<String, String>,
    /// `None` disables the client-side deadline (config value `0`).
    pub request_timeout: Option<Duration>,
    /// Server-level default trust level.
    pub trust: TrustLevel,
    /// Side effects the user declared for the server's tools.
    pub side_effects: SideEffects,
    /// Per-tool trust overrides, keyed by tool name.
    pub tool_trust: BTreeMap<String, TrustLevel>,
    /// Cap on the bytes one tool call may return before truncation.
    pub max_output_bytes: usize,
}

impl ServerSettings {
    /// Per-tool trust override, falling back to the server-level value.
    pub fn effective_trust(&self, tool: &str) -> TrustLevel {
        self.tool_trust.get(tool).copied().unwrap_or(self.trust)
    }

    /// Effective request deadline for this server.
    pub fn effective_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    /// Endpoint label for status output: stdio argv or a redacted URL. For
    /// stdio the joined argv runs through
    /// [`crate::redact::sanitize_diagnostic_text`], which masks URL userinfo
    /// and query values, `token=...`/`Authorization: Bearer ...` assignments,
    /// secret-looking `--flag` values and detected secret patterns, because
    /// `${VAR}` references are expanded before this point; the result is then
    /// shortened. For HTTP only the URL is rendered, with userinfo and query
    /// values masked by [`crate::redact::redact_url_for_log`]. Env and header
    /// values are never part of it.
    pub fn endpoint_label(&self) -> String {
        match self.transport {
            TransportKind::Stdio => {
                let mut parts = Vec::new();
                if let Some(command) = &self.command {
                    parts.push(command.clone());
                }
                parts.extend(self.args.iter().cloned());
                let argv = crate::redact::sanitize_diagnostic_text(&parts.join(" "));
                shorten(&argv, 120)
            }
            TransportKind::Http => {
                crate::redact::redact_url_for_log(self.url.as_deref().unwrap_or_default())
            }
        }
    }

    /// Environment for the child process.
    ///
    /// Only a small allowlist of the parent environment is inherited — the
    /// variables a launcher needs to find toolchains, its caches and the
    /// locale — plus the server's configured `env` overlay, which wins. The
    /// full parent environment is deliberately *not* passed: an MCP server is
    /// untrusted by default, and handing it `AISH_API_KEY`, cloud credentials
    /// or `SSH_AUTH_SOCK` would contradict that. Anything else a server needs
    /// must be declared explicitly in its `env:` block.
    pub fn child_env(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = std::env::vars()
            .filter(|(key, _)| is_inherited_env(key))
            .collect();
        for (key, value) in &self.env {
            env.insert(key.clone(), value.clone());
        }
        env
    }
}

/// Parent variables handed to an MCP child process (see
/// [`ServerSettings::child_env`]).
///
/// Mirrors the allowlist the reference MCP SDKs use (`HOME`, `LOGNAME`, `PATH`,
/// `SHELL`, `TERM`, `USER`) plus the locale, temp-directory, TLS-trust and
/// XDG base-directory variables that package launchers (`uvx`, `npx`) and
/// locale-aware programs need to behave the way they do in the user's shell.
const INHERITED_ENV_VARS: [&str; 12] = [
    "HOME",
    "LOGNAME",
    "PATH",
    "SHELL",
    "TERM",
    "USER",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// True when `key` may be inherited by an MCP child process.
fn is_inherited_env(key: &str) -> bool {
    // `XDG_*` are base *directories* (cache/config/state), never credentials,
    // and `uv`/`npx` resolve their caches through them.
    INHERITED_ENV_VARS.contains(&key) || key.starts_with("XDG_")
}

/// Resolved `mcp:` section.
#[derive(Debug, Clone)]
pub struct McpSettings {
    /// Whether MCP is enabled at all.
    pub enabled: bool,
    /// Default per-request deadline for servers that do not set their own;
    /// `None` disables client-side deadlines.
    pub request_timeout: Option<Duration>,
    /// Default cap on the bytes one tool call may return before truncation.
    pub max_output_bytes: usize,
    /// Resolved per-server settings, in configuration order.
    pub servers: Vec<ServerSettings>,
}

impl McpSettings {
    /// No MCP work at all: disabled, or nothing enabled to connect.
    pub fn is_inactive(&self) -> bool {
        !self.enabled || !self.servers.iter().any(|server| server.enabled)
    }

    /// Servers in configuration order that are enabled.
    pub fn enabled_servers(&self) -> impl Iterator<Item = &ServerSettings> {
        self.servers.iter().filter(|server| server.enabled)
    }

    /// Settings of the named server, when it is configured.
    pub fn server(&self, name: &str) -> Option<&ServerSettings> {
        self.servers.iter().find(|server| server.name == name)
    }
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            request_timeout: Some(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS)),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            servers: Vec::new(),
        }
    }
}

fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut kept: String = text.chars().take(max_chars).collect();
    kept.push_str(" ...");
    kept
}

/// Expand `${VAR}` references from the process environment.
///
/// An unset variable expands to an empty string and is reported as a warning:
/// silently keeping the literal would send `${GITHUB_TOKEN}` to the server as
/// a credential.
fn expand_env_refs(value: &str, context: &str, warnings: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(expanded) => out.push_str(&expanded),
                    Err(_) => {
                        warnings.push(format!(
                            "{context}: environment variable `{name}` is not set"
                        ));
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn expand_map(
    source: &BTreeMap<String, String>,
    context: &str,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    source
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                expand_env_refs(value, &format!("{context} (key `{key}`)"), warnings),
            )
        })
        .collect()
}

/// Validate and resolve one server entry. Returns `None` (with a warning) when
/// the entry cannot be used at all.
fn resolve_server(
    raw: &McpServerConfig,
    default_timeout: Option<Duration>,
    default_max_output_bytes: usize,
    warnings: &mut Vec<String>,
) -> Option<ServerSettings> {
    let name = raw.name.trim().to_string();
    if name.is_empty() {
        warnings.push("mcp: a server entry has an empty name and was ignored".to_string());
        return None;
    }

    let transport = match raw.transport.trim().to_ascii_lowercase().as_str() {
        "" | "stdio" => TransportKind::Stdio,
        "http" | "streamable-http" => TransportKind::Http,
        other => {
            warnings.push(format!(
                "mcp: server `{name}` has unsupported transport `{other}` (expected `stdio` or `http`) and was ignored"
            ));
            return None;
        }
    };

    let command = raw
        .command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let url = raw
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    match transport {
        TransportKind::Stdio => {
            if command.is_none() {
                warnings.push(format!(
                    "mcp: server `{name}` uses the stdio transport but has no `command` and was ignored"
                ));
                return None;
            }
            if url.is_some() {
                warnings.push(format!(
                    "mcp: server `{name}` sets both `command` and `url`; `command` wins for the stdio transport"
                ));
            }
        }
        TransportKind::Http => {
            if url.is_none() {
                warnings.push(format!(
                    "mcp: server `{name}` uses the http transport but has no `url` and was ignored"
                ));
                return None;
            }
            if command.is_some() {
                warnings.push(format!(
                    "mcp: server `{name}` sets both `command` and `url`; `url` wins for the http transport"
                ));
            }
        }
    }

    let trust = match TrustLevel::parse(&raw.trust) {
        Some(parsed) => parsed,
        None => {
            if !raw.trust.trim().is_empty() {
                warnings.push(format!(
                    "mcp: server `{name}` has unknown trust `{}`; using `ask`",
                    raw.trust
                ));
            }
            TrustLevel::Ask
        }
    };

    let side_effects = match SideEffects::parse(&raw.side_effects) {
        Some(parsed) => parsed,
        None => {
            if !raw.side_effects.trim().is_empty() {
                warnings.push(format!(
                    "mcp: server `{name}` has unknown side_effects `{}`; using `unknown`",
                    raw.side_effects
                ));
            }
            SideEffects::Unknown
        }
    };

    let mut tool_trust = BTreeMap::new();
    for (tool, value) in &raw.tools {
        match TrustLevel::parse(value) {
            Some(level) => {
                tool_trust.insert(tool.clone(), level);
            }
            None => warnings.push(format!(
                "mcp: server `{name}` tool `{tool}` has unknown trust `{value}` and was ignored"
            )),
        }
    }

    let request_timeout = match raw.request_timeout_ms {
        Some(0) => None,
        Some(ms) => Some(Duration::from_millis(ms)),
        None => default_timeout,
    };

    let max_output_bytes = raw
        .max_output_bytes
        .filter(|value| *value > 0)
        .unwrap_or(default_max_output_bytes);

    let context = format!("mcp server `{name}`");
    Some(ServerSettings {
        name,
        transport,
        enabled: raw.enabled,
        command,
        args: raw
            .args
            .iter()
            .map(|arg| expand_env_refs(arg, &context, warnings))
            .collect(),
        env: expand_map(&raw.env, &context, warnings),
        cwd: raw
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        url,
        headers: expand_map(&raw.headers, &context, warnings),
        request_timeout,
        trust,
        side_effects,
        tool_trust,
        max_output_bytes,
    })
}

/// Resolve the `mcp:` config section. The second value holds human-readable
/// warnings for entries that were ignored or adjusted; the shell shows them
/// once at startup.
pub fn resolve_settings(config: &McpConfig) -> (McpSettings, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    let default_timeout = match config.request_timeout_ms {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    };
    let max_output_bytes = if config.max_output_bytes == 0 {
        DEFAULT_MAX_OUTPUT_BYTES
    } else {
        config.max_output_bytes
    };

    let mut servers: Vec<ServerSettings> = Vec::new();
    for raw in &config.servers {
        let Some(server) = resolve_server(raw, default_timeout, max_output_bytes, &mut warnings)
        else {
            continue;
        };
        if servers.iter().any(|existing| existing.name == server.name) {
            warnings.push(format!(
                "mcp: duplicate server name `{}` ignored (names must be unique)",
                server.name
            ));
            continue;
        }
        servers.push(server);
    }

    (
        McpSettings {
            enabled: config.enabled,
            request_timeout: default_timeout,
            max_output_bytes,
            servers,
        },
        warnings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An MCP server is untrusted by default, so it must not receive the
    /// parent's credentials just because it was started from within aish: only
    /// the launcher/locale allowlist plus the configured overlay may cross the
    /// process boundary.
    #[test]
    fn child_env_keeps_the_allowlist_and_the_overlay_only() {
        // SAFETY: this test only reads these variables through `std::env::vars`
        // inside `child_env`, and it removes them again at the end.
        std::env::set_var("AISH_TEST_SECRET_TOKEN", "leak-me");
        std::env::set_var("SSH_AUTH_SOCK", "/tmp/agent.sock");

        let mut entry = raw("env", "stdio");
        entry.command = Some("/bin/true".to_string());
        entry
            .env
            .insert("DECLARED".to_string(), "value".to_string());
        let config = McpConfig {
            enabled: true,
            servers: vec![entry],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        assert!(warnings.is_empty(), "{warnings:?}");
        let server = settings.server("env").expect("resolved");

        let env = server.child_env();
        assert!(
            !env.contains_key("AISH_TEST_SECRET_TOKEN"),
            "an undeclared parent variable must not reach the child"
        );
        assert!(!env.contains_key("SSH_AUTH_SOCK"));
        assert_eq!(env.get("DECLARED").map(String::as_str), Some("value"));
        if let Ok(path) = std::env::var("PATH") {
            assert_eq!(env.get("PATH"), Some(&path), "launchers need PATH");
        }

        // The configured overlay wins over the inherited value.
        let mut override_entry = raw("env2", "stdio");
        override_entry.command = Some("/bin/true".to_string());
        override_entry
            .env
            .insert("PATH".to_string(), "/custom/bin".to_string());
        let config = McpConfig {
            enabled: true,
            servers: vec![override_entry],
            ..McpConfig::default()
        };
        let (settings, _) = resolve_settings(&config);
        assert_eq!(
            settings
                .server("env2")
                .expect("resolved")
                .child_env()
                .get("PATH")
                .map(String::as_str),
            Some("/custom/bin")
        );

        std::env::remove_var("AISH_TEST_SECRET_TOKEN");
        std::env::remove_var("SSH_AUTH_SOCK");
    }

    fn raw(name: &str, transport: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: transport.to_string(),
            enabled: true,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            request_timeout_ms: None,
            trust: String::new(),
            side_effects: String::new(),
            tools: BTreeMap::new(),
            max_output_bytes: None,
            expose: String::new(),
        }
    }

    #[test]
    fn parses_expose_mode_aliases() {
        assert_eq!(ExposeMode::parse(""), Some(ExposeMode::Gateway));
        assert_eq!(ExposeMode::parse(" Gateway "), Some(ExposeMode::Gateway));
        assert_eq!(ExposeMode::parse("tools"), Some(ExposeMode::Tools));
        assert_eq!(ExposeMode::parse("direct"), Some(ExposeMode::Tools));
        assert_eq!(ExposeMode::parse("EAGER"), Some(ExposeMode::Tools));
        assert_eq!(ExposeMode::parse("toolz"), None);
        assert_eq!(ExposeMode::Tools.as_str(), "tools");
        assert_eq!(ExposeMode::Gateway.as_str(), "gateway");
    }

    #[test]
    fn ignores_stdio_without_command() {
        let config = McpConfig {
            enabled: true,
            servers: vec![raw("broken", "stdio")],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        assert!(settings.servers.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no `command`"));
    }

    #[test]
    fn resolves_http_server_defaults() {
        let mut entry = raw("github", "http");
        entry.url = Some("https://api.example.com/mcp?token=abc".to_string());
        let config = McpConfig {
            enabled: true,
            servers: vec![entry],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        assert!(warnings.is_empty(), "{warnings:?}");
        let server = &settings.servers[0];
        assert_eq!(server.transport, TransportKind::Http);
        assert_eq!(server.trust, TrustLevel::Ask);
        assert_eq!(server.side_effects, SideEffects::Unknown);
        assert_eq!(server.effective_trust("anything"), TrustLevel::Ask);
        assert_eq!(
            server.effective_timeout(),
            Some(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS))
        );
        assert_eq!(
            server.endpoint_label(),
            "https://api.example.com/mcp?token=[redacted]"
        );
    }

    #[test]
    fn stdio_endpoint_label_masks_flag_credentials() {
        let mut entry = raw("fs", "stdio");
        entry.command = Some("npx".to_string());
        entry.args = vec![
            "--token".to_string(),
            "s3cret".to_string(),
            "--verbose".to_string(),
        ];
        let config = McpConfig {
            enabled: true,
            servers: vec![entry],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        assert!(warnings.is_empty(), "{warnings:?}");
        let label = settings.servers[0].endpoint_label();
        assert!(!label.contains("s3cret"), "{label}");
        assert!(label.contains("--verbose"), "{label}");
    }

    #[test]
    fn honours_zero_timeout_override() {
        let mut entry = raw("fs", "stdio");
        entry.command = Some("npx".to_string());
        entry.request_timeout_ms = Some(0);
        let config = McpConfig {
            enabled: true,
            servers: vec![entry],
            ..McpConfig::default()
        };
        let (settings, _) = resolve_settings(&config);
        assert_eq!(settings.servers[0].effective_timeout(), None);
    }

    #[test]
    fn rejects_duplicate_and_unknown_transport() {
        let config = McpConfig {
            enabled: true,
            servers: vec![raw("dup", "sse"), {
                let mut entry = raw("dup", "stdio");
                entry.command = Some("x".to_string());
                entry
            }],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        assert_eq!(settings.servers.len(), 1);
        assert!(warnings.iter().any(|w| w.contains("unsupported transport")));
    }

    #[test]
    fn per_tool_trust_overrides_server_trust() {
        let mut entry = raw("fs", "stdio");
        entry.command = Some("srv".to_string());
        entry.trust = "allow".to_string();
        entry.side_effects = "read".to_string();
        entry
            .tools
            .insert("write_file".to_string(), "block".to_string());
        let config = McpConfig {
            enabled: true,
            servers: vec![entry],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        assert!(warnings.is_empty(), "{warnings:?}");
        let server = &settings.servers[0];
        assert_eq!(server.effective_trust("read_file"), TrustLevel::Allow);
        assert_eq!(server.effective_trust("write_file"), TrustLevel::Block);
    }

    #[test]
    fn expands_env_references_and_warns_when_unset() {
        std::env::set_var("AISH_MCP_TEST_TOKEN", "s3cret");
        let mut entry = raw("gh", "http");
        entry.url = Some("https://example.com".to_string());
        entry.headers.insert(
            "Authorization".to_string(),
            "Bearer ${AISH_MCP_TEST_TOKEN}".to_string(),
        );
        entry.headers.insert(
            "X-Missing".to_string(),
            "${AISH_MCP_TEST_MISSING}".to_string(),
        );
        let config = McpConfig {
            enabled: true,
            servers: vec![entry],
            ..McpConfig::default()
        };
        let (settings, warnings) = resolve_settings(&config);
        let headers = &settings.servers[0].headers;
        assert_eq!(headers["Authorization"], "Bearer s3cret");
        assert_eq!(headers["X-Missing"], "");
        assert!(warnings.iter().any(|w| w.contains("AISH_MCP_TEST_MISSING")));
        std::env::remove_var("AISH_MCP_TEST_TOKEN");
    }
}
