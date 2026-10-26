// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments
)]

use clap::{Parser, Subcommand};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

/// Routes tracing log lines through the shell animation's terminal lock so a
/// `WARN` emitted during an active `Thinking` animation starts on a fresh
/// line instead of gluing onto the partially rendered frame (issue #490).
struct AnimationAwareMakeWriter;

impl<'a> MakeWriter<'a> for AnimationAwareMakeWriter {
    type Writer = aish_shell::animation::LogLineWriter;

    fn make_writer(&'a self) -> Self::Writer {
        aish_shell::animation::LogLineWriter::new()
    }
}

mod install_channel;
mod models_auth;
mod skill_cmd;
mod tty_watchdog;
mod uninstall;
mod update;

/// AI Shell - A shell with built-in LLM capabilities
#[derive(Parser)]
#[command(
    name = "aish",
    version,
    about = "AI Shell - A shell with built-in LLM capabilities"
)]
struct Cli {
    /// LLM model to use
    #[arg(long, short = 'm')]
    model: Option<String>,

    /// API key for the LLM provider
    #[arg(long)]
    api_key: Option<String>,

    /// API base URL for the LLM provider
    #[arg(long)]
    api_base: Option<String>,

    /// Path to configuration file
    #[arg(long)]
    config: Option<String>,

    /// Continue: attach to an existing live session or pick from a list
    #[arg(short = 'c', long = "continue")]
    continue_session: bool,

    #[arg(long, hide = true)]
    sandbox_daemon: bool,

    #[arg(long, hide = true)]
    sandbox_socket: Option<String>,

    #[arg(long, hide = true)]
    sandbox_worker: bool,

    // Hidden entry point: run as PTY daemon holding a bash session.
    #[arg(long, hide = true)]
    pty_daemon: bool,

    // Unix socket path for the PTY daemon to listen on.
    #[arg(long, hide = true)]
    pty_socket: Option<String>,

    // Session UUID the PTY daemon is bound to.
    #[arg(long, hide = true)]
    pty_session: Option<String>,

    // Kill every active PTY daemon session and exit.
    #[arg(long, hide = true)]
    kill_pty_session: bool,

    // Attach to an existing PTY daemon via socket path (raw terminal passthrough).
    #[arg(long, hide = true)]
    pty_attach: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the AI Shell — creates a new PTY session by default.
    /// Use --continue/-c to attach to an existing live session.
    Run,

    /// Kill PTY session(s) by ID prefix. Supports multiple IDs and `all`.
    Kill {
        /// Session ID(s) to kill (prefix match). Use `all` to kill every session.
        ids: Vec<String>,
    },

    /// Kill all PTY sessions
    KillAll,

    /// Resume a previous AI Shell session
    Resume {
        /// Session UUID to resume
        session_id: String,
    },

    /// Show information about AI Shell
    Info,

    /// Run interactive setup
    Setup,

    /// Show model usage status
    ModelsUsage,

    /// Check tool calling support for a model
    CheckToolSupport {
        /// Model name to check
        #[arg(long)]
        model: Option<String>,

        /// API base URL
        #[arg(long)]
        api_base: Option<String>,

        /// API key
        #[arg(long)]
        api_key: Option<String>,
    },

    /// Check Langfuse observability connectivity
    CheckLangfuse {
        /// Langfuse public key
        #[arg(long)]
        public_key: Option<String>,

        /// Langfuse secret key
        #[arg(long)]
        secret_key: Option<String>,

        /// Langfuse host URL
        #[arg(long)]
        host: Option<String>,
    },

    /// Update aish to the latest version
    Update {
        #[arg(long)]
        check_only: bool,
        #[arg(long, short = 'p')]
        pre_release: bool,
    },

    /// Uninstall aish
    Uninstall {
        #[arg(long)]
        purge: bool,
    },

    /// Manage provider authentication
    ModelsAuth {
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, default_value = "")]
        model: String,
        #[arg(long, default_value = "true")]
        set_default: bool,
        #[arg(long, default_value = "browser")]
        auth_flow: models_auth::AuthFlow,
        #[arg(long, default_value = "false")]
        force: bool,
        #[arg(long, default_value = "true")]
        open_browser: bool,
        #[arg(long, default_value_t = 8402)]
        callback_port: u16,
        #[arg(long)]
        config: Option<String>,
    },

    /// Run system diagnostics
    Doctor {
        /// Attempt to auto-fix issues
        #[arg(long)]
        fix: bool,
        /// Output results as machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Search, install, and manage skills
    Skill {
        #[command(subcommand)]
        action: SkillSubcommand,
    },
}

/// Subcommands for `aish skill`.
#[derive(Subcommand)]
enum SkillSubcommand {
    /// Search for skills in registries
    Search {
        /// Search keywords
        query: Vec<String>,
        /// Max results
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Install a skill from a registry
    Install {
        /// Skill ID (e.g. "skills_sh/my-skill")
        skill_id: String,
    },
    /// Verify an installed skill
    Verify {
        /// Skill name
        name: String,
    },
    /// List installed skills
    List,
    /// Remove an installed skill
    Remove {
        /// Skill name
        name: String,
    },
    /// Mark a reviewed skill as trusted (remove quarantine)
    Trust {
        /// Skill name
        name: String,
    },
    /// Print a skill's files for manual review
    Vet {
        /// Skill name
        name: String,
    },
    /// Manage registry sources
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },
}

/// Subcommands for `aish skill registry`.
#[derive(Subcommand)]
enum RegistryAction {
    /// List configured registries
    List,
    /// Add a new registry source
    Add {
        name: String,
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        url: String,
    },
    /// Remove a registry source
    Remove { name: String },
}

