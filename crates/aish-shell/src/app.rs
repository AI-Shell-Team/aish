use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aish_config::ConfigModel;
use aish_core::{LlmEvent, LlmEventType, MemoryCategory};
use aish_i18n::{t, t_with_args};
use aish_llm::{LlmCallbackResult, LlmSession};
use aish_memory::MemoryManager;
use aish_security::{SecurityManager, SecurityPolicy};
use aish_session::SessionStore;
use aish_skills::hotreload::SkillHotReloader;
use aish_skills::SkillManager;
use aish_tools::ToolRegistry;

use crate::ai_handler::{AiHandler, SharedMemoryManager};
use crate::animation::SharedAnimation;
use crate::environment;
use crate::input;
use crate::prompt;
use crate::readline::ShellReadline;
use crate::renderer::ShellRenderer;
use crate::types::ShellState;

/// Braille spinner frames used in the reasoning overlay.
const DOTS_FRAMES: &[&str] = &["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"];

/// Shell lifecycle phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPhase {
    /// Shell is initializing (loading config, skills, etc.)
    Booting,
    /// Shell is ready and waiting for user input
    Editing,
    /// A command has been submitted and is executing
    Running,
    /// Shell is shutting down
    Exiting,
}

impl std::fmt::Display for ShellPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellPhase::Booting => write!(f, "booting"),
            ShellPhase::Editing => write!(f, "editing"),
            ShellPhase::Running => write!(f, "running"),
            ShellPhase::Exiting => write!(f, "exiting"),
        }
    }
}

/// State of user interruption handling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InterruptionState {
    /// Normal operation
    #[default]
    Normal,
    /// User is providing input
    Inputting,
    /// A clear/exit is pending (Ctrl+C was pressed once)
    ClearPending,
    /// Exit has been confirmed (Ctrl+C pressed twice)
    ExitPending,
}

/// Main shell application that ties together the REPL loop, command routing,
/// AI handler, security manager, session store, skill manager, and memory manager.
pub struct AishShell {
    pub state: ShellState,
    pub config: ConfigModel,
    pub ai_handler: AiHandler,
    pub security_manager: SecurityManager,
    pub session_store: Option<SessionStore>,
    pub skill_manager: SkillManager,
    pub skill_hot_reloader: Option<SkillHotReloader>,
    pub memory_manager: SharedMemoryManager,
    pub version: String,
    pub operation_in_progress: bool,
    /// Persistent PTY session for executing all external commands.
    pty: aish_pty::PersistentPty,
    /// UUID for the current session, used to associate history entries.
    session_uuid: String,
    /// Whether streaming has started printing content (to avoid double-printing).
    streamed_content: Arc<AtomicBool>,
    /// Current shell lifecycle phase
    phase: ShellPhase,
    /// Current interruption state
    interruption: InterruptionState,
    /// Timestamp of last Ctrl+C press (for double-press detection)
    last_ctrl_c: Option<std::time::Instant>,
}

impl AishShell {
    /// Create a new shell instance from the given configuration.
    pub fn new(config: ConfigModel) -> aish_core::Result<Self> {
        // Set terminal defaults and load bash environment
        environment::ensure_terminal_defaults();
        let _new_vars = environment::load_bash_env();

        let mut state = ShellState::new();
        for cmd in &config.approved_ai_commands {
            state.approved_ai_commands.insert(cmd.clone());
        }

        // Initialize LLM session
        let mut llm_session = LlmSession::new(
            &config.api_base,
            &config.api_key,
            &config.model,
            Some(config.temperature),
            config.max_tokens,
        );

        // Initialize security manager (before tool registration)
        let security_manager = SecurityManager::from_config(None)
            .unwrap_or_else(|_| SecurityManager::new(SecurityPolicy::default_policy()));

        // Register tools with security-checked bash execution
        let security_check = {
            let mgr = SecurityManager::new(security_manager.policy().clone());
            move |cmd: &str| mgr.check_command(cmd)
        };
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(aish_tools::SecureBashTool::with_security_check(
            security_check,
        )));
        tool_registry.register(Box::new(aish_tools::fs::ReadFileTool::new()));
        tool_registry.register(Box::new(aish_tools::fs::WriteFileTool::new()));
        tool_registry.register(Box::new(aish_tools::fs::EditFileTool::new()));
        tool_registry.register(Box::new(aish_tools::AskUserTool::new()));
        tool_registry.register(Box::new(aish_tools::PythonTool::new()));
        tool_registry.register(Box::new(aish_tools::GlobTool::new()));
        tool_registry.register(Box::new(aish_tools::GrepTool::new()));
        tool_registry.register(Box::new(aish_tools::EnterPlanModeTool::new()));
        tool_registry.register(Box::new(aish_tools::ExitPlanModeTool::new()));

        // Initialize shared memory manager (best-effort)
        let memory_manager: SharedMemoryManager = Arc::new(Mutex::new(
            MemoryManager::new(MemoryManager::default_path()).ok(),
        ));

        // Create MemoryTool with real callbacks connected to the shared MemoryManager
        let mm_for_search = memory_manager.clone();
        let mm_for_store = memory_manager.clone();
        let mm_for_delete = memory_manager.clone();
        let mm_for_list = memory_manager.clone();

        let memory_tool = aish_tools::MemoryTool::new(
            // search callback
            Box::new(move |query, limit| {
                let mut guard = mm_for_search.lock().unwrap();
                if let Some(ref mut mm) = *guard {
                    let results = mm.recall(query, limit);
                    results
                        .into_iter()
                        .map(|e| aish_tools::MemorySearchResult {
                            id: e.id as usize,
                            content: e.content.clone(),
                            category: format!("{:?}", e.category).to_lowercase(),
                        })
                        .collect()
                } else {
                    vec![]
                }
            }),
            // store callback
            Box::new(move |content, category, source, importance| {
                let mut guard = mm_for_store.lock().unwrap();
                if let Some(ref mut mm) = *guard {
                    let cat = parse_category_str(category);
                    match mm.store(content, cat, source, importance as f64) {
                        Ok(id) => id.to_string(),
                        Err(e) => format!("error: {}", e),
                    }
                } else {
                    "memory not available".to_string()
                }
            }),
            // delete callback
            Box::new(move |id| {
                let mut guard = mm_for_delete.lock().unwrap();
                if let Some(ref mut mm) = *guard {
                    mm.remove(id as i64).unwrap_or(false)
                } else {
                    false
                }
            }),
            // list callback
            Box::new(move |limit| {
                let guard = mm_for_list.lock().unwrap();
                if let Some(ref mm) = *guard {
                    mm.list()
                        .iter()
                        .rev()
                        .take(limit)
                        .map(|e| aish_tools::MemorySearchResult {
                            id: e.id as usize,
                            content: e.content.clone(),
                            category: format!("{:?}", e.category).to_lowercase(),
                        })
                        .collect()
                } else {
                    vec![]
                }
            }),
        );
        tool_registry.register(Box::new(memory_tool));
        // Load skills (best-effort, before tool registration so SkillTool can use real data)
        let mut skill_manager = SkillManager::new();
        let _ = skill_manager.load_all_skills();
        let skill_count = skill_manager.list_skills().len();

