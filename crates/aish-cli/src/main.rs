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
use tracing_subscriber::EnvFilter;

mod install_channel;
mod models_auth;
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
    /// Run the AI Shell (default) — attaches to existing PTY session or creates new
    Run,

    /// Start a new PTY session (like `tmux new`)
    New,

    /// Rename a live PTY session interactively
    RenameLiveSessions,

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
    },
}

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    if cli.sandbox_daemon {
        let socket_path = cli.sandbox_socket.as_deref().map(std::path::Path::new);
        if let Err(error) = aish_security::run_sandbox_daemon(socket_path) {
            eprintln!("sandbox daemon failed: {}", error);
            std::process::exit(1);
        }
        return;
    }

    if cli.sandbox_worker {
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

    // Hidden entry: raw terminal passthrough attach to an existing daemon.
    if let Some(socket_path) = &cli.pty_attach {
        let session_id = cli
            .pty_session
            .clone()
            .unwrap_or_else(|| "attach".to_string());
        let _ = run_pty_raw_attach(socket_path, &session_id);
        return;
    }

    // Load configuration
    let config_path = cli.config.as_deref().map(std::path::Path::new);
    let mut config = match aish_config::ConfigLoader::load(config_path) {
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

    match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => run_shell(config),
        Commands::New => run_shell_new(config),
        Commands::RenameLiveSessions => rename_live_sessions(),
        Commands::Kill { ids } => kill_session_by_id(&ids),
        Commands::KillAll => kill_session_by_id(&["all".to_string()]),
        Commands::Resume { session_id } => run_shell_resume(config, &session_id),
        Commands::Info => show_info(&config),
        Commands::Setup => {
            if !run_setup(&mut config) {
                std::process::exit(1);
            }
        }
        Commands::ModelsUsage => show_models_usage(&config),
        Commands::CheckToolSupport {
            model,
            api_base,
            api_key,
        } => check_tool_support(&config, model, api_base, api_key),
        Commands::CheckLangfuse {
            public_key,
            secret_key,
            host,
        } => check_langfuse(&config, public_key, secret_key, host),
        Commands::Update {
            check_only,
            pre_release,
        } => {
            update::run_update(check_only, pre_release);
        }
        Commands::Uninstall { purge } => {
            uninstall::run_uninstall(purge);
        }
        Commands::Doctor { fix } => {
            let doctor = aish_shell::doctor::Doctor::new();
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                doctor.run(fix).await;
            });
        }
        Commands::ModelsAuth {
            provider,
            model,
            set_default,
            auth_flow,
            force,
            open_browser,
            callback_port,
            config: auth_config,
        } => {
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
    model: Option<&str>,
    api_base: Option<&str>,
) -> Result<aish_pty::DaemonSessionInfo, Box<dyn std::error::Error>> {
    use uuid::Uuid;

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

    // Pass configuration to the daemon via environment.
    cmd.env("AISH_DAEMON_CWD", cwd);
    if let Some(m) = model {
        cmd.env("AISH_DAEMON_MODEL", m);
    }
    if let Some(b) = api_base {
        cmd.env("AISH_DAEMON_API_BASE", b);
    }
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
                model: model.map(String::from),
                api_base: api_base.map(String::from),
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

fn run_shell(config: aish_config::ConfigModel) {
    let pty_daemon_enabled = config.pty_daemon_enabled
        && std::env::var("AISH_PTY_DAEMON")
            .map(|v| v != "0" && v != "false" && v != "no")
            .unwrap_or(true);

    if !pty_daemon_enabled {
        run_shell_normal(config);
        return;
    }

    let mut sessions = aish_pty::discover_sessions();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));

    if !sessions.is_empty() {
        match show_session_picker(&sessions) {
            SessionAction::Attach(idx) => {
                let s = &sessions[idx];
                eprintln!(
                    "\x1b[32m[aish] Attaching to session {}\x1b[0m",
                    &s.session_id[..8.min(s.session_id.len())]
                );
                if run_pty_raw_attach(&s.socket_path.to_string_lossy(), &s.session_id) {
                    return;
                }
                eprintln!("\x1b[33m[aish] Attach failed.\x1b[0m");
            }
            SessionAction::New => {}
            SessionAction::Cancel => return,
        }
    }

    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    eprintln!("\x1b[32m[aish] Starting new PTY daemon session...\x1b[0m");
    match spawn_pty_daemon(
        &cwd,
        if config.model.is_empty() {
            None
        } else {
            Some(config.model.as_str())
        },
        if config.api_base.is_empty() {
            None
        } else {
            Some(config.api_base.as_str())
        },
    ) {
        Ok(info) => {
            if run_pty_raw_attach(&info.socket_path.to_string_lossy(), &info.session_id) {
                return;
            }
        }
        Err(e) => eprintln!("\x1b[33m[aish] PTY daemon failed: {}\x1b[0m", e),
    }
    eprintln!("\x1b[33m[aish] Falling back to standalone shell.\x1b[0m");
    run_shell_normal(config);
}

