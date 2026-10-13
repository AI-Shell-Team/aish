//! MCP runtime manager: per-server lifecycle, tool cache and reconnect policy.
//!
//! One [`McpManager`] owns every configured server. It runs one background
//! driver task per enabled server, owns the tool cache, derives the
//! [`ServerSnapshot`]s `/mcp` renders and is the object the gateway tool calls
//! through ([`McpRuntime`]).
//!
//! Rules the implementation keeps to:
//!
//! - **One driver per server.** A driver owns exactly one
//!   connect → session → reconnect cycle and is identified by a
//!   `(generation, epoch)` pair. Every state write goes through
//!   [`McpManager::driver_update`], which drops the write when the pair is
//!   stale, so a superseded task can never resurrect a connection that
//!   `reload()`/`shutdown()` tore down.
//! - **One in-flight connect attempt per server.** Attempts serialize on a
//!   `tokio::sync::Mutex`; callers wait on a `tokio::sync::watch` status
//!   channel instead of the lock, so many concurrent calls share one attempt
//!   and no blocking mutex guard is ever held across an `.await`.
//! - **Bounded everything.** Reconnects stop after four backoff steps (500,
//!   1000, 2000, 4000 ms), and `shutdown()` closes every transport under a
//!   per-server and a global deadline.
//! - **No credentials.** The manager exposes only [`ServerSnapshot`]s, whose
//!   endpoint label comes from [`ServerSettings::endpoint_label`] (redacted);
//!   env and header values are never stored on the manager.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::{Mutex as ReconfigGate, RwLock};
use serde_json::Value;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use aish_llm::CancellationToken;

use crate::cancel::CancelListener;
use crate::client::{self, ConnectOptions, Connection, ServerEvent};
use crate::config::{McpSettings, ServerSettings};
use crate::types::{
    McpError, McpFailureClass, McpFailureStage, McpToolDefinition, ServerSnapshot, ServerStatus,
    ToolCallResult, ToolSummary, TransportKind, DEFAULT_REQUEST_TIMEOUT_MS,
};

/// Delays before each automatic reconnect attempt, in order. Four steps is the
/// whole ladder: a server that stays down is left `Failed` instead of being
/// probed forever.
const RECONNECT_BACKOFF: [Duration; 4] = [
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(2000),
    Duration::from_millis(4000),
];

/// How many full backoff ladders one driver may run. A server that loses every
/// connection it establishes would otherwise reconnect forever.
const MAX_RECOVERY_LADDERS: usize = 2;

/// Upper bound for closing one transport (`SIGTERM` → 1s → `SIGKILL` inside the
/// stdio transport, plus the HTTP client's per-call deadline).
const CLOSE_BUDGET: Duration = Duration::from_secs(3);

/// Upper bound for the whole `shutdown()`: every connection plus every driver.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

/// How long `reload()` waits for the fresh attempts before reporting the
/// servers that already failed.
const RELOAD_GATE: Duration = Duration::from_secs(5);

/// Server notification that invalidates the cached tool list.
const TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";

/// What the gateway tool needs from the MCP runtime (implemented by
/// [`McpManager`]).
///
/// The trait exists so the tool layer never depends on a live connection: it
/// sees snapshots and tool definitions, and calls go through the manager's
/// connection/wait/cache policy.
pub trait McpRuntime: Send + Sync {
    /// One snapshot per configured server, in configuration order.
    fn snapshots(&self) -> Vec<ServerSnapshot>;

    /// Settings of an enabled server; `None` for unknown or disabled servers.
    fn server_settings(&self, server: &str) -> Option<ServerSettings>;

    /// Cached tool definitions for a server, when a listing has succeeded.
    fn cached_tools(&self, server: &str) -> Option<Vec<McpToolDefinition>>;

    /// Call one tool, waiting for the connection when it is still coming up.
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        args: &'a serde_json::Value,
        cancel: Option<Arc<CancelListener>>,
    ) -> BoxFuture<'a, Result<ToolCallResult, McpError>>;

    /// Re-read the tool catalogue from the server.
    fn refresh_tools<'a>(
        &'a self,
        server: &'a str,
        cancel: Option<Arc<CancelListener>>,
    ) -> BoxFuture<'a, Result<Vec<McpToolDefinition>, McpError>>;

    /// True when the server's cached tool catalogue is known to be out of date.
    ///
    /// The manager keeps the previous catalogue after a
    /// `notifications/tools/list_changed` (it only flags it), so a caller that
    /// decides policy from cached annotations has to know they may be stale.
    /// Defaults to `false` for runtimes that never flag staleness (test
    /// doubles).
    fn tools_are_stale(&self, server: &str) -> bool {
        let _ = server;
        false
    }
}

/// Runtime state of one configured server.
///
/// The entry is keyed by [`ServerState::name`] only: the configuration itself
/// lives in [`McpManager::settings`], which is the single source of truth for
/// every `ServerSettings` field.
struct ServerState {
    name: String,
    /// Published status; also the wake-up channel callers wait on.
    status: watch::Sender<ServerStatus>,
    status_rx: watch::Receiver<ServerStatus>,
    /// Cached catalogue. Kept across a `list_changed` notification (marked
    /// stale) so status output does not blink empty, and across a reconnect so
    /// a dropped transport does not erase what the server used to expose.
    tools: Option<Vec<McpToolDefinition>>,
    /// The cache no longer matches what the server exposes.
    tools_stale: bool,
    /// The server sent `instructions` during the handshake.
    instructions: bool,
    /// Current connection, when the driver has one installed.
    connection: Option<Arc<Connection>>,
    /// Sender half of the connection's [`ServerEvent`] channel.
    ///
    /// Kept so a call that proves the connection is gone can wake the driver
    /// with `Closed`: an HTTP transport has no read loop to notice on its own.
    events: Option<mpsc::UnboundedSender<ServerEvent>>,
    /// Cancels a handshake that is still in flight when this entry's driver is
    /// superseded, so `shutdown()`/`reload()`/disabling the server end the
    /// attempt instead of letting its child outlive the state.
    connect_cancel: Option<Arc<CancellationToken>>,
    /// A driver task owns this server.
    attempting: bool,
    /// Ownership token: bumped whenever a driver is superseded.
    epoch: u64,
    /// Serializes connect attempts for this server.
    connect_lock: Arc<Mutex<()>>,
    /// The driver task, awaited (with a deadline) by `shutdown()`.
    driver: Option<JoinHandle<()>>,
}

/// One connect attempt.
enum Attempt {
    /// The handshake and the first `tools/list` succeeded.
    Connected(Arc<Connection>, mpsc::UnboundedReceiver<ServerEvent>),
    /// The attempt failed and published `Failed`.
    Failed,
    /// The attempt belongs to a superseded generation/epoch.
    Stale,
}

/// How a server settled after its status was reset to `Connecting`.
enum Settled {
    Connected,
    Failed(String),
    /// Still `Connecting` when the deadline expired.
    Timeout,
    /// Not a server that can be connected (unknown, disabled, shut down).
    Unavailable,
}

/// What one [`McpManager::apply_settings`] call changed, so the shell can
/// register or unregister the gateway tools of exactly those servers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Servers that appeared in the configuration and were started.
    pub added: Vec<String>,
    /// Servers that are gone from the runtime: deleted from the configuration
    /// or disabled. Their transport was closed and their state removed, so
    /// [`McpManager::snapshots`] no longer lists them.
    pub removed: Vec<String>,
    /// Servers that stayed configured but had to be reconnected: their
    /// settings changed, or they went from disabled to enabled.
    pub updated: Vec<String>,
}

/// Runtime work a reconfiguration decided on, executed after the state locks
/// are released so no transport is closed while a state guard is held.
#[derive(Default)]
struct ReconfigurePlan {
    outcome: ApplyOutcome,
    /// Connections to close before their replacement is started.
    close: Vec<(String, Arc<Connection>)>,
    /// Superseded drivers, awaited (with a deadline) after their transport is
    /// closed.
    drivers: Vec<JoinHandle<()>>,
    /// Servers that need a fresh driver started.
    spawn: Vec<String>,
}

/// MCP runtime: connection lifecycle, tool cache and status.
pub struct McpManager {
    /// The configuration, and the only place a `ServerSettings` lives. It is
    /// replaced wholesale by [`McpManager::apply_settings`].
    settings: RwLock<McpSettings>,
    /// Runtime entry per server that can run, in configuration order.
    servers: RwLock<Vec<ServerState>>,
    /// Serializes the synchronous part of a reconfiguration (`settings` +
    /// `servers` mutation) so concurrent calls cannot interleave. Never held
    /// across an `.await`.
    reconfig: ReconfigGate<()>,
    /// Bumped by `reload()`/`shutdown()`; invalidates every in-flight attempt.
    generation: AtomicU64,
    /// `true` once `shutdown()` ran: no new attempt may start.
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    /// Weak handle so `&self` methods can spawn a driver without `Arc<Self>`.
    self_ref: Weak<McpManager>,
}

impl McpManager {
    /// Build a manager for the resolved settings. No I/O happens here: call
    /// [`McpManager::start`] (or simply make a call) to connect.
    pub fn new(settings: McpSettings) -> Arc<Self> {
        // A disabled server still gets an entry, so `/mcp` can list a
        // configured-but-off server before anything is reconnected. A
        // reconfiguration that changes that entry removes it instead (see
        // `reconfig_locked`).
        let servers: Vec<ServerState> = settings
            .servers
            .iter()
            .map(|server| {
                let status = initial_status(settings.enabled, server.enabled);
                new_server_state(server.name.clone(), status)
            })
            .collect();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // `new_cyclic` hands the constructor its own weak handle, so the
        // `&self` entry points (call, tool listing) can start a driver lazily
        // without the manager storing a strong reference to itself.
        Arc::new_cyclic(|me| Self {
            settings: RwLock::new(settings),
            servers: RwLock::new(servers),
            reconfig: ReconfigGate::new(()),
            generation: AtomicU64::new(0),
            shutdown_tx,
            shutdown_rx,
            self_ref: me.clone(),
        })
    }

    /// Start one background connection task per enabled server. Returns
    /// immediately: progress is observed through [`McpManager::snapshots`].
    pub fn start(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        if !self.settings.read().enabled {
            tracing::debug!("mcp: disabled; no server will be contacted");
            return;
        }
        let names = self.enabled_names();
        for name in names {
            self.spawn_driver(&name, handle);
        }
    }

