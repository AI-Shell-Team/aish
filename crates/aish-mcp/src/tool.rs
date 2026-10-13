//! The `Tool` the LLM sees for each MCP server.
//!
//! One gateway tool per server exposes `list` / `describe` / `call` (see
//! [`crate::prompt`]) so the server's whole tool catalogue never lands in the
//! system prompt. The tool name encodes the server (`mcp__<server>`), which is
//! how the model routes a call back to the right server.
//!
//! Safety: every `call` goes through the normal aish approval path
//! ([`Tool::preflight_with_context`]). A call is auto-approved only when the
//! user declared the server read-only *and* trusted it, and the server's own
//! annotations do not widen that to a write or network scope; an approval is
//! bound to one exact argument set ([`McpGatewayTool::approval_memory_key`]),
//! so changing any argument forces a new approval.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use aish_llm::{
    LlmSession, PreflightResult, PreflightSecurityContext, SecurityPanelMode, Tool, ToolContext,
    ToolResult,
};
use aish_security::{MatchedRuleSummary, RiskLevel, SecurityAnalysis, SecurityDecision};

use crate::cancel::CancelListener;
use crate::config::ServerSettings;
use crate::manager::{McpManager, McpRuntime};
use crate::prompt;
use crate::trust::{self, TrustDecision};
use crate::types::{
    args_summary, render_tool_result, truncate_utf8, McpError, McpFailureClass, McpToolDefinition,
    ServerSnapshot, SideEffects, ToolCallResult,
};

/// Prefix every gateway tool name carries, so a name identifies its origin.
pub const GATEWAY_TOOL_PREFIX: &str = "mcp__";

/// Hex digits of the full-name hash kept when a minted name overflows.
const NAME_HASH_HEX_LEN: usize = 8;

/// Cap on tool lines returned by `list`.
const LIST_MAX_LINES: usize = 200;

/// Byte cap on the `describe` output (schema included).
const DESCRIBE_MAX_BYTES: usize = 8000;

/// Longest approval-detail summary shown for a `call`.
const CONFIRM_ARGS_SUMMARY_LEN: usize = 300;

/// Deterministic gateway tool name for a server: `mcp__` + the sanitized server
/// name, capped at the provider limit.
pub fn gateway_tool_name(server: &str) -> String {
    let sanitized = sanitize_part(server, "server");
    cap_tool_name(format!("{GATEWAY_TOOL_PREFIX}{sanitized}"))
}

/// Lower-case, replace every run of characters outside `[a-z0-9_]` with a
/// single `_`, trim leading/trailing `_`, and fall back when nothing remains.
fn sanitize_part(value: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        let lower = character.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            out.push(lower);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Cap a minted name at the provider limit: keep a readable prefix and append a
/// deterministic hash of the full name so distinct long names stay distinct and
/// the same name is stable across processes.
fn cap_tool_name(name: String) -> String {
    let max = crate::types::MAX_TOOL_NAME_LEN;
    if name.len() <= max {
        return name;
    }
    let hash = sha256_hex(name.as_bytes());
    let keep = max - NAME_HASH_HEX_LEN - 1;
    format!("{}_{}", &name[..keep], &hash[..NAME_HASH_HEX_LEN])
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Gateway tools for every enabled server of a running manager.
pub fn gateway_tools(manager: &Arc<McpManager>) -> Vec<Box<dyn aish_llm::Tool>> {
    let runtime: Arc<dyn McpRuntime> = manager.clone();
    let servers: Vec<ServerSettings> = manager.settings().enabled_servers().cloned().collect();
    gateway_tools_with(runtime, &servers)
}

/// Gateway tools for an explicit runtime and set of servers, in the given
/// order.
///
/// Two servers may sanitize to the same tool name; the first one keeps the
/// name and every later collision is dropped with a warning, because the model
/// can only call the name one server can own.
pub fn gateway_tools_with(
    runtime: Arc<dyn McpRuntime>,
    servers: &[ServerSettings],
) -> Vec<Box<dyn aish_llm::Tool>> {
    let mut tools: Vec<Box<dyn aish_llm::Tool>> = Vec::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for settings in servers.iter().filter(|settings| settings.enabled) {
        let name = gateway_tool_name(&settings.name);
        if !claimed.insert(name.clone()) {
            tracing::warn!(
                server = %settings.name,
                tool = %name,
                "MCP gateway tool name collision; keeping the first server"
            );
            continue;
        }
        tools.push(Box::new(McpGatewayTool::new(
            runtime.clone(),
            settings.clone(),
        )));
    }
    tools
}

/// The gateway `Tool` bound to one MCP server.
pub struct McpGatewayTool {
    runtime: Arc<dyn McpRuntime>,
    settings: ServerSettings,
    name: String,
    description: String,
    /// Append-only cache of rendered prompt texts (see [`Tool::prompt`]).
    prompt_cache: parking_lot::Mutex<PromptCache>,
}

/// Every prompt text this tool has rendered.
///
/// Entries are only ever appended: an existing version is never removed or
/// rewritten, so a `&str` handed out by [`Tool::prompt`] stays valid for the
/// life of the tool no matter how many times the tool list changes. Identical
/// renders are interned once, so only *distinct* texts accumulate, and each
/// render is size-capped by [`prompt::tool_list_prompt`].
#[derive(Default)]
struct PromptCache {
    versions: Vec<Box<str>>,
    /// Index of every interned text, so a catalogue that flips back and forth
    /// does not append a new copy.
    indexes: std::collections::HashMap<String, usize>,
}

impl PromptCache {
    /// Index of the version equal to `text`, appending it when it is new.
    fn intern(&mut self, text: &str) -> usize {
        if let Some(index) = self.indexes.get(text) {
            return *index;
        }
        self.versions.push(text.into());
        let index = self.versions.len() - 1;
        self.indexes.insert(text.to_string(), index);
        index
    }
}

impl McpGatewayTool {
    /// Build the gateway tool of one server; the tool name
    /// (`mcp__<server>`) and description are derived from the settings.
    pub fn new(runtime: Arc<dyn McpRuntime>, settings: ServerSettings) -> Self {
        let name = gateway_tool_name(&settings.name);
        let description = prompt::DESCRIPTION
            .replace("{server}", &settings.name)
            .replace("{transport}", settings.transport.as_str());
        Self {
            runtime,
            settings,
            name,
            description,
            prompt_cache: parking_lot::Mutex::new(PromptCache::default()),
        }
    }

    /// Current snapshot of this tool's server, when the manager knows it.
    fn snapshot(&self) -> Option<ServerSnapshot> {
        self.runtime
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.name == self.settings.name)
    }
}

impl Tool for McpGatewayTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        prompt::gateway_parameters()
    }

    /// The system-prompt fragment for this gateway: the server's current status
    /// and tool catalogue.
    ///
    /// The trait returns a reference borrowed from `&self`, but the text
    /// changes whenever the server's tool list changes. Rendered versions are
    /// therefore interned in an append-only cache: a version is written once
    /// and never removed, so the returned reference stays valid however many
    /// times the catalogue changes, without leaking anything for the life of
    /// the process.
    fn prompt(&self) -> &str {
        let (status_label, tools) = match self.snapshot() {
            Some(snapshot) => (snapshot.status.label().to_string(), snapshot.tools),
            None => ("not configured".to_string(), Vec::new()),
        };
        // `ServerSnapshot` reports only whether the server sent instructions,
        // not their text, so the prompt cannot embed them.
        let text = prompt::tool_list_prompt(&self.settings.name, &status_label, &tools, None);

        let mut cache = self.prompt_cache.lock();
        let current = cache.intern(&text);
        let pointer: *const str = &*cache.versions[current];
        drop(cache);
        // SAFETY: the cache only ever appends. An interned `Box<str>` keeps its
        // heap buffer when `versions` reallocates, and no entry is removed or
        // rewritten, so the buffer lives until the tool is dropped; the
        // reference borrows `&self`, which cannot outlive the tool.
        unsafe { &*pointer }
    }

    fn preflight_with_context(&self, args: &Value, ctx: &ToolContext<'_>) -> PreflightResult {
        let server = self.settings.name.clone();

        // `list`/`describe` are read-only protocol operations; only `call`
        // executes anything. A missing or unknown action is refused.
        match args.get("action").and_then(Value::as_str) {
            Some("list") | Some("describe") => return PreflightResult::Allow,
            Some("call") => {}
            _ => {
                return PreflightResult::Block {
                    message: aish_i18n::t("shell.mcp.invalid_action"),
                    security: None,
                }
            }
        }

        let Some(tool) = args
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
        else {
            return PreflightResult::Block {
                message: aish_i18n::t("shell.mcp.tool_required"),
                security: None,
            };
        };
        let tool = tool.to_string();

        // Annotations come from the tool cache, which the manager may keep
        // after a `notifications/tools/list_changed` (it only flags it). Both a
        // missing entry and a stale catalogue are *unknown* evidence, not
        // absent evidence: trusting them would fall back to the user's
        // `side_effects: read` declaration and auto-approve a tool whose
        // annotations the server has since widened. Unknown side effects always
        // ask.
        let reported = if self.runtime.tools_are_stale(&server) {
            Some(SideEffects::Unknown)
        } else {
            match self
                .runtime
                .cached_tools(&server)
                .and_then(|tools| tools.into_iter().find(|candidate| candidate.name == tool))
            {
                Some(definition) => trust::reported_side_effects(&definition.annotations),
                None => Some(SideEffects::Unknown),
            }
        };
        trust_preflight(
            &self.name,
            &self.settings,
            &tool,
            reported,
            &call_arguments(args),
            ctx,
        )
    }

    /// Bind an approval to one exact call: server, tool and argument digest.
    fn approval_memory_key(&self, args: &Value) -> Option<String> {
        if args.get("action").and_then(Value::as_str) != Some("call") {
            return None;
        }
        let tool = args
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|tool| !tool.is_empty())?;
        Some(call_memory_key(
            &self.settings.name,
            tool,
            &call_arguments(args),
        ))
    }

    /// The synchronous path cannot reach the MCP transports; the tool must be
    /// executed through [`Tool::execute_async_in_session`].
    fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::error(aish_i18n::t("shell.mcp.needs_async"))
    }

    fn execute_async_in_session<'a>(
        &'a self,
        args: Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let runtime = Arc::clone(&self.runtime);
        let settings = self.settings.clone();
        let cancel = Some(CancelListener::new(session.cancellation_token_arc()));
        Box::pin(async move {
            match args.get("action").and_then(Value::as_str) {
                Some("list") => run_list(runtime.as_ref(), &settings, cancel).await,
                Some("describe") => run_describe(runtime.as_ref(), &settings, &args, cancel).await,
                Some("call") => run_call(runtime.as_ref(), &settings, &args, cancel).await,
                _ => ToolResult::error(aish_i18n::t("shell.mcp.invalid_action")),
            }
        })
    }
}