enum SessionAction {
    Attach(usize),
    New,
    Cancel,
}

/// Interactive session picker using arrow-key navigation.
fn show_session_picker(sessions: &[aish_pty::DaemonSessionInfo]) -> SessionAction {
    use aish_ui::{
        PanelOutcome, PanelRuntime, SearchSelectItem, SearchSelectOutcome, SearchSelectPanel,
    };

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
            // Label shows name (if set) or short_id, followed by cwd.
            let label = if !name.is_empty() {
                format!("{}  {}", name, cwd_display)
            } else {
                format!("{}  {}", short_id, cwd_display)
            };
            // Search text includes both name and short_id so users can
            // search by either.
            let search_text = format!("{} {} {}", name, short_id, cwd_display);
            SearchSelectItem::new(format!("session:{}", i), label)
                .with_detail(format!("started: {}", age_str))
                .with_search_text(search_text)
        })
        .collect();

    // "Create new" is the default (last item, pre-selected)
    items.push(SearchSelectItem::new(
        "new",
        "Create new session".to_string(),
    ));

    let panel = SearchSelectPanel::new("Select PTY Session", "Type to search sessions...", items)
        .with_footer(
            "↑↓ navigate · Enter select · Esc cancel · Run 'aish kill <id>' to terminate a session",
        )
        .with_selected_value(Some("new"));

    match PanelRuntime::new().run(panel) {
        Ok(PanelOutcome::Submitted(SearchSelectOutcome::Selected(value))) => {
            if value == "new" {
                SessionAction::New
            } else if let Some(idx_str) = value.strip_prefix("session:") {
                match idx_str.parse::<usize>() {
                    Ok(idx) if idx < sessions.len() => SessionAction::Attach(idx),
                    _ => SessionAction::New,
                }
            } else {
                SessionAction::New
            }
        }
        _ => SessionAction::Cancel,
    }
}
/// Run the normal (non-daemon) AishShell.
fn run_shell_normal(mut config: aish_config::ConfigModel) {
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

/// Interactively rename a live PTY session. With active sessions, opens an
/// interactive panel: select a session to rename it (Enter), Esc to cancel.
/// Without active sessions, prints a hint and exits.
fn rename_live_sessions() {
    let sessions = aish_pty::discover_sessions();
    if sessions.is_empty() {
        println!("No active PTY sessions.");
        println!("Run 'aish' to start one.");
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Build the session selection panel.
    use aish_ui::{
        ChoiceOutcome, ChoicePanel, PanelOutcome, PanelRuntime, SearchSelectItem,
        SearchSelectOutcome, SearchSelectPanel,
    };

    let items: Vec<SearchSelectItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let cwd_display = display_cwd(&s.cwd);
            let age_str = format_duration(now.saturating_sub(s.started_at));
            let short_id = &s.session_id[..8.min(s.session_id.len())];
            let mut item = SearchSelectItem::new(
                format!("session:{}", i),
                format!("{}  {}", short_id, cwd_display),
            )
            .with_detail(format!("started: {}", age_str));
            if let Some(n) = &s.name {
                if !n.is_empty() {
                    item = item.with_badge(n.clone());
                }
            }
            item
        })
        .collect();

    let panel = SearchSelectPanel::new(
        "Live PTY Sessions",
        "Select a session to rename (Enter) · Esc to cancel",
        items,
    )
    .with_footer("↑↓ navigate · Enter rename · Esc cancel");

    let selected = match PanelRuntime::new().run(panel) {
        Ok(PanelOutcome::Submitted(SearchSelectOutcome::Selected(value))) => value,
        _ => {
            // User cancelled; fall back to printing the plain list.
            print_session_list(&sessions, now);
            return;
        }
    };

    let idx = match selected.strip_prefix("session:") {
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n < sessions.len() => n,
            _ => {
                print_session_list(&sessions, now);
                return;
            }
        },
        None => {
            print_session_list(&sessions, now);
            return;
        }
    };

    let session = &sessions[idx];
    let short_id = &session.session_id[..8.min(session.session_id.len())];
    let current = session.name.as_deref().unwrap_or("");

    // Second step: enter a new name via ChoicePanel custom input.
    let rename_panel = ChoicePanel::new(
        format!("Rename session {}", short_id),
        format!(
            "Current name: {}",
            if current.is_empty() {
                "(none)"
            } else {
                current
            }
        ),
        Vec::new(),
    )
    .with_custom_label("Enter new name (empty to clear)")
    .with_allow_cancel(true)
    .with_allow_empty_custom_input(true)
    .with_footer("Type a name | Enter save | Esc cancel");

    match PanelRuntime::new().run(rename_panel) {
        Ok(PanelOutcome::Submitted(ChoiceOutcome::CustomInput(name))) => {
            match aish_pty::rename_session(&session.session_id, &name) {
                Ok(()) => {
                    if name.trim().is_empty() {
                        println!("Cleared name for session {}.", short_id);
                    } else {
                        println!("Renamed session {} → \"{}\".", short_id, name.trim());
                    }
                }
                Err(e) => eprintln!("\x1b[31mFailed to rename: {}\x1b[0m", e),
            }
        }
        _ => {
            // Cancelled or selected an empty list item — just print the list.
        }
    }

    // Re-discover so the printed list reflects the rename we just performed.
    let refreshed = aish_pty::discover_sessions();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    print_session_list(&refreshed, now);
}