    /// The resolved `mcp:` settings as they are right now. Reconfiguration
    /// replaces them, so this returns an owned copy of the current revision.
    pub fn settings(&self) -> McpSettings {
        self.settings.read().clone()
    }

    /// `true` when MCP is enabled and at least one server is enabled.
    pub fn is_active(&self) -> bool {
        let settings = self.settings.read();
        settings.enabled && settings.servers.iter().any(|server| server.enabled)
    }

    /// Configured settings of one server (enabled or not).
    pub fn server(&self, name: &str) -> Option<ServerSettings> {
        self.settings.read().server(name).cloned()
    }

    /// Every configured server, in configuration order.
    pub fn servers(&self) -> Vec<ServerSettings> {
        self.settings.read().servers.clone()
    }

    /// Settings of an enabled server; the shape the gateway tool consumes.
    pub fn server_settings(&self, server: &str) -> Option<ServerSettings> {
        self.enabled_settings(server)
    }

    /// Cached tool definitions, without touching the server.
    pub fn cached_tools(&self, server: &str) -> Option<Vec<McpToolDefinition>> {
        self.cached_tools_of(server)
    }

    /// One status snapshot per configured server, in configuration order.
    pub fn snapshots(&self) -> Vec<ServerSnapshot> {
        self.collect_snapshots()
    }

    /// Cached tools when they are current; otherwise wait for the connection
    /// and pull the catalogue from the server.
    pub async fn cached_or_fetch_tools(
        &self,
        server: &str,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<Vec<McpToolDefinition>, McpError> {
        self.resolve_tools(server, cancel).await
    }

    /// Re-read the catalogue even when a cached one exists.
    pub async fn refresh_tools(
        &self,
        server: &str,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<Vec<McpToolDefinition>, McpError> {
        self.refresh_server_tools(server, cancel).await
    }

    /// Call one tool on one server.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: &serde_json::Value,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<ToolCallResult, McpError> {
        self.invoke_tool(server, tool, args, cancel).await
    }

    /// Reconnect every enabled server (close first, then connect) and report
    /// the servers that failed before the gate expired. Servers still
    /// connecting when the gate expires keep connecting in the background.
    pub async fn reload(self: &Arc<Self>) -> Vec<(String, McpError)> {
        self.shutdown_tx.send_replace(false);
        self.generation.fetch_add(1, Ordering::SeqCst);

        let (connections, drivers, names) = self.reset_runtime(true);
        self.close_connections(connections).await;
        self.retire_drivers(drivers).await;
        if names.is_empty() {
            return Vec::new();
        }

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("mcp: reload requires a tokio runtime; nothing was connected");
            return Vec::new();
        };
        for name in &names {
            self.spawn_driver(name, &handle);
        }

        let deadline = Instant::now() + RELOAD_GATE;
        let mut failures = Vec::new();
        for name in &names {
            if let Settled::Failed(reason) = self.wait_settled(name, deadline).await {
                failures.push((
                    name.clone(),
                    self.not_connected_error(name, &format!("last error: {reason}")),
                ));
            }
        }
        failures
    }

    /// Tear down one server and connect it again, waiting for the new attempt
    /// to settle within the server's request deadline.
    pub async fn reconnect(self: &Arc<Self>, server: &str) -> Result<(), McpError> {
        let settings = self
            .enabled_settings(server)
            .ok_or_else(|| self.unavailable_error(server))?;
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return Err(self.not_connected_error(server, "no tokio runtime is available"));
        };

        // Supersede the running driver first: it observes the stale epoch and
        // exits without touching the state this call is about to rebuild.
        let (connection, _driver) = self.reset_server(server, true);
        if let Some(connection) = connection {
            let close_deadline = Instant::now() + CLOSE_BUDGET;
            if tokio::time::timeout_at(close_deadline, connection.close())
                .await
                .is_err()
            {
                tracing::warn!(server = %server, "mcp: reconnect could not close the old transport in time");
            }
        }
        self.spawn_driver(server, &handle);