        // Start skill hot-reloader if skill directories exist
        let skill_hot_reloader = {
            let dirs = skill_manager.get_skill_dirs();
            if dirs.is_empty() {
                None
            } else {
                let reloader = SkillHotReloader::new(dirs);
                reloader.start();
                Some(reloader)
            }
        };

        // Wire SkillTool with real callbacks that look up skills from the loaded manager
        let skill_tool = {
            let skills_snapshot: std::collections::HashMap<String, aish_tools::SkillInfo> =
                skill_manager
                    .list_skills()
                    .iter()
                    .map(|s| {
                        (
                            s.metadata.name.clone(),
                            aish_tools::SkillInfo {
                                name: s.metadata.name.clone(),
                                content: s.content.clone(),
                                description: s.metadata.description.clone(),
                                base_dir: s.base_dir.clone(),
                            },
                        )
                    })
                    .collect();
            let skill_names: Vec<String> = skills_snapshot.keys().cloned().collect();
            let lookup = Box::new(move |name: &str| skills_snapshot.get(name).cloned());
            let list = Box::new(move || skill_names.clone());
            aish_tools::SkillTool::new(lookup, list)
        };
        tool_registry.register(Box::new(skill_tool));

        let tools: Vec<(String, Box<dyn aish_llm::Tool>)> = tool_registry.drain_tools();
        for (_name, tool) in tools {
            llm_session.register_tool(tool);
        }

        // Open session store (best-effort)
        let session_store = match &config.session_db_path {
            Some(path) => SessionStore::open(Some(std::path::Path::new(path))).ok(),
            None => SessionStore::open(None).ok(),
        };

        // Create session record if store is available
        let session_uuid = if let Some(ref store) = session_store {
            match store.create_session(&config.model, Some(&config.api_base)) {
                Ok(record) => record.session_uuid,
                Err(_) => uuid::Uuid::new_v4().to_string(),
            }
        } else {
            uuid::Uuid::new_v4().to_string()
        };

        // Resolve memory config (use defaults if not specified)
        let memory_config = config.memory.clone().unwrap_or_default();

        // Track whether content was streamed for display coordination
        let streamed_content = Arc::new(AtomicBool::new(false));

        // Shared animation controlled by event callback
        let animation: Arc<SharedAnimation> = Arc::new(SharedAnimation::new());
        // Shared renderer for streaming markdown re-rendering
        let renderer = Arc::new(std::sync::Mutex::new(ShellRenderer::new()));
        let renderer_ref = renderer.clone();