fn main() {
    let cli = Cli::parse();
    let config_path = cli.config.clone();
    let model_override = cli.model.clone();
    let api_base_override = cli.api_base.clone();
    let api_key_override = cli.api_key.clone();

    // Initialize logging. RUST_LOG wins; otherwise honor the selected
    // config's `log_level` (best-effort early load). Passing the --config
    // path here (not None) matters: load() runs an additive migration that
    // rewrites the file when keys are missing, so load(None) would touch the
    // DEFAULT path even when --config points elsewhere. Falls back to "warn".
    let config_file = config_path.as_deref().map(std::path::Path::new);
    let log_level = aish_config::ConfigLoader::load(config_file)
        .map(|c| c.log_level)
        .unwrap_or_else(|_| "warn".to_string());
    tracing_subscriber::fmt()
        .with_writer(AnimationAwareMakeWriter)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level)),
        )
        .init();

    if cli.sandbox_daemon {
        tty_watchdog::disarm();
        let socket_path = cli.sandbox_socket.as_deref().map(std::path::Path::new);
        if let Err(error) = aish_security::run_sandbox_daemon(socket_path) {
            eprintln!("sandbox daemon failed: {}", error);
            std::process::exit(1);
        }
        return;
    }

    if cli.sandbox_worker {
        tty_watchdog::disarm();
        if let Err(error) = aish_security::run_sandbox_worker() {
            eprintln!("sandbox worker failed: {}", error);
            std::process::exit(1);
        }
        return;
    }

    // Hidden PTY daemon entry point: hold a PTY running aish, serve clients
    // over a Unix socket. Survives client disconnects; replays scrollback
    // on reattach.
    if cli.pty_daemon {
        tty_watchdog::disarm();
        // Apply the effective CLI overrides before the PTY fork so the shell
        // child inherits them without needing to read or mutate the process
        // environment after fork.
        for (daemon_var, shell_var) in [
            ("AISH_DAEMON_MODEL", "AISH_MODEL"),
            ("AISH_DAEMON_API_BASE", "AISH_API_BASE"),
            ("AISH_DAEMON_API_KEY", "AISH_API_KEY"),
        ] {
            if let Ok(value) = std::env::var(daemon_var) {
                std::env::set_var(shell_var, value);
            }
        }
        let cwd = std::env::var("AISH_DAEMON_CWD").unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
        let socket_path = cli.pty_socket.expect("--pty-socket required");
        let session_id = cli.pty_session.expect("--pty-session required");
        let shell_exe =
            std::env::var("AISH_DAEMON_SHELL_EXE").unwrap_or_else(|_| "aish".to_string());

        let (rows, cols) = get_terminal_size();

        if let Err(e) = aish_pty::run_pty_daemon_shell(
            &cwd,
            rows,
            cols,
            std::path::Path::new(&socket_path),
            &session_id,
            &shell_exe,
            cli.config.as_deref(),
        ) {
            eprintln!("PTY daemon error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Hidden entry: discover and kill every active PTY daemon session.
    if cli.kill_pty_session {
        let sessions = aish_pty::discover_sessions();
        if sessions.is_empty() {
            println!("No active PTY sessions found.");
            return;
        }
        for s in &sessions {
            println!("Killing session {} (pid {})...", s.session_id, s.child_pid);
            let _ = aish_pty::kill_session(&s.socket_path);
        }
        // Give daemons a moment to clean up their sockets/session files.
        std::thread::sleep(std::time::Duration::from_millis(500));
        return;
    }

    let mut config =
        match aish_config::ConfigLoader::load(config_path.as_deref().map(std::path::Path::new)) {
            Ok(config) => config,
            Err(e) => {
                tracing::debug!("Config load failed (using defaults): {}", e);
                aish_config::ConfigModel::default()
            }
        };

    // Apply CLI arg overrides
    if let Some(model) = cli.model {
        config.model = model;
    }
    if let Some(api_key) = cli.api_key {
        config.api_key = api_key;
    }
    if let Some(api_base) = cli.api_base {
        config.api_base = api_base;
    }
    let daemon_launch_config = PtyDaemonLaunchConfig::from_cli_overrides(
        config_path.as_deref(),
        model_override.as_deref(),
        api_base_override.as_deref(),
        api_key_override.as_deref(),
    );

    // Hidden entry: raw terminal passthrough attach to an existing daemon.
    if let Some(socket_path) = &cli.pty_attach {
        tty_watchdog::disarm();
        let session_id = cli
            .pty_session
            .clone()
            .unwrap_or_else(|| "attach".to_string());
        let _ = run_pty_raw_attach(socket_path, &session_id, &daemon_launch_config);
        return;
    }

    match cli.command {
        // `aish` or `aish run`: start a fresh shell by default.
        // Use --continue/-c to attach to an existing live session.
        None | Some(Commands::Run) => {
            if cli.continue_session {
                run_shell_continue(config, &daemon_launch_config);
            } else {
                // Standalone shell only: the daemon-attach fallback inside
                // run_shell_new must not run with a live watchdog, because
                // exit(0) mid-attach would skip backend detach and termios
                // restore.
                tty_watchdog::arm_for_interactive();
                run_shell_new(config, &daemon_launch_config);
            }
        }
        Some(Commands::Resume { session_id }) => {
            // Resume may attach to a daemon session (run_pty_raw_attach);
            // the standalone-shell fallback inside it re-arms the watchdog.
            run_shell_resume(config, &session_id, &daemon_launch_config);
        }
        Some(Commands::Kill { ids }) => kill_session_by_id(&ids),
        Some(Commands::KillAll) => kill_session_by_id(&["all".to_string()]),
        Some(Commands::Info) => show_info(&config),
        Some(Commands::Setup) => {
            if !run_setup(&mut config) {
                std::process::exit(1);
            }
        }
        Some(Commands::ModelsUsage) => show_models_usage(&config),
        Some(Commands::CheckToolSupport {
            model,
            api_base,
            api_key,
        }) => check_tool_support(&config, model, api_base, api_key),
        Some(Commands::CheckLangfuse {
            public_key,
            secret_key,
            host,
        }) => check_langfuse(&config, public_key, secret_key, host),
        Some(Commands::Update {
            check_only,
            pre_release,
        }) => {
            update::run_update(check_only, pre_release);
        }
        Some(Commands::Uninstall { purge }) => {
            uninstall::run_uninstall(purge);
        }
        Some(Commands::Doctor { fix, json }) => {
            let doctor = aish_shell::doctor::Doctor::new();
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                doctor.run(fix, json).await;
            });
        }
        Some(Commands::ModelsAuth {
            provider,
            model,
            set_default,
            auth_flow,
            force,
            open_browser,
            callback_port,
            config: auth_config,
        }) => {
            let mut cfg = load_config(auth_config.as_deref());
            models_auth::run_models_auth(
                &mut cfg,
                provider.as_deref(),
                &model,
                set_default,
                auth_flow,
                force,
                open_browser,
                callback_port,
            );
        }
        Some(Commands::Skill { action }) => {
            let outcome: Result<(), String> = match action {
                SkillSubcommand::Search { query, limit } => {
                    skill_cmd::cmd_search(&query.join(" "), limit)
                }
                SkillSubcommand::Install { skill_id } => skill_cmd::cmd_install(&skill_id),
                SkillSubcommand::Verify { name } => skill_cmd::cmd_verify(&name),
                SkillSubcommand::List => skill_cmd::cmd_list(),
                SkillSubcommand::Remove { name } => skill_cmd::cmd_remove(&name),
                SkillSubcommand::Trust { name } => skill_cmd::cmd_trust(&name),
                SkillSubcommand::Vet { name } => skill_cmd::cmd_vet(&name),
                SkillSubcommand::Registry { action } => match action {
                    RegistryAction::List => skill_cmd::cmd_registry_list(),
                    RegistryAction::Add { name, r#type, url } => {
                        skill_cmd::cmd_registry_add(&name, &r#type, &url)
                    }
                    RegistryAction::Remove { name } => skill_cmd::cmd_registry_remove(&name),
                },
            };
            if let Err(msg) = outcome {
                eprintln!("{msg}");
                std::process::exit(1);
            }
        }
    }
}

fn load_config(config_path: Option<&str>) -> aish_config::ConfigModel {
    let path = config_path.map(std::path::Path::new);
    aish_config::ConfigLoader::load(path).unwrap_or_default()
}

/// Query stdout's window size via TIOCGWINSZ; fall back to 24x80.
fn get_terminal_size() -> (u16, u16) {
    use std::os::fd::AsRawFd;
    // SAFETY: [Category 4 — Uninitialized memory] `winsize` is a POD C struct;
    // `zeroed()` produces a valid instance filled by ioctl below.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = std::io::stdout().as_raw_fd();
    // SAFETY: [Category 8 — FFI] `ioctl(TIOCGWINSZ)` queries the terminal
    // size. `fd` is stdout (a valid open fd). `&mut ws` points to the
    // initialised struct. A non-zero return means stdout is not a terminal.
    let ret = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
        (ws.ws_row, ws.ws_col)
    } else {
        (24, 80) // fallback
    }
}

/// Configuration captured by the CLI for daemon launches and later `/new` actions.
#[derive(Clone, Debug, Default)]
struct PtyDaemonLaunchConfig {
    config_path: Option<String>,
    model_override: Option<String>,
    api_base_override: Option<String>,
    api_key_override: Option<String>,
}

impl PtyDaemonLaunchConfig {
    /// Capture only explicit CLI overrides before terminal attachment.
    fn from_cli_overrides(
        config_path: Option<&str>,
        model_override: Option<&str>,
        api_base_override: Option<&str>,
        api_key_override: Option<&str>,
    ) -> Self {
        Self {
            config_path: config_path.map(str::to_owned),
            model_override: model_override.map(str::to_owned),
            api_base_override: api_base_override.map(str::to_owned),
            api_key_override: api_key_override.map(str::to_owned),
        }
    }
}