/// Registry name of the tool that exposes `tool` of `server` directly.
///
/// Mirrors oh-my-pi's mint: sanitize both halves, drop a redundant
/// `"<server>_"` prefix from the tool name (`server`/`server_read_file` must
/// not become `mcp__server_server_read_file`), and cap the result at 64
/// characters with a deterministic hash suffix.
pub fn direct_tool_name(server: &str, tool: &str) -> String {
    let server_part = sanitize_part(server, "server");
    let tool_part = sanitize_part(tool, "tool");
    let redundant = format!("{server_part}_");
    let normalized = tool_part
        .strip_prefix(redundant.as_str())
        .unwrap_or(tool_part.as_str());
    cap_tool_name(format!("{GATEWAY_TOOL_PREFIX}{server_part}_{normalized}"))
}

/// The schema a provider should see for one remote tool.
///
/// Real servers omit `type` or send a draft URI in `$schema`; both upset strict
/// providers, so the shape is normalized instead of passed through verbatim.
fn normalized_schema(schema: Option<&Value>) -> Value {
    let mut value = schema.cloned().unwrap_or_else(|| json!({}));
    match value.as_object_mut() {
        Some(map) => {
            map.remove("$schema");
            map.entry("type").or_insert_with(|| json!("object"));
            value
        }
        None => json!({"type": "object"}),
    }
}

/// One tool per remote tool, for servers configured with `expose: tools`.
///
/// The catalogue must already be known (the shell waits for it, or calls this
/// again after `/mcp reload`); an unknown server yields no tools rather than a
/// placeholder the model cannot use.
pub fn direct_tools(
    runtime: Arc<dyn McpRuntime>,
    settings: &ServerSettings,
    tools: &[McpToolDefinition],
) -> Vec<Box<dyn aish_llm::Tool>> {
    tools
        .iter()
        .map(|definition| -> Box<dyn aish_llm::Tool> {
            Box::new(McpDirectTool::new(
                Arc::clone(&runtime),
                settings.clone(),
                definition.clone(),
            ))
        })
        .collect()
}

/// A remote MCP tool exposed directly, with its real schema in the request.
///
/// Used when the server opts into `expose: tools`: the model sees the tool's
/// parameters up front instead of going through the gateway's
/// `list`/`describe`/`call` discovery. Trust, approval binding, rendering and
/// failure classification are identical to the gateway path.
pub struct McpDirectTool {
    runtime: Arc<dyn McpRuntime>,
    settings: ServerSettings,
    definition: McpToolDefinition,
    name: String,
    description: String,
}

impl McpDirectTool {
    /// Build the direct tool of one remote tool; the name is
    /// `mcp__<server>_<tool>` and the description comes from the server's
    /// definition.
    pub fn new(
        runtime: Arc<dyn McpRuntime>,
        settings: ServerSettings,
        definition: McpToolDefinition,
    ) -> Self {
        let name = direct_tool_name(&settings.name, &definition.name);
        let description = match definition.description.as_deref().map(str::trim) {
            Some(description) if !description.is_empty() => format!(
                "{} (MCP server `{}`)",
                first_line(Some(description)),
                settings.name
            ),
            _ => format!(
                "MCP tool `{}` on server `{}`.",
                definition.name, settings.name
            ),
        };
        Self {
            runtime,
            settings,
            definition,
            name,
            description,
        }
    }

    /// Side effects the server currently reports for this tool.
    ///
    /// Policy never uses the registration snapshot: a server can change its
    /// annotations, and both a stale catalogue and a tool that is missing from
    /// the live catalogue (still reconnecting, or dropped by the server) are
    /// *unknown* evidence. Unknown side effects always ask.
    fn reported_side_effects(&self) -> Option<SideEffects> {
        let server = &self.settings.name;
        if self.runtime.tools_are_stale(server) {
            return Some(SideEffects::Unknown);
        }
        match self.runtime.cached_tools(server).and_then(|tools| {
            tools
                .into_iter()
                .find(|candidate| candidate.name == self.definition.name)
        }) {
            Some(live) => trust::reported_side_effects(&live.annotations),
            None => Some(SideEffects::Unknown),
        }
    }
}