        let timeout = settings
            .effective_timeout()
            .unwrap_or(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS));
        match self.wait_settled(server, Instant::now() + timeout).await {
            Settled::Connected => Ok(()),
            Settled::Failed(reason) => {
                Err(self.not_connected_error(server, &format!("last error: {reason}")))
            }
            Settled::Timeout => Err(self.not_connected_error(
                server,
                &format!(
                    "the connection attempt did not finish within {}ms",
                    timeout.as_millis()
                ),
            )),
            Settled::Unavailable => Err(self.unavailable_error(server)),
        }
    }

    /// Close every transport, stop every driver and clear the runtime state.
    ///
    /// Bounded by [`SHUTDOWN_BUDGET`] overall and [`CLOSE_BUDGET`] per server,
    /// and idempotent: a second call finds nothing left to do. The manager
    /// object stays usable for `is_active()`/`snapshots()`; only `reload()`
    /// starts servers again.
    pub async fn shutdown(&self) {
        self.shutdown_tx.send_replace(true);
        self.generation.fetch_add(1, Ordering::SeqCst);
        let (connections, drivers, _names) = self.reset_runtime(false);
        self.close_connections(connections).await;
        self.retire_drivers(drivers).await;
    }

    // -----------------------------------------------------------------------
    // Runtime reconfiguration
    // -----------------------------------------------------------------------

    /// Apply a freshly resolved configuration: what a `/mcp add`, `remove`,
    /// `enable` or `disable` leaves behind once it rewrote the config file.
    ///
    /// The runtime follows the configuration. A server that is enabled and
    /// either new or changed is reconnected; a server that was deleted or
    /// disabled is closed and forgotten. Unchanged servers keep the connection
    /// they have, so applying the same settings twice changes nothing and
    /// returns an empty [`ApplyOutcome`].
    ///
    /// Concurrent calls are serialized by the state locks: each call commits
    /// its own configuration revision and closes what that revision orphaned
    /// before starting replacements.
    pub async fn apply_settings(self: &Arc<Self>, settings: McpSettings) -> ApplyOutcome {
        let plan = {
            let _gate = self.reconfig.lock();
            self.reconfig_locked(settings)
        };
        self.settle(plan).await
    }

    /// Track one new server without touching the others: the convenience entry
    /// point a quick-add uses.
    ///
    /// Returns `false` when a server of that name is already configured.
    pub fn add_server(self: &Arc<Self>, settings: ServerSettings) -> bool {
        let plan = {
            let _gate = self.reconfig.lock();
            let mut updated = self.settings.read().clone();
            if updated.server(&settings.name).is_some() {
                return false;
            }
            updated.servers.push(settings);
            self.reconfig_locked(updated)
        };
        // An addition removes nothing, so no transport has to be closed and no
        // driver has to be retired: only the spawn half of `settle` applies.
        self.spawn_planned(&plan.spawn);
        true
    }

    /// Close one server's transport and drop it from the runtime.
    ///
    /// The configuration entry goes away with it, so the server disappears
    /// from both [`McpManager::snapshots`] and [`McpManager::server`]. Returns
    /// `false` when no server of that name is configured.
    pub async fn remove_server(self: &Arc<Self>, name: &str) -> bool {
        let plan = {
            let _gate = self.reconfig.lock();
            let mut updated = self.settings.read().clone();
            let configured = updated.servers.len();
            updated.servers.retain(|server| server.name != name);
            if updated.servers.len() == configured {
                return false;
            }
            self.reconfig_locked(updated)
        };
        self.settle(plan).await;
        true
    }

    /// Enable or disable one configured server without touching its other
    /// parameters.
    ///
    /// Enabling connects (or reconnects) the server. Disabling closes its
    /// transport and removes its runtime state, so [`McpManager::snapshots`]
    /// stops listing it. Returns `false` when no server of that name is
    /// configured.
    pub async fn set_server_enabled(self: &Arc<Self>, name: &str, enabled: bool) -> bool {
        let plan = {
            let _gate = self.reconfig.lock();
            let mut updated = self.settings.read().clone();
            let Some(server) = updated
                .servers
                .iter_mut()
                .find(|server| server.name == name)
            else {
                return false;
            };
            if server.enabled == enabled {
                // Already in the requested state: nothing to reconcile.
                return true;
            }
            server.enabled = enabled;
            self.reconfig_locked(updated)
        };
        self.settle(plan).await;
        true
    }

    /// Reconcile the runtime with `settings`, assuming the reconfiguration
    /// gate is held and never awaiting.
    ///
    /// Everything decidable from the state alone happens here, under the state
    /// locks, so no caller can observe a half-applied configuration. The
    /// transport and driver the change orphaned are handed back in the
    /// returned plan, because closing a transport needs `.await`.
    fn reconfig_locked(&self, settings: McpSettings) -> ReconfigurePlan {
        let mut plan = ReconfigurePlan::default();
        // A configuration change revives a stopped runtime: enabled servers are
        // rebuilt instead of staying in the `Disabled` state `shutdown()` left.
        let restart = *self.shutdown_rx.borrow();
        let mcp_enabled = settings.enabled;

        let mut states = self.servers.write();
        let mut config = self.settings.write();
        let previous = std::mem::replace(&mut *config, settings);

        let mut existing = std::mem::take(&mut *states);
        let mut next: Vec<ServerState> = Vec::with_capacity(config.servers.len());

        for entry in &config.servers {
            let state = take_server_state(&mut existing, &entry.name);
            let prev = previous.server(&entry.name);
            let was_enabled = prev.map(|server| server.enabled).unwrap_or(false);
            let changed = restart || prev.is_none_or(|server| !same_server_settings(server, entry));

            if !(mcp_enabled && entry.enabled) {
                // A server that cannot run keeps no runtime state: close the
                // transport, retire the driver and forget both. This is the
                // deleted-or-disabled path.
                if let Some(mut state) = state {
                    let (connection, driver) = retire_server_state(&mut state);
                    if let Some(connection) = connection {
                        plan.close.push((entry.name.clone(), connection));
                    }
                    if let Some(driver) = driver {
                        plan.drivers.push(driver);
                    }
                    if was_enabled {
                        plan.outcome.removed.push(entry.name.clone());
                    }
                }
                continue;
            }

            match state {
                // Unchanged: the live connection — or the disabled placeholder
                // the manager was built with — stays exactly as it is.
                Some(state) if !changed => next.push(state),
                Some(mut state) => {
                    let (connection, driver) = retire_server_state(&mut state);
                    if let Some(connection) = connection {
                        plan.close.push((entry.name.clone(), connection));
                    }
                    if let Some(driver) = driver {
                        plan.drivers.push(driver);
                    }
                    state.status.send_replace(ServerStatus::Connecting);
                    next.push(state);
                    plan.spawn.push(entry.name.clone());
                    plan.outcome.updated.push(entry.name.clone());
                }
                None => {
                    next.push(new_server_state(
                        entry.name.clone(),
                        ServerStatus::Connecting,
                    ));
                    plan.spawn.push(entry.name.clone());
                    if prev.is_none() {
                        plan.outcome.added.push(entry.name.clone());
                    } else {
                        plan.outcome.updated.push(entry.name.clone());
                    }
                }
            }
        }

        // Whatever no configuration entry claims any more was deleted while
        // this manager was running.
        for mut state in existing {
            let (connection, driver) = retire_server_state(&mut state);
            if let Some(connection) = connection {
                plan.close.push((state.name.clone(), connection));
            }
            if let Some(driver) = driver {
                plan.drivers.push(driver);
            }
            if previous
                .server(&state.name)
                .map(|server| server.enabled)
                .unwrap_or(false)
            {
                plan.outcome.removed.push(state.name);
            }
        }

        *states = next;
        plan
    }

    /// Execute the asynchronous half of a reconfiguration: close the orphaned
    /// transports first (their drivers only notice a superseded epoch after
    /// the event stream ends), await the retired drivers, then start the
    /// replacements.
    async fn settle(&self, plan: ReconfigurePlan) -> ApplyOutcome {
        self.close_connections(plan.close).await;
        self.retire_drivers(plan.drivers).await;
        self.spawn_planned(&plan.spawn);
        plan.outcome
    }

    /// Start a driver for every server in `names`.
    fn spawn_planned(&self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        // Controllers are re-armed before spawning: a reconfiguration asks for
        // the runtime to be live again, even when `shutdown()` ran before it.
        self.shutdown_tx.send_replace(false);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // Without a runtime the first call connects lazily, exactly like a
            // manager that was never `start()`ed.
            tracing::warn!(
                "mcp: no tokio runtime available; {} server(s) will connect on first use",
                names.len()
            );
            return;
        };
        for name in names {
            self.spawn_driver(name, &handle);
        }
    }

    // -----------------------------------------------------------------------
    // Public API internals
    // -----------------------------------------------------------------------

    fn collect_snapshots(&self) -> Vec<ServerSnapshot> {
        // Lock order is `servers` before `settings` throughout this module.
        let servers = self.servers.read();
        let settings = self.settings.read();
        servers
            .iter()
            .filter_map(|state| {
                // A runtime entry without a configuration entry cannot happen:
                // `reconfig_locked` drops both together.
                let server = settings.server(&state.name)?;
                let status = state.status.borrow().clone();
                let connected = is_connected(&status);
                let tools = if connected {
                    state
                        .tools
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(|tool| ToolSummary {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            side_effects: crate::trust::combine_side_effects(
                                server.side_effects,
                                crate::trust::reported_side_effects(&tool.annotations),
                            ),
                            required: ToolSummary::required_from_schema(tool.input_schema.as_ref()),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Some(ServerSnapshot {
                    name: server.name.clone(),
                    transport: server.transport,
                    endpoint: server.endpoint_label(),
                    status,
                    trust: server.trust,
                    declared_side_effects: server.side_effects,
                    tools,
                    instructions: connected && state.instructions,
                })
            })
            .collect()
    }

    /// Settings of an enabled server, when MCP itself is enabled.
    fn enabled_settings(&self, name: &str) -> Option<ServerSettings> {
        let settings = self.settings.read();
        if !settings.enabled {
            return None;
        }
        settings
            .server(name)
            .filter(|server| server.enabled)
            .cloned()
    }

    fn enabled_names(&self) -> Vec<String> {
        let settings = self.settings.read();
        if !settings.enabled {
            return Vec::new();
        }
        settings
            .servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| server.name.clone())
            .collect()
    }

    fn cached_tools_of(&self, name: &str) -> Option<Vec<McpToolDefinition>> {
        self.servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .and_then(|state| state.tools.clone())
    }

    fn tools_are_stale(&self, name: &str) -> bool {
        self.servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .map(|state| state.tools_stale)
            .unwrap_or(true)
    }

    /// The live connection of a server, when there is one.
    fn live_connection(&self, name: &str) -> Option<Arc<Connection>> {
        self.servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .and_then(|state| state.connection.clone())
            .filter(|connection| connection.is_alive())
    }

    /// `true` when a connected and still alive incarnation exists.
    fn has_live_incarnation(&self, name: &str) -> bool {
        let servers = self.servers.read();
        let Some(state) = servers.iter().find(|state| state.name == name) else {
            return false;
        };
        is_connected(&state.status.borrow())
            && state
                .connection
                .as_ref()
                .map(|connection| connection.is_alive())
                .unwrap_or(false)
    }

    /// A wake-up receiver for one server's status.
    ///
    /// The state lock is taken and released inside this function, so no caller
    /// holds a `parking_lot` guard across an `.await`. Callers keep this one
    /// receiver for the whole wait: a freshly cloned receiver counts as
    /// "unseen", and `changed()` on it returns `Ok` immediately, which would
    /// turn a wait loop into a busy loop.
    fn status_watch(&self, name: &str) -> Option<watch::Receiver<ServerStatus>> {
        self.servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .map(|state| state.status_rx.clone())
    }

    /// Error for a server that cannot be called: unknown, disabled, or without
    /// a live connection.
    fn unavailable_error(&self, name: &str) -> McpError {
        let settings = self.settings.read();
        let detail = match settings.server(name) {
            None => "the server is not configured",
            Some(_) if !settings.enabled => "mcp is disabled",
            Some(server) if !server.enabled => "the server is disabled",
            Some(_) => "the server is not connected",
        };
        drop(settings);
        self.not_connected_error(name, detail)
    }

    fn not_connected_error(&self, name: &str, detail: &str) -> McpError {
        let transport = self
            .settings
            .read()
            .server(name)
            .map(|server| server.transport)
            .unwrap_or(TransportKind::Stdio);
        McpError::new(
            name,
            transport,
            McpFailureStage::Connect,
            McpFailureClass::Connect,
            true,
            format!("server `{name}` is not connected ({detail})"),
        )
    }

    /// Wait until the server is connected or definitively failed.
    ///
    /// Waiting happens on the status channel, so concurrent callers share one
    /// connect attempt and none of them blocks the attempt itself.
    async fn wait_for_connection(
        &self,
        name: &str,
        timeout: Option<Duration>,
    ) -> Result<Arc<Connection>, McpError> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let Some(mut status_rx) = self.status_watch(name) else {
            return Err(self.unavailable_error(name));
        };
        loop {
            // `borrow_and_update` marks the current revision as seen, so the
            // `changed()` below waits for the next real transition instead of
            // reporting the value this iteration already handled.
            let status = status_rx.borrow_and_update().clone();
            match status {
                ServerStatus::Connected { .. } => {
                    if let Some(connection) = self.live_connection(name) {
                        return Ok(connection);
                    }
                    // The transport died; the driver publishes `Failed` shortly.
                }
                ServerStatus::Failed { reason } => {
                    return Err(self.not_connected_error(name, &format!("last error: {reason}")));
                }
                ServerStatus::Disabled => {
                    return Err(self.not_connected_error(
                        name,
                        "the server is disabled or the runtime is shut down",
                    ));
                }
                ServerStatus::Connecting => {}
            }
            if *self.shutdown_rx.borrow() {
                // Nothing will ever connect: refuse now instead of waiting out
                // the request deadline for a driver that cannot start.
                return Err(self.not_connected_error(name, "the runtime is shut down"));
            }
            // A manager that was never `start()`ed still connects on demand.
            self.ensure_driver(name);

            let changed = match deadline {
                Some(deadline) => {
                    match tokio::time::timeout_at(deadline, status_rx.changed()).await {
                        Ok(changed) => changed,
                        Err(_) => {
                            let millis = timeout.map(|timeout| timeout.as_millis()).unwrap_or(0);
                            return Err(self.not_connected_error(
                                name,
                                &format!("the connection attempt did not settle within {millis}ms"),
                            ));
                        }
                    }
                }
                None => status_rx.changed().await,
            };
            if changed.is_err() {
                return Err(self.not_connected_error(name, "the connection state channel is gone"));
            }
        }
    }

    /// Cached catalogue when it is current, else a fresh `tools/list`.
    async fn resolve_tools(
        &self,
        server: &str,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<Vec<McpToolDefinition>, McpError> {
        self.enabled_settings(server)
            .ok_or_else(|| self.unavailable_error(server))?;
        if let Some(tools) = self.cached_tools_of(server) {
            if !self.tools_are_stale(server) {
                return Ok(tools);
            }
        }
        self.refresh_server_tools(server, cancel).await
    }

    async fn refresh_server_tools(
        &self,
        server: &str,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<Vec<McpToolDefinition>, McpError> {
        let settings = self
            .enabled_settings(server)
            .ok_or_else(|| self.unavailable_error(server))?;
        let connection = self
            .wait_for_connection(server, settings.effective_timeout())
            .await?;
        let tools = match connection.list_tools(cancel).await {
            Ok(tools) => tools,
            Err(error) => {
                self.report_lost_connection(server, &connection, &error);
                return Err(error);
            }
        };
        self.store_tools(server, &connection, tools.clone());
        Ok(tools)
    }

    /// Record that a call proved the connection is gone, and let the driver
    /// react.
    ///
    /// The stdio transport notices on its own and reports
    /// `ServerEvent::Closed`; an HTTP transport has no read loop to notice, so
    /// the failing call is the only signal and, without this, `/mcp status`
    /// would keep saying `Connected` for a server that is gone. Forwarding the
    /// same event keeps a single teardown owner: the driver clears the
    /// connection and runs its bounded reconnect ladder.
    fn report_lost_connection(&self, name: &str, connection: &Arc<Connection>, error: &McpError) {
        if !error.is_connection_loss() {
            return;
        }
        let events = {
            let servers = self.servers.read();
            let Some(state) = servers.iter().find(|state| state.name == name) else {
                return;
            };
            // Only the connection that failed: a newer session installed while
            // the call was in flight must not be torn down by an old error.
            let current = state
                .connection
                .as_ref()
                .map(|current| Arc::ptr_eq(current, connection))
                .unwrap_or(false);
            if !current {
                return;
            }
            state.events.clone()
        };
        let Some(events) = events else {
            return;
        };
        tracing::debug!(
            server = %name,
            "mcp: a call reported a lost connection: {}",
            error.describe()
        );
        let _ = events.send(ServerEvent::Closed {
            error: Some(error.clone()),
        });
    }

    async fn invoke_tool(
        &self,
        server: &str,
        tool: &str,
        args: &Value,
        cancel: Option<Arc<CancelListener>>,
    ) -> Result<ToolCallResult, McpError> {
        let settings = self
            .enabled_settings(server)
            .ok_or_else(|| self.unavailable_error(server))?;
        let known = self
            .cached_tools_of(server)
            .map(|tools| tools.iter().any(|definition| definition.name == tool))
            .unwrap_or(false);
        if !known {
            // The server may have gained the tool since the last listing. One
            // refresh, and the call proceeds either way: the server is the
            // authority on its own catalogue.
            if let Err(error) = self.refresh_server_tools(server, cancel.clone()).await {
                tracing::debug!(
                    server = %server,
                    "mcp: refreshing the tool cache before `{tool}` failed: {}",
                    error.describe()
                );
            }
        }
        let connection = self
            .wait_for_connection(server, settings.effective_timeout())
            .await?;
        match connection.call_tool(tool, args, cancel).await {
            Ok(result) => Ok(result),
            Err(error) => {
                self.report_lost_connection(server, &connection, &error);
                Err(error)
            }
        }
    }

    /// Record a fresh catalogue when it still belongs to the live connection.
    fn store_tools(&self, name: &str, connection: &Arc<Connection>, tools: Vec<McpToolDefinition>) {
        let count = tools.len();
        let version = connection.protocol_version().to_string();
        let mut servers = self.servers.write();
        let Some(state) = servers.iter_mut().find(|state| state.name == name) else {
            return;
        };
        let current = state
            .connection
            .as_ref()
            .map(|current| Arc::ptr_eq(current, connection))
            .unwrap_or(false);
        if !current {
            return;
        }
        state.tools = Some(tools);
        state.tools_stale = false;
        if is_connected(&state.status.borrow()) {
            state.status.send_replace(ServerStatus::Connected {
                protocol_version: version,
                tool_count: count,
            });
        }
    }

    // -----------------------------------------------------------------------
    // Driver lifecycle
    // -----------------------------------------------------------------------

    fn upgrade(&self) -> Option<Arc<Self>> {
        self.self_ref.upgrade()
    }

    /// Start a driver for one server unless one already owns it.
    fn spawn_driver(&self, name: &str, handle: &tokio::runtime::Handle) -> bool {
        let epoch = {
            let mut servers = self.servers.write();
            let Some(state) = servers.iter_mut().find(|state| state.name == name) else {
                return false;
            };
            // The configuration is read while the state lock is held; `servers`
            // before `settings` is the lock order this module keeps to.
            let runnable = {
                let settings = self.settings.read();
                settings.enabled
                    && settings
                        .server(name)
                        .map(|server| server.enabled)
                        .unwrap_or(false)
            };
            if *self.shutdown_rx.borrow() || !runnable || state.attempting {
                return false;
            }
            // Liveness is read from the state already under this guard; taking
            // the read lock again here would deadlock a parking_lot write guard.
            let alive = is_connected(&state.status.borrow())
                && state
                    .connection
                    .as_ref()
                    .map(|connection| connection.is_alive())
                    .unwrap_or(false);
            if alive {
                return false;
            }
            state.attempting = true;
            state.epoch += 1;
            if !matches!(&*state.status.borrow(), ServerStatus::Connecting) {
                state.status.send_replace(ServerStatus::Connecting);
            }
            state.epoch
        };

        let Some(this) = self.upgrade() else {
            self.clear_attempting(name);
            return false;
        };
        let generation = self.generation.load(Ordering::SeqCst);
        let task_name = name.to_string();
        let driver = handle.spawn(async move {
            this.drive_server(task_name, generation, epoch).await;
        });

        let mut servers = self.servers.write();
        if let Some(state) = servers.iter_mut().find(|state| state.name == name) {
            if state.epoch == epoch {
                state.driver = Some(driver);
            }
        }
        true
    }

    /// Connect a server that has no driver yet (the lazy path used by `call`
    /// and the tool listing when `start()` was never called).
    fn ensure_driver(&self, name: &str) {
        if *self.shutdown_rx.borrow() {
            return;
        }
        let owned = self
            .servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .map(|state| state.attempting)
            .unwrap_or(true);
        if owned {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(server = %name, "mcp: no tokio runtime available to start the connection");
            return;
        };
        self.spawn_driver(name, &handle);
    }

    /// Own one server's connection from connect through teardown.
    async fn drive_server(self: Arc<Self>, name: String, generation: u64, epoch: u64) {
        let mut attempt = self.attempt_connect(&name, generation, epoch).await;
        let mut ladders = 0usize;
        loop {
            match attempt {
                // A failed initial attempt, an exhausted reconnect ladder or a
                // superseded driver is terminal for this task: the next explicit
                // reload/reconnect starts a new one.
                Attempt::Failed | Attempt::Stale => {
                    self.finish_driver(&name, generation, epoch);
                    return;
                }
                Attempt::Connected(connection, mut events) => {
                    while let Some(event) = events.recv().await {
                        if !self.is_current(&name, generation, epoch) {
                            break;
                        }
                        match event {
                            ServerEvent::Notification { method, params: _ } => {
                                if method == TOOLS_LIST_CHANGED {
                                    self.driver_update(&name, generation, epoch, |state| {
                                        state.tools_stale = true;
                                    });
                                    tracing::debug!(
                                        server = %name,
                                        "mcp: server announced a tool list change"
                                    );
                                }
                            }
                            ServerEvent::Closed { error } => {
                                self.mark_connection_loss(&name, generation, epoch, error);
                                break;
                            }
                        }
                    }
                    // Idempotent and cheap on an already dead transport; it also
                    // stops a server that keeps its stdout open after the
                    // connection was superseded.
                    if connection.is_alive() {
                        connection.close().await;
                    }
                    if !self.is_current(&name, generation, epoch) {
                        self.finish_driver(&name, generation, epoch);
                        return;
                    }
                    self.mark_connection_loss(&name, generation, epoch, None);
                    if ladders >= MAX_RECOVERY_LADDERS {
                        tracing::warn!(
                            server = %name,
                            "mcp: giving up automatic reconnection after {} attempts",
                            MAX_RECOVERY_LADDERS * RECONNECT_BACKOFF.len()
                        );
                        self.finish_driver(&name, generation, epoch);
                        return;
                    }
                    ladders += 1;
                    attempt = self.reconnect_ladder(&name, generation, epoch).await;
                }
            }
        }
    }

    /// Walk the backoff ladder once; every step is one full connect attempt.
    async fn reconnect_ladder(&self, name: &str, generation: u64, epoch: u64) -> Attempt {
        for (step, delay) in RECONNECT_BACKOFF.iter().enumerate() {
            if !self.is_current(name, generation, epoch) {
                return Attempt::Stale;
            }
            tracing::debug!(
                server = %name,
                attempt = step + 1,
                "mcp: reconnecting in {}ms",
                delay.as_millis()
            );
            if self.sleep_interruptible(*delay).await {
                return Attempt::Stale;
            }
            if !self.is_current(name, generation, epoch) {
                return Attempt::Stale;
            }
            match self.attempt_connect(name, generation, epoch).await {
                Attempt::Failed => continue,
                settled => return settled,
            }
        }
        tracing::warn!(
            server = %name,
            "mcp: reconnection failed after {} attempts",
            RECONNECT_BACKOFF.len()
        );
        Attempt::Failed
    }

    async fn attempt_connect(&self, name: &str, generation: u64, epoch: u64) -> Attempt {
        let Some(lock) = self.connect_lock_of(name) else {
            return Attempt::Stale;
        };
        // One attempt at a time per server; a second caller that got here anyway
        // re-checks the state below and abandons the work.
        let _guard = lock.lock().await;
        if !self.is_current(name, generation, epoch) {
            return Attempt::Stale;
        }
        if self.has_live_incarnation(name) {
            return Attempt::Stale;
        }
        let Some(settings) = self.enabled_settings(name) else {
            return Attempt::Stale;
        };

        self.driver_update(name, generation, epoch, |state| {
            state.status.send_replace(ServerStatus::Connecting);
        });

        // A handshake that is still in flight when the driver is superseded has
        // to end with it: a server that never answers `initialize` would
        // otherwise keep its driver - and its child - alive past the
        // `shutdown()`/`reload()`/disable that retired this attempt.
        let cancel = Arc::new(CancellationToken::new());
        self.driver_update(name, generation, epoch, |state| {
            state.connect_cancel = Some(cancel.clone());
        });
        // The write above is dropped when the driver was superseded first;
        // cancelling the local token still ends the connect instead of letting
        // it run to its own deadline.
        if !self.is_current(name, generation, epoch) {
            cancel.cancel();
        }

        let (events, events_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let driver_events = events.clone();
        let options = ConnectOptions {
            roots: advertised_roots(),
            cancel: Some(CancelListener::new(cancel.clone())),
            events,
        };
        let outcome = client::connect(&settings, options).await;
        self.driver_update(name, generation, epoch, |state| {
            state.connect_cancel = None;
        });

        match outcome {
            Ok(connection) => {
                let version = connection.protocol_version().to_string();
                let instructions = connection.instructions().is_some();
                match connection.list_tools(None).await {
                    Ok(tools) => {
                        let connection = Arc::new(connection);
                        if !self
                            .install_or_reap(
                                name,
                                generation,
                                epoch,
                                &connection,
                                tools,
                                instructions,
                                version,
                                driver_events,
                            )
                            .await
                        {
                            return Attempt::Stale;
                        }
                        Attempt::Connected(connection, events_rx)
                    }
                    Err(error) => {
                        // A server that cannot list its tools is not usable, so
                        // it counts as a failed connection.
                        connection.close().await;
                        self.report_failure(name, generation, epoch, &error);
                        Attempt::Failed
                    }
                }
            }
            Err(error) => {
                self.report_failure(name, generation, epoch, &error);
                Attempt::Failed
            }
        }
    }

    /// Install a session a driver just established.
    ///
    /// Returns `false` when the state no longer accepts writes from that driver
    /// (a superseded generation, a bumped epoch, a shutdown); see
    /// [`McpManager::install_or_reap`], which callers should use.
    #[allow(clippy::too_many_arguments)]
    fn install_session(
        &self,
        name: &str,
        generation: u64,
        epoch: u64,
        connection: &Arc<Connection>,
        tools: Vec<McpToolDefinition>,
        instructions: bool,
        protocol_version: String,
        driver_events: mpsc::UnboundedSender<ServerEvent>,
    ) -> bool {
        let count = tools.len();
        self.driver_update(name, generation, epoch, |state| {
            state.connection = Some(connection.clone());
            state.tools = Some(tools);
            state.tools_stale = false;
            state.instructions = instructions;
            state.events = Some(driver_events);
            state.status.send_replace(ServerStatus::Connected {
                protocol_version,
                tool_count: count,
            });
        })
    }

    /// Install a session a driver just established, or reap it when the state
    /// no longer accepts writes from that driver.
    ///
    /// A refused session has no owner: returning it anyway would let a live
    /// connection - and, for stdio, its child process - outlive the state that
    /// was supposed to own it, because the driver only closes the connection
    /// once its event stream ends and nothing would ever end it.
    #[allow(clippy::too_many_arguments)]
    async fn install_or_reap(
        &self,
        name: &str,
        generation: u64,
        epoch: u64,
        connection: &Arc<Connection>,
        tools: Vec<McpToolDefinition>,
        instructions: bool,
        protocol_version: String,
        driver_events: mpsc::UnboundedSender<ServerEvent>,
    ) -> bool {
        if self.install_session(
            name,
            generation,
            epoch,
            connection,
            tools,
            instructions,
            protocol_version,
            driver_events,
        ) {
            return true;
        }
        tracing::debug!(
            server = %name,
            "mcp: a session finished after its driver was superseded; reaping it"
        );
        connection.close().await;
        false
    }

    fn report_failure(&self, name: &str, generation: u64, epoch: u64, error: &McpError) {
        // `describe()` is already credential-free: `McpError` sanitizes every
        // free-text field before it is stored.
        let reason = error.describe();
        tracing::warn!(server = %name, "mcp: connection failed: {reason}");
        self.driver_update(name, generation, epoch, |state| {
            state.connection = None;
            state.status.send_replace(ServerStatus::Failed { reason });
        });
    }

    fn mark_connection_loss(
        &self,
        name: &str,
        generation: u64,
        epoch: u64,
        error: Option<McpError>,
    ) {
        let reason = match &error {
            Some(error) => error.describe(),
            None => "the connection was closed".to_string(),
        };
        if let Some(error) = &error {
            tracing::warn!(
                server = %name,
                "mcp: connection lost: {}",
                error.describe()
            );
        } else {
            tracing::debug!(server = %name, "mcp: connection closed by the server");
        }
        self.driver_update(name, generation, epoch, |state| {
            state.connection = None;
            if !matches!(&*state.status.borrow(), ServerStatus::Failed { .. }) {
                state.status.send_replace(ServerStatus::Failed { reason });
            }
        });
    }

    fn finish_driver(&self, name: &str, generation: u64, epoch: u64) {
        self.driver_update(name, generation, epoch, |state| {
            state.attempting = false;
            state.driver = None;
        });
    }

    fn clear_attempting(&self, name: &str) {
        let mut servers = self.servers.write();
        if let Some(state) = servers.iter_mut().find(|state| state.name == name) {
            state.attempting = false;
        }
    }

    /// Apply a state change only when the driver still owns the server.
    ///
    /// Returns `true` when the write was applied. A `false` means the caller's
    /// driver was superseded (generation bumped, epoch bumped, shutdown) and it
    /// must not treat the state as its own - see [`McpManager::install_session`].
    fn driver_update(
        &self,
        name: &str,
        generation: u64,
        epoch: u64,
        update: impl FnOnce(&mut ServerState),
    ) -> bool {
        if self.generation.load(Ordering::SeqCst) != generation {
            return false;
        }
        if *self.shutdown_rx.borrow() {
            return false;
        }
        let mut servers = self.servers.write();
        let Some(state) = servers.iter_mut().find(|state| state.name == name) else {
            return false;
        };
        if state.epoch != epoch {
            return false;
        }
        update(state);
        true
    }

    fn is_current(&self, name: &str, generation: u64, epoch: u64) -> bool {
        if *self.shutdown_rx.borrow() {
            return false;
        }
        if self.generation.load(Ordering::SeqCst) != generation {
            return false;
        }
        self.servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .map(|state| state.epoch == epoch)
            .unwrap_or(false)
    }

    fn connect_lock_of(&self, name: &str) -> Option<Arc<Mutex<()>>> {
        self.servers
            .read()
            .iter()
            .find(|state| state.name == name)
            .map(|state| state.connect_lock.clone())
    }

    /// Sleep, unless the runtime was shut down or reloaded; `true` means
    /// "give up": either event supersedes the driver that is sleeping.
    async fn sleep_interruptible(&self, delay: Duration) -> bool {
        let mut shutdown = self.shutdown_tx.subscribe();
        if *shutdown.borrow() {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => false,
            _ = shutdown.changed() => true,
        }
    }

    // -----------------------------------------------------------------------
    // Teardown helpers
    // -----------------------------------------------------------------------

    /// Close connections under a global and a per-server deadline.
    async fn close_connections(&self, connections: Vec<(String, Arc<Connection>)>) {
        let deadline = Instant::now() + SHUTDOWN_BUDGET;
        for (name, connection) in connections {
            let close_deadline = (Instant::now() + CLOSE_BUDGET).min(deadline);
            if tokio::time::timeout_at(close_deadline, connection.close())
                .await
                .is_err()
            {
                tracing::warn!(
                    server = %name,
                    "mcp: transport did not close within {}ms",
                    CLOSE_BUDGET.as_millis()
                );
            }
        }
    }

    /// Wait for drivers to observe the new generation; never past the budget.
    async fn retire_drivers(&self, drivers: Vec<JoinHandle<()>>) {
        if drivers.is_empty() {
            return;
        }
        let pending = futures::future::join_all(drivers);
        if tokio::time::timeout_at(Instant::now() + SHUTDOWN_BUDGET, pending)
            .await
            .is_err()
        {
            tracing::warn!(
                "mcp: background tasks did not stop within {}ms",
                SHUTDOWN_BUDGET.as_millis()
            );
        }
    }

    /// Invalidate every driver and clear every per-server runtime field.
    ///
    /// `restart` decides what an enabled server shows afterwards: `Connecting`
    /// for `reload()` (a fresh driver is about to start) and `Disabled` for
    /// `shutdown()` (nothing will connect again until a reload).
    #[allow(clippy::type_complexity)]
    fn reset_runtime(
        &self,
        restart: bool,
    ) -> (
        Vec<(String, Arc<Connection>)>,
        Vec<JoinHandle<()>>,
        Vec<String>,
    ) {
        let mut connections = Vec::new();
        let mut drivers = Vec::new();
        let mut enabled = Vec::new();
        let mut servers = self.servers.write();
        // Lock order: `servers` first, then `settings`.
        let settings = self.settings.read();
        for state in servers.iter_mut() {
            state.epoch += 1;
            state.attempting = false;
            cancel_pending_connect(state);
            if let Some(connection) = state.connection.take() {
                connections.push((state.name.clone(), connection));
            }
            if let Some(driver) = state.driver.take() {
                drivers.push(driver);
            }
            state.tools = None;
            state.tools_stale = false;
            state.instructions = false;
            let runnable = settings
                .server(&state.name)
                .map(|server| server.enabled)
                .unwrap_or(false);
            let status = if restart {
                initial_status(settings.enabled, runnable)
            } else {
                ServerStatus::Disabled
            };
            state.status.send_replace(status);
            if restart && runnable && settings.enabled {
                enabled.push(state.name.clone());
            }
        }
        (connections, drivers, enabled)
    }

    /// Same as [`McpManager::reset_runtime`] for one server.
    fn reset_server(
        &self,
        name: &str,
        connecting: bool,
    ) -> (Option<Arc<Connection>>, Option<JoinHandle<()>>) {
        let mut servers = self.servers.write();
        let Some(state) = servers.iter_mut().find(|state| state.name == name) else {
            return (None, None);
        };
        state.epoch += 1;
        state.attempting = false;
        cancel_pending_connect(state);
        state.tools = None;
        state.tools_stale = false;
        state.instructions = false;
        if connecting {
            state.status.send_replace(ServerStatus::Connecting);
        }
        (state.connection.take(), state.driver.take())
    }

    /// Wait for a status change to leave `Connecting`.
    async fn wait_settled(&self, name: &str, deadline: Instant) -> Settled {
        let Some(mut status_rx) = self.status_watch(name) else {
            return Settled::Unavailable;
        };
        loop {
            // See `wait_for_connection`: one receiver, marked as seen on every
            // pass, is what makes `changed()` block instead of spin.
            let status = status_rx.borrow_and_update().clone();
            match status {
                ServerStatus::Connected { .. } => return Settled::Connected,
                ServerStatus::Failed { reason } => return Settled::Failed(reason),
                ServerStatus::Disabled => return Settled::Unavailable,
                ServerStatus::Connecting => {}
            }
            match tokio::time::timeout_at(deadline, status_rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Settled::Unavailable,
                Err(_) => return Settled::Timeout,
            }
        }
    }

    /// Test-only: put one server into a state without a real connection, so
    /// status rendering can be exercised for states that need a live fixture.
    #[cfg(test)]
    pub(crate) fn force_state(
        &self,
        server: &str,
        status: ServerStatus,
        tools: Option<Vec<McpToolDefinition>>,
    ) {
        let mut servers = self.servers.write();
        let Some(state) = servers.iter_mut().find(|state| state.name == server) else {
            return;
        };
        state.instructions = tools.is_some();
        state.tools = tools;
        state.tools_stale = false;
        state.status.send_replace(status);
    }
}

impl McpRuntime for McpManager {
    fn snapshots(&self) -> Vec<ServerSnapshot> {
        self.collect_snapshots()
    }

    fn server_settings(&self, server: &str) -> Option<ServerSettings> {
        self.enabled_settings(server)
    }

    fn cached_tools(&self, server: &str) -> Option<Vec<McpToolDefinition>> {
        self.cached_tools_of(server)
    }

    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        args: &'a serde_json::Value,
        cancel: Option<Arc<CancelListener>>,
    ) -> BoxFuture<'a, Result<ToolCallResult, McpError>> {
        Box::pin(self.invoke_tool(server, tool, args, cancel))
    }

    fn refresh_tools<'a>(
        &'a self,
        server: &'a str,
        cancel: Option<Arc<CancelListener>>,
    ) -> BoxFuture<'a, Result<Vec<McpToolDefinition>, McpError>> {
        Box::pin(self.refresh_server_tools(server, cancel))
    }

    fn tools_are_stale(&self, server: &str) -> bool {
        // The inherent method of the same name is the state lookup; method
        // call syntax would resolve here instead.
        McpManager::tools_are_stale(self, server)
    }
}