/// Spawn a detached PTY daemon process (via exec of the current binary) and
/// return its session info. The daemon survives client disconnects and keeps
/// the underlying bash PTY alive. Blocks up to 8 seconds waiting for the
/// daemon's Unix socket to accept connections.
///
/// `child_pid` is left as 0 here because the real bash child pid lives inside
/// the daemon process; callers that need it should read the persisted session
/// file via `aish_pty::discover_sessions()`.
fn spawn_pty_daemon(
    cwd: &str,
    launch_config: &PtyDaemonLaunchConfig,
) -> Result<aish_pty::DaemonSessionInfo, Box<dyn std::error::Error>> {
    use uuid::Uuid;

    // A daemon-created "new session" inherits the config selected by the
    // shell process inside the PTY. Ordinary CLI launches pass the path
    // explicitly and never consult this internal daemon-only variable.
    let daemon_child = std::env::var("AISH_DAEMON_MODE").as_deref() == Ok("1");
    let config_path = resolve_pty_daemon_config_path(
        launch_config.config_path.as_deref(),
        daemon_child,
        std::env::var("AISH_DAEMON_CONFIG").ok(),
        std::path::Path::new(cwd),
    );

    let session_id = Uuid::new_v4().to_string();
    let socket_dir = aish_pty::pty_socket_dir().map_err(|e| e.to_string())?;
    let socket_path = socket_dir.join(format!("pty-{}.sock", &session_id[..8]));

    // Spawn daemon process
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(&current_exe);
    cmd.arg("--pty-daemon")
        .arg("--pty-socket")
        .arg(&socket_path)
        .arg("--pty-session")
        .arg(&session_id);

    forward_pty_daemon_config(
        &mut cmd,
        config_path.as_deref(),
        launch_config.model_override.as_deref(),
        launch_config.api_base_override.as_deref(),
        launch_config.api_key_override.as_deref(),
        daemon_child,
    );

    // Pass the original working directory and executable to the daemon.
    cmd.env("AISH_DAEMON_CWD", cwd);
    // The daemon runs the aish binary itself inside a PTY, so the full
    // AishShell UI (prompt, AI mode, completions) is preserved.
    cmd.env("AISH_DAEMON_SHELL_EXE", &current_exe);

    // Detach: redirect stdio so the daemon does not touch our terminal.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Put daemon in a new process group so terminal SIGHUP doesn't kill it.
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);

    // Start process
    cmd.spawn().map_err(|e| e.to_string())?;

    // Wait for socket to be ready (aish takes longer to start than bare bash)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        if aish_pty::check_daemon_alive(&socket_path) {
            return Ok(aish_pty::DaemonSessionInfo {
                session_id,
                socket_path,
                daemon_pid: 0, // real pid is in session JSON written by daemon
                child_pid: 0,
                started_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                cwd: cwd.to_string(),
                model: launch_config.model_override.clone(),
                api_base: launch_config.api_base_override.clone(),
                name: None,
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    Err(format!(
        "PTY daemon failed to start within 8 seconds (socket: {:?})",
        socket_path
    )
    .into())
}

/// Forward the selected config and effective overrides to the hidden daemon.
/// The API key travels in the environment, never in process arguments.
fn forward_pty_daemon_config(
    cmd: &mut std::process::Command,
    config_path: Option<&str>,
    model: Option<&str>,
    api_base: Option<&str>,
    api_key: Option<&str>,
    inherit_daemon_config: bool,
) {
    if !inherit_daemon_config {
        for name in [
            "AISH_DAEMON_CONFIG",
            "AISH_DAEMON_MODEL",
            "AISH_DAEMON_API_BASE",
            "AISH_DAEMON_API_KEY",
        ] {
            cmd.env_remove(name);
        }
    }
    if let Some(path) = config_path {
        cmd.arg("--config")
            .arg(path)
            .env("AISH_DAEMON_CONFIG", path);
    }
    if let Some(model) = model {
        cmd.env("AISH_DAEMON_MODEL", model);
    }
    if let Some(api_base) = api_base {
        cmd.env("AISH_DAEMON_API_BASE", api_base);
    }
    if let Some(api_key) = api_key {
        cmd.env("AISH_DAEMON_API_KEY", api_key);
    }
}

fn resolve_pty_daemon_config_path(
    explicit: Option<&str>,
    daemon_child: bool,
    inherited: Option<String>,
    cwd: &std::path::Path,
) -> Option<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| daemon_child.then_some(inherited).flatten())
        .map(|path| {
            let path = std::path::Path::new(&path);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            }
            .to_string_lossy()
            .into_owned()
        })
}

/// Whether PTY daemon mode is active: enabled in config *and* not
/// suppressed by `AISH_PTY_DAEMON=0` (set inside daemon children to
/// prevent infinite recursion).
fn is_daemon_mode(config: &aish_config::ConfigModel) -> bool {
    config.pty_daemon_enabled
        && std::env::var("AISH_PTY_DAEMON")
            .map(|v| v != "0" && v != "false" && v != "no")
            .unwrap_or(true)
}

/// Spawn a fresh PTY daemon session from the current executable and
/// attach to it.  Returns `true` on successful attach (caller returns),
/// `false` if spawning or attaching failed (caller falls back).
fn spawn_daemon_and_attach(launch_config: &PtyDaemonLaunchConfig) -> bool {
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    match spawn_pty_daemon(&cwd, launch_config) {
        Ok(info) => run_pty_raw_attach(
            &info.socket_path.to_string_lossy(),
            &info.session_id,
            launch_config,
        ),
        Err(e) => {
            eprintln!("\x1b[33m[aish] PTY daemon failed: {}\x1b[0m", e);
            false
        }
    }
}

/// Attach to a live session or create a daemon using the current CLI settings.
fn run_shell_continue(config: aish_config::ConfigModel, launch_config: &PtyDaemonLaunchConfig) {
    if !is_daemon_mode(&config) {
        run_shell_normal(config);
        return;
    }

    let sessions = aish_pty::discover_sessions();

    if !sessions.is_empty() {
        match show_session_picker(sessions) {
            SessionAction::Attach(s) => {
                let short = &s.session_id[..8.min(s.session_id.len())];
                eprintln!("\x1b[32m[aish] Attaching to session {short}\x1b[0m");
                if run_pty_raw_attach(
                    &s.socket_path.to_string_lossy(),
                    &s.session_id,
                    launch_config,
                ) {
                    record_session_use(&s.session_id);
                    return;
                }
                eprintln!("\x1b[33m[aish] Attach failed.\x1b[0m");
            }
            SessionAction::New => {}
            SessionAction::Cancel => return,
        }
    }

    if !spawn_daemon_and_attach(launch_config) {
        eprintln!("\x1b[33m[aish] Falling back to standalone shell.\x1b[0m");
        run_shell_normal(config);
    }
}

enum SessionAction {
    Attach(aish_pty::DaemonSessionInfo),
    New,
    Cancel,
}

/// Interactive session picker with inline rename support. Arrow keys move,
/// Enter attaches to the highlighted session, Ctrl+E renames it (then re-shows
/// the refreshed list), and Esc cancels. "Create new session" spawns a fresh
/// daemon. Owns the session list so renames can refresh it between rounds.
fn show_session_picker(initial_sessions: Vec<aish_pty::DaemonSessionInfo>) -> SessionAction {
    use aish_ui::{
        PanelOutcome, PanelRuntime, SearchSelectItem, SearchSelectOutcome, SearchSelectPanel,
    };

    let mut sessions = initial_sessions;
    sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));

    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut items: Vec<SearchSelectItem> = sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let cwd_display = display_cwd(&s.cwd);
                let age_str = format_duration(now.saturating_sub(s.started_at));
                let short_id = &s.session_id[..8.min(s.session_id.len())];
                let name = s.name.as_deref().unwrap_or("");
                // Label is the short id + cwd; a custom name (if any) is the
                // bold green highlight prefix so it stands out at a glance.
                let label = format!("{short_id}  {cwd_display}");
                let search_text = format!("{name} {short_id} {cwd_display}");
                let mut item = SearchSelectItem::new(format!("session:{i}"), label)
                    .with_detail(aish_i18n::t_with_args(
                        "shell.live_sessions.started_label",
                        &{
                            let mut a = std::collections::HashMap::new();
                            a.insert("age".to_string(), age_str);
                            a
                        },
                    ))
                    .with_search_text(search_text)
                    .with_renamable();
                if !name.is_empty() {
                    item = item.with_highlight(name);
                }
                item
            })
            .collect();

        // "Create new" is the last item. The default row is the session the
        // user attached to most often; without any attach history it is the
        // first session (newest start). "Create new" is never preselected
        // here — it only runs when no session is live, and then no picker is
        // shown at all.
        items.push(SearchSelectItem::new(
            "new",
            aish_i18n::t("shell.live_sessions.create_new"),
        ));

        let default = default_session_index(&sessions, &read_session_usage())
            .map(|idx| format!("session:{idx}"));

        let panel = SearchSelectPanel::new(
            aish_i18n::t("shell.live_sessions.panel_title"),
            aish_i18n::t("shell.live_sessions.search_placeholder"),
            items,
        )
        .with_footer(aish_i18n::t("shell.live_sessions.panel_footer"))
        .with_selected_value(default.as_deref());

        match PanelRuntime::new().run(panel) {
            Ok(PanelOutcome::Submitted(SearchSelectOutcome::Selected(value))) => {
                if value == "new" {
                    return SessionAction::New;
                }
                if let Some(idx) = index_from_value(&value, sessions.len()) {
                    return SessionAction::Attach(sessions[idx].clone());
                }
                return SessionAction::New;
            }
            Ok(PanelOutcome::Submitted(SearchSelectOutcome::Rename(value))) => {
                if let Some(idx) = index_from_value(&value, sessions.len()) {
                    rename_session_interactive(&sessions[idx]);
                }
                // Refresh so the new name shows up, then re-show the picker.
                sessions = aish_pty::discover_sessions();
                sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
                continue;
            }
            _ => return SessionAction::Cancel,
        }
    }
}

/// Parse a picker item value like "session:3" into a bounded index.
fn index_from_value(value: &str, len: usize) -> Option<usize> {
    let idx_str = value.strip_prefix("session:")?;
    let idx: usize = idx_str.parse().ok()?;
    (idx < len).then_some(idx)
}