        // Set up LLM event callback for real-time streaming display
        let streamed_flag = streamed_content.clone();
        let content_started = Arc::new(AtomicBool::new(false));
        let content_started_flag = content_started.clone();
        let reasoning_buf = Arc::new(Mutex::new(String::new()));
        let reasoning_buf_ref = reasoning_buf.clone();
        let animation_ref = animation.clone();
        // TTFT tracking state
        let thinking_start: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let ttft_recorded = Arc::new(AtomicBool::new(false));
        let ttft_value: Arc<Mutex<f64>> = Arc::new(Mutex::new(0.0));
        let thinking_start_ref = thinking_start.clone();
        let ttft_recorded_ref = ttft_recorded.clone();
        let ttft_value_ref = ttft_value.clone();
        // Reasoning display state: whether reasoning overlay is on-screen
        // and a frame counter for the spinner.
        let reasoning_active = Arc::new(AtomicBool::new(false));
        let reasoning_active_ref = reasoning_active.clone();
        let reasoning_frame = Arc::new(AtomicUsize::new(0));
        let reasoning_frame_ref = reasoning_frame.clone();
        let reasoning_lines_displayed = Arc::new(AtomicUsize::new(0));
        let reasoning_lines_displayed_ref = reasoning_lines_displayed.clone();
        let event_callback: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync> =
            Arc::new(move |event: LlmEvent| {
                // Helper: clear multi-line reasoning overlay and reset state.
                let clear_reasoning = || {
                    let prev = reasoning_lines_displayed_ref.swap(0, Ordering::SeqCst);
                    if prev > 0 {
                        print!("\x1b[{}A", prev);
                        for _ in 0..prev {
                            print!("\r\x1b[K\n");
                        }
                        print!("\x1b[{}A", prev);
                        let _ = io::stdout().flush();
                    }
                    reasoning_active_ref.store(false, Ordering::SeqCst);
                };

                match event.event_type {
                    LlmEventType::OpStart => {
                        // Operation begins — start thinking animation
                        *thinking_start_ref.lock().unwrap() = Some(Instant::now());
                        ttft_recorded_ref.store(false, Ordering::SeqCst);
                        *ttft_value_ref.lock().unwrap() = 0.0;
                        reasoning_frame_ref.store(0, Ordering::SeqCst);
                        reasoning_lines_displayed_ref.store(0, Ordering::SeqCst);
                        reasoning_active_ref.store(false, Ordering::SeqCst);
                        animation_ref.start(&t("shell.status.thinking"));
                    }
                    LlmEventType::OpEnd => {
                        // Operation ends — stop animation and show timing
                        animation_ref.stop();
                        let ttft = *ttft_value_ref.lock().unwrap();
                        if ttft >= 0.1 {
                            println!("\x1b[2m思考: {:.1}s\x1b[0m", ttft);
                        }
                        *thinking_start_ref.lock().unwrap() = None;
                    }
                    LlmEventType::GenerationStart => {
                        animation_ref.stop();
                        clear_reasoning();
                        content_started_flag.store(false, Ordering::SeqCst);
                        reasoning_buf_ref.lock().unwrap().clear();
                        reasoning_frame_ref.store(0, Ordering::SeqCst);
                        renderer_ref.lock().unwrap().reset();
                        animation_ref.start(&t("shell.status.thinking"));
                    }
                    LlmEventType::GenerationEnd => {
                        animation_ref.stop();
                        clear_reasoning();
                        // Finalize streaming display (newline + reset)
                        if content_started_flag.load(Ordering::SeqCst) {
                            renderer_ref.lock().unwrap().finalize_stream();
                        }
                    }
                    LlmEventType::ContentDelta => {
                        if let Some(delta) = event.data.get("delta").and_then(|d| d.as_str()) {
                            if !delta.is_empty() {
                                animation_ref.stop();
                                if !ttft_recorded_ref.load(Ordering::SeqCst) {
                                    if let Some(start) = *thinking_start_ref.lock().unwrap() {
                                        let elapsed = start.elapsed().as_secs_f64();
                                        *ttft_value_ref.lock().unwrap() = elapsed;
                                        ttft_recorded_ref.store(true, Ordering::SeqCst);
                                    }
                                }
                                streamed_flag.store(true, Ordering::SeqCst);
                                clear_reasoning();
                                // Robot emoji prefix on first content chunk
                                if !content_started_flag.load(Ordering::SeqCst) {
                                    content_started_flag.store(true, Ordering::SeqCst);
                                    renderer_ref.lock().unwrap().render_separator();
                                    print!("\x1b[1;90m🤖 ");
                                }
                                // Accumulate delta and print raw text
                                renderer_ref.lock().unwrap().append_delta(delta);
                            }
                        }
                    }
                    LlmEventType::ReasoningStart => {
                        animation_ref.stop();
                        reasoning_active_ref.store(true, Ordering::SeqCst);
                        reasoning_frame_ref.store(0, Ordering::SeqCst);
                        reasoning_lines_displayed_ref.store(0, Ordering::SeqCst);
                        reasoning_buf_ref.lock().unwrap().clear();
                    }
                    LlmEventType::ReasoningDelta => {
                        if let Some(delta) = event.data.get("delta").and_then(|d| d.as_str()) {
                            if !delta.is_empty() {
                                animation_ref.stop();
                                let mut buf = reasoning_buf_ref.lock().unwrap();
                                buf.push_str(delta);

                                // Get last 2 non-empty lines
                                let all_lines: Vec<&str> =
                                    buf.lines().filter(|l| !l.trim().is_empty()).collect();
                                let display_lines: Vec<&str> = all_lines
                                    .iter()
                                    .rev()
                                    .take(2)
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .rev()
                                    .copied()
                                    .collect();

                                let max_cols = crossterm::terminal::size()
                                    .map(|(_, cols)| cols as usize)
                                    .unwrap_or(80)
                                    .max(20)
                                    .saturating_sub(4);

                                let frame = reasoning_frame_ref.fetch_add(1, Ordering::SeqCst);
                                let spinner = DOTS_FRAMES[frame % DOTS_FRAMES.len()];

                                // Elapsed time for header
                                let elapsed_str = thinking_start_ref
                                    .lock()
                                    .unwrap()
                                    .map(|s| {
                                        let e = s.elapsed().as_secs_f64();
                                        if e >= 1.0 {
                                            format!(" 思考中 {:.1}s", e)
                                        } else {
                                            " 思考中".to_string()
                                        }
                                    })
                                    .unwrap_or_else(|| " 思考中".to_string());

                                let prev = reasoning_lines_displayed_ref.load(Ordering::SeqCst);
                                let new_count = 1 + display_lines.len();

                                // Move cursor up to overwrite previous display
                                if prev > 0 {
                                    print!("\x1b[{}A", prev);
                                }

                                // Header line
                                if display_lines.is_empty() {
                                    print!(
                                        "\r\x1b[K\x1b[90m{}{}...\x1b[0m\n",
                                        spinner, elapsed_str
                                    );
                                } else {
                                    print!("\r\x1b[K\x1b[90m{}{}\x1b[0m\n", spinner, elapsed_str);
                                }

                                // Content lines
                                for line in &display_lines {
                                    let truncated = truncate_display_width(line.trim(), max_cols);
                                    print!("\r\x1b[K\x1b[90m{}\x1b[0m\n", truncated);
                                }

                                // Clear leftover lines from previous larger display
                                for _ in new_count..prev {
                                    print!("\r\x1b[K\n");
                                }

                                // Move cursor back up from extra cleared lines
                                if prev > new_count {
                                    print!("\x1b[{}A", prev - new_count);
                                }

                                reasoning_lines_displayed_ref.store(new_count, Ordering::SeqCst);
                                reasoning_active_ref.store(true, Ordering::SeqCst);
                                let _ = io::stdout().flush();
                            }
                        }
                    }
                    LlmEventType::ReasoningEnd => {
                        clear_reasoning();
                        reasoning_buf_ref.lock().unwrap().clear();
                    }
                    LlmEventType::ToolExecutionStart => {
                        if let Some(name) = event.data.get("tool_name").and_then(|n| n.as_str()) {
                            animation_ref.stop();
                            clear_reasoning();
                            let args_preview = event
                                .data
                                .get("tool_args")
                                .map(|a| format_tool_args_for_display(name, a))
                                .unwrap_or_default();
                            // Ensure we're on a fresh line after content streaming
                            if content_started_flag.load(Ordering::SeqCst) {
                                println!();
                            }
                            println!(
                                "\x1b[36m{}: {} ({})\x1b[0m",
                                t("shell.tool.prefix"),
                                name,
                                args_preview
                            );
                            let _ = io::stdout().flush();
                        }
                    }
                    LlmEventType::ToolExecutionEnd => {
                        let is_diagnose = event
                            .data
                            .get("source")
                            .and_then(|s| s.as_str())
                            .map(|s| s == "system_diagnose_agent")
                            .unwrap_or(false);
                        if is_diagnose {
                            let name = event
                                .data
                                .get("tool_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            let ok = event
                                .data
                                .get("ok")
                                .and_then(|o| o.as_bool())
                                .unwrap_or(true);
                            let (status, color) = if ok { ("✓", "32") } else { ("✗", "31") };
                            println!("\x1b[{}m  {} {}\x1b[0m", color, status, name);
                        }
                    }
                    LlmEventType::Error => {
                        animation_ref.stop();
                        clear_reasoning();
                        let error_msg = event
                            .data
                            .get("error")
                            .or_else(|| event.data.get("error_message"))
                            .and_then(|e| e.as_str())
                            .unwrap_or("Unknown error");
                        let msg = {
                            let mut args = std::collections::HashMap::new();
                            args.insert("error".to_string(), error_msg.to_string());
                            t_with_args("shell.error.llm_error_message", &args)
                        };
                        eprintln!("\x1b[31m{}\x1b[0m", msg);
                    }
                    LlmEventType::Cancelled => {
                        animation_ref.stop();
                        clear_reasoning();
                        println!("\x1b[33m{}\x1b[0m", t("shell.command_cancelled"));
                    }
                    LlmEventType::ToolConfirmationRequired => {
                        // Handled by separate confirmation_callback
                    }
                    LlmEventType::InteractionRequired => {
                        let prompt_text = event
                            .data
                            .get("prompt")
                            .and_then(|p| p.as_str())
                            .unwrap_or("");
                        if !prompt_text.is_empty() {
                            println!("\x1b[36m{}\x1b[0m", prompt_text);
                        }
                    }
                }
                None // Always continue
            });

        llm_session.set_event_callback(event_callback);

        // Set up confirmation callback for tool approval flow
        let confirmation_callback: Arc<dyn Fn(&str, &str) -> bool + Send + Sync> =
            Arc::new(|tool_name: &str, message: &str| {
                let width = std::env::var("COLUMNS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(80);
                let border = "─".repeat(width.saturating_sub(4));
                println!();
                println!("\x1b[33m╭{}╮\x1b[0m", border);
                println!(
                    "\x1b[33m│\x1b[1;33m ⚠  Security Confirmation Required\x1b[0m{}",
                    pad_to_width("", width.saturating_sub(38))
                );
                println!("\x1b[33m│\x1b[0m");
                println!("\x1b[33m│\x1b[0m  \x1b[1;36mTool:\x1b[0m   {}", tool_name);
                let reason_lines = wrap_text(message, width.saturating_sub(14));
                println!(
                    "\x1b[33m│\x1b[0m  \x1b[1;36mReason:\x1b[0m {}",
                    reason_lines.lines().next().unwrap_or("")
                );
                for line in reason_lines.lines().skip(1) {
                    println!("\x1b[33m│\x1b[0m         {}", line);
                }
                println!("\x1b[33m│\x1b[0m");
                println!("\x1b[33m│\x1b[0m  \x1b[36mAllow? [y/N]\x1b[0m");
                println!("\x1b[33m╰{}╯\x1b[0m", border);
                print!("  ");
                let _ = std::io::stdout().flush();

                let mut answer = String::new();
                if std::io::stdin().read_line(&mut answer).is_err() {
                    return false;
                }
                let answer = answer.trim().to_lowercase();
                answer == "y" || answer == "yes"
            });

        llm_session.set_confirmation_callback(confirmation_callback);

        // Build AI handler with all subsystems
        let ai_handler = AiHandler::new(
            llm_session,
            memory_manager.clone(),
            skill_manager,
            memory_config,
            config.max_llm_messages,
            config.max_shell_messages,
            config.context_token_budget,
        );

        // Note: event_callback is already set on the LlmSession before AiHandler takes ownership

        let version = env!("CARGO_PKG_VERSION").to_string();

        // Print welcome banner
        print!(
            "{}",
            prompt::render_welcome(&version, &config.model, skill_count)
        );
        let _ = io::stdout().flush();

        // Initialize persistent PTY session
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let pty = aish_pty::PersistentPty::start(&state.cwd, rows, cols).map_err(|e| {
            aish_core::AishError::Pty(format!("failed to start persistent PTY: {e}"))
        })?;

        // Placeholder instances for struct fields.  The real subsystems live
        // inside AiHandler which needs mutable access during each turn.
        let shell_skill_manager = SkillManager::new();

        Ok(Self {
            state,
            config,
            ai_handler,
            security_manager,
            session_store,
            skill_manager: shell_skill_manager,
            skill_hot_reloader,
            memory_manager: memory_manager.clone(),
            version,
            operation_in_progress: false,
            pty,
            session_uuid,
            streamed_content,
            phase: ShellPhase::Booting,
            interruption: InterruptionState::default(),
            last_ctrl_c: None,
        })
    }

    /// Run the main REPL loop.
    pub fn run(&mut self) -> aish_core::Result<()> {
        let runtime = tokio::runtime::Runtime::new()?;

        // Set up SIGTERM handler for graceful shutdown
        let sigterm_exit = Arc::new(AtomicBool::new(false));
        let sigterm_flag = sigterm_exit.clone();
        runtime.spawn(async move {
            let mut sigterm_stream =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(_) => return,
                };
            sigterm_stream.recv().await;
            sigterm_flag.store(true, Ordering::SeqCst);
        });

        // Initialize readline with history, tab completion, and line editing
        let mut rl = ShellReadline::new().map_err(|e| {
            aish_core::AishError::Config(format!("Failed to initialize readline: {}", e))
        })?;

        // Load history from default location
        let history_path = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("aish")
            .join("history.txt");
        if let Some(parent) = history_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        rl.load_history(&history_path);

        // Load recent history from SQLite across all sessions
        if let Some(ref store) = self.session_store {
            if let Ok(sessions) = store.list_sessions(5) {
                for session in sessions.iter() {
                    if let Ok(entries) = store.get_history(&session.session_uuid, 200) {
                        for entry in entries.iter().rev() {
                            rl.add_history_entry(&entry.command);
                        }
                    }
                }
            }
        }

        self.set_phase(ShellPhase::Editing);

        loop {
            if self.state.should_exit {
                break;
            }
            if sigterm_exit.load(Ordering::SeqCst) {
                break;
            }

            // Check for skill hot-reload changes
            if let Some(ref reloader) = self.skill_hot_reloader {
                let affected = reloader.apply_changes(&mut self.skill_manager);
                if !affected.is_empty() {
                    for name in &affected {
                        tracing::info!("Skill '{}' hot-reloaded", name);
                    }
                }
            }

            // Render prompt and read input via rustyline
            let mode = match self.ai_handler.plan_phase() {
                aish_core::PlanPhase::Planning => "plan",
                aish_core::PlanPhase::Normal => "aish",
            };
            let prompt_str = prompt::render_prompt(
                &self.state.cwd,
                &self.config.model,
                self.state.last_exit_code,
                mode,
            );
            let input = match rl.read_line(&prompt_str) {
                Ok(Some(line)) => line,
                Ok(None) => break, // EOF (Ctrl-D)
                Err(e) => {
                    // Check if Shift+Tab or F2 triggered the interrupt (mode toggle)
                    if matches!(e, rustyline::error::ReadlineError::Interrupted)
                        && rl.was_mode_toggle_requested()
                    {
                        let new_phase = self.ai_handler.toggle_plan_mode(&self.session_uuid);
                        match new_phase {
                            aish_core::PlanPhase::Planning => {
                                println!("\x1b[1;33m=== Plan Mode ===\x1b[0m");
                                println!("\x1b[2mType ; followed by your planning request.\x1b[0m");
                            }
                            aish_core::PlanPhase::Normal => {
                                println!("\x1b[33mExited plan mode.\x1b[0m");
                            }
                        }
                        continue;
                    }
                    // Interrupt (Ctrl-C) — handle double-press exit
                    if matches!(e, rustyline::error::ReadlineError::Interrupted) {
                        if self.handle_ctrl_c() {
                            break;
                        }
                        continue;
                    }
                    eprintln!("Readline error: {}", e);
                    break;
                }
            };
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            // Add to history
            self.state.history.push(input.to_string());

            // Reset streamed-content flag before each AI call
            self.streamed_content.store(false, Ordering::SeqCst);

            // Classify and route
            match input::classify_input(input) {
                crate::types::InputIntent::Empty => {}
                crate::types::InputIntent::Ai => {
                    let question = input::extract_ai_question(input);

                    // If just ";" with no question and there's a pending error,
                    // trigger error correction instead of a normal AI query.
                    if question.is_empty() && self.state.can_correct_error {
                        if let Some(ref cmd) = self.state.last_command.clone() {
                            let result = runtime.block_on(async {
                                tokio::select! {
                                    r = self.ai_handler.handle_error_correction(
                                        cmd,
                                        self.state.last_exit_code,
                                        &self.state.last_output,
                                    ) => r,
                                    _ = tokio::signal::ctrl_c() => {
                                        println!("\n\x1b[33mInterrupted\x1b[0m");
                                        Err(aish_core::AishError::Cancelled)
                                    }
                                }
                            });
                            let did_stream = self.streamed_content.load(Ordering::SeqCst);

                            match result {
                                Ok(Some(corrected)) => {
                                    if !did_stream {
                                        println!("\x1b[36m{}\x1b[0m", corrected);
                                    }
                                    println!(
                                        "\n\x1b[2m{}\x1b[0m",
                                        t("shell.error_correction.retry_hint")
                                    );
                                    // Auto-execute the corrected command
                                    let exit_code = self.execute_external_command(&corrected);
                                    self.record_history(&corrected, exit_code);
                                    self.state.can_correct_error = false;
                                }
                                Ok(None) => {
                                    println!(
                                        "\x1b[33m{}\x1b[0m",
                                        t("shell.error_correction.no_valid_command")
                                    );
                                }
                                Err(e) => {
                                    let msg = t("shell.error.llm_error_message")
                                        .replace("{error}", &e.to_string());
                                    eprintln!("\x1b[31m{}\x1b[0m", msg);
                                }
                            }
                            continue;
                        }
                    }

                    let result = runtime.block_on(async {
                        tokio::select! {
                            r = self.ai_handler.handle_question(&question) => r,
                            _ = tokio::signal::ctrl_c() => {
                                // Signal the LLM session to cancel the in-flight request.
                                self.ai_handler.cancel();
                                println!("\n\x1b[33mInterrupted\x1b[0m");
                                Err(aish_core::AishError::Cancelled)
                            }
                        }
                    });

                    let did_stream = self.streamed_content.load(Ordering::SeqCst);

                    match result {
                        Ok(response) => {
                            if !did_stream && !response.is_empty() {
                                // Non-streaming fallback: print full response with formatting
                                let mut sep_renderer = ShellRenderer::new();
                                sep_renderer.render_separator();
                                print_md(&response);
                                sep_renderer.render_separator();
                            } else if did_stream {
                                // Streaming display already handled by event callback
                                // No additional output needed here.
                            }

                            // Check if plan mode was exited during this AI turn.
                            // If exit_plan_mode tool was called, show plan approval UI.
                            let plan_state = self.ai_handler.plan_state();
                            if plan_state.phase == aish_core::PlanPhase::Normal
                                && plan_state.plan_id.is_some()
                            {
                                if let Some(artifact_path) = plan_state.artifact_path.as_ref() {
                                    // Plan was exited — read artifact and present for approval
                                    let artifact_text =
                                        aish_core::plan::read_artifact_text(artifact_path);

                                    // Use the enhanced plan approval flow
                                    use crate::wizard::plan_approval::{
                                        PlanApprovalDecision, PlanApprovalFlow,
                                    };
                                    let decision = PlanApprovalFlow::review_plan(
                                        &artifact_text,
                                        plan_state.summary.as_deref(),
                                        if plan_state.draft_revision > 0 {
                                            Some(plan_state.draft_revision)
                                        } else {
                                            None
                                        },
                                    );

                                    match decision {
                                        PlanApprovalDecision::Approved => {
                                            // Create approved snapshot and transition state
                                            let mut state = self.ai_handler.plan_state();
                                            if let Ok(_snapshot) =
                                                aish_core::plan::create_approved_snapshot(
                                                    &mut state,
                                                )
                                            {
                                                println!("\x1b[32m\u{2713} Plan approved. Implementation tools are now available.\x1b[0m");
                                                println!(
                                                    "\x1b[2m  Snapshot saved at: {}\x1b[0m",
                                                    state
                                                        .approved_artifact_path
                                                        .as_deref()
                                                        .unwrap_or("(none)")
                                                );
                                            }
                                        }
                                        PlanApprovalDecision::ChangesRequested { feedback } => {
                                            // Keep in planning phase — re-enter plan mode with feedback
                                            println!("\x1b[33m\u{21B3} Changes requested. Re-entering plan mode...\x1b[0m");

                                            // Re-enter plan mode to let the AI revise
                                            self.ai_handler.enter_plan_mode(&self.session_uuid);

                                            // Set the approval status and feedback directly on the mutex
                                            {
                                                let plan_state_lock =
                                                    self.ai_handler.plan_state_ptr();
                                                let mut ps = plan_state_lock.lock().unwrap();
                                                ps.approval_status =
                                                    aish_core::PlanApprovalStatus::ChangesRequested;
                                                ps.approval_feedback_summary =
                                                    if feedback.is_empty() {
                                                        None
                                                    } else {
                                                        Some(feedback.clone())
                                                    };
                                                // Bump revision since we're requesting changes
                                                aish_core::plan::bump_draft_revision(&mut ps);
                                                // Preserve the artifact path from the previous plan
                                                ps.artifact_path = plan_state.artifact_path.clone();
                                                ps.plan_id = plan_state.plan_id.clone();
                                            }

                                            // If feedback was provided, send it back to the AI
                                            // by injecting it as context
                                            if !feedback.is_empty() {
                                                let feedback_msg = format!(
                                                    "[Plan Review Feedback]\nThe user requested changes to the plan:\n{}\n\nPlease revise the plan accordingly and use exit_plan_mode when ready.",
                                                    feedback
                                                );
                                                self.ai_handler.add_shell_context(&feedback_msg);
                                                println!("\x1b[2m  Feedback sent to AI. Type ; to continue planning.\x1b[0m");
                                            }
                                        }
                                        PlanApprovalDecision::Cancelled => {
                                            println!("\x1b[33mPlan review cancelled. Plan mode exited.\x1b[0m");
                                            println!("\x1b[2mUse /plan to start a new planning session.\x1b[0m");
                                        }
                                    }
                                }
                            }

                            self.record_history(input, 0);
                        }
                        Err(e) => {
                            let msg = t("shell.error.llm_error_message")
                                .replace("{error}", &e.to_string());
                            eprintln!("\x1b[31m{}\x1b[0m", msg);
                            self.record_history(input, 1);
                        }
                    }
                }
                crate::types::InputIntent::Help => {
                    let result = self.state.handle_builtin("help", &[]);
                    if let Some(output) = result.output {
                        println!("{}", output);
                    }
                    self.record_history(input, 0);
                }
                crate::types::InputIntent::BuiltinCommand => {
                    let parts: Vec<&str> = input.split_whitespace().collect();
                    if let Some(cmd) = parts.first() {
                        let result = self.state.handle_builtin(cmd, &parts[1..]);
                        if let Some(output) = result.output {
                            println!("{}", output);
                        }
                        if result.should_exit {
                            self.record_history(input, 0);
                            break;
                        }
                        // PTY-required commands (su, sudo) — route directly to PTY
                        if result.route_to_pty {
                            if let Some(ref pty_cmd) = result.pty_command {
                                self.set_phase(ShellPhase::Running);
                                let exit_code = self.execute_external_command(pty_cmd);
                                self.set_phase(ShellPhase::Editing);
                                self.record_history(input, exit_code);
                                self.reset_interruption();
                                continue;
                            }
                        }
                        // State-modifying commands (cd, pushd, popd, export,
                        // unset) also need to be sent to the PTY bash process
                        // so that the persistent bash session stays in sync.
                        // Otherwise bash's CWD/env diverges from the Rust
                        // shell's tracking, causing the next external command
                        // to run in the wrong directory/environment.
                        if crate::commands::is_state_modifying(cmd)
                            && !crate::commands::is_rejected(cmd)
                        {
                            self.sync_command_to_pty(input);
                        }
                    }
                    self.record_history(input, 0);
                }
                crate::types::InputIntent::SpecialCommand => {
                    self.handle_special_command(input);
                    self.record_history(input, 0);
                }
                crate::types::InputIntent::OperatorCommand | crate::types::InputIntent::Command => {
                    self.set_phase(ShellPhase::Running);
                    let exit_code = self.execute_external_command(input);
                    self.set_phase(ShellPhase::Editing);
                    self.record_history(input, exit_code);
                    self.reset_interruption();

                    // Track for error correction
                    self.state.last_command = Some(input.to_string());
                    self.state.last_exit_code = exit_code;
                    self.state.can_correct_error = exit_code != 0 && exit_code != 130;

                    // Inject command result into LLM context so AI can reference
                    // previous command output in follow-up questions.
                    let output_preview = if self.state.last_output.len() > 4096 {
                        // Safe UTF-8 truncation: find nearest char boundary
                        let end = {
                            let mut j = 4096;
                            while j > 0 && !self.state.last_output.is_char_boundary(j) {
                                j -= 1;
                            }
                            j
                        };
                        &self.state.last_output[..end]
                    } else {
                        &self.state.last_output
                    };
                    if !output_preview.trim().is_empty() {
                        let entry = format!(
                            "[Shell] {}\n<returncode>{}</returncode>\n<output>{}</output>",
                            input, exit_code, output_preview
                        );
                        self.ai_handler.add_shell_context(&entry);
                    }

                    // Show error correction hint
                    if exit_code != 0 && exit_code != 130 {
                        let hint = t("shell.error_correction.press_semicolon_hint");
                        eprintln!("\x1b[2m\x1b[37m<{}>\x1b[0m", hint);
                    }
                }
                crate::types::InputIntent::ScriptCall => {
                    let exit_code = self.execute_script(input);
                    self.record_history(input, exit_code);
                }
            }
        }

        // Save history on exit
        rl.save_history(&history_path);

        self.set_phase(ShellPhase::Exiting);

        Ok(())
    }