/// Runtime entry for one server, with its status channel wired up.
fn new_server_state(name: String, status: ServerStatus) -> ServerState {
    let (status_tx, status_rx) = watch::channel(status);
    ServerState {
        name,
        status: status_tx,
        status_rx,
        tools: None,
        tools_stale: false,
        instructions: false,
        connection: None,
        events: None,
        connect_cancel: None,
        attempting: false,
        epoch: 0,
        connect_lock: Arc::new(Mutex::new(())),
        driver: None,
    }
}

/// Take one server's runtime entry out of the pool being reconciled.
fn take_server_state(states: &mut Vec<ServerState>, name: &str) -> Option<ServerState> {
    let index = states.iter().position(|state| state.name == name)?;
    Some(states.remove(index))
}

/// Supersede one entry's driver and take its connection and task.
///
/// The epoch is bumped before anything is closed, which is what stops the
/// retiring driver from writing to the state its replacement will own. The
/// cached catalogue goes with the connection: the replacement is a new server
/// incarnation and may expose a different tool list.
fn retire_server_state(
    state: &mut ServerState,
) -> (Option<Arc<Connection>>, Option<JoinHandle<()>>) {
    state.epoch += 1;
    state.attempting = false;
    cancel_pending_connect(state);
    state.tools = None;
    state.tools_stale = false;
    state.instructions = false;
    (state.connection.take(), state.driver.take())
}