impl Tool for McpDirectTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        normalized_schema(self.definition.input_schema.as_ref())
    }

    /// The schema travels with the tool spec, so the prompt appendix stays empty.
    fn prompt(&self) -> &str {
        ""
    }

    fn preflight_with_context(&self, args: &Value, ctx: &ToolContext<'_>) -> PreflightResult {
        trust_preflight(
            &self.name,
            &self.settings,
            &self.definition.name,
            self.reported_side_effects(),
            args,
            ctx,
        )
    }

    /// Bind an approval to one exact call: server, tool and argument digest.
    fn approval_memory_key(&self, args: &Value) -> Option<String> {
        // A direct tool receives the remote tool's arguments verbatim (the
        // gateway unwraps them from its `arguments` field instead).
        Some(call_memory_key(
            &self.settings.name,
            &self.definition.name,
            args,
        ))
    }

    fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::error(aish_i18n::t("shell.mcp.needs_async"))
    }

    fn execute_async_in_session<'a>(
        &'a self,
        args: Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let runtime = Arc::clone(&self.runtime);
        let settings = self.settings.clone();
        let tool = self.definition.name.clone();
        let cancel = Some(CancelListener::new(session.cancellation_token_arc()));
        Box::pin(async move {
            match await_or_cancel(
                &cancel,
                runtime.call(&settings.name, &tool, &args, cancel.clone()),
                &settings.name,
                Some(&tool),
            )
            .await
            {
                Err(cancelled) => cancelled,
                Ok(Err(error)) => error_result(&settings.name, Some(&tool), error, "call"),
                Ok(Ok(result)) => render_call_result(&settings, &tool, &result),
            }
        })
    }
}

/// Shared trust decision for one concrete `tools/call`.
///
/// Both exposure modes end here, so the policy cannot drift: a call is
/// auto-approved only when the user trusted that tool and its effective side
/// effects are `read`, otherwise it asks, and a blocked tool never runs.
fn trust_preflight(
    registry_name: &str,
    settings: &ServerSettings,
    tool: &str,
    reported: Option<SideEffects>,
    arguments: &Value,
    ctx: &ToolContext<'_>,
) -> PreflightResult {
    let server = settings.name.clone();
    let trust_level = settings.effective_trust(tool);
    let effective = trust::combine_side_effects(settings.side_effects, reported);
    let summary = args_summary(arguments, CONFIRM_ARGS_SUMMARY_LEN);

    match trust::evaluate_call_trust(trust_level, effective) {
        TrustDecision::Allow => PreflightResult::Allow,
        TrustDecision::Block => {
            let analysis = build_analysis(
                RiskLevel::High,
                &server,
                tool,
                trust_level,
                settings.side_effects,
                reported,
                &summary,
            );
            let message = aish_i18n::t_with_args(
                "shell.mcp.blocked_message",
                &strings(&[("server", server.as_str()), ("tool", tool)]),
            );
            PreflightResult::Block {
                message: message.clone(),
                security: Some(PreflightSecurityContext {
                    tool_name: registry_name.to_string(),
                    target: Some(format!("{server}/{tool}")),
                    message,
                    mode: SecurityPanelMode::Blocked,
                    decision: Some(SecurityDecision::block(RiskLevel::High, analysis)),
                }),
            }
        }
        TrustDecision::Confirm => {
            // A remembered approval for this exact call relaxes the
            // confirmation. This is checked here, after the block arm, because
            // `trust: block` is the strongest statement a user can make and
            // always wins — which is also why the session-level shortcut in
            // `run_tool_preflight` is skipped for MCP tools.
            if ctx
                .session
                .is_approval_remembered(registry_name, &call_memory_key(&server, tool, arguments))
            {
                return PreflightResult::Allow;
            }
            // The confirm path never escalates to High: read-only calls are
            // Low, anything that might write or reach the network is Medium.
            let risk = if effective == SideEffects::Read {
                RiskLevel::Low
            } else {
                RiskLevel::Medium
            };
            let analysis = build_analysis(
                risk,
                &server,
                tool,
                trust_level,
                settings.side_effects,
                reported,
                &summary,
            );
            let message = aish_i18n::t_with_args(
                "shell.mcp.confirm_message",
                &strings(&[
                    ("server", server.as_str()),
                    ("tool", tool),
                    ("side_effects", effective.as_str()),
                    ("args", summary.as_str()),
                ]),
            );
            PreflightResult::Confirm {
                message: message.clone(),
                security: Some(PreflightSecurityContext {
                    tool_name: registry_name.to_string(),
                    target: Some(format!("{server}/{tool}")),
                    message,
                    mode: SecurityPanelMode::Confirm,
                    decision: Some(SecurityDecision::confirm(risk, analysis)),
                }),
            }
        }
    }
}

/// Build the security analysis shown in the approval/block panel for a `call`.
///
/// The reasons entry is a single multi-line string: the panel surfaces only the
/// first non-internal reason, so packing every detail line into it keeps them
/// all visible (the panel wraps the string line by line).
fn build_analysis(
    risk: RiskLevel,
    server: &str,
    tool: &str,
    trust_level: crate::types::TrustLevel,
    declared: SideEffects,
    reported: Option<SideEffects>,
    summary: &str,
) -> SecurityAnalysis {
    let identity = format!("mcp:{server}:{tool}");
    let reported_label = reported.map(SideEffects::as_str).unwrap_or("none");
    let reasons = vec![format!(
        "{}\n{}\n{}\n{}",
        aish_i18n::t_with_args(
            "shell.mcp.reason_server",
            &strings(&[("server", server), ("tool", tool)]),
        ),
        aish_i18n::t_with_args(
            "shell.mcp.reason_side_effects",
            &strings(&[
                ("declared", declared.as_str()),
                ("reported", reported_label)
            ]),
        ),
        aish_i18n::t_with_args("shell.mcp.reason_arguments", &strings(&[("args", summary)]),),
        aish_i18n::t_with_args(
            "shell.mcp.reason_trust",
            &strings(&[("trust", trust_level.as_str())]),
        ),
    )];

    let mut analysis = SecurityAnalysis::default();
    analysis.risk_level = risk;
    analysis.reasons = reasons;
    analysis.matched_rule = Some(MatchedRuleSummary {
        id: Some(identity.clone()),
        name: Some(tool.to_string()),
        pattern: identity,
    });
    analysis.suggested_alternatives = vec![aish_i18n::t_with_args(
        "shell.mcp.alternative_hint",
        &strings(&[("server", server), ("tool", tool)]),
    )];
    analysis
}

/// `list`: refresh and render the server's tool catalogue, one line per tool.
async fn run_list(
    runtime: &dyn McpRuntime,
    settings: &ServerSettings,
    cancel: Option<Arc<CancelListener>>,
) -> ToolResult {
    let tools = match await_or_cancel(
        &cancel,
        runtime.refresh_tools(&settings.name, cancel.clone()),
        &settings.name,
        None,
    )
    .await
    {
        Ok(Ok(tools)) => tools,
        Ok(Err(error)) => return error_result(&settings.name, None, error, "list"),
        Err(cancelled) => return cancelled,
    };

    if tools.is_empty() {
        return ToolResult {
            ok: true,
            output: "No tools are available from this MCP server.".to_string(),
            meta: Some(json!({"command_status": "ok", "mcp_server": settings.name})),
        };
    }

    let mut lines: Vec<String> = Vec::new();
    for tool in tools.iter().take(LIST_MAX_LINES) {
        let side_effects = trust::combine_side_effects(
            settings.side_effects,
            trust::reported_side_effects(&tool.annotations),
        );
        lines.push(format!(
            "- {}: {} ({})",
            tool.name,
            first_line(tool.description.as_deref()),
            side_effects.as_str()
        ));
    }
    if tools.len() > LIST_MAX_LINES {
        lines.push(format!(
            "...and {} more tools",
            tools.len() - LIST_MAX_LINES
        ));
    }

    ToolResult {
        ok: true,
        output: lines.join("\n"),
        meta: Some(json!({"command_status": "ok", "mcp_server": settings.name})),
    }
}