    /// Record a command to the session store.
    fn record_history(&self, command: &str, returncode: i32) {
        if let Some(ref store) = self.session_store {
            let _ = store.add_history_entry(&aish_session::HistoryEntry {
                id: None,
                session_uuid: self.session_uuid.clone(),
                command: command.to_string(),
                source: "user".to_string(),
                returncode: Some(returncode),
                stdout: None,
                stderr: None,
                created_at: chrono::Utc::now(),
            });
        }
    }

    /// Handle special slash commands (/model, /setup, /plan, etc.).
    fn handle_special_command(&mut self, input: &str) {
        let parts: Vec<&str> = input.split_whitespace().collect();
        match parts.first().copied() {
            Some("/model") => self.handle_model_command(&parts),
            Some("/setup") => self.run_setup_wizard(),
            Some("/plan") => self.handle_plan_command(&parts),
            Some("/token") => self.handle_token_command(),
            _ => {
                eprintln!("Unknown special command: {}", input);
            }
        }
    }

    /// Handle `/model [name]` — show current model or switch to a new one.
    fn handle_model_command(&mut self, parts: &[&str]) {
        if parts.len() == 1 {
            let mut args = std::collections::HashMap::new();
            args.insert("model".to_string(), self.config.model.clone());
            println!("{}", t_with_args("shell.model.current", &args));
            return;
        }

        if parts.len() > 1 && (parts[1] == "--help" || parts[1] == "-h") {
            println!("\x1b[36mUsage: /model [model-name]\x1b[0m");
            return;
        }

        let new_model = parts[1..].join(" ");
        if new_model == self.config.model {
            let mut args = std::collections::HashMap::new();
            args.insert("model".to_string(), new_model);
            println!("{}", t_with_args("shell.model.switch_same", &args));
            return;
        }

        // Detect provider for the new model
        let _provider = aish_llm::detect_provider(&new_model, &self.config.api_base);

        // Update LLM session
        self.ai_handler.update_model(
            &new_model,
            Some(&self.config.api_base),
            Some(&self.config.api_key),
        );

        // Update config
        self.config.model = new_model.clone();

        // Persist to config file
        let config_path = aish_config::ConfigLoader::default_config_path();
        if let Err(e) = aish_config::ConfigLoader::save(&self.config, &config_path) {
            eprintln!("\x1b[33mWarning: could not save config: {}\x1b[0m", e);
        }

        let mut args = std::collections::HashMap::new();
        args.insert("model".to_string(), new_model);
        println!("{}", t_with_args("shell.model.switch_success", &args));
    }