/// Cache file mapping session id → number of successful attaches, so
/// `aish -c` preselects the session the user chooses most often.
fn session_usage_path() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("aish").join("session-usage.json"))
}

fn read_session_usage() -> std::collections::HashMap<String, u64> {
    let Some(path) = session_usage_path() else {
        return std::collections::HashMap::new();
    };
    if path.exists() {
        return read_session_usage_file_at(&path);
    }
    // The cache does not exist yet: the one-time legacy import rewrites the
    // file, so it has to hold the same lock as the attach writers or it can
    // replace a count they just wrote. Without the lock, read what is there
    // and leave the import to a later run.
    let Some(_lock) = lock_session_usage(&path) else {
        return read_session_usage_file_at(&path);
    };
    read_session_usage_at(&path)
}

fn read_session_usage_file_at(path: &std::path::Path) -> std::collections::HashMap<String, u64> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return std::collections::HashMap::new();
    };
    serde_json::from_str::<std::collections::HashMap<String, u64>>(&text).unwrap_or_default()
}

/// Read that also runs the one-time legacy import. The import rewrites the
/// cache, so callers must hold `lock_session_usage` for `path`.
fn read_session_usage_at(path: &std::path::Path) -> std::collections::HashMap<String, u64> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return seed_from_legacy_last_attach(path);
    };
    serde_json::from_str::<std::collections::HashMap<String, u64>>(&text).unwrap_or_default()
}

/// Upgrade path for the previous single-id cache (`last-attach-session`):
/// count that id once so the user keeps their old default, then drop the file.
fn seed_from_legacy_last_attach(
    usage_path: &std::path::Path,
) -> std::collections::HashMap<String, u64> {
    let legacy = usage_path.with_file_name("last-attach-session");
    let mut usage = std::collections::HashMap::new();
    if let Ok(id) = std::fs::read_to_string(&legacy) {
        let id = id.trim();
        if !id.is_empty() {
            usage.insert(id.to_string(), 1);
            let _ = write_session_usage_at(usage_path, &usage);
        }
        let _ = std::fs::remove_file(&legacy);
    }
    usage
}