/// End a handshake that is still in flight for this entry.
///
/// The connect future observes the token, gives up and closes its transport, so
/// a superseded attempt cannot leave a subprocess behind. Idempotent: a second
/// call finds nothing to cancel.
fn cancel_pending_connect(state: &mut ServerState) {
    if let Some(cancel) = state.connect_cancel.take() {
        cancel.cancel();
    }
}

/// `true` when two revisions of one server describe the same runtime behavior,
/// so its connection can be kept instead of rebuilt.
fn same_server_settings(left: &ServerSettings, right: &ServerSettings) -> bool {
    left.name == right.name
        && left.transport == right.transport
        && left.enabled == right.enabled
        && left.command == right.command
        && left.args == right.args
        && left.env == right.env
        && left.cwd == right.cwd
        && left.url == right.url
        && left.headers == right.headers
        && left.request_timeout == right.request_timeout
        && left.trust == right.trust
        && left.side_effects == right.side_effects
        && left.tool_trust == right.tool_trust
        && left.max_output_bytes == right.max_output_bytes
}

/// Status an enabled server has before its driver ran.
fn initial_status(mcp_enabled: bool, server_enabled: bool) -> ServerStatus {
    if mcp_enabled && server_enabled {
        ServerStatus::Connecting
    } else {
        ServerStatus::Disabled
    }
}