    /// Handle `/plan [start|status|exit]` — plan mode lifecycle.
    fn handle_plan_command(&mut self, parts: &[&str]) {
        use aish_core::PlanPhase;

        if parts.len() > 1 && (parts[1] == "--help" || parts[1] == "-h") {
            println!("\x1b[36mUsage: /plan [start|status|exit]\x1b[0m");
            return;
        }

        let plan_state = self.ai_handler.plan_state();
        let current_phase = self.ai_handler.plan_phase();
        let subcommand = parts.get(1).copied().unwrap_or("");

        // Reject unknown subcommands
        if !subcommand.is_empty() && !["start", "status", "exit"].contains(&subcommand) {
            eprintln!("\x1b[31mUnknown /plan subcommand: {}\x1b[0m", subcommand);
            return;
        }

        match current_phase {
            PlanPhase::Planning => {
                match subcommand {
                    "exit" => {
                        self.ai_handler.exit_plan_mode();
                        println!("\x1b[33mExited plan mode.\x1b[0m");
                    }
                    _ => {
                        // Bare `/plan` or `/plan status` while planning → show status
                        let plan_id = plan_state.plan_id.as_deref().unwrap_or("unknown");
                        println!("\x1b[1;36mPlan Mode (active)\x1b[0m");
                        println!("  Plan ID: {}", plan_id);
                        println!("  Approval: {}", plan_state.approval_status);
                        println!(
                            "  Artifact: {}",
                            plan_state.artifact_path.as_deref().unwrap_or("-")
                        );
                    }
                }
            }
            PlanPhase::Normal => {
                match subcommand {
                    "exit" => {
                        println!("mode=shell, approval_status=draft, artifact=-");
                    }
                    _ => {
                        // `/plan` or `/plan start` from shell mode → enter planning
                        self.ai_handler.enter_plan_mode(&self.session_uuid);
                        let plan_state = self.ai_handler.plan_state();
                        let plan_id = plan_state.plan_id.as_deref().unwrap_or("unknown");
                        println!("\x1b[1;36m=== Plan Mode ===\x1b[0m");
                        println!("\x1b[2mPlan ID: {}\x1b[0m", plan_id);
                        println!("\x1b[2mDuring planning, the AI has access to read-only tools and write_file/edit_file for the plan artifact.\x1b[0m");
                        println!(
                            "\x1b[2mType ; followed by your planning request to start.\x1b[0m"
                        );
                    }
                }
            }
        }
    }