fn write_session_usage_at(
    path: &std::path::Path,
    usage: &std::collections::HashMap<String, u64>,
) -> std::io::Result<()> {
    let json = serde_json::to_string(usage)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-then-rename so an interrupted write cannot corrupt the cache, with
    // a per-process temp name so concurrent writers cannot clobber each other.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if let Err(error) = std::fs::write(&tmp, json) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

/// Exclusive advisory lock held across the usage read-modify-write so two
/// attaches finishing at the same time cannot drop a count; dropping the
/// guard closes the file and releases the lock.
struct SessionUsageLock {
    _file: std::fs::File,
}

/// Bounded wait: the attach path must never stall on the cache, so give up
/// after ~100 ms and skip the counter update instead.
fn lock_session_usage(path: &std::path::Path) -> Option<SessionUsageLock> {
    use std::os::unix::io::AsRawFd;

    let lock_path = path.with_file_name("session-usage.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .ok()?;
    for _ in 0..20 {
        // SAFETY: [Category 8 — FFI] `flock` takes a valid fd from a live
        // `File` and a flag word; `LOCK_NB` makes it non-blocking, and a
        // failure return is retried with the same live fd.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(SessionUsageLock { _file: file });
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

fn record_session_use(session_id: &str) {
    if let Some(path) = session_usage_path() {
        // Best effort: the attach already succeeded, a lost count only
        // nudges the picker default.
        let _ = record_session_use_at(&path, session_id);
    }
}

/// `Ok(false)` means the lock stayed busy for the whole bounded wait and this
/// count was skipped; `Err` means the cache itself could not be written.
fn record_session_use_at(path: &std::path::Path, session_id: &str) -> std::io::Result<bool> {
    let Some(_lock) = lock_session_usage(path) else {
        return Ok(false);
    };
    let mut usage = read_session_usage_at(path);
    *usage.entry(session_id.to_string()).or_insert(0) += 1;
    write_session_usage_at(path, &usage)?;
    Ok(true)
}

/// Default row of the session picker: the live session with the highest
/// attach count. Sessions are sorted newest-first, so ties and an empty usage
/// map both keep the newest session. `None` only when nothing is live, in
/// which case the caller spawns a new session without showing the picker.
fn default_session_index(
    sessions: &[aish_pty::DaemonSessionInfo],
    usage: &std::collections::HashMap<String, u64>,
) -> Option<usize> {
    let mut best: Option<(usize, u64)> = None;
    for (idx, session) in sessions.iter().enumerate() {
        let count = usage.get(&session.session_id).copied().unwrap_or(0);
        if best.is_none_or(|(_, best_count)| count > best_count) {
            best = Some((idx, count));
        }
    }
    best.map(|(idx, _)| idx)
}

/// Run the normal (non-daemon) AishShell.
fn run_shell_normal(mut config: aish_config::ConfigModel) {
    // Arm the terminal-death watchdog only for the standalone shell: this
    // function is reached from the daemon-attach fallbacks, so arming here
    // never covers run_pty_raw_attach (whose exit(0) mid-attach would skip
    // backend detach and termios restore).
    tty_watchdog::arm_for_interactive();
    if aish_shell::needs_interactive_setup(&config) {
        println!("\x1b[33mConfiguration incomplete — launching setup wizard.\x1b[0m\n");
        if !run_setup(&mut config) {
            eprintln!(
                "\n\x1b[33m{}\x1b[0m",
                aish_i18n::t("cli.setup.required_cancelled")
            );
            std::process::exit(1);
        }
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Ok(loaded) = aish_config::ConfigLoader::load(Some(&config_path)) {
            config = loaded;
        }
    }
    match aish_shell::AishShell::new(config) {
        Ok(mut shell) => {
            if let Err(e) = shell.run() {
                shell.shutdown();
                eprintln!("Shell error: {}", e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to initialize shell: {}", e);
            std::process::exit(1);
        }
    }
}

/// Open a text-input panel to rename a live session. Returns `true` when the
/// name was written to disk (so callers can refresh their session list).
fn rename_session_interactive(session: &aish_pty::DaemonSessionInfo) -> bool {
    use aish_ui::{ChoiceOutcome, ChoicePanel, PanelOutcome, PanelRuntime};

    let short_id = &session.session_id[..8.min(session.session_id.len())];
    let current = session.name.as_deref().unwrap_or("");

    let panel = ChoicePanel::new(
        aish_i18n::t_with_args("shell.live_sessions.rename_title", &{
            let mut a = std::collections::HashMap::new();
            a.insert("id".to_string(), short_id.to_string());
            a
        }),
        if current.is_empty() {
            aish_i18n::t("shell.live_sessions.rename_current_none")
        } else {
            aish_i18n::t_with_args("shell.live_sessions.rename_current_set", &{
                let mut a = std::collections::HashMap::new();
                a.insert("name".to_string(), current.to_string());
                a
            })
        },
        Vec::new(),
    )
    .with_custom_label(aish_i18n::t("shell.live_sessions.rename_input_label"))
    .with_allow_cancel(true)
    .with_allow_empty_custom_input(true)
    .with_footer(aish_i18n::t("shell.live_sessions.rename_input_footer"));

    match PanelRuntime::new().run(panel) {
        Ok(PanelOutcome::Submitted(ChoiceOutcome::CustomInput(name))) => {
            match aish_pty::rename_session(&session.session_id, &name) {
                Ok(()) => {
                    if name.trim().is_empty() {
                        println!(
                            "{}",
                            aish_i18n::t_with_args("shell.live_sessions.rename_cleared", &{
                                let mut a = std::collections::HashMap::new();
                                a.insert("id".to_string(), short_id.to_string());
                                a
                            })
                        );
                    } else {
                        println!(
                            "{}",
                            aish_i18n::t_with_args("shell.live_sessions.rename_done", &{
                                let mut a = std::collections::HashMap::new();
                                a.insert("id".to_string(), short_id.to_string());
                                a.insert("name".to_string(), name.trim().to_string());
                                a
                            })
                        );
                    }
                    true
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        aish_i18n::t_with_args("shell.live_sessions.rename_failed", &{
                            let mut a = std::collections::HashMap::new();
                            a.insert("err".to_string(), e.to_string());
                            a
                        })
                    );
                    false
                }
            }
        }
        _ => false,
    }
}

fn kill_session_by_id(ids: &[String]) {
    let sessions = aish_pty::discover_sessions();
    if sessions.is_empty() {
        println!("No active PTY sessions.");
        return;
    }

    // No args: list sessions so the user can pick an ID.
    if ids.is_empty() {
        println!("\x1b[1mActive PTY sessions:\x1b[0m");
        for s in &sessions {
            println!("  {} {}", session_label(s), display_cwd(&s.cwd));
        }
        println!("\n\x1b[2mUsage: aish kill <id> [id ...]  ·  aish kill all\x1b[0m");
        return;
    }

    // `aish kill all`
    if ids.len() == 1 && ids[0] == "all" {
        for s in &sessions {
            print!("Killing {} ... ", session_label(s));
            match aish_pty::kill_session(&s.socket_path) {
                Ok(()) => println!("done"),
                Err(e) => println!("failed: {}", e),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("\n{} session(s) terminated.", sessions.len());
        return;
    }

    // Kill each specified ID prefix.
    let mut killed = 0u32;
    let mut errors = 0u32;
    for id in ids {
        let target = sessions
            .iter()
            .find(|s| s.session_id == *id || s.session_id.starts_with(id.as_str()));
        match target {
            Some(s) => {
                print!("Killing {} ... ", session_label(s));
                match aish_pty::kill_session(&s.socket_path) {
                    Ok(()) => {
                        println!("done");
                        killed += 1;
                    }
                    Err(e) => {
                        println!("failed: {}", e);
                        errors += 1;
                    }
                }
            }
            None => {
                eprintln!("Session '{}' not found.", id);
                errors += 1;
            }
        }
    }
    if killed > 0 {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if killed + errors > 1 {
        println!("\n{} killed, {} failed.", killed, errors);
    }
    if errors > 0 {
        std::process::exit(1);
    }
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}min ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Render a cwd path with the home directory abbreviated to `~`.
fn display_cwd(cwd: &str) -> String {
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd.to_string()
    }
}

/// Render a session's primary label: its custom name if set, otherwise the
/// 8-char short ID. Used by the attach picker, `live-sessions`, and `kill`.
fn session_label(s: &aish_pty::DaemonSessionInfo) -> String {
    let short_id = &s.session_id[..8.min(s.session_id.len())];
    match &s.name {
        // Bold green name + dim short id, so custom names stand out at a glance.
        Some(n) if !n.is_empty() => {
            format!("\x1b[1;32m{n}\x1b[0m \x1b[2m({short_id})\x1b[0m")
        }
        _ => format!("\x1b[2m{short_id}\x1b[0m"),
    }
}

/// Start a new daemon-backed shell with the selected CLI configuration.
fn run_shell_new(config: aish_config::ConfigModel, launch_config: &PtyDaemonLaunchConfig) {
    if !is_daemon_mode(&config) {
        run_shell_normal(config);
        return;
    }

    if !spawn_daemon_and_attach(launch_config) {
        eprintln!("\x1b[33m[aish] Falling back to standalone shell.\x1b[0m");
        run_shell_normal(config);
    }
}

/// Resume a live daemon session, preserving CLI settings for any later `/new`.
fn run_shell_resume(
    config: aish_config::ConfigModel,
    session_id: &str,
    launch_config: &PtyDaemonLaunchConfig,
) {
    // First check if this is an active daemon session (by UUID or 8-char prefix)
    let sessions = aish_pty::discover_sessions();
    if let Some(session) = sessions
        .iter()
        .find(|s| s.session_id == session_id || s.session_id.starts_with(session_id))
    {
        eprintln!(
            "\x1b[32m[aish] Resuming daemon session {}\x1b[0m",
            &session.session_id[..8.min(session.session_id.len())]
        );
        if run_pty_raw_attach(
            &session.socket_path.to_string_lossy(),
            &session.session_id,
            launch_config,
        ) {
            record_session_use(&session.session_id);
        }
        return;
    }

    // Not a daemon session — fall back to SQLite AI context resume
    eprintln!(
        "\x1b[33m[aish] No active daemon session '{}', resuming AI context.\x1b[0m",
        session_id
    );
    run_shell_normal_resume(config, session_id);
}

fn run_shell_normal_resume(mut config: aish_config::ConfigModel, session_id: &str) {
    // Standalone resume: same watchdog scoping as run_shell_normal — the
    // daemon-attach path above (run_shell_resume) must stay unwatched.
    tty_watchdog::arm_for_interactive();
    // Auto-trigger setup wizard on first run if config is incomplete
    if aish_shell::needs_interactive_setup(&config) {
        println!("\x1b[33mConfiguration incomplete — launching setup wizard.\x1b[0m\n");
        if !run_setup(&mut config) {
            eprintln!(
                "\n\x1b[33m{}\x1b[0m",
                aish_i18n::t("cli.setup.required_cancelled")
            );
            std::process::exit(1);
        }
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Ok(loaded) = aish_config::ConfigLoader::load(Some(&config_path)) {
            config = loaded;
        }
    }

    match aish_shell::AishShell::resume(config, session_id) {
        Ok(mut shell) => {
            if let Err(e) = shell.run() {
                shell.shutdown();
                eprintln!("Shell error: {}", e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!(
                "{}",
                aish_i18n::t_with_args("cli.resume_failed", &{
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), e.to_string());
                    args
                })
            );
            std::process::exit(1);
        }
    }
}

fn show_info(config: &aish_config::ConfigModel) {
    println!("AI Shell v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!(
        "  Model:     {}",
        if config.model.is_empty() {
            "(not set)"
        } else {
            &config.model
        }
    );
    println!("  API Base:  {}", config.api_base);
    println!("  Config:    ~/.config/aish/config.yaml");
    println!();
    println!(
        "  Platform:  {}-{}",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
}

fn run_setup(config: &mut aish_config::ConfigModel) -> bool {
    let config_dir = aish_config::ConfigLoader::default_config_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("aish")
        });

    let mut wizard = aish_shell::wizard::SetupWizard::new(config_dir);
    match wizard.run() {
        Ok(new_config) => {
            *config = aish_shell::wizard::apply_setup_result(config, new_config);
            aish_shell::wizard::print_setup_complete_hint();
            true
        }
        Err(aish_core::AishError::Cancelled) => {
            eprintln!("\x1b[33m{}\x1b[0m", aish_i18n::t("cli.setup.cancelled"));
            false
        }
        Err(e) => {
            eprintln!("\x1b[31m{}\x1b[0m", e);
            false
        }
    }
}

fn show_models_usage(config: &aish_config::ConfigModel) {
    let model = &config.model;
    let api_base = &config.api_base;
    let api_key_set = !config.api_key.is_empty();

    let provider = aish_llm::detect_provider(model, api_base);

    println!("\x1b[1mModel Configuration\x1b[0m");
    println!("  Model:      \x1b[36m{}\x1b[0m", model);
    println!("  Provider:   \x1b[36m{}\x1b[0m", provider.display_name);
    println!("  API Base:   \x1b[2m{}\x1b[0m", api_base);
    println!(
        "  API Key:    {}",
        if api_key_set {
            "\x1b[32mset\x1b[0m"
        } else {
            "\x1b[31mnot set\x1b[0m"
        }
    );

    println!();
    println!("\x1b[1mProvider Capabilities\x1b[0m");
    println!(
        "  Streaming:      {}",
        if provider.supports_streaming {
            "\x1b[32myes\x1b[0m"
        } else {
            "\x1b[33mno\x1b[0m"
        }
    );
    println!(
        "  Tool Calling:   {}",
        if provider.supports_tools {
            "\x1b[32myes\x1b[0m"
        } else {
            "\x1b[33mno\x1b[0m"
        }
    );

    if let Some(dashboard) = &provider.dashboard_url {
        println!();
        println!("\x1b[1mDashboard\x1b[0m");
        println!("  \x1b[4m{}\x1b[0m", dashboard);
    }

    println!();
    println!("\x1b[2mConfig file: ~/.config/aish/config.yaml\x1b[0m");
    println!("\x1b[2mOverride:    AISH_MODEL, AISH_API_KEY, AISH_API_BASE\x1b[0m");
}

fn check_tool_support(
    config: &aish_config::ConfigModel,
    model: Option<String>,
    api_base: Option<String>,
    api_key: Option<String>,
) {
    let model = model.unwrap_or_else(|| config.model.clone());
    let api_base = api_base.unwrap_or_else(|| config.api_base.clone());
    let api_key = api_key.unwrap_or_else(|| config.api_key.clone());

    if model.is_empty() {
        eprintln!("Error: No model specified. Use --model or set it in config.");
        std::process::exit(1);
    }
    if api_key.is_empty() {
        eprintln!("Error: No API key specified. Use --api-key or set it in config.");
        std::process::exit(1);
    }

    println!("Checking tool calling support for: {}", model);
    println!("API Base: {}", api_base);

    // Mirror interactive shell: dialect-aware streaming request with normal token budget.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(aish_llm::probe_live_tool_support(
        &api_base, &api_key, &model,
    ));

    match result {
        Ok(()) => {
            println!("\x1b[32mTool calling is supported.\x1b[0m");
        }
        Err(e) => {
            eprintln!("\x1b[31mTool calling may not be supported: {}\x1b[0m", e);
            std::process::exit(1);
        }
    }
}

fn check_langfuse(
    config: &aish_config::ConfigModel,
    public_key: Option<String>,
    secret_key: Option<String>,
    host: Option<String>,
) {
    // Priority: CLI args > env vars > config.yaml
    let public_key = public_key
        .or_else(|| std::env::var("LANGFUSE_PUBLIC_KEY").ok())
        .or_else(|| config.langfuse_public_key.clone());
    let secret_key = secret_key
        .or_else(|| std::env::var("LANGFUSE_SECRET_KEY").ok())
        .or_else(|| config.langfuse_secret_key.clone());
    let host = host
        .or_else(|| std::env::var("LANGFUSE_BASE_URL").ok())
        .or_else(|| config.langfuse_host.clone());

    match (public_key, secret_key) {
        (Some(pk), Some(sk)) => {
            if pk.is_empty() || sk.is_empty() {
                eprintln!("Langfuse configuration is incomplete.");
                return;
            }
            let base_url = host
                .as_deref()
                .map(|h| h.trim_end_matches('/').to_string())
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "https://cloud.langfuse.com".to_string());
            let cfg = aish_llm::LangfuseConfig {
                enabled: true,
                public_key: pk.clone(),
                secret_key: sk.clone(),
                base_url: base_url.clone(),
            };
            let _client = aish_llm::LangfuseClient::new(cfg);
            println!("Langfuse configuration found.");
            println!("  Host: {}", base_url);
            if pk.len() > 8 {
                println!("  Public Key: {}...{}", &pk[..4], &pk[pk.len() - 4..]);
            } else {
                println!(
                    "  Public Key: {}...{}",
                    &pk[..2.min(pk.len())],
                    &pk[(pk.len() - 2).min(pk.len())..]
                );
            }
            println!("\x1b[32mLangfuse is configured and ready.\x1b[0m");
        }
        (None, _) | (_, None) => {
            eprintln!("Langfuse is not configured.");
            eprintln!("Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY environment variables,");
            eprintln!("or add langfuse_public_key and langfuse_secret_key to config.yaml.");
        }
    }
}

/// Raw terminal passthrough: attach to a PTY daemon and relay stdin/stdout.
///
/// This is a simple terminal forwarder that connects to the daemon's Unix
/// socket and passes bytes back and forth. It demonstrates the detach/reattach
/// lifecycle: exiting this function (via Ctrl+Q) detaches from the daemon
/// without killing the underlying bash process.
fn run_pty_raw_attach(
    socket_path: &str,
    session_id: &str,
    launch_config: &PtyDaemonLaunchConfig,
) -> bool {
    use aish_pty::PtyBackend;
    use std::os::fd::AsRawFd;

    let (mut rows, mut cols) = get_terminal_size();

    let mut backend = match aish_pty::AttachedBackend::attach(
        std::path::Path::new(socket_path),
        session_id,
        rows,
        cols,
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("\x1b[33m[aish] Failed to attach: {}\x1b[0m", e);
            return false;
        }
    };

    // Save terminal state and switch to raw mode
    let stdin_fd = std::io::stdin().as_raw_fd();
    // SAFETY: [Category 4 — Uninitialized memory] `termios` is a POD C struct;
    // `zeroed()` produces a valid instance filled by tcgetattr below.
    let mut orig_termios: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: [Category 8 — FFI] `tcgetattr` reads the terminal attributes
    // into `orig_termios`. `stdin_fd` is stdin (a valid open fd). Non-zero
    // return means stdin is not a terminal.
    if unsafe { libc::tcgetattr(stdin_fd, &mut orig_termios) } != 0 {
        eprintln!("\x1b[31m[aish] Failed to get terminal attributes\x1b[0m");
        std::process::exit(1);
    }
    let mut raw = orig_termios;
    // SAFETY: [Category 8 — FFI] `cfmakeraw` mutates the `termios` struct
    // in place to set raw mode flags. `raw` is a valid initialised struct.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_iflag &= !libc::IXON;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: [Category 8 — FFI] `tcsetattr(TCSANOW)` applies the raw-mode
    // attributes immediately. `stdin_fd` is stdin (valid). `&raw` points to
    // the initialised struct.
    if unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &raw) } != 0 {
        eprintln!("\x1b[31m[aish] Failed to set raw mode\x1b[0m");
        std::process::exit(1);
    }

    let stdout_fd = std::io::stdout().as_raw_fd();
    let mut osc = OscScanner::new();
    // When true, OSC 5151 commands extracted from PTY output are ignored.
    // Set after every session switch so that stale OSC sequences surviving
    // in the scrollback replay (defence-in-depth: the daemon should have
    // stripped them, but TYPE_PTY_OUTPUT frames that arrive during the
    // attach handshake are also buffered as scrollback) don't re-trigger
    // a switch. Cleared after the first `drain_events` call consumes the
    // buffered scrollback.
    let mut skip_osc_commands = false;

    // Flush scrollback immediately. The attach handshake consumed all
    // scrollback bytes from the socket into `pending_events`, so the
    // kernel socket buffer is now empty. Without this explicit drain,
    // select() would not report the socket readable and the buffered
    // scrollback (including the prompt) would never be displayed until
    // the user presses a key to generate new output.
    let mut initial_exit = false;
    // Track whether any PTY output was actually received during scrollback
    // replay. When the inner shell exits immediately without producing any
    // output (e.g. PersistentPty::start failed before the welcome banner),
    // we return false so the caller can fall back to standalone mode.
    let mut had_output = false;
    if let Ok(events) = backend.drain_events() {
        for event in events {
            match event {
                aish_pty::PtyEvent::Output(bytes) => {
                    if !bytes.is_empty() {
                        let (clean, _cmds) = osc.process(&bytes);
                        if !clean.is_empty() {
                            had_output = true;
                            // SAFETY: [Category 8 — FFI] `write()` to stdout.
                            // `stdout_fd` is valid; `clean` is a Vec whose
                            // buffer is valid for the duration of the call.
                            unsafe {
                                libc::write(stdout_fd, clean.as_ptr() as *const _, clean.len());
                            }
                        }
                    }
                }
                aish_pty::PtyEvent::Control(evt) => {
                    if matches!(evt, aish_pty::BackendControlEvent::ShellExiting { .. }) {
                        initial_exit = true;
                    }
                }
            }
        }
    }
    // Discard any partial OSC bytes the scanner may have buffered from
    // scrollback so they don't leak into live-output processing.
    osc.clear();

    if initial_exit || !backend.is_running() {
        let _ = backend.detach();
        // SAFETY: [Category 8 — FFI] `tcsetattr` restores the original
        // terminal attributes saved at attach time. `stdin_fd` is valid.
        unsafe {
            libc::tcsetattr(stdin_fd, libc::TCSANOW, &orig_termios);
        }
        // If the inner shell exited without producing any output, the user
        // saw a blank screen. Return false so the caller falls back to
        // standalone mode and shows the welcome banner.
        // When output was produced (banner/prompt visible), return true —
        // the user already saw what they need and the session is done.
        return had_output;
    }
    let mut stdin_buf = [0u8; 1024];
    // True when the inner shell sent ShellExiting (user typed `exit`/`logout`).
    // Distinguishes a true detach (Ctrl+Q — session still alive) from a shell
    // exit (session is dying, nothing to reattach to).
    let mut shell_exited = false;

    loop {
        let socket_fd = backend.readable_fds()[0];

        // SAFETY: [Category 4 — Uninitialized memory] `fd_set` is a POD C
        // struct; `zeroed()` produces a valid instance. FD_ZERO/FD_SET below
        // populate it.
        let mut read_set: libc::fd_set = unsafe { std::mem::zeroed() };
        // SAFETY: [Category 8 — FFI] FD_ZERO clears, FD_SET adds stdin and the
        // socket fd. Both fds are below FD_SETSIZE (stdin=0, socket is a
        // UnixStream fd allocated by the kernel, well within FD_SETSIZE).
        unsafe {
            libc::FD_ZERO(&mut read_set);
            libc::FD_SET(stdin_fd, &mut read_set);
            libc::FD_SET(socket_fd, &mut read_set);
        }
        let max_fd = stdin_fd.max(socket_fd);
        let mut tv = libc::timeval {
            tv_sec: 1,
            tv_usec: 0,
        };

        // SAFETY: [Category 8 — FFI] `select()` polls stdin and the socket for
        // readability. `max_fd+1` is nfds, `read_set` is initialised, the
        // other sets are null, `&mut tv` is a valid 1-second timeout.
        let ret = unsafe {
            libc::select(
                max_fd + 1,
                &mut read_set,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };

        if ret < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }

        // stdin → daemon
        // SAFETY: [Category 8 — FFI] `FD_ISSET` tests if stdin is in the
        // post-select `read_set`. `stdin_fd` and `read_set` are valid.
        if unsafe { libc::FD_ISSET(stdin_fd, &read_set) } {
            // SAFETY: [Category 8 — FFI] `read()` from stdin into `stdin_buf`.
            // `stdin_fd` is valid; `stdin_buf` is a 1024-byte stack array;
            // the pointer and length are within bounds.
            let n =
                unsafe { libc::read(stdin_fd, stdin_buf.as_mut_ptr() as *mut _, stdin_buf.len()) };
            if n > 0 {
                let data = &stdin_buf[..n as usize];
                if let Some(qpos) = data.iter().position(|&b| b == 0x11) {
                    if qpos > 0 {
                        let _ = backend.write_input(&data[..qpos]);
                    }
                    // Ctrl+Q detaches. Restore cooked terminal mode and, when
                    // the current session is still unnamed, prompt for a note
                    // so it stays identifiable in `aish -c`.
                    // SAFETY: tcsetattr restores `orig_termios` saved at attach.
                    unsafe {
                        libc::tcsetattr(stdin_fd, libc::TCSANOW, &orig_termios);
                    }
                    ctrl_q_detach_nudge(session_id);
                    break;
                }
                if data.len() == 1 && data[0] == 0x04 {
                    break;
                }
                if let Err(e) = backend.write_input(data) {
                    eprint!("\r\n[aish] write error: {}\r\n", e);
                    break;
                }
            } else if n == 0 {
                // EOF: terminal side is gone.
                break;
            } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                // EIO etc.: the pty master died (terminal closed). Without
                // this branch the select loop would spin exactly like the
                // crossterm bug this PR fixes. Detach through the normal
                // break path so the backend and termios are restored.
                eprintln!(
                    "\r\n[aish] terminal I/O error: {}",
                    std::io::Error::last_os_error()
                );
                break;
            }
        }

        // socket → stdout (with OSC scanning)
        // SAFETY: [Category 8 — FFI] `FD_ISSET` tests if the socket is in the
        // post-select `read_set`. `socket_fd` and `read_set` are valid.
        if unsafe { libc::FD_ISSET(socket_fd, &read_set) } {
            match backend.drain_events() {
                Ok(events) => {
                    let mut should_exit = false;
                    let mut osc_action: Option<String> = None;
                    for event in events {
                        match event {
                            aish_pty::PtyEvent::Output(bytes) => {
                                if bytes.is_empty() {
                                    continue;
                                }
                                let (clean, cmds) = osc.process(&bytes);
                                if !clean.is_empty() {
                                    had_output = true;
                                    // SAFETY: [Category 8 — FFI] `write()` to
                                    // stdout. `stdout_fd` is valid; `clean`
                                    // is a Vec whose buffer is valid.
                                    unsafe {
                                        libc::write(
                                            stdout_fd,
                                            clean.as_ptr() as *const _,
                                            clean.len(),
                                        );
                                    }
                                }
                                // Ignore OSC commands while draining scrollback
                                // (the first drain_events after attach/switch).
                                if !skip_osc_commands {
                                    for cmd in cmds {
                                        osc_action = Some(cmd);
                                    }
                                }
                            }
                            aish_pty::PtyEvent::Control(evt) => {
                                if matches!(evt, aish_pty::BackendControlEvent::ShellExiting { .. })
                                {
                                    should_exit = true;
                                    shell_exited = true;
                                }
                            }
                        }
                    }

                    // First drain after attach/switch consumed all buffered
                    // scrollback — re-enable OSC command extraction and clear
                    // any partial OSC bytes the scanner may have buffered.
                    if skip_osc_commands {
                        skip_osc_commands = false;
                        osc.clear();
                    }

                    // Handle OSC command (session switch/new/detach)
                    if let Some(action) = osc_action {
                        match handle_osc_action(
                            &mut backend,
                            &action,
                            &mut rows,
                            &mut cols,
                            stdout_fd,
                            launch_config,
                        ) {
                            OscResult::Continue => {
                                // Clear scanner buffer after switch
                                osc.clear();
                                // Ignore OSC from the next drain (scrollback
                                // replay of the newly-attached session).
                                skip_osc_commands = true;
                                // Trigger prompt redraw in new session
                                use aish_pty::PtyBackend;
                                let _ = backend.write_input(b"\n");
                            }
                            OscResult::Exit => break,
                        }
                    }

                    if should_exit {
                        break;
                    }
                }
                Err(e) => {
                    eprint!("\r\n\x1b[31m[aish] connection lost: {}\x1b[0m\r\n", e);
                    break;
                }
            }
        }

        if !backend.is_running() {
            break;
        }

        // Terminal resize polling
        let (cur_rows, cur_cols) = get_terminal_size();
        if cur_rows != rows || cur_cols != cols {
            rows = cur_rows;
            cols = cur_cols;
            let _ = backend.resize(rows, cols);
        }
    }

    let _ = backend.detach();
    // SAFETY: [Category 8 — FFI] `tcsetattr` restores the original terminal
    // attributes saved at attach time. `stdin_fd` is valid.
    unsafe {
        libc::tcsetattr(stdin_fd, libc::TCSANOW, &orig_termios);
    }
    // Only show the reattach hint when the session is actually still alive
    // (Ctrl+Q/Ctrl+D detach). When the inner shell typed `exit`, the daemon
    // sends EXIT_NOTICE which sets running=false — nothing to reattach to.
    if !shell_exited && backend.is_running() {
        eprintln!("\x1b[32m[aish] {}\x1b[0m", aish_i18n::t("cli.detach_hint"));
    }
    // The inner shell exited during the live loop: apply the same
    // blank-screen rule as the initial drain — success only if output
    // was actually shown, otherwise let the caller fall back.
    if shell_exited {
        return had_output;
    }
    true
}