fn is_connected(status: &ServerStatus) -> bool {
    matches!(status, ServerStatus::Connected { .. })
}

/// Filesystem root advertised to servers: the working directory the shell was
/// started in. It is a URL-free path, so nothing here can leak a credential.
fn advertised_roots() -> Vec<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string())
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::types::{
        render_tool_result, SideEffects, TrustLevel, DEFAULT_MAX_OUTPUT_BYTES, MCP_PROTOCOL_VERSION,
    };

    /// A `/bin/sh` MCP server: it answers `initialize`, `tools/list` and
    /// `tools/call`, counts every `tools/list` in `$1`, and announces
    /// `notifications/tools/list_changed` right after each listing so the tests
    /// can observe cache invalidation.
    const FIXTURE_SERVER: &str = r#"count="$1"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0.1.0"},"instructions":"fixture instructions"}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      served=0
      if [ -f "$count" ]; then served=$(cat "$count"); fi
      served=$((served + 1))
      printf '%s\n' "$served" > "$count"
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo the input text back","annotations":{"readOnlyHint":true}},{"name":"write","description":"Write a file to disk","annotations":{"destructiveHint":true}}]}}\n' "$id"
      printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'
      ;;
    *'"method":"tools/call"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"called"}],"isError":false}}\n' "$id"
      ;;
  esac
done"#;

    fn fixture_settings(counter: &Path, name: &str) -> ServerSettings {
        script_settings(FIXTURE_SERVER, counter, name)
    }

    /// A server that serves normally, can be told to drop the connection by
    /// creating `<counter>.die`, fails the next start attempt outright and then
    /// serves again: walking the bounded backoff ladder is the only way aish
    /// recovers it.
    const RECOVERING_SERVER: &str = r#"count="$1"
die="$1.die"
served=0
if [ -f "$count" ]; then served=$(cat "$count"); fi
served=$((served + 1))
printf '%s\n' "$served" > "$count"
if [ "$served" -eq 2 ]; then exit 1; fi
if [ "$served" -eq 1 ]; then
  ( while [ ! -f "$die" ]; do sleep 0.05; done; kill -9 $$ ) &
fi
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"recovering","version":"0.1.0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo the input text back","annotations":{"readOnlyHint":true}}]}}\n' "$id"
      ;;
  esac