/// Print the session list in plain (non-interactive) form.
fn print_session_list(sessions: &[aish_pty::DaemonSessionInfo], now: u64) {
    println!("Active PTY sessions:\n");
    for s in sessions {
        let cwd_display = display_cwd(&s.cwd);
        let age = format_duration(now.saturating_sub(s.started_at));
        let model = s.model.as_deref().unwrap_or("");
        let label = session_label(s);
        println!(
            "  {label}  cwd: {cwd:<20}  started: {age}{model}",
            label = label,
            cwd = cwd_display,
            age = age,
            model = if model.is_empty() {
                String::new()
            } else {
                format!("  {}", model)
            }
        );
    }
    println!("\n{} session(s) total", sessions.len());
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
        Some(n) if !n.is_empty() => format!("{} ({})", n, short_id),
        _ => short_id.to_string(),
    }
}

fn run_shell_new(config: aish_config::ConfigModel) {
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    eprintln!("\x1b[32m[aish] Creating new PTY daemon session...\x1b[0m");
    match spawn_pty_daemon(
        &cwd,
        if config.model.is_empty() {
            None
        } else {
            Some(config.model.as_str())
        },
        if config.api_base.is_empty() {
            None
        } else {
            Some(config.api_base.as_str())
        },
    ) {
        Ok(info) => {
            if run_pty_raw_attach(&info.socket_path.to_string_lossy(), &info.session_id) {
                return;
            }
        }
        Err(e) => {
            eprintln!("\x1b[33m[aish] PTY daemon failed: {}\x1b[0m", e);
        }
    }
    eprintln!("\x1b[33m[aish] Falling back to normal shell.\x1b[0m");
    run_shell_normal(config);
}

fn run_shell_resume(config: aish_config::ConfigModel, session_id: &str) {
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
        run_pty_raw_attach(&session.socket_path.to_string_lossy(), &session.session_id);
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
fn run_pty_raw_attach(socket_path: &str, session_id: &str) -> bool {
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

    // SAFETY: [Category 8 — FFI] `write()` to stdout_fd — a single short
    // message. `stdout_fd` is stdout (valid). The byte slice is a static
    // literal; `.as_ptr()` is valid for its lifetime.
    unsafe {
        let msg = b"\r\n\x1b[32m[aish] Attached (Ctrl+Q to detach)\x1b[0m\r\n";
        libc::write(stdout_fd, msg.as_ptr() as *const _, msg.len());
    }

    // Flush scrollback immediately. The attach handshake consumed all
    // scrollback bytes from the socket into `pending_events`, so the
    // kernel socket buffer is now empty. Without this explicit drain,
    // select() would not report the socket readable and the buffered
    // scrollback (including the prompt) would never be displayed until
    // the user presses a key to generate new output.
    let mut initial_exit = false;
    if let Ok(events) = backend.drain_events() {
        for event in events {
            match event {
                aish_pty::PtyEvent::Output(bytes) => {
                    if !bytes.is_empty() {
                        let (clean, _cmds) = osc.process(&bytes);
                        if !clean.is_empty() {
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
        return true;
    }

    let mut stdin_buf = [0u8; 1024];

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
    eprintln!("\x1b[32m[aish] Detached. Run `aish` to reattach.\x1b[0m");
    true
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
        let info = match spawn_pty_daemon(&cwd, None, None) {
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
            // Check for OSC 5151 prefix at this position
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
                    // Incomplete payload — save for next chunk
                    self.pending = buf[i..].to_vec();
                    break;
                }
            } else if buf[i] == 0x1b && i + prefix.len() > buf.len() {
                // Partial prefix at buffer end: the ESC byte and possibly a
                // few following bytes could be the start of an OSC 5151
                // sequence split across chunks. Save them for next time
                // instead of emitting as clean output.
                self.pending = buf[i..].to_vec();
                break;
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
    fn test_show_models_usage_does_not_panic() {
        let config = aish_config::ConfigModel::default();
        show_models_usage(&config);
    }
}