/// Ctrl+Q detach nudge: when the current session has no user note, prompt for
/// one so it stays identifiable in `aish -c` after detach. Enter saves &
/// detaches; Esc (or an empty submit) detaches without naming. The caller
/// must restore cooked terminal mode first so the panel can render.
fn ctrl_q_detach_nudge(session_id: &str) {
    use aish_i18n::t;
    use aish_ui::{ChoiceOutcome, ChoicePanel, PanelOutcome, PanelRuntime};

    let needs_note = aish_pty::discover_sessions()
        .into_iter()
        .find(|s| s.session_id == session_id)
        .map(|s| s.name.as_deref().unwrap_or("").trim().is_empty())
        .unwrap_or(false);
    if !needs_note {
        return;
    }

    let panel = ChoicePanel::new(
        t("shell.live_sessions.exit_no_note_title"),
        t("shell.live_sessions.exit_no_note_hint"),
        Vec::new(),
    )
    .with_custom_label(t("shell.live_sessions.rename_input_label"))
    .with_allow_cancel(true)
    .with_allow_empty_custom_input(true)
    .with_footer(t("shell.live_sessions.exit_no_note_footer"));

    if let Ok(PanelOutcome::Submitted(ChoiceOutcome::CustomInput(name))) =
        PanelRuntime::new().run(panel)
    {
        if !name.trim().is_empty() {
            let _ = aish_pty::rename_session(session_id, &name);
        }
    }
}