/// `describe`: refresh and render one tool's name, scope, description and input
/// schema.
async fn run_describe(
    runtime: &dyn McpRuntime,
    settings: &ServerSettings,
    args: &Value,
    cancel: Option<Arc<CancelListener>>,
) -> ToolResult {
    let Some(tool) = args
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
    else {
        return ToolResult::error(aish_i18n::t("shell.mcp.tool_required"));
    };
    let tool = tool.to_string();

    let tools = match await_or_cancel(
        &cancel,
        runtime.refresh_tools(&settings.name, cancel.clone()),
        &settings.name,
        Some(&tool),
    )
    .await
    {
        Ok(Ok(tools)) => tools,
        Ok(Err(error)) => return error_result(&settings.name, Some(&tool), error, "describe"),
        Err(cancelled) => return cancelled,
    };

    let Some(definition) = tools.iter().find(|candidate| candidate.name == tool) else {
        return ToolResult::error(format!(
            "MCP tool \"{tool}\" was not found on server \"{}\". Available tools: {}",
            settings.name,
            tool_names(&tools)
        ));
    };

    let side_effects = trust::combine_side_effects(
        settings.side_effects,
        trust::reported_side_effects(&definition.annotations),
    );
    let schema = definition
        .input_schema
        .as_ref()
        .map(|schema| serde_json::to_string_pretty(schema).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_else(|| "{}".to_string());
    let body = format!(
        "Tool: {}\nDescription: {}\nSide effects: {}\nInput schema:\n{}",
        definition.name,
        definition.description.as_deref().unwrap_or("(none)"),
        side_effects.as_str(),
        schema
    );
    let (mut output, truncated) = truncate_utf8(&body, DESCRIBE_MAX_BYTES);
    if truncated {
        output.push_str("\n...(truncated)");
    }

    ToolResult {
        ok: true,
        output,
        meta: Some(json!({
            "command_status": "ok",
            "mcp_server": settings.name,
            "mcp_tool": tool,
        })),
    }
}

/// `call`: make sure the tool exists, invoke it and render the server's result.
async fn run_call(
    runtime: &dyn McpRuntime,
    settings: &ServerSettings,
    args: &Value,
    cancel: Option<Arc<CancelListener>>,
) -> ToolResult {
    let Some(tool) = args
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
    else {
        return ToolResult::error(aish_i18n::t("shell.mcp.tool_required"));
    };
    let tool = tool.to_string();
    let arguments = match args.get("arguments") {
        Some(value) if !value.is_null() => value.clone(),
        _ => json!({}),
    };

    // The cache is authoritative; a miss triggers one refresh so a tool that
    // appeared after connect is still callable, and a genuine miss is refused
    // with the names the server does expose.
    let cached = runtime
        .cached_tools(&settings.name)
        .is_some_and(|tools| tools.iter().any(|candidate| candidate.name == tool));
    if !cached {
        let tools = match await_or_cancel(
            &cancel,
            runtime.refresh_tools(&settings.name, cancel.clone()),
            &settings.name,
            Some(&tool),
        )
        .await
        {
            Ok(Ok(tools)) => tools,
            Ok(Err(error)) => return error_result(&settings.name, Some(&tool), error, "call"),
            Err(cancelled) => return cancelled,
        };
        if !tools.iter().any(|candidate| candidate.name == tool) {
            return ToolResult::error(format!(
                "MCP tool \"{tool}\" was not found on server \"{}\". Available tools: {}",
                settings.name,
                tool_names(&tools)
            ));
        }
    }

    let result = match await_or_cancel(
        &cancel,
        runtime.call(&settings.name, &tool, &arguments, cancel.clone()),
        &settings.name,
        Some(&tool),
    )
    .await
    {
        Ok(result) => result,
        Err(cancelled) => return cancelled,
    };

    match result {
        Err(error) => error_result(&settings.name, Some(&tool), error, "call"),
        Ok(result) => render_call_result(settings, &tool, &result),
    }
}

/// Render a `tools/call` result: success text, or the server's own error text
/// prefixed so the model can tell a tool-level failure from a transport one.
fn render_call_result(
    settings: &ServerSettings,
    tool: &str,
    result: &ToolCallResult,
) -> ToolResult {
    let rendered = render_tool_result(result, settings.max_output_bytes);
    let meta = |status: &str| {
        Some(json!({
            "command_status": status,
            "mcp_server": settings.name,
            "mcp_tool": tool,
        }))
    };
    if result.is_error {
        ToolResult {
            ok: false,
            output: format!("MCP tool reported an error:\n{rendered}"),
            // The tool ran and the operation failed; that must not trip the
            // tool loop's consecutive-failure fuse.
            meta: meta("exit_nonzero"),
        }
    } else {
        ToolResult {
            ok: true,
            output: rendered,
            meta: meta("ok"),
        }
    }
}

/// Await an MCP operation but let the session's cancellation win. Returns
/// `Err(result)` with the canned cancelled result when cancellation fires first.
async fn await_or_cancel<T>(
    cancel: &Option<Arc<CancelListener>>,
    operation: impl Future<Output = T>,
    server: &str,
    tool: Option<&str>,
) -> Result<T, ToolResult> {
    match cancel {
        Some(listener) => tokio::select! {
            biased;
            _ = listener.cancelled() => Err(cancelled_result(server, tool)),
            value = operation => Ok(value),
        },
        None => Ok(operation.await),
    }
}

fn cancelled_result(server: &str, tool: Option<&str>) -> ToolResult {
    let mut meta = json!({"command_status": "cancelled", "mcp_server": server});
    if let Some(tool) = tool {
        meta["mcp_tool"] = json!(tool);
    }
    let output = match tool {
        Some(tool) => aish_i18n::t_with_args(
            "shell.mcp.call_failed",
            &strings(&[
                ("server", server),
                ("tool", tool),
                ("detail", "cancelled by user"),
            ]),
        ),
        None => format!("MCP request cancelled (server \"{server}\")."),
    };
    ToolResult {
        ok: false,
        output,
        meta: Some(meta),
    }
}

/// Map an [`McpError`] onto a tool result, preserving the failure class in
/// `meta` for the audit trail.
fn error_result(server: &str, tool: Option<&str>, error: McpError, action: &str) -> ToolResult {
    let status = match error.failure {
        McpFailureClass::Timeout => "timeout",
        McpFailureClass::Cancelled => "cancelled",
        _ => "dispatch_failed",
    };
    let mut meta = json!({
        "command_status": status,
        "mcp_server": server,
        "mcp_failure": error.failure.as_str(),
    });
    if let Some(tool) = tool {
        meta["mcp_tool"] = json!(tool);
    }
    let output = match tool {
        Some(tool) => format!(
            "{}\n{}",
            aish_i18n::t_with_args(
                "shell.mcp.call_failed",
                &strings(&[
                    ("server", server),
                    ("tool", tool),
                    ("detail", error.message.as_str()),
                ]),
            ),
            error.describe()
        ),
        None => format!(
            "MCP {action} failed on server \"{server}\": {}",
            error.describe()
        ),
    };
    ToolResult {
        ok: false,
        output,
        meta: Some(meta),
    }
}

/// The call's `arguments` value as it will be sent to the server.
fn call_arguments(args: &Value) -> Value {
    match args.get("arguments") {
        Some(value) if !value.is_null() => value.clone(),
        _ => json!({}),
    }
}

/// Approval-memory key of one exact remote call: server, tool, argument digest.
///
/// One helper for both exposure modes and for the trust preflight, so the key a
/// confirmed call remembers is exactly the key a later identical call looks up,
/// and the digest is always taken over the arguments that reach the server.
fn call_memory_key(server: &str, tool: &str, arguments: &Value) -> String {
    format!("mcp:{server}:{tool}#{}", trust::args_digest(arguments))
}

/// First non-empty line of a description, for one-line listings.
fn first_line(description: Option<&str>) -> String {
    description
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(no description)")
        .to_string()
}

/// Comma-separated tool names, or `(none)` when the server exposes nothing.
fn tool_names(tools: &[McpToolDefinition]) -> String {
    if tools.is_empty() {
        return "(none)".to_string();
    }
    tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the placeholder map for `t_with_args` without repeating boilerplate.
fn strings(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use aish_llm::approval_memory::ApprovalMemory;
    use aish_llm::ToolExecutionPolicy;
    use futures::future::BoxFuture;

    use crate::types::{
        McpContent, ServerStatus, ToolAnnotations, ToolSummary, TransportKind, TrustLevel,
    };

    /// Configurable stand-in for the manager: tests drive the cache, the
    /// refreshed tool list, the call result and the snapshots directly.
    struct FakeRuntime {
        snapshots: parking_lot::Mutex<Vec<ServerSnapshot>>,
        cached: parking_lot::Mutex<Option<Vec<McpToolDefinition>>>,
        refreshed: parking_lot::Mutex<Vec<McpToolDefinition>>,
        call_result: parking_lot::Mutex<Option<Result<ToolCallResult, McpError>>>,
        settings: Option<ServerSettings>,
        refresh_calls: AtomicUsize,
        stale: std::sync::atomic::AtomicBool,
    }

    impl FakeRuntime {
        fn new(settings: ServerSettings) -> Arc<Self> {
            Arc::new(Self {
                snapshots: parking_lot::Mutex::new(Vec::new()),
                cached: parking_lot::Mutex::new(None),
                refreshed: parking_lot::Mutex::new(Vec::new()),
                call_result: parking_lot::Mutex::new(None),
                settings: Some(settings),
                refresh_calls: AtomicUsize::new(0),
                stale: std::sync::atomic::AtomicBool::new(false),
            })
        }

        fn set_stale(&self, stale: bool) {
            self.stale.store(stale, Ordering::SeqCst);
        }

        fn set_cached(&self, tools: Vec<McpToolDefinition>) {
            *self.cached.lock() = Some(tools);
        }

        fn clear_cached(&self) {
            *self.cached.lock() = None;
        }

        fn set_refreshed(&self, tools: Vec<McpToolDefinition>) {
            *self.refreshed.lock() = tools;
        }

        fn set_snapshots(&self, snapshots: Vec<ServerSnapshot>) {
            *self.snapshots.lock() = snapshots;
        }

        fn set_call(&self, result: Result<ToolCallResult, McpError>) {
            *self.call_result.lock() = Some(result);
        }

        fn refresh_count(&self) -> usize {
            self.refresh_calls.load(Ordering::SeqCst)
        }
    }

    impl McpRuntime for FakeRuntime {
        fn snapshots(&self) -> Vec<ServerSnapshot> {
            self.snapshots.lock().clone()
        }

        fn server_settings(&self, server: &str) -> Option<ServerSettings> {
            self.settings
                .as_ref()
                .filter(|settings| settings.name == server)
                .cloned()
        }

        fn cached_tools(&self, _server: &str) -> Option<Vec<McpToolDefinition>> {
            self.cached.lock().clone()
        }

        fn tools_are_stale(&self, _server: &str) -> bool {
            self.stale.load(Ordering::SeqCst)
        }

        fn call<'a>(
            &'a self,
            _server: &'a str,
            _tool: &'a str,
            _args: &'a Value,
            _cancel: Option<Arc<CancelListener>>,
        ) -> BoxFuture<'a, Result<ToolCallResult, McpError>> {
            let result = self.call_result.lock().clone();
            Box::pin(async move { result.unwrap_or_else(|| Ok(ToolCallResult::default())) })
        }

        fn refresh_tools<'a>(
            &'a self,
            _server: &'a str,
            _cancel: Option<Arc<CancelListener>>,
        ) -> BoxFuture<'a, Result<Vec<McpToolDefinition>, McpError>> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            let tools = self.refreshed.lock().clone();
            Box::pin(async move { Ok(tools) })
        }
    }

    fn server_settings(name: &str) -> ServerSettings {
        ServerSettings {
            name: name.to_string(),
            transport: TransportKind::Stdio,
            enabled: true,
            command: Some("mcp-echo".to_string()),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
            url: None,
            headers: Default::default(),
            request_timeout: None,
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Read,
            tool_trust: Default::default(),
            max_output_bytes: 4096,
        }
    }

    fn definition(
        name: &str,
        description: &str,
        annotations: ToolAnnotations,
    ) -> McpToolDefinition {
        McpToolDefinition {
            name: name.to_string(),
            description: Some(description.to_string()),
            input_schema: Some(json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            })),
            annotations,
        }
    }

    fn tool_for(runtime: &Arc<FakeRuntime>, settings: ServerSettings) -> McpGatewayTool {
        let runtime: Arc<dyn McpRuntime> = runtime.clone();
        McpGatewayTool::new(runtime, settings)
    }

    fn direct_tool_for(
        runtime: &Arc<FakeRuntime>,
        settings: ServerSettings,
        definition: McpToolDefinition,
    ) -> McpDirectTool {
        let runtime: Arc<dyn McpRuntime> = runtime.clone();
        McpDirectTool::new(runtime, settings, definition)
    }

    #[test]
    fn direct_tool_names_follow_oh_my_pi() {
        assert_eq!(direct_tool_name("fixture", "echo"), "mcp__fixture_echo");
        assert_eq!(
            direct_tool_name("Fixture Server", "Write Note"),
            "mcp__fixture_server_write_note"
        );
        // A redundant server prefix inside the tool name is dropped once.
        assert_eq!(
            direct_tool_name("puppeteer", "puppeteer_screenshot"),
            "mcp__puppeteer_screenshot"
        );

        // Long names stay under the 64-character provider cap, deterministically.
        let long = "x".repeat(120);
        let capped = direct_tool_name("srv", &long);
        assert_eq!(capped, direct_tool_name("srv", &long));
        assert!(capped.len() <= 64, "{capped}");
        assert_ne!(capped, direct_tool_name("srv", &format!("{long}y")));
    }

    #[test]
    fn direct_tool_schema_is_provider_safe() {
        let runtime = FakeRuntime::new(server_settings("fixture"));
        let mut schema_definition = definition("echo", "Echo text", ToolAnnotations::default());
        schema_definition.input_schema = Some(json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {"text": {"type": "string"}}
        }));
        let tool = direct_tool_for(&runtime, server_settings("fixture"), schema_definition);
        let schema = tool.parameters();
        assert!(schema.get("$schema").is_none(), "{schema}");
        assert_eq!(schema["type"], "object");

        // A tool without a schema still gets a usable object shape.
        let mut bare = definition("bare", "No schema", ToolAnnotations::default());
        bare.input_schema = None;
        let tool = direct_tool_for(&runtime, server_settings("fixture"), bare);
        assert_eq!(tool.parameters()["type"], "object");
    }

    #[test]
    fn direct_tool_trust_policy_matches_the_gateway() {
        let read_only = ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        };
        let destructive = ToolAnnotations {
            destructive_hint: Some(true),
            ..ToolAnnotations::default()
        };
        let args = json!({"path": "/tmp/a"});
        let ctx_settings = |trust: TrustLevel, side_effects: SideEffects| {
            let mut settings = server_settings("fixture");
            settings.trust = trust;
            settings.side_effects = side_effects;
            settings
        };

        let runtime = FakeRuntime::new(server_settings("fixture"));
        let session = session();
        for (trust, side_effects, annotations, expected_allow) in [
            (TrustLevel::Allow, SideEffects::Read, read_only, true),
            (TrustLevel::Allow, SideEffects::Unknown, read_only, false),
            (TrustLevel::Ask, SideEffects::Read, read_only, false),
            (TrustLevel::Allow, SideEffects::Read, destructive, false),
        ] {
            // The live catalogue is what policy reads; the registration
            // snapshot is never used as trust evidence.
            runtime.set_cached(vec![definition("read_note", "Read a note", annotations)]);
            let settings = ctx_settings(trust, side_effects);
            let tool = direct_tool_for(
                &runtime,
                settings,
                definition("read_note", "Read a note", annotations),
            );
            match tool.preflight_with_context(&args, &ctx(&session)) {
                PreflightResult::Allow => assert!(expected_allow, "unexpected allow"),
                PreflightResult::Confirm { security, .. } => {
                    assert!(!expected_allow, "unexpected confirm");
                    let security = security.expect("context");
                    assert_eq!(security.mode, SecurityPanelMode::Confirm);
                    assert_eq!(security.target.as_deref(), Some("fixture/read_note"));
                }
                PreflightResult::Block { .. } => panic!("never blocks when trusted"),
            }
        }

        // A tool the live catalogue does not know (still reconnecting, or
        // dropped by the server) is unknown evidence: it asks even though the
        // registration snapshot claimed it was read-only.
        runtime.clear_cached();
        let tool = direct_tool_for(
            &runtime,
            ctx_settings(TrustLevel::Allow, SideEffects::Read),
            definition("read_note", "Read a note", read_only),
        );
        assert!(
            matches!(
                tool.preflight_with_context(&args, &ctx(&session)),
                PreflightResult::Confirm { .. }
            ),
            "an uncached direct tool must ask"
        );

        // A blocked tool is refused with the panel context.
        let tool = direct_tool_for(
            &runtime,
            ctx_settings(TrustLevel::Block, SideEffects::Read),
            definition("read_note", "Read a note", read_only),
        );
        match tool.preflight_with_context(&args, &ctx(&session)) {
            PreflightResult::Block { security, .. } => {
                assert_eq!(security.expect("context").mode, SecurityPanelMode::Blocked);
            }
            other => panic!("expected block, got {other:?}"),
        }

        // Approvals bind to the exact arguments, like the gateway's.
        let first = tool.approval_memory_key(&args);
        assert_eq!(first, tool.approval_memory_key(&json!({"path": "/tmp/a"})));
        assert_ne!(first, tool.approval_memory_key(&json!({"path": "/tmp/b"})));
    }

    #[tokio::test]
    async fn direct_tool_call_renders_like_the_gateway() {
        let settings = server_settings("fixture");
        let runtime = FakeRuntime::new(settings.clone());
        let tool = direct_tool_for(
            &runtime,
            settings.clone(),
            definition("echo", "Echo text", ToolAnnotations::default()),
        );
        let session = session();
        let args = json!({"text": "hi"});

        runtime.set_call(Ok(ToolCallResult {
            content: vec![McpContent::Text {
                text: "hi".to_string(),
            }],
            structured_content: None,
            is_error: false,
        }));
        let ok = tool.execute_async_in_session(args.clone(), &session).await;
        assert!(ok.ok, "{}", ok.output);
        assert_eq!(ok.output, "hi");
        let meta = ok.meta.expect("meta");
        assert_eq!(meta["command_status"], "ok");
        assert_eq!(meta["mcp_server"], "fixture");
        assert_eq!(meta["mcp_tool"], "echo");

        // The server ran the tool and reported a failure: not a dispatch error.
        runtime.set_call(Ok(ToolCallResult {
            content: vec![McpContent::Text {
                text: "boom".to_string(),
            }],
            structured_content: None,
            is_error: true,
        }));
        let failed = tool.execute_async_in_session(args.clone(), &session).await;
        assert!(!failed.ok);
        assert_eq!(failed.meta.expect("meta")["command_status"], "exit_nonzero");

        // Transport failures keep their class in meta for the audit trail.
        runtime.set_call(Err(McpError::timeout(
            "fixture",
            TransportKind::Stdio,
            std::time::Duration::from_millis(50),
        )));
        let timed_out = tool.execute_async_in_session(args.clone(), &session).await;
        assert!(!timed_out.ok);
        let meta = timed_out.meta.expect("meta");
        assert_eq!(meta["command_status"], "timeout");
        assert_eq!(meta["mcp_failure"], "timeout");

        runtime.set_call(Err(McpError::eof("fixture", TransportKind::Stdio, "gone")));
        let dead = tool.execute_async_in_session(args, &session).await;
        assert_eq!(
            dead.meta.expect("meta")["command_status"],
            "dispatch_failed"
        );
    }

    fn session() -> LlmSession {
        LlmSession::new("http://localhost", "key", "model", None, None)
    }

    fn ctx<'a>(session: &'a LlmSession) -> ToolContext<'a> {
        ToolContext::with_policy(
            session,
            ToolExecutionPolicy {
                enforce_read_only_bash: false,
            },
        )
    }

    // -- naming -------------------------------------------------------------

    #[test]
    fn gateway_tool_name_sanitizes() {
        assert_eq!(gateway_tool_name("My Server 2"), "mcp__my_server_2");
        assert_eq!(gateway_tool_name("foo-bar"), "mcp__foo_bar");
        assert_eq!(gateway_tool_name("  "), "mcp__server");
        assert_eq!(gateway_tool_name("Ünïcode Name"), "mcp__n_code_name");
        assert_eq!(gateway_tool_name("A__B"), "mcp__a_b");
    }

    #[test]
    fn gateway_tool_name_is_deterministic_and_capped() {
        let long = "a".repeat(200);
        let first = gateway_tool_name(&long);
        let second = gateway_tool_name(&long);
        assert_eq!(first, second, "the capped name must be deterministic");
        assert_eq!(first.len(), crate::types::MAX_TOOL_NAME_LEN);
        assert!(first.starts_with("mcp__aaaa"));

        let other = format!("{}b", "a".repeat(199));
        assert_ne!(
            gateway_tool_name(&other),
            first,
            "distinct long names must stay distinct"
        );
    }

    #[test]
    fn gateway_tools_with_deduplicates_sanitized_collisions() {
        let runtime: Arc<dyn McpRuntime> = FakeRuntime::new(server_settings("Foo Bar"));
        let servers = vec![server_settings("Foo Bar"), server_settings("foo-bar")];
        let tools = gateway_tools_with(runtime, &servers);
        assert_eq!(tools.len(), 1, "the colliding server must be dropped");
        assert_eq!(tools[0].name(), "mcp__foo_bar");
    }

    // -- preflight matrix ---------------------------------------------------

    fn preflight(
        runtime: &Arc<FakeRuntime>,
        settings: ServerSettings,
        args: Value,
    ) -> PreflightResult {
        let tool = tool_for(runtime, settings);
        let session = session();
        tool.preflight_with_context(&args, &ctx(&session))
    }

    fn read_only() -> ToolAnnotations {
        ToolAnnotations {
            read_only_hint: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn preflight_allows_trusted_read_only() {
        let settings = ServerSettings {
            trust: TrustLevel::Allow,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "read_file", "arguments": {}}),
        );
        assert_eq!(result, PreflightResult::Allow);
    }

    #[test]
    fn preflight_confirms_trusted_unknown_scope() {
        let settings = ServerSettings {
            trust: TrustLevel::Allow,
            side_effects: SideEffects::Unknown,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition(
            "mystery",
            "who knows",
            ToolAnnotations::default(),
        )]);
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "mystery", "arguments": {"x": 1}}),
        );
        match result {
            PreflightResult::Confirm { security, .. } => {
                let security = security.expect("confirm carries a panel context");
                let decision = security.decision.expect("decision present");
                assert!(decision.require_confirmation);
                assert_eq!(decision.level, RiskLevel::Medium);
                assert_eq!(security.target.as_deref(), Some("srv/mystery"));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn preflight_confirms_ask_even_for_read_only() {
        let settings = ServerSettings {
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "read_file", "arguments": {}}),
        );
        match result {
            PreflightResult::Confirm { security, .. } => {
                let decision = security.expect("context").decision.expect("decision");
                assert!(decision.require_confirmation);
                assert_eq!(decision.level, RiskLevel::Low);
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn preflight_blocks_untrusted() {
        let settings = ServerSettings {
            trust: TrustLevel::Block,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "read_file", "arguments": {}}),
        );
        match result {
            PreflightResult::Block { security, .. } => {
                let security = security.expect("block carries a panel context");
                let decision = security.decision.expect("decision present");
                assert!(!decision.allow);
                assert!(!decision.require_confirmation);
                assert_eq!(decision.level, RiskLevel::High);
                assert_eq!(security.mode, SecurityPanelMode::Blocked);
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn preflight_confirm_when_annotations_contradict_declared_read() {
        let settings = ServerSettings {
            trust: TrustLevel::Allow,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        let destructive = ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(true),
            ..Default::default()
        };
        runtime.set_cached(vec![definition("wipe", "removes", destructive)]);
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "wipe", "arguments": {}}),
        );
        match result {
            PreflightResult::Confirm { security, .. } => {
                let decision = security.expect("context").decision.expect("decision");
                assert!(decision.require_confirmation);
                assert_eq!(decision.level, RiskLevel::Medium);
                assert_eq!(
                    decision
                        .analysis
                        .matched_rule
                        .as_ref()
                        .unwrap()
                        .name
                        .as_deref(),
                    Some("wipe")
                );
                assert!(!decision.analysis.reasons.is_empty());
                assert!(decision.analysis.reasons[0]
                    .lines()
                    .next()
                    .is_some_and(|line| !line.trim().is_empty()));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn preflight_list_is_allowed() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        let result = preflight(&runtime, settings, json!({"action": "list"}));
        assert_eq!(result, PreflightResult::Allow);
        let result = preflight(
            &runtime,
            server_settings("srv"),
            json!({"action": "describe", "tool": "x"}),
        );
        assert_eq!(result, PreflightResult::Allow);
    }

    #[test]
    fn preflight_missing_or_invalid_action_blocks() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        assert!(matches!(
            preflight(&runtime, settings.clone(), json!({})),
            PreflightResult::Block { .. }
        ));
        assert!(matches!(
            preflight(&runtime, settings, json!({"action": "explode"})),
            PreflightResult::Block { .. }
        ));
    }

    #[test]
    fn preflight_call_requires_tool() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        assert!(matches!(
            preflight(&runtime, settings, json!({"action": "call"})),
            PreflightResult::Block { .. }
        ));
    }

    /// A tool the client has never seen annotated is *unknown*, not
    /// *unannotated*: run_call refreshes the catalogue and then executes, so a
    /// missing entry must never be judged against the bare declaration.
    #[test]
    fn preflight_asks_for_a_tool_missing_from_the_catalogue() {
        let settings = ServerSettings {
            trust: TrustLevel::Allow,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        let result = preflight(
            &runtime,
            settings.clone(),
            json!({"action": "call", "tool": "unlisted", "arguments": {}}),
        );
        assert!(
            matches!(result, PreflightResult::Confirm { .. }),
            "an uncached tool must ask, got {result:?}"
        );

        runtime.clear_cached();
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "read_file", "arguments": {}}),
        );
        assert!(
            matches!(result, PreflightResult::Confirm { .. }),
            "an empty catalogue must ask, got {result:?}"
        );
    }

    /// `notifications/tools/list_changed` only flags the catalogue, so the
    /// manager keeps serving the pre-change annotations. That view is unknown
    /// evidence and must not auto-approve.
    #[test]
    fn preflight_asks_while_the_catalogue_is_stale() {
        let settings = ServerSettings {
            trust: TrustLevel::Allow,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        runtime.set_stale(true);
        let result = preflight(
            &runtime,
            settings,
            json!({"action": "call", "tool": "read_file", "arguments": {}}),
        );
        assert!(
            matches!(result, PreflightResult::Confirm { .. }),
            "a stale catalogue must ask, got {result:?}"
        );
    }

    /// A remembered approval relaxes the confirmation but never a block: the
    /// session shortcut that used to handle this ran before the block arm, so
    /// MCP tools resolve it here instead (see `run_tool_preflight`).
    #[test]
    fn remembered_approval_relaxes_confirm_but_never_block() {
        let settings = ServerSettings {
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Read,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        let tool = tool_for(&runtime, settings.clone());
        let args = json!({"action": "call", "tool": "read_file", "arguments": {"path": "a"}});
        assert!(matches!(
            preflight(
                &runtime,
                settings.clone(),
                json!({"action": "call", "tool": "read_file", "arguments": {"path": "a"}})
            ),
            PreflightResult::Confirm { .. }
        ));

        let mut session = session();
        let memory = Arc::new(parking_lot::Mutex::new(ApprovalMemory::new()));
        let key = tool.approval_memory_key(&args).expect("call has a key");
        memory.lock().remember_target(tool.name(), &key);
        session.set_approval_memory(Arc::clone(&memory));
        assert_eq!(
            tool.preflight_with_context(&args, &ctx(&session)),
            PreflightResult::Allow,
            "a remembered exact call must not ask again"
        );

        let blocked = ServerSettings {
            trust: TrustLevel::Block,
            side_effects: SideEffects::Read,
            ..settings
        };
        let blocked_tool = tool_for(&runtime, blocked);
        assert!(
            matches!(
                blocked_tool.preflight_with_context(&args, &ctx(&session)),
                PreflightResult::Block { .. }
            ),
            "trust: block must win over a remembered approval"
        );

        // A different argument set carries a different key and asks again.
        memory.lock().clear();
        assert!(matches!(
            tool.preflight_with_context(
                &json!({"action": "call", "tool": "read_file", "arguments": {"path": "b"}}),
                &ctx(&session)
            ),
            PreflightResult::Confirm { .. }
        ));
    }

    // -- approval key -------------------------------------------------------

    #[test]
    fn approval_key_is_stable_per_arguments() {
        let runtime = FakeRuntime::new(server_settings("srv"));
        let tool = tool_for(&runtime, server_settings("srv"));
        let args = json!({"action": "call", "tool": "read_file", "arguments": {"path": "a.txt"}});
        let first = tool.approval_memory_key(&args).expect("call has a key");
        let second = tool.approval_memory_key(&args).expect("stable");
        assert_eq!(first, second);
        assert!(first.starts_with("mcp:srv:read_file#"));

        let changed =
            json!({"action": "call", "tool": "read_file", "arguments": {"path": "b.txt"}});
        assert_ne!(first, tool.approval_memory_key(&changed).expect("key"));

        assert!(tool
            .approval_memory_key(&json!({"action": "list"}))
            .is_none());
    }

    /// The key is taken over the arguments that reach the server, so an omitted
    /// `arguments` field and an explicit `{}` are one approval (both send `{}`).
    #[test]
    fn approval_key_matches_the_arguments_that_are_sent() {
        let runtime = FakeRuntime::new(server_settings("srv"));
        let tool = tool_for(&runtime, server_settings("srv"));
        let omitted = tool
            .approval_memory_key(&json!({"action": "call", "tool": "echo"}))
            .expect("call has a key");
        let empty = tool
            .approval_memory_key(&json!({"action": "call", "tool": "echo", "arguments": {}}))
            .expect("call has a key");
        let null = tool
            .approval_memory_key(&json!({"action": "call", "tool": "echo", "arguments": null}))
            .expect("call has a key");
        assert_eq!(omitted, empty);
        assert_eq!(empty, null);
        assert_eq!(
            empty,
            format!("mcp:srv:echo#{}", trust::args_digest(&json!({})))
        );
    }

    // -- execute ------------------------------------------------------------

    #[tokio::test]
    async fn execute_list_renders_tools() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_refreshed(vec![definition("read_file", "reads a file", read_only())]);
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(json!({"action": "list"}), &session())
            .await;
        assert!(result.ok);
        assert!(result.output.contains("read_file"), "{}", result.output);
        assert!(result.output.contains("read"));
    }

    #[tokio::test]
    async fn execute_describe_includes_schema() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_refreshed(vec![definition("read_file", "reads a file", read_only())]);
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(
                json!({"action": "describe", "tool": "read_file"}),
                &session(),
            )
            .await;
        assert!(result.ok);
        assert!(result.output.contains("Input schema"));
        assert!(result.output.contains("\"path\""));
    }

    #[tokio::test]
    async fn execute_describe_missing_lists_available() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_refreshed(vec![definition("other", "something", read_only())]);
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(json!({"action": "describe", "tool": "nope"}), &session())
            .await;
        assert!(!result.ok);
        assert!(result.output.contains("other"));
    }

    #[tokio::test]
    async fn execute_call_success() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        runtime.set_call(Ok(ToolCallResult {
            content: vec![McpContent::Text {
                text: "hello".to_string(),
            }],
            structured_content: None,
            is_error: false,
        }));
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session(),
            )
            .await;
        assert!(result.ok);
        assert_eq!(result.output, "hello");
        assert_eq!(result.meta.as_ref().unwrap()["command_status"], json!("ok"));
    }

    #[tokio::test]
    async fn execute_call_reports_tool_error() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        runtime.set_call(Ok(ToolCallResult {
            content: vec![McpContent::Text {
                text: "boom".to_string(),
            }],
            structured_content: None,
            is_error: true,
        }));
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session(),
            )
            .await;
        assert!(!result.ok);
        assert!(result.output.starts_with("MCP tool reported an error:"));
        assert_eq!(
            result.meta.as_ref().unwrap()["command_status"],
            json!("exit_nonzero")
        );
    }

    #[tokio::test]
    async fn execute_call_maps_timeout() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        runtime.set_call(Err(McpError::timeout(
            "srv",
            TransportKind::Stdio,
            std::time::Duration::from_millis(10),
        )));
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session(),
            )
            .await;
        assert!(!result.ok);
        assert_eq!(
            result.meta.as_ref().unwrap()["command_status"],
            json!("timeout")
        );
        assert_eq!(
            result.meta.as_ref().unwrap()["mcp_failure"],
            json!("timeout")
        );
    }

    #[tokio::test]
    async fn execute_call_maps_dispatch_failure() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        runtime.set_call(Err(McpError::new(
            "srv",
            TransportKind::Stdio,
            crate::types::McpFailureStage::Send,
            McpFailureClass::Connect,
            true,
            "no connection",
        )));
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session(),
            )
            .await;
        assert!(!result.ok);
        assert_eq!(
            result.meta.as_ref().unwrap()["command_status"],
            json!("dispatch_failed")
        );
    }

    #[tokio::test]
    async fn execute_call_cancelled_when_token_set() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        let tool = tool_for(&runtime, settings);
        let session = session();
        session.cancellation_token().cancel();
        let result = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session,
            )
            .await;
        assert!(!result.ok);
        assert_eq!(
            result.meta.as_ref().unwrap()["command_status"],
            json!("cancelled")
        );
    }

    #[tokio::test]
    async fn execute_call_truncates_large_output() {
        let settings = ServerSettings {
            max_output_bytes: 16,
            ..server_settings("srv")
        };
        let runtime = FakeRuntime::new(settings.clone());
        runtime.set_cached(vec![definition("read_file", "reads", read_only())]);
        runtime.set_call(Ok(ToolCallResult {
            content: vec![McpContent::Text {
                text: "x".repeat(1000),
            }],
            structured_content: None,
            is_error: false,
        }));
        let tool = tool_for(&runtime, settings);
        let result = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session(),
            )
            .await;
        assert!(result.ok);
        assert!(result.output.contains("truncated at 16 bytes"));
        assert!(result.output.len() < 200);
    }

    #[tokio::test]
    async fn execute_refreshes_once_when_cache_is_empty() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        runtime.clear_cached();
        runtime.set_refreshed(vec![definition("read_file", "reads", read_only())]);
        runtime.set_call(Ok(ToolCallResult::default()));
        let tool = tool_for(&runtime, settings);
        let _ = tool
            .execute_async_in_session(
                json!({"action": "call", "tool": "read_file", "arguments": {}}),
                &session(),
            )
            .await;
        assert_eq!(runtime.refresh_count(), 1);
    }

    #[test]
    fn execute_sync_requires_async() {
        let runtime = FakeRuntime::new(server_settings("srv"));
        let tool = tool_for(&runtime, server_settings("srv"));
        let result = tool.execute(json!({"action": "list"}));
        assert!(!result.ok);
        assert_eq!(result.output, aish_i18n::t("shell.mcp.needs_async"));
    }

    // -- prompt -------------------------------------------------------------

    #[test]
    fn prompt_updates_when_tool_list_changes() {
        let settings = server_settings("srv");
        let runtime = FakeRuntime::new(settings.clone());
        let tool = tool_for(&runtime, settings);

        runtime.set_snapshots(vec![ServerSnapshot {
            name: "srv".to_string(),
            transport: TransportKind::Stdio,
            endpoint: "mcp-echo".to_string(),
            status: ServerStatus::Connected {
                protocol_version: "2025-11-25".to_string(),
                tool_count: 1,
            },
            trust: TrustLevel::Ask,
            declared_side_effects: SideEffects::Read,
            tools: vec![ToolSummary {
                name: "alpha".to_string(),
                description: Some("first".to_string()),
                side_effects: SideEffects::Read,
                required: Vec::new(),
            }],
            instructions: false,
        }]);
        let first: &str = tool.prompt();
        assert!(first.contains("alpha"));
        assert!(!first.contains("beta"));

        runtime.set_snapshots(vec![ServerSnapshot {
            name: "srv".to_string(),
            transport: TransportKind::Stdio,
            endpoint: "mcp-echo".to_string(),
            status: ServerStatus::Connected {
                protocol_version: "2025-11-25".to_string(),
                tool_count: 1,
            },
            trust: TrustLevel::Ask,
            declared_side_effects: SideEffects::Read,
            tools: vec![ToolSummary {
                name: "beta".to_string(),
                description: Some("second".to_string()),
                side_effects: SideEffects::Write,
                required: Vec::new(),
            }],
            instructions: false,
        }]);
        let second = tool.prompt();
        assert!(second.contains("beta"));
        // The previously returned reference stays valid after the change.
        assert!(first.contains("alpha"));
        assert_ne!(first, second);
    }
}