    /// Handle `/token` — show cumulative token usage statistics (last 7 days).
    fn handle_token_command(&self) {
        let stats = self.ai_handler.token_stats();
        let total = stats.total_input + stats.total_output;
        println!();
        println!("Token Usage (last 7 days)");
        println!("  Input tokens:   {}", format_number(stats.total_input));
        println!("  Output tokens:  {}", format_number(stats.total_output));
        println!("  Total:          {}", format_number(total));
        println!("  API calls:      {}", format_number(stats.request_count));
        println!();
    }

    /// Interactive setup wizard for configuring provider, API key, model, etc.
    fn run_setup_wizard(&mut self) {
        let config_dir = aish_config::ConfigLoader::default_config_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("aish")
            });

        let mut wizard = crate::wizard::SetupWizard::new(config_dir);
        match wizard.run() {
            Ok(new_config) => {
                self.config = new_config;
                // Update LLM session with new config
                self.ai_handler.update_model(
                    &self.config.model,
                    Some(&self.config.api_base),
                    Some(&self.config.api_key),
                );
                let mut args = std::collections::HashMap::new();
                args.insert("model".to_string(), self.config.model.clone());
                println!(
                    "\n\x1b[32m{}\x1b[0m",
                    t_with_args("shell.setup.applied", &args)
                );
            }
            Err(e) => {
                eprintln!("\x1b[33mSetup cancelled: {}\x1b[0m", e);
            }
        }
    }

    /// Execute an external command via the persistent PTY session.
    fn execute_external_command(&mut self, command: &str) -> i32 {
        // Sync terminal size before each command
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            self.pty.resize(rows, cols);
        }

        // Ensure the PTY is alive before sending a command.
        // If the bash process died (e.g., after `cd` triggered a subshell
        // exit, or a race with the control pipe), restart it first so the
        // command runs in the correct working directory.
        if !self.pty.is_running() {
            self.restart_pty();
        }

        let (exit_code, cwd, output) = match self.pty.send_command_interactive(command) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("PTY error: {}", e);
                // PTY may have died, try restart
                self.restart_pty();
                return 1;
            }
        };

        // Store captured output for error correction and LLM context
        self.state.last_output = output.clone();

        // Update CWD from PTY event
        if !cwd.is_empty() && cwd != self.state.cwd {
            self.state.prev_cwd = Some(self.state.cwd.clone());
            self.state.cwd = cwd.clone();
            // Sync the actual process CWD so that any spawned subprocesses
            // (e.g., via AI tool execution) inherit the correct directory.
            let _ = std::env::set_current_dir(&cwd);
        }

        // Check if PTY is still running, restart if not
        if !self.pty.is_running() {
            self.restart_pty();
        }

        exit_code
    }

    /// Silently sync a state-modifying command (cd, export, etc.) to the
    /// persistent PTY bash process so that bash's CWD and env stay in sync
    /// with the Rust shell's tracking. Output is discarded.
    fn sync_command_to_pty(&mut self, command: &str) {
        if !self.pty.is_running() {
            return;
        }
        let _ = self
            .pty
            .execute_command(command, std::time::Duration::from_secs(5));
    }

    /// Restart the PTY session (e.g., after bash exits or crashes).
    fn restart_pty(&mut self) {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        match aish_pty::PersistentPty::start(&self.state.cwd, rows, cols) {
            Ok(new_pty) => {
                self.pty = new_pty;
                println!("\x1b[33mbash session restarted\x1b[0m");
            }
            Err(e) => {
                eprintln!("Failed to restart bash: {}", e);
                self.state.should_exit = true;
            }
        }
    }

    /// Get the current shell phase.
    pub fn phase(&self) -> &ShellPhase {
        &self.phase
    }

    /// Transition to a new phase.
    pub fn set_phase(&mut self, phase: ShellPhase) {
        tracing::debug!("Shell phase: {} → {}", self.phase, phase);
        self.phase = phase;
    }

    /// Handle a Ctrl+C interruption.
    /// Returns true if the shell should exit.
    pub fn handle_ctrl_c(&mut self) -> bool {
        let now = std::time::Instant::now();

        match self.interruption {
            InterruptionState::Normal | InterruptionState::Inputting => {
                self.interruption = InterruptionState::ClearPending;
                self.last_ctrl_c = Some(now);
                println!("\n\x1b[33m(Ctrl+C again within 1s to exit)\x1b[0m");
                false
            }
            InterruptionState::ClearPending => {
                if let Some(last) = self.last_ctrl_c {
                    if now.duration_since(last).as_secs() < 1 {
                        self.interruption = InterruptionState::ExitPending;
                        println!("\n\x1b[33mExiting...\x1b[0m");
                        return true;
                    }
                }
                self.interruption = InterruptionState::ClearPending;
                self.last_ctrl_c = Some(now);
                println!("\n\x1b[33m(Ctrl+C again within 1s to exit)\x1b[0m");
                false
            }
            InterruptionState::ExitPending => true,
        }
    }

    /// Reset interruption state to normal.
    pub fn reset_interruption(&mut self) {
        self.interruption = InterruptionState::Normal;
        self.last_ctrl_c = None;
    }

    /// Check if a command is pre-approved and should skip confirmation.
    pub fn is_command_approved(&self, command: &str) -> bool {
        self.state.approved_ai_commands.contains(command)
    }

    /// Remember a command as approved for future use.
    pub fn remember_approved_command(&mut self, command: &str) {
        if command.is_empty() {
            return;
        }
        self.state.approved_ai_commands.insert(command.to_string());

        // Persist to config if not already tracked
        if !self
            .config
            .approved_ai_commands
            .contains(&command.to_string())
        {
            self.config.approved_ai_commands.push(command.to_string());
            let config_path = aish_config::ConfigLoader::default_config_path();
            let _ = aish_config::ConfigLoader::save(&self.config, &config_path);
        }
    }

    /// Execute a .aish script file.
    fn execute_script(&mut self, input: &str) -> i32 {
        let parts: Vec<&str> = input.split_whitespace().collect();
        let script_path = match parts.first() {
            Some(p) => p,
            None => return 1,
        };

        // Try to load and parse the script
        let script =
            match aish_scripts::loader::parse_script_file(std::path::Path::new(script_path)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to load script '{}': {}", script_path, e);
                    // Fall back to executing via bash
                    return self.execute_external_command(input);
                }
            };

        // Collect arguments
        let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

        // Check if the script contains any AI calls
        let has_ai_calls = script.content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("ai ") || trimmed.starts_with("ai\t")
        });

        if !has_ai_calls {
            // No AI calls — use ScriptExecutor directly (faster, no async needed)
            let executor = aish_scripts::ScriptExecutor::new();
            let result = executor.execute(&script, &args);

            if !result.output.is_empty() {
                print!("{}", result.output);
            }
            if !result.error.is_empty() {
                eprint!("{}", result.error);
            }

            self.apply_script_result(&result);
            return if result.success { 0 } else { result.returncode };
        }

        // Script has AI calls — execute line by line, handling AI calls inline
        let ai_call_re = regex::Regex::new(r#"^\s*ai\s+["']([^"']+)["']\s*$"#).unwrap();
        let mut returncode = 0;

        // Build runtime env for variable substitution
        let mut script_env: std::collections::HashMap<String, String> = std::env::vars().collect();
        script_env.insert("AISH_SCRIPT_DIR".to_string(), script.base_dir.clone());
        script_env.insert("AISH_SCRIPT_NAME".to_string(), script.metadata.name.clone());
        for (i, arg) in args.iter().enumerate() {
            script_env.insert(format!("AISH_ARG_{}", i), arg.clone());
        }

        // Accumulate non-AI lines into segments and execute as bash
        let mut bash_segment = String::new();

        for line in script.content.lines() {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Check for AI call
            if let Some(caps) = ai_call_re.captures(trimmed) {
                // Flush any accumulated bash commands first
                if !bash_segment.is_empty() {
                    returncode = self.flush_bash_segment(&bash_segment, returncode);
                    bash_segment.clear();
                }

                // Execute AI call via the AI handler
                if let Some(prompt) = caps.get(1) {
                    let prompt_str = prompt.as_str();
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    match rt.block_on(self.ai_handler.handle_question(prompt_str)) {
                        Ok(response) => {
                            print_md(&response);
                            script_env.insert("AISH_LAST_OUTPUT".to_string(), response);
                        }
                        Err(e) => {
                            eprintln!("\x1b[31mAI error: {}\x1b[0m", e);
                            returncode = 1;
                        }
                    }
                }
                continue;
            }

            // Accumulate into bash segment
            bash_segment.push_str(line);
            bash_segment.push('\n');
        }

        // Flush remaining bash commands
        if !bash_segment.is_empty() {
            returncode = self.flush_bash_segment(&bash_segment, returncode);
        }

        returncode
    }

    /// Execute accumulated bash commands from a script segment.
    fn flush_bash_segment(&mut self, segment: &str, base_rc: i32) -> i32 {
        let (exit_code, cwd, output) = self.pty.send_command_interactive(segment).unwrap_or((
            -1,
            self.state.cwd.clone(),
            String::new(),
        ));

        if !output.is_empty() {
            // Basic ANSI stripping: remove escape sequences for display
            let re = regex::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
            let clean = re.replace_all(&output, "").trim_end().to_string();
            if !clean.is_empty() {
                println!("{}", clean);
            }
        }

        if !cwd.is_empty() && cwd != self.state.cwd {
            self.state.prev_cwd = Some(self.state.cwd.clone());
            self.state.cwd = cwd;
        }
        self.state.last_output = output;
        self.state.last_exit_code = exit_code;

        if exit_code != 0 && base_rc == 0 {
            exit_code
        } else {
            base_rc
        }
    }

    /// Apply state changes from a ScriptExecutionResult.
    fn apply_script_result(&mut self, result: &aish_scripts::executor::ScriptExecutionResult) {
        if let Some(ref new_cwd) = result.new_cwd {
            let path = std::path::Path::new(new_cwd);
            if path.is_dir() {
                let _ = std::env::set_current_dir(path);
                self.state.prev_cwd = Some(self.state.cwd.clone());
                self.state.cwd = new_cwd.clone();
            }
        }
        for (key, value) in &result.env_changes {
            std::env::set_var(key, value);
            self.state.env_vars.insert(key.clone(), value.clone());
        }
    }
}