/// Result of OSC action handling.
enum OscResult {
    Continue,
    Exit,
}

/// Handle an OSC 5151 command by switching/creating/detaching sessions.
/// Strategy: connect to new session FIRST, then detach old — so failures
/// don't leave the user disconnected.
fn handle_osc_action(
    backend: &mut aish_pty::AttachedBackend,
    action: &str,
    rows: &mut u16,
    cols: &mut u16,
    stdout_fd: i32,
    launch_config: &PtyDaemonLaunchConfig,
) -> OscResult {
    if action == "detach" {
        return OscResult::Exit;
    }

    if action == "new" {
        eprint!("\r\n\x1b[32m[aish] Creating new session...\x1b[0m\r\n");
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let info = match spawn_pty_daemon(&cwd, launch_config) {
            Ok(info) => info,
            Err(e) => {
                eprint!(
                    "\r\n\x1b[31m[aish] Spawn failed: {} — staying on current session.\x1b[0m\r\n",
                    e
                );
                return OscResult::Continue;
            }
        };
        let new_backend = match aish_pty::AttachedBackend::attach(
            std::path::Path::new(&info.socket_path),
            &info.session_id,
            *rows,
            *cols,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprint!(
                    "\r\n\x1b[31m[aish] Attach failed: {} — staying on current session.\x1b[0m\r\n",
                    e
                );
                return OscResult::Continue;
            }
        };
        // Success — switch over: detach old (ignore errors), assign new
        let _ = backend.detach();
        // SAFETY: [Category 8 — FFI] `write()` clears the terminal screen.
        // `stdout_fd` is valid; the byte slice is a static literal (7 bytes).
        unsafe {
            libc::write(stdout_fd, b"\x1b[2J\x1b[H".as_ptr() as *const _, 7);
        }
        *backend = new_backend;
        return OscResult::Continue;
    }

    if let Some(target_id) = action.strip_prefix("switch:") {
        // Find target BEFORE detaching
        let sessions = aish_pty::discover_sessions();
        let target = match sessions
            .iter()
            .find(|s| s.session_id == target_id || s.session_id.starts_with(target_id))
        {
            Some(t) => t,
            None => {
                eprint!(
                    "\r\n\x1b[31m[aish] Session {} not found — staying on current.\x1b[0m\r\n",
                    target_id
                );
                return OscResult::Continue;
            }
        };
        // Attach to target FIRST
        let new_backend = match aish_pty::AttachedBackend::attach(
            &target.socket_path,
            &target.session_id,
            *rows,
            *cols,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprint!(
                    "\r\n\x1b[31m[aish] Attach failed: {} — staying on current.\x1b[0m\r\n",
                    e
                );
                return OscResult::Continue;
            }
        };
        // Success — switch over
        let _ = backend.detach();
        // SAFETY: [Category 8 — FFI] `write()` clears the terminal screen.
        // `stdout_fd` is valid; the byte slice is a static literal (7 bytes).
        unsafe {
            libc::write(stdout_fd, b"\x1b[2J\x1b[H".as_ptr() as *const _, 7);
        }
        eprint!(
            "\r\n\x1b[32m[aish] Switched to session {}.\x1b[0m\r\n",
            target_id
        );
        *backend = new_backend;
        return OscResult::Continue;
    }

    OscResult::Continue
}