done"#;

    /// Settings for an arbitrary `/bin/sh` fixture script, which sees
    /// `argument` as `$1`.
    fn script_settings(script: &str, argument: &Path, name: &str) -> ServerSettings {
        ServerSettings {
            name: name.to_string(),
            transport: TransportKind::Stdio,
            enabled: true,
            command: Some("/bin/sh".to_string()),
            args: vec![
                "-c".to_string(),
                script.to_string(),
                "aish-mcp-fixture".to_string(),
                argument.display().to_string(),
            ],
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            request_timeout: Some(Duration::from_secs(10)),
            trust: TrustLevel::Ask,
            side_effects: SideEffects::Read,
            tool_trust: BTreeMap::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    fn manager_for(servers: Vec<ServerSettings>) -> Arc<McpManager> {
        McpManager::new(McpSettings {
            enabled: true,
            request_timeout: Some(Duration::from_secs(10)),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            servers,
        })
    }

    fn snapshot_of(manager: &McpManager, name: &str) -> ServerSnapshot {
        manager
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.name == name)
            .unwrap_or_else(|| panic!("server `{name}` must have a snapshot"))
    }

    async fn wait_until<T>(mut check: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(value) = check() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the fixture"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn wait_connected(manager: &McpManager, name: &str) {
        wait_until(|| match snapshot_of(manager, name).status {
            ServerStatus::Connected { .. } => Some(()),
            ServerStatus::Failed { reason } => {
                panic!("server `{name}` failed to connect: {reason}")
            }
            _ => None,
        })
        .await
    }

    fn tools_list_count(counter: &Path) -> u64 {
        std::fs::read_to_string(counter)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    /// A server that lost its connection is recovered by the bounded backoff
    /// ladder; a regression here would leave it `Failed` until the user
    /// intervenes, even though it is healthy again.
    #[tokio::test]
    async fn a_lost_connection_is_recovered_through_the_backoff_ladder() {
        let counter = tempfile::NamedTempFile::new().expect("counter file");
        let name = "recovering";
        let manager = manager_for(vec![script_settings(
            RECOVERING_SERVER,
            counter.path(),
            name,
        )]);
        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, name).await;

        // Tell the child to exit: the driver sees EOF, the ladder's next start
        // attempt fails on purpose, and the third one has to succeed.
        let die = counter.path().with_file_name(format!(
            "{}.die",
            counter
                .path()
                .file_name()
                .expect("temp file name")
                .to_string_lossy()
        ));
        std::fs::write(&die, "die").expect("die marker");

        let snapshot = wait_until(|| {
            let attempts = tools_list_count(counter.path());
            let snapshot = snapshot_of(&manager, name);
            (attempts >= 3 && matches!(snapshot.status, ServerStatus::Connected { .. }))
                .then_some(snapshot)
        })
        .await;
        let _ = std::fs::remove_file(&die);
        assert!(
            snapshot.tools.iter().any(|tool| tool.name == "echo"),
            "the recovered connection must expose the catalogue"
        );
        manager.shutdown().await;
        assert_eq!(
            tools_list_count(counter.path()),
            3,
            "attempt 1 served, attempt 2 failed, attempt 3 served"
        );
    }

    fn tools_are_stale(manager: &McpManager, name: &str) -> bool {
        manager.tools_are_stale(name)
    }

    #[test]
    fn the_manager_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<McpManager>();
        assert_send_sync::<Arc<dyn McpRuntime>>();
    }

    #[test]
    fn an_unstarted_manager_reports_every_configured_server() {
        let mut inactive = fixture_settings(Path::new("/tmp/aish-mcp-unused"), "inactive");
        inactive.enabled = false;
        let manager = manager_for(vec![
            inactive,
            fixture_settings(Path::new("/tmp/aish-mcp-unused"), "lazy"),
        ]);

        let snapshots = manager.snapshots();
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.name.as_str())
                .collect::<Vec<_>>(),
            ["inactive", "lazy"]
        );
        // Disabled stays disabled; enabled but not started is `Connecting`,
        // because the first call is what starts it.
        assert_eq!(snapshots[0].status, ServerStatus::Disabled);
        assert_eq!(snapshots[1].status, ServerStatus::Connecting);
        assert!(snapshots.iter().all(|snapshot| snapshot.tools.is_empty()));
        assert!(manager.is_active());
        assert_eq!(manager.servers().len(), 2);
        assert!(manager.server("lazy").is_some());
        assert!(manager.server("missing").is_none());
        assert!(manager.server_settings("inactive").is_none());
        assert!(manager.server_settings("lazy").is_some());
        assert!(manager.cached_tools("lazy").is_none());
    }

    #[tokio::test]
    async fn calling_an_unknown_server_is_a_connection_loss() {
        let manager = manager_for(Vec::new());
        let error = manager
            .call("ghost", "echo", &json!({}), None)
            .await
            .expect_err("an unknown server must fail");
        assert!(error.is_connection_loss());
        assert_eq!(error.server, "ghost");
        assert_eq!(error.stage, McpFailureStage::Connect);
        assert_eq!(error.failure, McpFailureClass::Connect);
        assert!(
            error.message.contains("not connected"),
            "unexpected message: {}",
            error.message
        );

        // The same holds for a configured but disabled server.
        let mut disabled = fixture_settings(Path::new("/tmp/aish-mcp-unused"), "off");
        disabled.enabled = false;
        let manager = manager_for(vec![disabled]);
        let error = manager
            .call("off", "echo", &json!({}), None)
            .await
            .expect_err("a disabled server must fail");
        assert!(error.is_connection_loss());
        assert!(
            error.message.contains("the server is disabled"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn connects_calls_and_shuts_down_a_fixture_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("tools-list-count");
        let manager = manager_for(vec![fixture_settings(&counter, "fixture")]);

        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, "fixture").await;

        let snapshot = snapshot_of(&manager, "fixture");
        assert_eq!(
            snapshot.status,
            ServerStatus::Connected {
                protocol_version: MCP_PROTOCOL_VERSION.to_string(),
                tool_count: 2,
            }
        );
        assert_eq!(snapshot.tools.len(), 2);
        assert!(snapshot.instructions);
        assert_eq!(snapshot.transport, TransportKind::Stdio);
        assert_eq!(snapshot.trust, TrustLevel::Ask);
        assert_eq!(snapshot.declared_side_effects, SideEffects::Read);
        // The endpoint is the redacted argv label, never env or credential data.
        assert!(
            snapshot.endpoint.starts_with("/bin/sh -c "),
            "{}",
            snapshot.endpoint
        );

        // Server annotations raise a tool above the declared scope; silence
        // keeps the declaration.
        let echo = snapshot
            .tools
            .iter()
            .find(|tool| tool.name == "echo")
            .expect("the fixture exposes `echo`");
        assert_eq!(echo.side_effects, SideEffects::Read);
        assert_eq!(
            echo.description.as_deref(),
            Some("Echo the input text back")
        );
        let write = snapshot
            .tools
            .iter()
            .find(|tool| tool.name == "write")
            .expect("the fixture exposes `write`");
        assert_eq!(write.side_effects, SideEffects::Write);

        let tools = manager
            .cached_or_fetch_tools("fixture", None)
            .await
            .expect("tools");
        assert_eq!(tools.len(), 2);

        let result = manager
            .call("fixture", "echo", &json!({"text": "hi"}), None)
            .await
            .expect("tool call");
        assert!(!result.is_error);
        assert_eq!(render_tool_result(&result, 4096), "called");

        let connection = {
            let servers = manager.servers.read();
            servers
                .iter()
                .find(|state| state.name == "fixture")
                .and_then(|state| state.connection.clone())
                .expect("a live connection")
        };
        assert!(connection.is_alive());

        manager.shutdown().await;
        assert!(
            manager.is_active(),
            "the config stays active after a shutdown"
        );
        assert!(!connection.is_alive(), "shutdown must close the transport");
        assert!(
            manager
                .call("fixture", "echo", &json!({}), None)
                .await
                .is_err(),
            "a shut down runtime must not serve calls"
        );

        let servers = manager.servers.read();
        let state = servers
            .iter()
            .find(|state| state.name == "fixture")
            .expect("the server entry stays");
        assert!(state.connection.is_none());
        assert!(!state.attempting, "no driver may stay in flight");
        assert!(state.tools.is_none());
        assert_eq!(*state.status.borrow(), ServerStatus::Disabled);
    }

    #[tokio::test]
    async fn a_tool_list_change_invalidates_the_cached_catalogue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("tools-list-count");
        let manager = manager_for(vec![fixture_settings(&counter, "fixture")]);

        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, "fixture").await;
        // The fixture announces a change after the listing the handshake made.
        wait_until(|| tools_are_stale(&manager, "fixture").then_some(())).await;
        assert_eq!(
            tools_list_count(&counter),
            1,
            "only the start-up listing ran"
        );

        // A stale cache stays visible in status output...
        assert_eq!(snapshot_of(&manager, "fixture").tools.len(), 2);
        // ...but asking for tools pulls the catalogue again.
        let tools = manager
            .cached_or_fetch_tools("fixture", None)
            .await
            .expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools_list_count(&counter),
            2,
            "the notification must force a new tools/list"
        );

        manager.shutdown().await;
    }

    #[test]
    fn force_state_is_visible_in_snapshots() {
        let manager = manager_for(vec![fixture_settings(
            Path::new("/tmp/aish-mcp-unused"),
            "fixture",
        )]);
        manager.force_state(
            "fixture",
            ServerStatus::Failed {
                reason: "connect connect: refused".to_string(),
            },
            None,
        );
        let snapshot = snapshot_of(&manager, "fixture");
        assert_eq!(
            snapshot.status,
            ServerStatus::Failed {
                reason: "connect connect: refused".to_string()
            }
        );
        assert!(snapshot.tools.is_empty());
        assert!(!snapshot.instructions);
    }

    // -----------------------------------------------------------------------
    // Runtime reconfiguration
    // -----------------------------------------------------------------------

    /// A `/bin/sh` MCP server that answers `initialize`, `tools/list` and
    /// `tools/call`, and writes `$1` when it exits, so a test can prove that a
    /// closed transport really ended the child process.
    const EXIT_MARKER_SERVER: &str = r#"marker="$1"
mark() { printf 'stopped\n' > "$marker"; }
trap 'mark; exit 0' TERM
trap 'mark' EXIT
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"marker","version":"0.1.0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo the input text back","annotations":{"readOnlyHint":true}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"called"}],"isError":false}}\n' "$id"
      ;;
  esac