/// Collapse long output for display, showing first/last N lines with a truncation notice.
pub fn collapse_output(
    output: &str,
    offload_path: Option<&str>,
    threshold_lines: usize,
    context_lines: usize,
) -> String {
    let all_lines: Vec<&str> = output.lines().collect();
    if all_lines.len() <= threshold_lines {
        return output.to_string();
    }

    let first: Vec<&str> = all_lines.iter().take(context_lines).copied().collect();
    let last: Vec<&str> = all_lines
        .iter()
        .rev()
        .take(context_lines)
        .rev()
        .copied()
        .collect();
    let omitted = all_lines.len() - first.len() - last.len();

    let mut result = first.join("\n");
    result.push_str(&format!(
        "\n\x1b[2m... ({} lines truncated{})\x1b[0m",
        omitted,
        offload_path
            .map(|p| format!(", see {}", p))
            .unwrap_or_default(),
    ));
    result.push('\n');
    result.push_str(&last.join("\n"));

    result
}

#[cfg(test)]
mod collapsing_tests {
    use super::*;

    #[test]
    fn test_collapse_output_short() {
        let output = "line1\nline2\nline3";
        let result = collapse_output(output, None, 20, 5);
        assert_eq!(result, output);
    }

    #[test]
    fn test_collapse_output_long() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {}", i)).collect();
        let output = lines.join("\n");
        let result = collapse_output(&output, None, 20, 5);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 29"));
        assert!(result.contains("truncated"));
        assert!(!result.contains("line 10"));
    }

    #[test]
    fn test_collapse_output_with_offload() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {}", i)).collect();
        let output = lines.join("\n");
        let result = collapse_output(&output, Some("/tmp/offload.raw"), 20, 5);
        assert!(result.contains("/tmp/offload.raw"));
    }
}