/// Scanner for OSC 5151 escape sequences in PTY output.
/// Strips the sequences and extracts embedded commands.
struct OscScanner {
    pending: Vec<u8>,
}

impl OscScanner {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.pending.clear();
    }

    /// Process a chunk of output bytes. Returns (clean_bytes, osc_commands).
    fn process(&mut self, data: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut buf = Vec::with_capacity(self.pending.len() + data.len());
        buf.extend_from_slice(&self.pending);
        buf.extend_from_slice(data);
        self.pending.clear();

        let mut clean = Vec::with_capacity(buf.len());
        let mut commands = Vec::new();
        let mut i = 0;
        let prefix = b"\x1b]5151;";

        while i < buf.len() {
            if i + prefix.len() <= buf.len() && &buf[i..i + prefix.len()] == prefix {
                // Found OSC 5151 start, find terminator
                let op_start = i + prefix.len();
                let mut found = None;
                for j in op_start..buf.len() {
                    if buf[j] == 0x07 {
                        found = Some((j, 1));
                        break;
                    }
                    if j + 1 < buf.len() && buf[j] == 0x1b && buf[j + 1] == b'\\' {
                        found = Some((j, 2));
                        break;
                    }
                }
                if let Some((end, term_len)) = found {
                    let op = String::from_utf8_lossy(&buf[op_start..end]).to_string();
                    commands.push(op);
                    i = end + term_len;
                } else {
                    // Incomplete — save for next chunk
                    self.pending = buf[i..].to_vec();
                    break;
                }
            } else {
                clean.push(buf[i]);
                i += 1;
            }
        }

        (clean, commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_config_and_overrides_are_forwarded_without_putting_key_in_args() {
        let mut command = std::process::Command::new("aish");
        forward_pty_daemon_config(
            &mut command,
            Some("/tmp/aish-config/custom.yaml"),
            Some("model-b"),
            Some("http://127.0.0.1:9000/v1"),
            Some("test-credential-b"),
            false,
        );

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--config".to_string(),
                "/tmp/aish-config/custom.yaml".to_string()
            ]
        );
        assert!(!args.iter().any(|arg| arg == "test-credential-b"));

        let env: std::collections::HashMap<String, String> = command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            env.get("AISH_DAEMON_CONFIG").map(String::as_str),
            Some("/tmp/aish-config/custom.yaml")
        );
        assert_eq!(
            env.get("AISH_DAEMON_MODEL").map(String::as_str),
            Some("model-b")
        );
        assert_eq!(
            env.get("AISH_DAEMON_API_BASE").map(String::as_str),
            Some("http://127.0.0.1:9000/v1")
        );
        assert_eq!(
            env.get("AISH_DAEMON_API_KEY").map(String::as_str),
            Some("test-credential-b")
        );
    }

    #[test]
    fn new_sessions_only_inherit_a_config_path_from_a_daemon_child() {
        assert_eq!(
            resolve_pty_daemon_config_path(
                None,
                true,
                Some("/tmp/aish-config/custom.yaml".into()),
                std::path::Path::new("/workspace")
            ),
            Some("/tmp/aish-config/custom.yaml".into())
        );
        assert_eq!(
            resolve_pty_daemon_config_path(
                None,
                false,
                Some("/tmp/stale.yaml".into()),
                std::path::Path::new("/workspace")
            ),
            None
        );
        assert_eq!(
            resolve_pty_daemon_config_path(
                Some("/tmp/explicit.yaml"),
                true,
                Some("/tmp/inherited.yaml".into()),
                std::path::Path::new("/workspace")
            ),
            Some("/tmp/explicit.yaml".into())
        );
        assert_eq!(
            resolve_pty_daemon_config_path(
                Some("configs/custom.yaml"),
                false,
                None,
                std::path::Path::new("/workspace/project")
            ),
            Some("/workspace/project/configs/custom.yaml".into())
        );
    }

    #[test]
    fn test_show_models_usage_does_not_panic() {
        let config = aish_config::ConfigModel::default();
        show_models_usage(&config);
    }

    fn session(id: &str, started_at: u64) -> aish_pty::DaemonSessionInfo {
        aish_pty::DaemonSessionInfo {
            session_id: id.to_string(),
            socket_path: std::path::PathBuf::from(format!("/tmp/{id}.sock")),
            daemon_pid: 1,
            child_pid: 2,
            started_at,
            cwd: "/tmp".to_string(),
            model: None,
            api_base: None,
            name: None,
        }
    }

    #[test]
    fn default_session_prefers_most_used() {
        // Newest-first ordering is the picker's invariant.
        let sessions = vec![
            session("newest", 300),
            session("old", 100),
            session("mid", 200),
        ];
        let usage =
            std::collections::HashMap::from([("old".to_string(), 5u64), ("newest".to_string(), 1)]);
        assert_eq!(default_session_index(&sessions, &usage), Some(1));
    }

    #[test]
    fn default_session_falls_back_to_newest_without_history() {
        let sessions = vec![session("newest", 300), session("old", 100)];
        assert_eq!(
            default_session_index(&sessions, &std::collections::HashMap::new()),
            Some(0)
        );
    }

    #[test]
    fn default_session_keeps_newest_on_tie() {
        let sessions = vec![session("newest", 300), session("old", 100)];
        let usage =
            std::collections::HashMap::from([("newest".to_string(), 2u64), ("old".to_string(), 2)]);
        assert_eq!(default_session_index(&sessions, &usage), Some(0));
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aish-usage-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("session-usage.json")
    }

    #[test]
    fn session_usage_counts_every_concurrent_attach() {
        let path = scratch_dir("concurrent");
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        // Retry a lock miss; an I/O failure must fail the test
                        // rather than spin.
                        while !record_session_use_at(&path, "sess").expect("usage write") {}
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("worker");
        }
        let usage = read_session_usage_at(&path);
        assert_eq!(usage.get("sess").copied(), Some(200));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn legacy_last_attach_seeds_usage_once() {
        let path = scratch_dir("legacy");
        let legacy = path.with_file_name("last-attach-session");
        std::fs::write(&legacy, "old-session\n").expect("legacy file");
        assert_eq!(
            read_session_usage_at(&path).get("old-session").copied(),
            Some(1)
        );
        assert!(!legacy.exists());
        assert_eq!(
            read_session_usage_at(&path).get("old-session").copied(),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn default_session_ignores_dead_session_history() {
        let sessions = vec![session("live", 100)];
        let usage = std::collections::HashMap::from([("gone".to_string(), 9u64)]);
        assert_eq!(default_session_index(&sessions, &usage), Some(0));
        assert_eq!(default_session_index(&[], &usage), None);
    }
}