done"#;

    /// Settings for the marker fixture, named `name`, recording its exit in
    /// `marker`.
    fn marker_settings(marker: &Path, name: &str) -> ServerSettings {
        let mut settings = fixture_settings(Path::new("/tmp/aish-mcp-unused"), name);
        settings.args = vec![
            "-c".to_string(),
            EXIT_MARKER_SERVER.to_string(),
            "aish-mcp-marker".to_string(),
            marker.display().to_string(),
        ];
        settings
    }

    fn live_connection(manager: &McpManager, name: &str) -> Arc<Connection> {
        manager
            .live_connection(name)
            .unwrap_or_else(|| panic!("server `{name}` must have a live connection"))
    }

    fn running_names(manager: &McpManager) -> Vec<String> {
        manager
            .snapshots()
            .into_iter()
            .map(|snapshot| snapshot.name)
            .collect()
    }

    fn marker_appeared(marker: &Path) -> bool {
        marker.exists()
    }

    #[tokio::test]
    async fn add_server_starts_a_new_connection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("stopped");
        let manager = manager_for(Vec::new());
        assert!(manager.snapshots().is_empty());

        assert!(manager.add_server(marker_settings(&marker, "late")));
        assert_eq!(running_names(&manager), ["late"]);
        assert_eq!(manager.servers().len(), 1);
        assert!(manager.server_settings("late").is_some());

        wait_connected(&manager, "late").await;
        let snapshot = snapshot_of(&manager, "late");
        assert_eq!(snapshot.tools.len(), 1);
        assert_eq!(snapshot.tools[0].name, "echo");

        let tools = manager
            .cached_or_fetch_tools("late", None)
            .await
            .expect("tools");
        assert_eq!(tools.len(), 1);
        let result = manager
            .call("late", "echo", &json!({"text": "hi"}), None)
            .await
            .expect("tool call");
        assert_eq!(render_tool_result(&result, 4096), "called");

        // The same name a second time is refused and changes nothing.
        assert!(!manager.add_server(marker_settings(&marker, "late")));
        assert_eq!(manager.servers().len(), 1);
        assert_eq!(running_names(&manager), ["late"]);

        assert!(!marker_appeared(&marker), "the fixture is still running");
        manager.shutdown().await;
        wait_until(|| marker_appeared(&marker).then_some(())).await;
    }

    #[tokio::test]
    async fn disabling_closes_the_child_and_enabling_connects_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("stopped");
        let manager = manager_for(vec![marker_settings(&marker, "fixture")]);

        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, "fixture").await;
        let connection = live_connection(&manager, "fixture");
        assert!(connection.is_alive());
        assert!(!marker_appeared(&marker));

        assert!(manager.set_server_enabled("fixture", false).await);
        // The server keeps its configuration entry ...
        let configured = manager.server("fixture").expect("the entry stays");
        assert!(!configured.enabled);
        assert!(!manager.is_active());
        // ... but leaves the runtime, with its transport closed.
        assert!(manager.snapshots().is_empty());
        assert!(manager.server_settings("fixture").is_none());
        assert!(!connection.is_alive(), "the transport must be closed");
        wait_until(|| marker_appeared(&marker).then_some(())).await;

        // Enabling it again connects a fresh child process.
        assert!(manager.set_server_enabled("fixture", true).await);
        assert!(manager.is_active());
        wait_connected(&manager, "fixture").await;
        assert_eq!(running_names(&manager), ["fixture"]);
        assert_eq!(snapshot_of(&manager, "fixture").tools.len(), 1);
        assert!(live_connection(&manager, "fixture").is_alive());

        assert!(manager.set_server_enabled("fixture", true).await);
        assert!(!manager.set_server_enabled("ghost", true).await);

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn remove_server_drops_the_transport_and_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gone_marker = dir.path().join("gone");
        let kept_marker = dir.path().join("kept");
        let manager = manager_for(vec![
            marker_settings(&gone_marker, "gone"),
            marker_settings(&kept_marker, "kept"),
        ]);

        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, "gone").await;
        wait_connected(&manager, "kept").await;
        let kept_connection = live_connection(&manager, "kept");

        assert!(manager.remove_server("gone").await);
        assert_eq!(running_names(&manager), ["kept"]);
        assert!(manager.server("gone").is_none());
        assert!(manager.server_settings("gone").is_none());
        assert!(!manager.remove_server("gone").await);
        // The surviving server kept the connection it had.
        assert!(kept_connection.is_alive());
        assert!(Arc::ptr_eq(
            &kept_connection,
            &live_connection(&manager, "kept")
        ));
        wait_until(|| marker_appeared(&gone_marker).then_some(())).await;
        assert!(!marker_appeared(&kept_marker));

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn apply_settings_classifies_added_removed_and_updated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        let removed_marker = dir.path().join("removed");
        let added_marker = dir.path().join("added");
        let manager = manager_for(vec![
            marker_settings(&first, "one"),
            marker_settings(&second, "two"),
            marker_settings(&removed_marker, "three"),
        ]);

        manager.start(&tokio::runtime::Handle::current());
        for name in ["one", "two", "three"] {
            wait_connected(&manager, name).await;
        }
        let one_connection = live_connection(&manager, "one");

        // `one` is byte-for-byte the same, `two` gains a timeout, `three` is
        // gone and `four` is new.
        let mut changed = marker_settings(&second, "two");
        changed.request_timeout = Some(Duration::from_secs(4));
        let outcome = manager
            .apply_settings(McpSettings {
                enabled: true,
                request_timeout: Some(Duration::from_secs(10)),
                max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
                servers: vec![
                    marker_settings(&first, "one"),
                    changed,
                    marker_settings(&added_marker, "four"),
                ],
            })
            .await;

        assert_eq!(outcome.added, ["four"]);
        assert_eq!(outcome.removed, ["three"]);
        assert_eq!(outcome.updated, ["two"]);
        assert_eq!(manager.servers().len(), 3);

        // The untouched server keeps its connection; the changed and the new
        // one connect to fresh child processes.
        assert!(Arc::ptr_eq(
            &one_connection,
            &live_connection(&manager, "one")
        ));
        wait_connected(&manager, "two").await;
        wait_connected(&manager, "four").await;
        assert_eq!(
            manager
                .server("two")
                .and_then(|server| server.request_timeout),
            Some(Duration::from_secs(4))
        );
        wait_until(|| marker_appeared(&removed_marker).then_some(())).await;

        // Applying the very same settings again changes nothing: same
        // outcome, same connections, no new child process.
        let two_connection = live_connection(&manager, "two");
        let four_connection = live_connection(&manager, "four");
        let outcome = manager.apply_settings(manager.settings()).await;
        assert_eq!(outcome, ApplyOutcome::default());
        assert_eq!(running_names(&manager), ["one", "two", "four"]);
        assert!(Arc::ptr_eq(
            &one_connection,
            &live_connection(&manager, "one")
        ));
        assert!(Arc::ptr_eq(
            &two_connection,
            &live_connection(&manager, "two")
        ));
        assert!(Arc::ptr_eq(
            &four_connection,
            &live_connection(&manager, "four")
        ));

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_apply_settings_commits_one_consistent_revision() {
        let dir = tempfile::tempdir().expect("tempdir");
        let alpha = dir.path().join("alpha");
        let beta = dir.path().join("beta");
        let gamma = dir.path().join("gamma");
        let manager = manager_for(vec![marker_settings(&alpha, "alpha")]);
        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, "alpha").await;

        let mut changed = marker_settings(&alpha, "alpha");
        changed.request_timeout = Some(Duration::from_secs(4));
        let with_beta = McpSettings {
            enabled: true,
            request_timeout: Some(Duration::from_secs(10)),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            servers: vec![
                marker_settings(&alpha, "alpha"),
                marker_settings(&beta, "beta"),
            ],
        };
        let with_gamma = McpSettings {
            enabled: true,
            request_timeout: Some(Duration::from_secs(10)),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            servers: vec![changed, marker_settings(&gamma, "gamma")],
        };

        // Both calls rebuild `alpha` and each swaps the second server, so the
        // two reconfigurations overlap on the same state.
        let first = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.apply_settings(with_beta).await })
        };
        let second = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.apply_settings(with_gamma).await })
        };
        let first = first.await.expect("the first call must not panic");
        let second = second.await.expect("the second call must not panic");

        // Each call reports every name exactly once.
        for outcome in [&first, &second] {
            let mut reported: Vec<&String> = outcome
                .added
                .iter()
                .chain(&outcome.removed)
                .chain(&outcome.updated)
                .collect();
            let before = reported.len();
            reported.sort();
            reported.dedup();
            assert_eq!(
                reported.len(),
                before,
                "a name must be classified once: {outcome:?}"
            );
        }

        // Whoever committed last owns the runtime: one entry per configured
        // server, no duplicates and nothing left from the other revision.
        let names = running_names(&manager);
        assert_eq!(names.len(), manager.servers().len());
        assert!(names.iter().all(|name| manager.server(name).is_some()));
        assert!(names.contains(&"alpha".to_string()));

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn apply_settings_handles_a_disabled_master_switch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("stopped");
        let manager = manager_for(vec![marker_settings(&marker, "fixture")]);
        manager.start(&tokio::runtime::Handle::current());
        wait_connected(&manager, "fixture").await;

        // Stage two disables MCP itself: nothing may stay connected.
        let mut off = manager.settings();
        off.enabled = false;
        let outcome = manager.apply_settings(off).await;
        assert_eq!(outcome.removed, ["fixture"]);
        assert!(outcome.added.is_empty());
        assert!(outcome.updated.is_empty());
        assert!(manager.snapshots().is_empty());
        assert!(!manager.is_active());
        wait_until(|| marker_appeared(&marker).then_some(())).await;

        // Stage three turns it back on: the server is rebuilt and connects.
        // The name never left the configuration, so it counts as updated.
        let mut on = manager.settings();
        on.enabled = true;
        let outcome = manager.apply_settings(on).await;
        assert_eq!(outcome.updated, ["fixture"]);
        assert!(outcome.added.is_empty());
        wait_connected(&manager, "fixture").await;
        assert!(manager.is_active());

        manager.shutdown().await;
    }

    /// A server that records its pid in `$1` and then never answers anything,
    /// so only the client can end a connect attempt against it.
    const HANGING_SERVER: &str = r#"printf '%s\n' "$$" > "$1"
while IFS= read -r line; do :; done"#;

    /// A server that records its pid in `$1`, answers `initialize` and
    /// `tools/list`, and ignores everything else.
    const HANDSHAKE_SERVER: &str = r#"printf '%s\n' "$$" > "$1"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0.1.0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=${line#*'"id":'}; id=${id%%,*}
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo"}]}}\n' "$id"
      ;;
  esac
done"#;

    /// Wait for a fixture child to record its pid.
    async fn wait_for_pid(path: &Path) -> i32 {
        wait_until(|| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
        })
        .await
    }

    /// True while `pid` still names a live process. Signal 0 performs the
    /// existence check without sending anything.
    #[cfg(unix)]
    fn pid_is_alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    /// Settings for a server that hangs the handshake, with the client-side
    /// deadline disabled the way `request_timeout: 0` documents.
    fn hanging_settings(pid_file: &Path) -> ServerSettings {
        let mut settings = script_settings(HANGING_SERVER, pid_file, "hanging");
        settings.request_timeout = None;
        settings
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_ends_a_pending_handshake() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child.pid");
        let manager = manager_for(vec![hanging_settings(&pid_file)]);
        manager.start(&tokio::runtime::Handle::current());

        let pid = wait_for_pid(&pid_file).await;
        assert!(pid_is_alive(pid), "the fixture child must be running");

        // With no client-side deadline only the manager's bound can end the
        // attempt, and `shutdown()` must not merely stop waiting for it. The
        // bound is well below the 5s a stuck driver would burn.
        let started = Instant::now();
        manager.shutdown().await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "shutdown waited for the driver instead of cancelling it"
        );

        wait_until(|| (!pid_is_alive(pid)).then_some(())).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disabling_a_server_ends_a_pending_handshake() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child.pid");
        let manager = manager_for(vec![hanging_settings(&pid_file)]);
        manager.start(&tokio::runtime::Handle::current());

        let pid = wait_for_pid(&pid_file).await;
        assert!(manager.set_server_enabled("hanging", false).await);

        wait_until(|| (!pid_is_alive(pid)).then_some(())).await;
        assert!(manager.snapshots().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reload_ends_a_pending_handshake() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child.pid");
        let manager = manager_for(vec![hanging_settings(&pid_file)]);
        manager.start(&tokio::runtime::Handle::current());

        let pid = wait_for_pid(&pid_file).await;
        // The reload starts a fresh attempt, so only the first child is gone
        // afterwards; the second one is reaped by the shutdown below.
        let _ = manager.reload().await;

        wait_until(|| (!pid_is_alive(pid)).then_some(())).await;
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_the_state_refused_is_reaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child.pid");
        let settings = script_settings(HANDSHAKE_SERVER, &pid_file, "fixture");
        let manager = manager_for(vec![settings.clone()]);

        // Build a real session the way `attempt_connect` does.
        let (events, _events) = mpsc::unbounded_channel();
        let connection = client::connect(
            &settings,
            ConnectOptions {
                roots: Vec::new(),
                cancel: None,
                events,
            },
        )
        .await
        .expect("the fixture must connect");
        let tools = connection.list_tools(None).await.expect("tools/list");
        let connection = Arc::new(connection);
        let pid = wait_for_pid(&pid_file).await;
        assert!(pid_is_alive(pid), "the fixture child must be running");

        // A driver from a generation that no longer exists: the state must
        // refuse the session instead of handing out a connection nobody owns.
        let (driver_events, _driver_events) = mpsc::unbounded_channel();
        let superseded = manager.generation.load(Ordering::SeqCst) + 1;
        let installed = manager
            .install_or_reap(
                "fixture",
                superseded,
                0,
                &connection,
                tools,
                false,
                MCP_PROTOCOL_VERSION.to_string(),
                driver_events,
            )
            .await;
        assert!(!installed, "a superseded driver must not install a session");

        // The refused connection and its child are gone.
        assert!(!connection.is_alive());
        wait_until(|| (!pid_is_alive(pid)).then_some(())).await;
    }
}