/// Format tool arguments for display in the streaming output.
/// Skips large fields like content, shows single values for single-key dicts,
/// and truncates long strings.
fn format_tool_args_for_display(tool_name: &str, args: &serde_json::Value) -> String {
    // For write_file, skip the content field
    if tool_name == "write_file" {
        if let Some(obj) = args.as_object() {
            let display: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .filter(|(k, _)| *k != "content")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            return truncate_str(&serde_json::Value::Object(display).to_string(), 120);
        }
    }

    // For single-key dicts, show just the value
    if let Some(obj) = args.as_object() {
        if obj.len() == 1 {
            if let Some(v) = obj.values().next() {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                return truncate_str(&s, 120);
            }
        }
    }

    truncate_str(&args.to_string(), 120)
}

/// Truncate a string to max_len *display columns*, accounting for CJK double-width chars.
fn truncate_display_width(s: &str, max_cols: usize) -> String {
    let mut cols = 0usize;
    let mut end = 0usize;
    for (i, ch) in s.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + w > max_cols {
            break;
        }
        cols += w;
        end = i + ch.len_utf8();
    }
    let truncated = &s[..end];
    if truncated.len() < s.len() && max_cols > 3 {
        // Re-truncate to leave room for "..."
        let mut cols2 = 0usize;
        let mut end2 = 0usize;
        for (i, ch) in s.char_indices() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cols2 + w > max_cols - 3 {
                break;
            }
            cols2 += w;
            end2 = i + ch.len_utf8();
        }
        format!("{}...", &s[..end2])
    } else {
        truncated.to_string()
    }
}

/// Truncate a string to max_len *characters* (UTF-8 safe), appending "..." if truncated.
/// Uses char count instead of byte count to avoid panicking on multi-byte characters.
fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s.to_string();
    }
    if max_len <= 3 {
        return s.chars().take(max_len).collect();
    }
    let truncated: String = s.chars().take(max_len - 3).collect();
    format!("{}...", truncated)
}

/// Pad a string with trailing spaces to fill the given width (for box borders).
fn pad_to_width(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{}{}\x1b[33m│\x1b[0m", s, " ".repeat(width - s.len()))
    }
}

/// Wrap text to the given width, preserving word boundaries.
fn wrap_text(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    let mut line_len = 0;
    for word in text.split_whitespace() {
        if line_len == 0 {
            result.push_str(word);
            line_len = word.len();
        } else if line_len + 1 + word.len() <= max_width {
            result.push(' ');
            result.push_str(word);
            line_len += 1 + word.len();
        } else {
            result.push('\n');
            result.push_str(word);
            line_len = word.len();
        }
    }
    result
}

/// Parse a category string (from the LLM tool call) into a MemoryCategory.
/// Falls back to `Other` for unrecognized values.
fn parse_category_str(s: &str) -> MemoryCategory {
    match s.to_lowercase().as_str() {
        "preference" => MemoryCategory::Preference,
        "environment" => MemoryCategory::Environment,
        "solution" => MemoryCategory::Solution,
        "pattern" => MemoryCategory::Pattern,
        _ => MemoryCategory::Other,
    }
}

/// Format a number with thousand separators.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Render markdown-formatted text to the terminal using richrs.
fn print_md(text: &str) {
    use crate::renderer::ShellRenderer;
    let mut renderer = ShellRenderer::new();
    renderer.render_markdown(text);
}

#[cfg(test)]
mod phase_tests {
    use super::*;

    #[test]
    fn test_shell_phase_display() {
        assert_eq!(ShellPhase::Booting.to_string(), "booting");
        assert_eq!(ShellPhase::Editing.to_string(), "editing");
        assert_eq!(ShellPhase::Running.to_string(), "running");
        assert_eq!(ShellPhase::Exiting.to_string(), "exiting");
    }

    #[test]
    fn test_interruption_default() {
        assert_eq!(InterruptionState::default(), InterruptionState::Normal);
    }

    #[test]
    fn test_phase_equality() {
        assert_eq!(ShellPhase::Booting, ShellPhase::Booting);
        assert_ne!(ShellPhase::Booting, ShellPhase::Editing);
    }
}
