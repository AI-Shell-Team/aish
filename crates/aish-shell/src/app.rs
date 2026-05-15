use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aish_config::ConfigModel;
use aish_core::{LlmEvent, LlmEventType, MemoryCategory};
use aish_i18n::{t, t_with_args};
use aish_llm::{
    langfuse::{LangfuseClient, LangfuseConfig},
    CancellationToken, ChatMessage, LlmCallbackResult, LlmSession,
};
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

// ---------------------------------------------------------------------------
// SIGINT handler for AI operation cancellation
// ---------------------------------------------------------------------------

/// Raw pointer to the current CancellationToken, set before an AI call and
/// cleared afterwards. Only accessed from `ai_sigint_handler`.
static CANCEL_TOKEN_PTR: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// POSIX signal handler for SIGINT during AI operations.
/// Sets the CancellationToken's atomic flag (async-signal-safe).
extern "C" fn ai_sigint_handler(_: std::ffi::c_int) {
    let ptr = CANCEL_TOKEN_PTR.load(Ordering::SeqCst) as *const CancellationToken;
    if !ptr.is_null() {
        unsafe { &*ptr }.cancel_atomic();
    }
}

/// Poll a CancellationToken until it is cancelled. Used inside `tokio::select!`
/// to race against the AI operation — when the token fires the AI future is
/// dropped, which aborts the in-flight HTTP stream.
///
/// # Safety
///
/// The caller must guarantee that `token` points to a live `CancellationToken`
/// that outlives this async task. This holds because the token lives inside
/// `AiHandler` which is owned by `AishShell`, and `poll_cancelled` is only
/// spawned as part of a `tokio::select!` block within `AishShell::run()`.
async fn poll_cancelled(token: *const CancellationToken) {
    while !unsafe { &*token }.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

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
    /// Wrapped in `Arc<Mutex<>>` so the readline completion handler can
    /// query the PTY bash for tab-completions.
    pty: Arc<Mutex<aish_pty::PersistentPty>>,
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
    /// Shared animation spinner, stored so it can be stopped on cancellation.
    animation: Arc<SharedAnimation>,
}

impl AishShell {
    /// Lock the PTY mutex, recovering from poison if a previous holder
    /// panicked. A poisoned PTY is still usable — the lock just means
    /// a prior operation failed, not that the PTY state is corrupt.
    fn lock_pty(&self) -> std::sync::MutexGuard<'_, aish_pty::PersistentPty> {
        match self.pty.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

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

        // Initialize Langfuse observability if configured
        if config.enable_langfuse {
            if let Some(lf_config) = LangfuseConfig::from_parts(
                config.langfuse_public_key.as_deref(),
                config.langfuse_secret_key.as_deref(),
                config.langfuse_host.as_deref(),
            ) {
                llm_session.set_langfuse(LangfuseClient::new(lf_config));
                tracing::info!("Langfuse observability enabled");
            }
        }

        // Initialize security manager (before tool registration)
        let security_manager = SecurityManager::new(SecurityPolicy::default());

        // Register tools
        let mut tool_registry = ToolRegistry::new();
        // Shared PTY slot — will be populated after PersistentPty starts.
        let pty_slot: aish_tools::bash::PtySlot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut bash_tool = aish_tools::bash::BashTool::new();
        bash_tool.set_cancellation_token(llm_session.cancellation_token_arc());
        bash_tool.set_pty_slot(pty_slot.clone());
        tool_registry.register(Box::new(bash_tool));
        tool_registry.register(Box::new(aish_tools::fs::ReadFileTool::new()));
        tool_registry.register(Box::new(aish_tools::fs::WriteFileTool::new()));
        tool_registry.register(Box::new(aish_tools::fs::EditFileTool::new()));
        tool_registry.register(Box::new(aish_tools::AskUserTool::new()));
        tool_registry.register(Box::new(aish_tools::PythonTool::new()));
        tool_registry.register(Box::new(aish_tools::GlobTool::new()));
        tool_registry.register(Box::new(aish_tools::GrepTool::new()));
        tool_registry.register(Box::new(aish_tools::EnterPlanModeTool::new()));
        tool_registry.register(Box::new(aish_tools::ExitPlanModeTool::new()));

        // System diagnose tool — needs session credentials to spawn sub-sessions.
        // The shared event callback holder allows setting the callback after
        // tool registration (the event callback is created later).
        let diagnose_event_callback: aish_tools::SharedEventCallback =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        // SystemDiagnoseTool registration is deferred until after skill loading
        // so we can wire skill callbacks. Store the construction parameters.
        let diagnose_tool_params = (
            config.api_base.clone(),
            config.api_key.clone(),
            config.model.clone(),
            config.temperature,
            config.max_tokens,
            diagnose_event_callback.clone(),
        );

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
                        Err(e) => {
                            let mut args = std::collections::HashMap::new();
                            args.insert("error".to_string(), e.to_string());
                            t_with_args("shell.general_error", &args)
                        }
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

        // Create SystemDiagnoseTool with skill callbacks wired from the loaded skills
        {
            let (api_base, api_key, model, temp, max_tok, ev_cb) = diagnose_tool_params;
            let diag_tool = aish_tools::SystemDiagnoseTool::new(
                &api_base,
                &api_key,
                &model,
                Some(temp),
                max_tok,
                ev_cb,
            );
            // Build skill callbacks for the diagnose agent (separate snapshot from main SkillTool)
            let diag_skills: std::collections::HashMap<String, aish_tools::SkillInfo> =
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
            let diag_skill_names: Vec<String> = diag_skills.keys().cloned().collect();
            let diag_lookup = std::sync::Arc::new(move |name: &str| diag_skills.get(name).cloned())
                as std::sync::Arc<dyn Fn(&str) -> Option<aish_tools::SkillInfo> + Send + Sync>;
            let diag_list = std::sync::Arc::new(move || diag_skill_names.clone())
                as std::sync::Arc<dyn Fn() -> Vec<String> + Send + Sync>;
            let callbacks: aish_tools::system_diagnose::SkillCallbacks =
                Some((diag_lookup, diag_list));
            tool_registry.register(Box::new(diag_tool.with_skill_callbacks(callbacks)));
        }

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
                            let mut ttft_args = std::collections::HashMap::new();
                            ttft_args.insert("time".to_string(), format!("{:.1}", ttft));
                            println!(
                                "\x1b[2m{}\x1b[0m",
                                aish_i18n::t_with_args("shell.thinking_time", &ttft_args)
                            );
                        }
                        *thinking_start_ref.lock().unwrap() = None;
                    }
                    LlmEventType::GenerationStart => {
                        animation_ref.stop();
                        clear_reasoning();
                        // Reset streamed flag so it only reflects the CURRENT
                        // generation, not a previous iteration that included
                        // tool calls with interleaved content.  Without this
                        // reset, tool-call preview text sets the flag to true,
                        // and the final text-only response is never printed.
                        streamed_flag.store(false, Ordering::SeqCst);
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
                                            let mut args = std::collections::HashMap::new();
                                            args.insert("elapsed".to_string(), format!("{:.1}", e));
                                            format!(
                                                " {}",
                                                aish_i18n::t_with_args(
                                                    "shell.session.thinking_elapsed",
                                                    &args
                                                )
                                            )
                                        } else {
                                            format!(" {}", aish_i18n::t("shell.session.thinking"))
                                        }
                                    })
                                    .unwrap_or_else(|| {
                                        format!(" {}", aish_i18n::t("shell.session.thinking"))
                                    });

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
                        if let Some(preview) =
                            event.data.get("output_preview").and_then(|p| p.as_str())
                        {
                            // Display collapsed output (first 2 lines) for bash tool,
                            // matching Python's _collapse_output_lines behavior.
                            let tool_name = event
                                .data
                                .get("tool_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            let interactive_bash = tool_name == "bash"
                                && event
                                    .data
                                    .get("tool_args")
                                    .and_then(|args| args.get("command"))
                                    .and_then(|command| command.as_str())
                                    .is_some_and(aish_tools::bash::command_needs_interactive);
                            if tool_name == "bash" && !preview.is_empty() && !interactive_bash {
                                let content = strip_tool_output_xml(preview);
                                if !content.is_empty() {
                                    let collapsed = collapse_display_lines(&content, 2);
                                    println!("\x1b[2m{}\x1b[0m", collapsed);
                                    let _ = io::stdout().flush();
                                }
                            }
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

        llm_session.set_event_callback(event_callback.clone());

        // Share the event callback with the diagnose tool so it can forward
        // sub-session events (bash_exec, read_file, etc.) to the UI.
        *diagnose_event_callback.lock().unwrap() = Some(event_callback);

        // Set up confirmation callback for tool approval flow
        let confirmation_callback: Arc<
            dyn Fn(&aish_llm::PreflightSecurityContext) -> bool + Send + Sync,
        > = Arc::new(|ctx: &aish_llm::PreflightSecurityContext| {
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
            println!(
                "\x1b[33m│\x1b[0m  \x1b[1;36m{}\x1b[0m   {}",
                t("shell.confirm_dialog_tool"),
                ctx.tool_name
            );
            let reason_lines = wrap_text(&ctx.message, width.saturating_sub(14));
            println!(
                "\x1b[33m│\x1b[0m  \x1b[1;36mReason:\x1b[0m {}",
                reason_lines.lines().next().unwrap_or("")
            );
            for line in reason_lines.lines().skip(1) {
                println!("\x1b[33m│\x1b[0m         {}", line);
            }
            println!("\x1b[33m│\x1b[0m");
            println!(
                "\x1b[33m│\x1b[0m  \x1b[36m{}\x1b[0m",
                t("shell.confirm_dialog_question")
            );
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

        // Set iteration limit callback: ask user whether to continue after 20 tool-call rounds
        let iteration_limit_callback: Arc<dyn Fn(u32) -> bool + Send + Sync> =
            Arc::new(|iterations: u32| {
                let width = std::env::var("COLUMNS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(80);
                let border = "─".repeat(width.saturating_sub(4));
                println!();
                println!("\x1b[33m╭{}╮\x1b[0m", border);
                println!(
                    "\x1b[33m│\x1b[1;33m {}\x1b[0m{}",
                    aish_i18n::t("shell.session.iteration_limit_title"),
                    pad_to_width("", width.saturating_sub(38))
                );
                println!("\x1b[33m│\x1b[0m");
                println!(
                    "\x1b[33m│\x1b[0m  {} {}",
                    aish_i18n::t_with_args("shell.session.iteration_limit_reached", &{
                        let mut m = std::collections::HashMap::new();
                        m.insert("count".to_string(), iterations.to_string());
                        m
                    },),
                    pad_to_width("", 0)
                );
                println!("\x1b[33m│\x1b[0m");
                println!(
                    "\x1b[33m│\x1b[0m  \x1b[36m{}\x1b[0m",
                    aish_i18n::t("shell.session.iteration_continue_prompt")
                );
                println!("\x1b[33m╰{}╯\x1b[0m", border);
                print!("  ");
                let _ = std::io::stdout().flush();

                let mut answer = String::new();
                if std::io::stdin().read_line(&mut answer).is_err() {
                    return false;
                }
                let answer = answer.trim().to_lowercase();
                answer == "y" || answer == "yes" || answer.is_empty()
            });

        llm_session.set_iteration_limit_callback(iteration_limit_callback);

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
            let mut args = std::collections::HashMap::new();
            args.insert(
                "error".to_string(),
                format!("failed to start persistent PTY: {e}"),
            );
            aish_core::AishError::Pty(t_with_args("shell.general_error", &args))
        })?;
        let pty = Arc::new(Mutex::new(pty));

        // Inject PersistentPty into the bash tool slot.
        {
            let mut slot = pty_slot.lock().unwrap();
            *slot = Some(pty.clone());
        }

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
            animation,
        })
    }

    /// Install a POSIX SIGINT handler that atomically sets the LLM
    /// session's cancellation flag. Returns the previous `SigAction`
    /// so it can be restored via `restore_ai_sigint_handler`.
    fn install_ai_sigint_handler(&self) -> Option<nix::sys::signal::SigAction> {
        use nix::sys::signal::{self, SigAction, SigHandler, SigSet, Signal};

        // Clear any leftover cancellation state from a previous operation.
        self.ai_handler.cancellation_token().reset();

        let token_ptr = self.ai_handler.cancellation_token() as *const CancellationToken;
        CANCEL_TOKEN_PTR.store(token_ptr as *mut (), Ordering::SeqCst);

        let action = SigAction::new(
            SigHandler::Handler(ai_sigint_handler),
            signal::SaFlags::empty(),
            SigSet::empty(),
        );
        unsafe { signal::sigaction(Signal::SIGINT, &action) }.ok()
    }

    /// Restore the SIGINT handler saved by `install_ai_sigint_handler`.
    fn restore_ai_sigint_handler(old: Option<nix::sys::signal::SigAction>) {
        CANCEL_TOKEN_PTR.store(std::ptr::null_mut(), Ordering::SeqCst);
        if let Some(old) = old {
            use nix::sys::signal::{self, Signal};
            let _ = unsafe { signal::sigaction(Signal::SIGINT, &old) };
        }
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
        let mut rl = ShellReadline::new(self.pty.clone()).map_err(|e| {
            let mut args = std::collections::HashMap::new();
            args.insert("error".to_string(), e.to_string());
            aish_core::AishError::Config(t_with_args("shell.readline_init_failed", &args))
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
                                println!("\x1b[1;33m{}\x1b[0m", t("shell.plan_mode_enabled"));
                                println!("\x1b[2m{}\x1b[0m", t("shell.plan_mode_hint"));
                            }
                            aish_core::PlanPhase::Normal => {
                                println!("\x1b[33m{}\x1b[0m", t("shell.plan_mode_disabled"));
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
                    eprintln!("{}", {
                        let mut args = std::collections::HashMap::new();
                        args.insert("error".to_string(), e.to_string());
                        t_with_args("shell.readline_error", &args)
                    });
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
                            let old_sigint = self.install_ai_sigint_handler();
                            let token_ptr =
                                self.ai_handler.cancellation_token() as *const CancellationToken;
                            let result = runtime.block_on(async {
                                tokio::select! {
                                    r = self.ai_handler.handle_error_correction(
                                        cmd,
                                        self.state.last_exit_code,
                                        &self.state.last_output,
                                    ) => r,
                                    _ = poll_cancelled(token_ptr) => {
                                        Err(aish_core::AishError::Cancelled)
                                    }
                                }
                            });
                            Self::restore_ai_sigint_handler(old_sigint);

                            match result {
                                Ok(correction) => {
                                    match &correction.command {
                                        Some(corrected) => {
                                            // Display corrected command and description
                                            println!(
                                                "{} \x1b[1;36m{}\x1b[0m",
                                                t("shell.error_correction.corrected_command_title"),
                                                corrected
                                            );
                                            if let Some(ref desc) = correction.description {
                                                if !desc.is_empty() {
                                                    println!("   {}", desc);
                                                }
                                            }
                                            // Ask user confirmation: Y/n
                                            let prompt = format!(
                                                "{}\x1b[1;36m{}\x1b[0m{}",
                                                t("shell.error_correction.confirm_execute_prefix"),
                                                corrected,
                                                t("shell.error_correction.confirm_execute_suffix")
                                            );
                                            print!("{}", prompt);
                                            let _ = std::io::stdout().flush();
                                            let mut answer = String::new();
                                            if std::io::stdin().read_line(&mut answer).is_err() {
                                                continue;
                                            }
                                            let answer = answer.trim().to_lowercase();
                                            if answer == "y" || answer == "yes" || answer.is_empty()
                                            {
                                                let exit_code =
                                                    self.execute_external_command(corrected);
                                                self.record_history(corrected, exit_code);
                                            }
                                            self.state.can_correct_error = false;
                                        }
                                        None => {
                                            // No valid command, show description if available
                                            println!(
                                                "\x1b[33m\u{26a0} {}\x1b[0m",
                                                t("shell.error_correction.no_valid_command")
                                            );
                                            if let Some(ref desc) = correction.description {
                                                let clean = desc
                                                    .split("Insufficient context")
                                                    .next()
                                                    .unwrap_or(desc)
                                                    .trim();
                                                if !clean.is_empty() {
                                                    println!("   {}", clean);
                                                }
                                            }
                                            println!(
                                                "   \x1b[36m{}\x1b[0m",
                                                t("shell.error_correction.retry_hint")
                                            );
                                        }
                                    }
                                }
                                Err(aish_core::AishError::Cancelled) => {
                                    self.animation.stop();
                                    println!("\x1b[33mInterrupted\x1b[0m");
                                }
                                Err(e) => {
                                    self.animation.stop();
                                    // Errors are already displayed via the LlmEventType::Error
                                    // event callback — avoid printing twice. Only handle
                                    // non-LLM errors that bypass the event system.
                                    if !matches!(e, aish_core::AishError::Llm(_)) {
                                        let msg = t("shell.error.llm_error_message")
                                            .replace("{error}", &e.to_string());
                                        eprintln!("\x1b[31m{}\x1b[0m", msg);
                                    }
                                }
                            }
                            continue;
                        }
                    }

                    let old_sigint = self.install_ai_sigint_handler();
                    let token_ptr =
                        self.ai_handler.cancellation_token() as *const CancellationToken;
                    let result = runtime.block_on(async {
                        tokio::select! {
                            r = self.ai_handler.handle_question(&question) => r,
                            _ = poll_cancelled(token_ptr) => {
                                Err(aish_core::AishError::Cancelled)
                            }
                        }
                    });
                    Self::restore_ai_sigint_handler(old_sigint);

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
                                                println!(
                                                    "\x1b[32m{}\x1b[0m",
                                                    t("shell.plan_approved")
                                                );
                                                println!(
                                                    "\x1b[2m  {}\x1b[0m",
                                                    t_with_args(
                                                        "shell.plan_approved_hint",
                                                        &std::collections::HashMap::new()
                                                    )
                                                );
                                            }
                                        }
                                        PlanApprovalDecision::ChangesRequested { feedback } => {
                                            // Keep in planning phase — re-enter plan mode with feedback
                                            println!(
                                                "\x1b[33m{}\x1b[0m",
                                                t("shell.plan_changes_requested")
                                            );

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
                                                println!(
                                                    "\x1b[2m  {}\x1b[0m",
                                                    t("shell.plan_feedback_sent")
                                                );
                                            }
                                        }
                                        PlanApprovalDecision::Cancelled => {
                                            println!(
                                                "\x1b[33m{}\x1b[0m",
                                                t("shell.plan_review_cancelled")
                                            );
                                            println!(
                                                "\x1b[2m{}\x1b[0m",
                                                t("shell.plan_review_hint")
                                            );
                                        }
                                    }
                                }
                            }

                            self.record_history(input, 0);
                        }
                        Err(aish_core::AishError::Cancelled) => {
                            self.animation.stop();
                            println!("\x1b[33m{}\x1b[0m", t("shell.interrupted"));
                        }
                        Err(e) => {
                            // Errors are already displayed via the LlmEventType::Error
                            // event callback — avoid printing twice.
                            if !matches!(e, aish_core::AishError::Llm(_)) {
                                let msg = t("shell.error.llm_error_message")
                                    .replace("{error}", &e.to_string());
                                eprintln!("\x1b[31m{}\x1b[0m", msg);
                            }
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
                    // NL detection: check if input looks like natural language
                    // and offer to route to AI instead of executing as a command.
                    let nl_verdict = crate::nl_detect::detect(input);
                    if nl_verdict.is_natural_language {
                        let prompt_msg = t("shell.nl_detection.confirm_ask_ai");
                        print!("{} ", prompt_msg);
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        if std::io::stdin().read_line(&mut answer).is_err() {
                            // Fall through to command execution on read error
                        } else {
                            let ans = answer.trim().to_lowercase();
                            if ans != "n" && ans != "no" {
                                // Route to AI
                                let question = input.trim().to_string();
                                let old_sigint = self.install_ai_sigint_handler();
                                let token_ptr = self.ai_handler.cancellation_token()
                                    as *const CancellationToken;
                                let result = runtime.block_on(async {
                                    tokio::select! {
                                        r = self.ai_handler.handle_question(&question) => r,
                                        _ = poll_cancelled(token_ptr) => {
                                            Err(aish_core::AishError::Cancelled)
                                        }
                                    }
                                });
                                Self::restore_ai_sigint_handler(old_sigint);

                                let did_stream = self.streamed_content.load(Ordering::SeqCst);
                                match result {
                                    Ok(response) => {
                                        if !did_stream && !response.is_empty() {
                                            let mut sep_renderer = ShellRenderer::new();
                                            sep_renderer.render_separator();
                                            print_md(&response);
                                            sep_renderer.render_separator();
                                        }
                                        self.record_history(input, 0);
                                    }
                                    Err(aish_core::AishError::Cancelled) => {
                                        self.animation.stop();
                                        println!("\x1b[33m{}\x1b[0m", t("shell.interrupted"));
                                    }
                                    Err(e) => {
                                        if !matches!(e, aish_core::AishError::Llm(_)) {
                                            let msg = t("shell.error.llm_error_message")
                                                .replace("{error}", &e.to_string());
                                            eprintln!("\x1b[31m{}\x1b[0m", msg);
                                        }
                                    }
                                }
                                continue;
                            }
                        }
                    }

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
                    // Always add, matching main branch's unconditional add_memory.
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
                    let entry = format!(
                        "[Shell] {}\n<returncode>{}</returncode>\n<output>{}</output>",
                        input, exit_code, output_preview
                    );
                    self.ai_handler.add_shell_context(&entry);

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
                eprintln!("{}", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("command".to_string(), input.to_string());
                    t_with_args("shell.unknown_command", &args)
                });
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
            println!("\x1b[36m{}\x1b[0m", t("shell.model_usage"));
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
            eprintln!("\x1b[33m{}\x1b[0m", {
                let mut args = std::collections::HashMap::new();
                args.insert("error".to_string(), e.to_string());
                t_with_args("shell.config_save_warning", &args)
            });
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
        println!("{}", aish_i18n::t("shell.token.title"));
        println!(
            "  {}  {}",
            aish_i18n::t("shell.token.input_tokens"),
            format_number(stats.total_input)
        );
        println!(
            "  {} {}",
            aish_i18n::t("shell.token.output_tokens"),
            format_number(stats.total_output)
        );
        println!(
            "  {}     {}",
            aish_i18n::t("shell.token.total"),
            format_number(total)
        );
        println!(
            "  {}  {}",
            aish_i18n::t("shell.token.api_calls"),
            format_number(stats.request_count)
        );
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
            self.lock_pty().resize(rows, cols);
        }

        // Ensure the PTY is alive before sending a command.
        if !self.lock_pty().is_running() {
            self.restart_pty();
        }

        // Send command via PTY (release the lock inside the block so the
        // MutexGuard is dropped before any potential restart_pty() call).
        let result = {
            let mut pty = self.lock_pty();
            let remote_host = extract_remote_host(command);
            let shared_host = Arc::new(Mutex::new(remote_host.clone()));
            let ai_cb =
                Self::build_session_ai_callback(&self.config, &self.animation, shared_host.clone());
            pty.send_command_interactive(command, ai_cb, Some(shared_host))
        };
        let (exit_code, cwd, output) = match result {
            Ok(result) => result,
            Err(e) => {
                eprintln!("{}", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), e.to_string());
                    aish_i18n::t_with_args("shell.error.pty_error", &args)
                });
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
        if !self.lock_pty().is_running() {
            self.restart_pty();
        }

        exit_code
    }

    /// Silently sync a state-modifying command (cd, export, etc.) to the
    /// persistent PTY bash process so that bash's CWD and env stay in sync
    /// with the Rust shell's tracking. Output is discarded.
    fn sync_command_to_pty(&mut self, command: &str) {
        if !self.lock_pty().is_running() {
            return;
        }
        let _ = self.pty.lock().unwrap().execute_command(
            command,
            std::time::Duration::from_secs(5),
            None,
            false,
        );
    }

    /// Restart the PTY session (e.g., after bash exits or crashes).
    fn restart_pty(&mut self) {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        match aish_pty::PersistentPty::start(&self.state.cwd, rows, cols) {
            Ok(new_pty) => {
                *self.lock_pty() = new_pty;
                println!("\x1b[33mbash session restarted\x1b[0m");
            }
            Err(e) => {
                eprintln!("{}", {
                    let mut args = std::collections::HashMap::new();
                    args.insert("error".to_string(), e.to_string());
                    aish_i18n::t_with_args("shell.error.restart_bash_failed", &args)
                });
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
                println!("\x1b[33m({})\x1b[0m", aish_i18n::t("shell.ctrl_c_again"));
                false
            }
            InterruptionState::ClearPending => {
                if let Some(last) = self.last_ctrl_c {
                    if now.duration_since(last).as_secs() < 1 {
                        self.interruption = InterruptionState::ExitPending;
                        println!("\x1b[33m{}\x1b[0m", aish_i18n::t("shell.exiting"));
                        return true;
                    }
                }
                self.interruption = InterruptionState::ClearPending;
                self.last_ctrl_c = Some(now);
                println!("\x1b[33m({})\x1b[0m", aish_i18n::t("shell.ctrl_c_again"));
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
                    eprintln!("{}", {
                        let mut args = std::collections::HashMap::new();
                        args.insert("script".to_string(), script_path.to_string());
                        args.insert("error".to_string(), e.to_string());
                        aish_i18n::t_with_args("shell.error.load_script_failed", &args)
                    });
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
        let (exit_code, cwd, output) = self
            .pty
            .lock()
            .unwrap()
            .send_command_interactive(segment, None, None)
            .unwrap_or((-1, self.state.cwd.clone(), String::new()));

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

    /// Create a HostNoteTool with shared host state for SSH sessions.
    fn make_host_note_tool(current_host: Arc<Mutex<Option<String>>>) -> Box<dyn aish_llm::Tool> {
        Box::new(aish_tools::HostNoteTool::new(
            {
                let h = current_host.clone();
                Box::new(move |content: &str| {
                    let host = match h.lock().unwrap().clone() {
                        Some(h) if !h.is_empty() => h,
                        _ => return "No active remote host.".to_string(),
                    };
                    let mut profile = aish_hosts::get_or_create_profile(&host);
                    let id = profile.add_note(content.to_string());
                    match aish_hosts::save_profile(&profile) {
                        Ok(()) => {
                            let mut args = std::collections::HashMap::new();
                            args.insert("id".to_string(), id.to_string());
                            t_with_args("tools.host_note.stored", &args)
                        }
                        Err(e) => format!("Failed to save note: {}", e),
                    }
                })
            },
            {
                let h = current_host.clone();
                Box::new(move || {
                    let host = match h.lock().unwrap().clone() {
                        Some(h) if !h.is_empty() => h,
                        _ => return Vec::new(),
                    };
                    let profile = aish_hosts::get_or_create_profile(&host);
                    profile
                        .notes
                        .iter()
                        .map(|n| aish_tools::HostNoteEntry {
                            id: n.id,
                            content: n.content.clone(),
                        })
                        .collect()
                })
            },
            {
                let h = current_host.clone();
                Box::new(move |keyword: &str| {
                    let host = match h.lock().unwrap().clone() {
                        Some(h) if !h.is_empty() => h,
                        _ => return "No active remote host.".to_string(),
                    };
                    let mut profile = aish_hosts::get_or_create_profile(&host);
                    let removed = profile.remove_notes(keyword);
                    match aish_hosts::save_profile(&profile) {
                        Ok(()) if removed > 0 => {
                            let mut args = std::collections::HashMap::new();
                            args.insert("count".to_string(), removed.to_string());
                            t_with_args("tools.host_note.forgot", &args)
                        }
                        Ok(()) => t("tools.host_note.forgot_none"),
                        Err(e) => format!("Failed to save changes: {}", e),
                    }
                })
            },
        ))
    }

    /// Build an AI callback for session commands (SSH, telnet).
    /// Uses the same oracle prompt as local aish (with remote host context),
    /// streaming output, thinking animation, and ShellRenderer for display.
    /// Build a followup closure that can chain itself for multi-round tool use.
    /// Each invocation: call LLM with command output → render analysis → if
    /// the LLM suggests another command, return Some(AiResponse) with another
    /// followup closure (same builder, shared history).
    fn build_followup_closure(
        api_base: &str,
        api_key: &str,
        model: &str,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        system_msg: &str,
        original_question: &str,
        animation: &Arc<crate::animation::SharedAnimation>,
        history: &Arc<Mutex<Vec<ChatMessage>>>,
        shared_host: Arc<Mutex<Option<String>>>,
    ) -> Box<aish_pty::FollowupCallback> {
        let api_base_f = api_base.to_string();
        let api_key_f = api_key.to_string();
        let model_f = model.to_string();
        let system_msg_f = system_msg.to_string();
        let question_f = original_question.to_string();
        let anim_f = animation.clone();
        let history_f = history.clone();
        let shared_host_f = shared_host.clone();

        Box::new(
            move |output: &str, _offload_path: Option<&str>| -> Option<aish_pty::AiResponse> {
                let followup_prompt = format!(
                    "I ran the command on the remote host. Here is the output:\n\
                ```\n{}\n```\n\n\
                Original question: {}\n\
                Please analyze the command output. If further action is needed, \
                suggest the next bash command in a ```bash code block. \
                If no further action is needed, just provide a summary.",
                    output, question_f
                );

                // Channel for ask_user communication
                let (event_tx, event_rx) = std::sync::mpsc::channel::<aish_pty::AiEvent>();
                let (answer_tx, answer_rx) = std::sync::mpsc::channel::<aish_pty::AskUserAnswer>();

                // Done signal: LLM thread sends when all rendering is complete.
                let (llm_done_tx, llm_done_rx) = std::sync::mpsc::channel::<()>();

                // Cancellation support for Ctrl+C
                let (token_tx, token_rx) =
                    std::sync::mpsc::channel::<std::sync::Arc<CancellationToken>>();
                let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let cancelled_cb = cancelled.clone();
                let cancelled_fu = cancelled.clone();

                // Shared reasoning state for cleanup after cancellation
                let reasoning_active_main =
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let reasoning_lines_main =
                    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let reasoning_active_cb = reasoning_active_main.clone();
                let reasoning_lines_cb = reasoning_lines_main.clone();

                let followup_start = std::time::Instant::now();

                anim_f.start(&t("shell.status.thinking"));
                let history_snapshot = history_f.lock().unwrap().clone();

                // Spawn LLM thread with ChannelAskUserTool
                let api_base_th = api_base_f.clone();
                let api_key_th = api_key_f.clone();
                let model_th = model_f.clone();
                let system_msg_th = system_msg_f.clone();
                let anim_th = anim_f.clone();
                let followup_prompt_th = followup_prompt.clone();
                let question_th = question_f.clone();
                let conversation_history_th = history_f.clone();
                let shared_host_th_f = shared_host_f.clone();

                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Runtime::new() {
                        Ok(rt) => rt,
                        Err(e) => {
                            let _ = event_tx.send(aish_pty::AiEvent::Done(None));
                            let msg = format!("\r\n\x1b[31mFollowup error: {}\x1b[0m\r\n", e);
                            unsafe {
                                nix::libc::write(
                                    nix::libc::STDOUT_FILENO,
                                    msg.as_ptr() as *const nix::libc::c_void,
                                    msg.len(),
                                );
                            }
                            return;
                        }
                    };
                    let mut session = LlmSession::new(
                        &api_base_th,
                        &api_key_th,
                        &model_th,
                        temperature,
                        max_tokens,
                    );
                    // Send cancellation token to main thread
                    let _ = token_tx.send(session.cancellation_token_arc());
                    let anim = anim_th.clone();
                    let reasoning_active = reasoning_active_cb.clone();
                    let reasoning_frame =
                        std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let reasoning_lines_displayed = reasoning_lines_cb.clone();
                    let reasoning_buf = std::sync::Mutex::new(String::new());
                    let thinking_start_followup =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(std::time::Instant::now())));
                    session.set_event_callback(std::sync::Arc::new(move |event| {
                        // Helper: clear reasoning overlay from terminal
                        let clear_reasoning = || {
                            if reasoning_active.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                let prev = reasoning_lines_displayed
                                    .swap(0, std::sync::atomic::Ordering::SeqCst);
                                if prev > 0 {
                                    use std::io::Write;
                                    print!("\x1b[{}A", prev);
                                    for _ in 0..prev {
                                        print!("\r\x1b[K\n");
                                    }
                                    print!("\x1b[{}A", prev);
                                    let _ = std::io::stdout().flush();
                                }
                                reasoning_buf.lock().unwrap().clear();
                            }
                        };
                        // Bail out if cancelled
                        if cancelled_cb.load(std::sync::atomic::Ordering::SeqCst) {
                            anim.stop();
                            clear_reasoning();
                            return None;
                        }
                        use aish_core::LlmEventType;
                        match event.event_type {
                            LlmEventType::GenerationStart => {
                                anim.stop();
                                reasoning_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                reasoning_frame.store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_lines_displayed
                                    .store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_buf.lock().unwrap().clear();
                                anim.start(&t("shell.status.thinking"));
                            }
                            LlmEventType::GenerationEnd => {
                                anim.stop();
                                clear_reasoning();
                            }
                            LlmEventType::ContentDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        clear_reasoning();
                                    }
                                }
                            }
                            LlmEventType::ReasoningDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        if !reasoning_active
                                            .load(std::sync::atomic::Ordering::SeqCst)
                                        {
                                            reasoning_active
                                                .store(true, std::sync::atomic::Ordering::SeqCst);
                                        }
                                        let mut buf = reasoning_buf.lock().unwrap();
                                        buf.push_str(delta);
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
                                        let max_cols = 76usize;
                                        let frame = reasoning_frame
                                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        let spinner = DOTS_FRAMES[frame % DOTS_FRAMES.len()];
                                        let elapsed_str = thinking_start_followup
                                            .lock()
                                            .unwrap()
                                            .map(|s| {
                                                let e = s.elapsed().as_secs_f64();
                                                if e >= 1.0 {
                                                    let mut args = std::collections::HashMap::new();
                                                    args.insert(
                                                        "elapsed".to_string(),
                                                        format!("{:.1}", e),
                                                    );
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t_with_args(
                                                            "shell.session.thinking_elapsed",
                                                            &args
                                                        )
                                                    )
                                                } else {
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t("shell.session.thinking")
                                                    )
                                                }
                                            })
                                            .unwrap_or_else(|| {
                                                format!(
                                                    " {}",
                                                    aish_i18n::t("shell.session.thinking")
                                                )
                                            });
                                        let prev = reasoning_lines_displayed
                                            .load(std::sync::atomic::Ordering::SeqCst);
                                        let new_count = 1 + display_lines.len();
                                        if prev > 0 {
                                            print!("\x1b[{}A", prev);
                                        }
                                        if display_lines.is_empty() {
                                            print!(
                                                "\r\x1b[K\x1b[90m{}{}...\x1b[0m\n",
                                                spinner, elapsed_str
                                            );
                                        } else {
                                            print!(
                                                "\r\x1b[K\x1b[90m{}{}\x1b[0m\n",
                                                spinner, elapsed_str
                                            );
                                        }
                                        for line in &display_lines {
                                            let truncated =
                                                truncate_display_width(line.trim(), max_cols);
                                            print!("\r\x1b[K\x1b[90m{}\x1b[0m\n", truncated);
                                        }
                                        for _ in new_count..prev {
                                            print!("\r\x1b[K\n");
                                        }
                                        if prev > new_count {
                                            print!("\x1b[{}A", prev - new_count);
                                        }
                                        reasoning_lines_displayed
                                            .store(new_count, std::sync::atomic::Ordering::SeqCst);
                                        reasoning_active
                                            .store(true, std::sync::atomic::Ordering::SeqCst);
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            }
                            LlmEventType::ReasoningEnd => {
                                clear_reasoning();
                            }
                            LlmEventType::Error => {
                                anim.stop();
                                clear_reasoning();
                                let err = event
                                    .data
                                    .get("error")
                                    .or_else(|| event.data.get("error_message"))
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("Unknown error");
                                let msg =
                                    format!("\r\n\x1b[31mFollowup LLM error: {}\x1b[0m\r\n", err);
                                unsafe {
                                    nix::libc::write(
                                        nix::libc::STDOUT_FILENO,
                                        msg.as_ptr() as *const nix::libc::c_void,
                                        msg.len(),
                                    );
                                }
                            }
                            _ => {}
                        }
                        None
                    }));
                    // Register channel-based tools for SSH followup
                    session.register_tool(Box::new(aish_tools::ChannelBashTool::new(
                        event_tx.clone(),
                    )));
                    session.register_tool(Box::new(aish_tools::ChannelAskUserTool::new(
                        event_tx.clone(),
                        answer_rx,
                    )));
                    // Register host_note tool for SSH followup sessions
                    session.register_tool(Self::make_host_note_tool(shared_host_th_f.clone()));
                    let result = rt.block_on(async {
                        session
                            .process_input(
                                &followup_prompt_th,
                                &history_snapshot,
                                Some(&system_msg_th),
                                true,
                            )
                            .await
                    });
                    let process_result = result.ok();
                    let text = process_result.as_ref().map(|r| r.text.clone());

                    // Update conversation history
                    {
                        let mut h = conversation_history_th.lock().unwrap();
                        h.push(ChatMessage::user(&followup_prompt));
                        if let Some(ref pr) = process_result {
                            h.extend(pr.new_messages.clone());
                            if pr.new_messages.is_empty() {
                                h.push(ChatMessage::assistant(&pr.text));
                            }
                        }
                        let excess = h.len().saturating_sub(50);
                        if excess > 0 {
                            h.drain(..excess);
                        }
                    }

                    // Render analysis
                    if let Some(ref t) = text {
                        if !t.trim().is_empty() {
                            let _ = std::io::stdout().flush();
                            let mut renderer = crate::renderer::ShellRenderer::new();
                            renderer.render_separator();
                            renderer.render_markdown(t);
                            let _ = std::io::stdout().flush();
                        }
                    }

                    // Build AiResponse with followup if another command was suggested
                    let next_cmd = text.as_ref().and_then(|t| extract_bash_command(t));
                    let ai_response = if next_cmd.is_some() {
                        let next_followup = Self::build_followup_closure(
                            &api_base_th,
                            &api_key_th,
                            &model_th,
                            temperature,
                            max_tokens,
                            &system_msg_th,
                            &question_th,
                            &anim_th,
                            &conversation_history_th,
                            shared_host_th_f.clone(),
                        );
                        Some(aish_pty::AiResponse {
                            command: next_cmd,
                            display_text: String::new(),
                            followup: Some(next_followup),
                            ask_user: None,
                        })
                    } else {
                        None
                    };
                    let _ = event_tx.send(aish_pty::AiEvent::Done(ai_response));
                    let _ = llm_done_tx.send(()); // signal rendering complete
                });

                // Wait for result with Ctrl+C cancellation support
                let session_cancel_token = token_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .ok();
                let result = loop {
                    match event_rx.try_recv() {
                        Ok(aish_pty::AiEvent::Done(ai_response)) => {
                            break ai_response;
                        }
                        Ok(aish_pty::AiEvent::AskUser(request)) => {
                            break Some(aish_pty::AiResponse {
                                command: None,
                                display_text: String::new(),
                                followup: None,
                                ask_user: Some((
                                    request,
                                    aish_pty::AskUserChannel {
                                        answer_sender: answer_tx.clone(),
                                        event_receiver: event_rx,
                                    },
                                )),
                            });
                        }
                        Ok(aish_pty::AiEvent::BashExec {
                            command,
                            output_sender,
                        }) => {
                            let osender = output_sender.clone();
                            let done_rx = std::sync::Mutex::new(llm_done_rx);
                            let cancelled_f = cancelled_fu.clone();
                            let followup: Box<aish_pty::FollowupCallback> = Box::new(
                            move |captured_output: &str,
                                  offload_path: Option<&str>|
                                  -> Option<aish_pty::AiResponse> {
                                // When the forwarding loop hard-aborts a
                                // followup (Ctrl+C during bashexec), it
                                // passes "Command cancelled by user".  Set
                                // the cancelled flag so the LLM event
                                // callback stops writing to stdout while
                                // the forwarding loop resumes shell I/O.
                                if captured_output.contains("cancelled by user") {
                                    cancelled_f.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                                let _ = osender.send(aish_pty::BashExecResult {
                                    output: captured_output.to_string(),
                                    offload_path: offload_path
                                        .map(|p| p.to_string()),
                                });
                                // Block until LLM thread finishes all rendering
                                let _ = done_rx
                                    .lock()
                                    .unwrap()
                                    .recv_timeout(std::time::Duration::from_secs(120));
                                None
                            },
                        );
                            break Some(aish_pty::AiResponse {
                                command: Some(command),
                                display_text: String::new(),
                                followup: Some(followup),
                                ask_user: None,
                            });
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    if aish_tools::bash::interactive_input_active() {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                    // Check for Ctrl+C on stdin (non-blocking)
                    let mut rfds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                    unsafe {
                        nix::libc::FD_ZERO(&mut rfds);
                        nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut rfds);
                    }
                    let mut tv = nix::libc::timeval {
                        tv_sec: 0,
                        tv_usec: 100_000,
                    };
                    let sel = unsafe {
                        nix::libc::select(
                            nix::libc::STDIN_FILENO + 1,
                            &mut rfds,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &mut tv,
                        )
                    };
                    if sel > 0 {
                        let mut byte = [0u8; 1];
                        if unsafe {
                            nix::libc::read(
                                nix::libc::STDIN_FILENO,
                                byte.as_mut_ptr() as *mut nix::libc::c_void,
                                1,
                            )
                        } == 1
                            && byte[0] == 0x03
                        {
                            cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                            if let Some(ref token) = session_cancel_token {
                                token.cancel();
                            }
                            anim_f.stop();
                            let msg =
                                format!("\r\n\x1b[33m{}\x1b[0m", t("shell.command_cancelled"));
                            unsafe {
                                nix::libc::write(
                                    nix::libc::STDOUT_FILENO,
                                    msg.as_ptr() as *mut nix::libc::c_void,
                                    msg.len(),
                                );
                            }
                            break None;
                        }
                    }
                    // Check timeout (60s)
                    if followup_start.elapsed() > std::time::Duration::from_secs(60) {
                        anim_f.stop();
                        let msg = b"\r\n\x1b[31mLLM timeout (60s)\x1b[0m";
                        unsafe {
                            nix::libc::write(
                                nix::libc::STDOUT_FILENO,
                                msg.as_ptr() as *mut nix::libc::c_void,
                                msg.len(),
                            );
                        }
                        break None;
                    }
                };

                anim_f.stop();

                // Clear residual reasoning overlay
                if reasoning_active_main.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let prev = reasoning_lines_main.load(std::sync::atomic::Ordering::SeqCst);
                    if prev > 0 {
                        use std::io::Write;
                        print!("\x1b[{}A", prev);
                        for _ in 0..prev {
                            print!("\r\x1b[K\n");
                        }
                        print!("\x1b[{}A", prev);
                        reasoning_lines_main.store(0, std::sync::atomic::Ordering::SeqCst);
                        let _ = std::io::stdout().flush();
                    }
                }

                result
            },
        )
    }

    fn build_session_ai_callback(
        config: &aish_config::ConfigModel,
        animation: &Arc<crate::animation::SharedAnimation>,
        shared_host: Arc<Mutex<Option<String>>>,
    ) -> Option<Box<aish_pty::AiCallback>> {
        let api_base = config.api_base.clone();
        let api_key = config.api_key.clone();
        let model = config.model.clone();
        let temperature = config.temperature;
        let max_tokens = config.max_tokens;
        let animation = animation.clone();

        // Load skills snapshot for SSH sessions (same as local session).
        // Stored in Arc so each AI query closure can create a fresh SkillTool.
        let ssh_skills_snapshot: std::sync::Arc<
            std::collections::HashMap<String, aish_tools::SkillInfo>,
        > = {
            let mut sm = aish_skills::SkillManager::new();
            let _ = sm.load_all_skills();
            std::sync::Arc::new(
                sm.list_skills()
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
                    .collect(),
            )
        };
        let ssh_skill_names: Vec<String> = ssh_skills_snapshot.keys().cloned().collect();
        let ssh_skills_description: String = ssh_skills_snapshot
            .values()
            .map(|s| format!("- **{}**: {}", s.name, s.description))
            .collect::<Vec<String>>()
            .join("\n");

        // Build oracle system prompt with remote host context
        let mut prompt_manager = aish_prompts::PromptManager::default_dir();
        prompt_manager.load_all();
        let role_prompt = prompt_manager.get("role").to_string();
        let mut vars = std::collections::HashMap::new();
        vars.insert("role_prompt".to_string(), role_prompt);
        vars.insert("username".to_string(), crate::ai_handler::whoami());
        vars.insert("hostname".to_string(), crate::ai_handler::hostname());
        vars.insert("os_info".to_string(), crate::ai_handler::os_info());
        vars.insert("cwd".to_string(), "~".to_string());
        vars.insert("system_info".to_string(), String::new());
        vars.insert("memory_context".to_string(), String::new());
        vars.insert("skill_list".to_string(), String::new());
        // Static base prompt — SSH context and dossier are built dynamically
        // inside the closure so nested SSH host changes are reflected.
        let system_msg_base = prompt_manager.render("oracle", &vars);

        // Shared host reference — updated by the forwarding loop when
        // nested SSH connections are detected or disconnected.
        let dossier_host = shared_host.clone();

        // Pre-compute static values for error correction template
        let ec_username = crate::ai_handler::whoami();
        let ec_os_info = crate::ai_handler::os_info();
        let conversation_history: Arc<Mutex<Vec<ChatMessage>>> = Arc::new(Mutex::new(Vec::new()));

        Some(Box::new(move |query: aish_pty::AiQuery| {
            // Detect error correction mode: user typed just `;` with no
            // question after a command failure.  In this case, the recent
            // output contains the error and we use a dedicated prompt.
            let is_error_correction = query.question.is_empty() && !query.recent_output.is_empty();

            // Build SSH context + dossier dynamically using the current host.
            // This ensures nested SSH host changes are reflected immediately.
            let current_host = dossier_host.lock().unwrap().clone();
            let mut system_msg_with_dossier = system_msg_base.clone();
            if let Some(ref host) = current_host {
                system_msg_with_dossier.push_str(&format!(
                    "\n\n**SSH Remote Session Context (overrides tool list above):** \
                     \n- The user is connected to a remote host **{host}** via SSH. \
                     \n- **Available tools:** `bash`, `ask_user`, `host_note`, `read_file`, and `skill`. \
                     \n- **DO NOT** call python_exec, write_file, edit_file, \
                     grep, glob, or any other tool — they do NOT exist in this session. \
                     \n- `bash` tool runs commands on the remote host. The command will be \
                     shown to the user for confirmation before execution. After execution, \
                     the output will be automatically returned to you for analysis. \
                     \n- `read_file` reads files on the LOCAL machine (where aish runs). \
                     Use it to read offload paths when bash output was offloaded to a local file. \
                     Do NOT use it for files on the remote host — use `bash` with `cat` for those. \
                     \n- `ask_user` asks the user a clarifying question (text_input or choice). \
                     \n- `host_note` tool saves, lists, or deletes notes about this remote host. \
                     When the user shares durable facts about this server (deployed services, \
                     known issues, key paths, assets, important warnings), \
                     proactively call `host_note` with action `store` to save them. These notes \
                     persist across sessions — next time the user connects to this host, you will \
                     automatically see them in the host dossier above. \
                     Do NOT save transient info like current directory, running process PIDs, \
                     or temporary file contents. \
                     \n- `skill` tool invokes a loaded skill plugin by name (use `skill_name` parameter). \
                     Do NOT use 'list' as a skill name — list the skills below instead. \
                     When the user's request matches a skill, invoke it BEFORE generating any other response. \
                     \n- **Available skills:**\n{ssh_skills_description} \
                     \n- **For reading/writing/searching files on the REMOTE host:** use `bash` tool with \
                     `cat`, `head`, `tail`, `echo`, `tee`, `grep`, `find`, `awk`, etc. \
                     \n- **IMPORTANT about offload:** when bash output is offloaded (status=offloaded), \
                     the file path is on the LOCAL machine. You MUST use `read_file` tool to read it. \
                     NEVER use `bash` to cat/wc/tail an offload path — that file does NOT exist on the remote host. \
                     \n- When the user asks to run a command, execute it, or check something, \
                     call the `bash` tool directly — do NOT just show the command in a code block."
                ));

                // Load host dossier on each invocation so notes added via
                // ;remember during this session are visible immediately.
                if let Some(profile) = aish_hosts::load_profile(host) {
                    let dossier = profile.format_for_prompt();
                    if !dossier.is_empty() {
                        system_msg_with_dossier.push_str(&format!("\n\n{}", dossier));
                    }
                }
            }

            let (context, effective_system_msg) = if is_error_correction {
                // Use aish's cmd_error template (same as local aish error correction)
                let mut pm = aish_prompts::PromptManager::default_dir();
                pm.load_all();

                // Extract the actual failed command from bash error output
                let failed_cmd = extract_failed_command(&query.recent_output);

                let stderr_section = if query.recent_output.is_empty() {
                    String::new()
                } else {
                    let s = &query.recent_output;
                    let preview = if s.len() > 2048 {
                        let mut end = 2048;
                        while end > 0 && !s.is_char_boundary(end) {
                            end -= 1;
                        }
                        &s[..end]
                    } else {
                        s
                    };
                    format!("\n**Command Output:**\n```\n{}\n```", preview)
                };

                let mut ec_vars = std::collections::HashMap::new();
                ec_vars.insert("username".to_string(), ec_username.clone());
                ec_vars.insert("os_info".to_string(), ec_os_info.clone());
                ec_vars.insert("command".to_string(), failed_cmd.clone());
                ec_vars.insert("exit_code".to_string(), "1".to_string());
                ec_vars.insert("stderr_section".to_string(), stderr_section);
                let mut sys = pm.render("cmd_error", &ec_vars);

                // Append SSH host context (use current dynamic host)
                if let Some(ref host) = current_host {
                    sys.push_str(&format!(
                        "\n\n**Important:** The command was executed on remote host **{}** via SSH.",
                        host
                    ));
                }

                // Same XML format as local aish's handle_error_correction
                let ctx = format!(
                    "<command_result>\nCommand: {}\nExit code: 1\n</command_result>\n\n\
                     Please analyze the error and suggest a fix. \
                     Check the recent terminal output above for the actual error output.",
                    failed_cmd
                );
                (ctx, sys)
            } else if query.recent_output.is_empty() {
                (query.question.clone(), system_msg_with_dossier.clone())
            } else {
                // Put user question first, recent output as reference context
                let ctx = format!(
                    "{}\n\nFor reference, recent terminal output:\n{}",
                    query.question, query.recent_output
                );
                (ctx, system_msg_with_dossier.clone())
            };

            // Start thinking spinner
            animation.start(&t("shell.status.thinking"));
            let thinking_start =
                std::sync::Arc::new(std::sync::Mutex::new(Some(std::time::Instant::now())));

            // Channel for ask_user communication between LLM thread and callback
            let (event_tx, event_rx) = std::sync::mpsc::channel::<aish_pty::AiEvent>();
            let (answer_tx, answer_rx) = std::sync::mpsc::channel::<aish_pty::AskUserAnswer>();

            // Channel to send the session's cancellation token back to the
            // main thread so Ctrl+C can cancel the in-flight LLM request.
            let (token_tx, token_rx) =
                std::sync::mpsc::channel::<std::sync::Arc<CancellationToken>>();
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancelled_t = cancelled.clone();
            let cancelled_fu = cancelled.clone();

            // Done signal: LLM thread sends when all rendering is complete.
            // The followup closure waits on this so the forwarding loop only
            // requests a new PTY prompt AFTER the LLM output is on screen.
            let (llm_done_tx, llm_done_rx) = std::sync::mpsc::channel::<()>();

            // Shared reasoning state — needed by both the LLM event callback
            // (inside the thread) and the main thread (to clear residual
            // reasoning lines before ask_user / normal completion).
            let reasoning_active_main =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reasoning_lines_main = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let reasoning_active_cb = reasoning_active_main.clone();
            let reasoning_lines_cb = reasoning_lines_main.clone();

            let api_base_t = api_base.clone();
            let api_key_f = api_key.clone();
            let model_f = model.clone();
            let animation_t = animation.clone();
            let thinking_start_thread = thinking_start.clone();
            let context_messages_t = conversation_history.lock().unwrap().clone();
            let context_for_thread = context.clone();
            let conversation_history_t = conversation_history.clone();
            let system_msg_t = effective_system_msg.clone();
            let query_question_t = query.question.clone();
            let api_base_th = api_base.clone();
            let api_key_th = api_key.clone();
            let model_th = model.clone();
            let animation_th = animation.clone();
            let conversation_history_th = conversation_history.clone();
            let system_msg_th = effective_system_msg.clone();
            let shared_host_th = dossier_host.clone();
            let skills_snapshot_th = ssh_skills_snapshot.clone();
            let skill_names_th = ssh_skill_names.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result = rt.block_on(async {
                    let mut session = LlmSession::new(
                        &api_base_t,
                        &api_key_f,
                        &model_f,
                        Some(temperature),
                        max_tokens,
                    );

                    // Send cancellation token to main thread
                    let _ = token_tx.send(session.cancellation_token_arc());
                    // Streaming event callback: only show reasoning overlay,
                    // collect content for formatted rendering after completion
                    let anim = animation_t.clone();
                    let cancelled_cb = cancelled_t.clone();
                    let reasoning_active = reasoning_active_cb.clone();
                    let reasoning_frame =
                        std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let reasoning_lines_displayed = reasoning_lines_cb.clone();
                    let reasoning_buf = std::sync::Mutex::new(String::new());
                    let thinking_start_r = thinking_start_thread.clone();
                    session.set_event_callback(std::sync::Arc::new(move |event| {
                        // Helper: clear reasoning overlay from terminal
                        let clear_reasoning = || {
                            if reasoning_active.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                let prev = reasoning_lines_displayed
                                    .swap(0, std::sync::atomic::Ordering::SeqCst);
                                if prev > 0 {
                                    use std::io::Write;
                                    print!("\x1b[{}A", prev);
                                    for _ in 0..prev {
                                        print!("\r\x1b[K\n");
                                    }
                                    print!("\x1b[{}A", prev);
                                    let _ = std::io::stdout().flush();
                                }
                                reasoning_buf.lock().unwrap().clear();
                            }
                        };
                        // Bail out if cancelled
                        if cancelled_cb.load(std::sync::atomic::Ordering::SeqCst) {
                            anim.stop();
                            clear_reasoning();
                            return None;
                        }
                        use aish_core::LlmEventType;
                        match event.event_type {
                            LlmEventType::GenerationStart => {
                                anim.stop();
                                reasoning_active.store(false, std::sync::atomic::Ordering::SeqCst);
                                reasoning_frame.store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_lines_displayed
                                    .store(0, std::sync::atomic::Ordering::SeqCst);
                                reasoning_buf.lock().unwrap().clear();
                                anim.start(&t("shell.status.thinking"));
                            }
                            LlmEventType::GenerationEnd => {
                                anim.stop();
                                clear_reasoning();
                            }
                            LlmEventType::ContentDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        clear_reasoning();
                                    }
                                }
                            }
                            LlmEventType::ReasoningDelta => {
                                if let Some(delta) =
                                    event.data.get("delta").and_then(|d| d.as_str())
                                {
                                    if !delta.is_empty() {
                                        anim.stop();
                                        if !reasoning_active
                                            .load(std::sync::atomic::Ordering::SeqCst)
                                        {
                                            reasoning_active
                                                .store(true, std::sync::atomic::Ordering::SeqCst);
                                        }
                                        let mut buf = reasoning_buf.lock().unwrap();
                                        buf.push_str(delta);
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
                                        let max_cols = 76usize;
                                        let frame = reasoning_frame
                                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        let spinner = DOTS_FRAMES[frame % DOTS_FRAMES.len()];
                                        let elapsed_str = thinking_start_r
                                            .lock()
                                            .unwrap()
                                            .map(|s| {
                                                let e = s.elapsed().as_secs_f64();
                                                if e >= 1.0 {
                                                    let mut args = std::collections::HashMap::new();
                                                    args.insert(
                                                        "elapsed".to_string(),
                                                        format!("{:.1}", e),
                                                    );
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t_with_args(
                                                            "shell.session.thinking_elapsed",
                                                            &args
                                                        )
                                                    )
                                                } else {
                                                    format!(
                                                        " {}",
                                                        aish_i18n::t("shell.session.thinking")
                                                    )
                                                }
                                            })
                                            .unwrap_or_else(|| {
                                                format!(
                                                    " {}",
                                                    aish_i18n::t("shell.session.thinking")
                                                )
                                            });
                                        let prev = reasoning_lines_displayed
                                            .load(std::sync::atomic::Ordering::SeqCst);
                                        let new_count = 1 + display_lines.len();
                                        if prev > 0 {
                                            print!("\x1b[{}A", prev);
                                        }
                                        if display_lines.is_empty() {
                                            print!(
                                                "\r\x1b[K\x1b[90m{}{}...\x1b[0m\n",
                                                spinner, elapsed_str
                                            );
                                        } else {
                                            print!(
                                                "\r\x1b[K\x1b[90m{}{}\x1b[0m\n",
                                                spinner, elapsed_str
                                            );
                                        }
                                        for line in &display_lines {
                                            let truncated =
                                                truncate_display_width(line.trim(), max_cols);
                                            print!("\r\x1b[K\x1b[90m{}\x1b[0m\n", truncated);
                                        }
                                        for _ in new_count..prev {
                                            print!("\r\x1b[K\n");
                                        }
                                        if prev > new_count {
                                            print!("\x1b[{}A", prev - new_count);
                                        }
                                        reasoning_lines_displayed
                                            .store(new_count, std::sync::atomic::Ordering::SeqCst);
                                        reasoning_active
                                            .store(true, std::sync::atomic::Ordering::SeqCst);
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            }
                            LlmEventType::ReasoningEnd => {
                                clear_reasoning();
                            }
                            LlmEventType::Error => {
                                anim.stop();
                                clear_reasoning();
                                let error_msg = event
                                    .data
                                    .get("error")
                                    .or_else(|| event.data.get("error_message"))
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("Unknown error");
                                eprintln!("\x1b[31mLLM error: {}\x1b[0m", error_msg);
                            }
                            LlmEventType::ToolExecutionStart => {
                                let tool_name = event
                                    .data
                                    .get("tool_name")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("unknown");
                                if tool_name == "read_file" {
                                    anim.stop();
                                    clear_reasoning();
                                    let path = event
                                        .data
                                        .get("tool_args")
                                        .and_then(|a| a.get("path"))
                                        .and_then(|p| p.as_str())
                                        .unwrap_or("?");
                                    use std::io::Write;
                                    println!("\x1b[90m📖 read_file({})\x1b[0m", path);
                                    let _ = std::io::stdout().flush();
                                }
                            }
                            LlmEventType::ToolExecutionEnd => {
                                let tool_name = event
                                    .data
                                    .get("tool_name")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("unknown");
                                if tool_name == "read_file" {
                                    let ok = event
                                        .data
                                        .get("ok")
                                        .and_then(|b| b.as_bool())
                                        .unwrap_or(false);
                                    if !ok {
                                        let preview = event
                                            .data
                                            .get("output_preview")
                                            .and_then(|p| p.as_str())
                                            .unwrap_or("error");
                                        use std::io::Write;
                                        println!("\x1b[31m{}\x1b[0m", preview);
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            }
                            _ => {}
                        }
                        None
                    }));

                    // Register channel-based tools for SSH sessions
                    session.register_tool(Box::new(aish_tools::ChannelBashTool::new(
                        event_tx.clone(),
                    )));
                    session.register_tool(Box::new(aish_tools::ChannelAskUserTool::new(
                        event_tx.clone(),
                        answer_rx,
                    )));
                    // Register host_note tool for SSH sessions
                    session.register_tool(Self::make_host_note_tool(shared_host_th.clone()));
                    // Register read_file (restricted to offload paths) for
                    // reading LOCAL offload files in SSH sessions.
                    session.register_tool(Box::new(aish_tools::fs::SshReadFileTool::new()));
                    // Register skill tool with loaded skills snapshot
                    {
                        let snap = skills_snapshot_th.clone();
                        let names = skill_names_th.clone();
                        let lookup = Box::new(move |name: &str| snap.get(name).cloned());
                        let list = Box::new(move || names.clone());
                        session.register_tool(Box::new(aish_tools::SkillTool::new(lookup, list)));
                    }

                    session
                        .process_input(
                            &context_for_thread,
                            &context_messages_t,
                            Some(&system_msg_t),
                            true,
                        )
                        .await
                });
                if cancelled_t.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let process_result = result.ok();
                let text = process_result.as_ref().map(|r| r.text.clone());

                // Update conversation history
                {
                    let mut h = conversation_history_t.lock().unwrap();
                    h.push(ChatMessage::user(&context_for_thread));
                    if let Some(ref pr) = process_result {
                        h.extend(pr.new_messages.clone());
                        if pr.new_messages.is_empty() {
                            h.push(ChatMessage::assistant(&pr.text));
                        }
                    }
                    let excess = h.len().saturating_sub(50);
                    if excess > 0 {
                        h.drain(..excess);
                    }
                }

                // Build AiResponse from text
                let ai_response = match text {
                    Some(ref t) if is_error_correction => {
                        // Render description
                        let ec_result = crate::ai_handler::parse_error_correction_response(t);
                        if let Some(ref desc) = ec_result.description {
                            if !desc.trim().is_empty() {
                                let _ = std::io::stdout().flush();
                                let mut renderer = crate::renderer::ShellRenderer::new();
                                renderer.render_separator();
                                renderer.render_markdown(desc);
                                let _ = std::io::stdout().flush();
                            }
                        }
                        Some(aish_pty::AiResponse {
                            command: ec_result.command,
                            display_text: String::new(),
                            followup: None,
                            ask_user: None,
                        })
                    }
                    Some(t) => {
                        // Render formatted markdown
                        if !t.trim().is_empty() {
                            let _ = std::io::stdout().flush();
                            let mut renderer = crate::renderer::ShellRenderer::new();
                            renderer.render_separator();
                            renderer.render_markdown(t.trim());
                            let elapsed = thinking_start_thread
                                .lock()
                                .unwrap()
                                .map(|s| s.elapsed().as_secs_f64())
                                .unwrap_or(0.0);
                            if elapsed >= 0.1 {
                                let mut elapsed_args = std::collections::HashMap::new();
                                elapsed_args.insert("time".to_string(), format!("{:.1}", elapsed));
                                println!(
                                    "\x1b[2m{}\x1b[0m",
                                    aish_i18n::t_with_args("shell.thinking_time", &elapsed_args)
                                );
                            }
                            renderer.render_separator();
                            let _ = std::io::stdout().flush();
                        }
                        let command = extract_bash_command(&t);
                        let followup = command.as_ref().map(|_cmd| {
                            Self::build_followup_closure(
                                &api_base_th,
                                &api_key_th,
                                &model_th,
                                Some(temperature),
                                max_tokens,
                                &system_msg_th,
                                &query_question_t,
                                &animation_th,
                                &conversation_history_th,
                                shared_host_th.clone(),
                            )
                        });
                        Some(aish_pty::AiResponse {
                            command,
                            display_text: String::new(),
                            followup,
                            ask_user: None,
                        })
                    }
                    None => Some(aish_pty::AiResponse {
                        command: None,
                        display_text: format!(
                            "\x1b[33m{}\x1b[0m",
                            aish_i18n::t("shell.session.ai_error")
                        ),
                        followup: None,
                        ask_user: None,
                    }),
                };
                let _ = event_tx.send(aish_pty::AiEvent::Done(ai_response));
                let _ = llm_done_tx.send(()); // signal rendering complete
            });

            // Wait for result with Ctrl+C cancellation support
            // Also handles ask_user events from the LLM thread
            enum CallbackEvent {
                Done(Option<aish_pty::AiResponse>),
                AskUser(aish_pty::AskUserRequest, aish_pty::AskUserChannel),
                BashExec {
                    command: String,
                    output_sender: std::sync::mpsc::Sender<aish_pty::BashExecResult>,
                    event_receiver: std::sync::mpsc::Receiver<aish_pty::AiEvent>,
                },
            }
            // Receive the session's cancellation token (sent by the LLM thread)
            let session_cancel_token = token_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .ok();
            let cb_event = loop {
                match event_rx.try_recv() {
                    Ok(aish_pty::AiEvent::Done(ai_response)) => {
                        break Some(CallbackEvent::Done(ai_response));
                    }
                    Ok(aish_pty::AiEvent::AskUser(request)) => {
                        break Some(CallbackEvent::AskUser(
                            request,
                            aish_pty::AskUserChannel {
                                answer_sender: answer_tx.clone(),
                                event_receiver: event_rx,
                            },
                        ));
                    }
                    Ok(aish_pty::AiEvent::BashExec {
                        command,
                        output_sender,
                    }) => {
                        break Some(CallbackEvent::BashExec {
                            command,
                            output_sender,
                            event_receiver: event_rx,
                        });
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                if aish_tools::bash::interactive_input_active() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                // Check for Ctrl+C on stdin (non-blocking)
                let mut rfds: nix::libc::fd_set = unsafe { std::mem::zeroed() };
                unsafe {
                    nix::libc::FD_ZERO(&mut rfds);
                    nix::libc::FD_SET(nix::libc::STDIN_FILENO, &mut rfds);
                }
                let mut tv = nix::libc::timeval {
                    tv_sec: 0,
                    tv_usec: 100_000,
                }; // 100ms
                let sel = unsafe {
                    nix::libc::select(
                        nix::libc::STDIN_FILENO + 1,
                        &mut rfds,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut tv,
                    )
                };
                if sel > 0 {
                    // Read one byte to check for Ctrl+C
                    let mut byte = [0u8; 1];
                    if unsafe {
                        nix::libc::read(
                            nix::libc::STDIN_FILENO,
                            byte.as_mut_ptr() as *mut nix::libc::c_void,
                            1,
                        )
                    } == 1
                        && byte[0] == 0x03
                    {
                        // Ctrl+C pressed — cancel the LLM request
                        cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                        if let Some(ref token) = session_cancel_token {
                            token.cancel();
                        }
                        animation.stop();
                        println!("\r\n\x1b[33m{}\x1b[0m", t("shell.command_cancelled"));
                        break None;
                    }
                    // Non-Ctrl-C byte during AI processing — discard.
                    // The user shouldn't be typing during AI processing;
                    // any stray bytes are not recoverable here.
                }
                // Check timeout (60s)
                if thinking_start
                    .lock()
                    .unwrap()
                    .is_some_and(|s| s.elapsed() > std::time::Duration::from_secs(60))
                {
                    animation.stop();
                    eprintln!("\x1b[31mLLM timeout (60s)\x1b[0m");
                    break None;
                }
            };

            animation.stop();

            // Clear any residual reasoning lines left on screen (the LLM
            // event callback may have shown reasoning deltas that were not
            // erased because GenerationEnd / ReasoningEnd haven't fired yet).
            if reasoning_active_main.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let prev = reasoning_lines_main.load(std::sync::atomic::Ordering::SeqCst);
                if prev > 0 {
                    use std::io::Write;
                    print!("\x1b[{}A", prev);
                    for _ in 0..prev {
                        print!("\r\x1b[K\n");
                    }
                    print!("\x1b[{}A", prev);
                    reasoning_lines_main.store(0, std::sync::atomic::Ordering::SeqCst);
                    let _ = std::io::stdout().flush();
                }
            }

            // Handle the callback event
            match cb_event {
                // Ask_user — return response with ask_user channel
                Some(CallbackEvent::AskUser(request, channel)) => Some(aish_pty::AiResponse {
                    command: None,
                    display_text: String::new(),
                    followup: None,
                    ask_user: Some((request, channel)),
                }),
                // Bash_exec — return command for execution on remote host,
                // with multi-round chaining support.
                Some(CallbackEvent::BashExec {
                    command,
                    output_sender,
                    event_receiver,
                }) => {
                    let shared_event_rx =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(event_receiver)));
                    let shared_done_rx =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(llm_done_rx)));
                    let answer_tx_f = answer_tx.clone();

                    fn make_chain_followup(
                        event_rx: std::sync::Arc<
                            std::sync::Mutex<Option<std::sync::mpsc::Receiver<aish_pty::AiEvent>>>,
                        >,
                        done_rx: std::sync::Arc<
                            std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
                        >,
                        answer_tx: std::sync::mpsc::Sender<aish_pty::AskUserAnswer>,
                        output_sender: std::sync::mpsc::Sender<aish_pty::BashExecResult>,
                        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
                        cancel_token: Option<std::sync::Arc<CancellationToken>>,
                    ) -> Box<aish_pty::FollowupCallback> {
                        Box::new(
                            move |captured_output: &str,
                                  offload_path: Option<&str>|
                                  -> Option<aish_pty::AiResponse> {
                                // When the forwarding loop hard-aborts (Ctrl+C
                                // during bashexec) or the user rejects the
                                // command, cancel the LLM session and stop
                                // the tool chain immediately.
                                if captured_output.contains("cancelled by user")
                                    || captured_output.contains("rejected by user")
                                {
                                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                                    let _ = output_sender.send(aish_pty::BashExecResult {
                                        output: captured_output.to_string(),
                                        offload_path: offload_path.map(|p| p.to_string()),
                                    });
                                    if let Some(ref token) = cancel_token {
                                        token.cancel();
                                    }
                                    // Drain remaining events so the LLM thread
                                    // can finish cleanly.
                                    if let Some(rx) = event_rx.lock().unwrap().take() {
                                        drop(rx);
                                    }
                                    return None;
                                }
                                let _ = output_sender.send(aish_pty::BashExecResult {
                                    output: captured_output.to_string(),
                                    offload_path: offload_path.map(|p| p.to_string()),
                                });

                                let rx = match event_rx.lock().unwrap().take() {
                                    Some(rx) => rx,
                                    None => return None,
                                };

                                match rx.recv_timeout(std::time::Duration::from_secs(120)) {
                                    Ok(aish_pty::AiEvent::BashExec {
                                        command,
                                        output_sender: new_sender,
                                    }) => {
                                        *event_rx.lock().unwrap() = Some(rx);
                                        Some(aish_pty::AiResponse {
                                            command: Some(command),
                                            display_text: String::new(),
                                            followup: Some(make_chain_followup(
                                                event_rx.clone(),
                                                done_rx.clone(),
                                                answer_tx.clone(),
                                                new_sender,
                                                cancelled.clone(),
                                                cancel_token.clone(),
                                            )),
                                            ask_user: None,
                                        })
                                    }
                                    Ok(aish_pty::AiEvent::AskUser(request)) => {
                                        Some(aish_pty::AiResponse {
                                            command: None,
                                            display_text: String::new(),
                                            followup: None,
                                            ask_user: Some((
                                                request,
                                                aish_pty::AskUserChannel {
                                                    answer_sender: answer_tx.clone(),
                                                    event_receiver: rx,
                                                },
                                            )),
                                        })
                                    }
                                    Ok(aish_pty::AiEvent::Done(_)) | Err(_) => {
                                        if let Some(drx) = done_rx.lock().unwrap().take() {
                                            let _ = drx
                                                .recv_timeout(std::time::Duration::from_secs(120));
                                        }
                                        None
                                    }
                                }
                            },
                        )
                    }

                    let followup = make_chain_followup(
                        shared_event_rx,
                        shared_done_rx,
                        answer_tx_f,
                        output_sender,
                        cancelled_fu,
                        session_cancel_token,
                    );
                    Some(aish_pty::AiResponse {
                        command: Some(command),
                        display_text: String::new(),
                        followup: Some(followup),
                        ask_user: None,
                    })
                }
                // Normal completion — AiResponse already built by LLM thread
                Some(CallbackEvent::Done(ai_response)) => ai_response,
                None => None,
            }
        }))
    }
}

/// Cached regex for stripping complete XML tags from tool output.
static TOOL_XML_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
/// Cached regex for removing trailing tool metadata blocks.
static TOOL_XML_TRAILING_METADATA_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
/// Cached regex for removing incomplete tags from truncation.
static TOOL_XML_INCOMPLETE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

/// Strip XML tags from tool output to extract plain text content for terminal display.
/// Handles multi-line <offload>JSON</offload> blocks, <return_code>, <stdout>,
/// <stderr>, and any incomplete tags from truncation.
fn strip_tool_output_xml(output: &str) -> String {
    let re_trailing_metadata = TOOL_XML_TRAILING_METADATA_RE.get_or_init(|| {
        regex::Regex::new(
            r"(?ms)(?:^|\n)<(?:offload|return_code|exit-code)>\n.*?\n</(?:offload|return_code|exit-code)>\s*$",
        )
        .unwrap()
    });
    let mut cleaned = output.trim().to_string();
    loop {
        let next = re_trailing_metadata.replace(&cleaned, "").to_string();
        if next == cleaned {
            break;
        }
        cleaned = next.trim_end().to_string();
    }
    // Remove incomplete tags (e.g. "<stdo" from truncation)
    let re_incomplete =
        TOOL_XML_INCOMPLETE_RE.get_or_init(|| regex::Regex::new(r"<[^>]*$").unwrap());
    let cleaned = re_incomplete.replace_all(&cleaned, "").to_string();
    // Remove remaining single-line XML tags
    let re = TOOL_XML_RE.get_or_init(|| {
        regex::Regex::new(r"</?(?:stdout|stderr|return_code|exit-code)/?>").unwrap()
    });
    let cleaned = re.replace_all(&cleaned, "").to_string();
    // Collapse multiple blank lines and trim
    cleaned
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse output to first N lines for terminal display, matching Python's
/// `_collapse_output_lines` behavior: show first `max_lines` lines and append
/// " ..." if truncated.
fn collapse_display_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_string();
    }
    let collapsed: Vec<&str> = lines.iter().take(max_lines).copied().collect();
    format!("{} ...", collapsed.join("\n"))
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
    fn test_strip_tool_output_xml_removes_return_code_block() {
        let output = "<stdout>\nconfig.yaml\n</stdout>\n<return_code>\n0\n</return_code>";
        assert_eq!(strip_tool_output_xml(output), "config.yaml");
    }

    #[test]
    fn test_strip_tool_output_xml_preserves_return_code_text_in_stdout() {
        let output = "<stdout>\nliteral <return_code>\n0\n</return_code> block\n</stdout>\n<return_code>\n0\n</return_code>";
        assert_eq!(strip_tool_output_xml(output), "literal \n0\n block");
    }

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

/// Extract the first ```bash code block from AI response text.
/// Extract the remote host from an SSH/telnet command.
/// Forward-parsing version that correctly skips SSH options with arguments.
/// e.g. "ssh -p 2222 user@example.com" → "user@example.com"
/// e.g. "ssh -o StrictHostKeyChecking=no root@host" → "root@host"
fn extract_remote_host(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let cmd = parts[0];
    if !matches!(cmd, "ssh" | "telnet" | "mosh" | "sftp" | "nc" | "netcat") {
        return None;
    }
    let opts_with_arg: &[&str] = &[
        "-p", "-l", "-i", "-o", "-L", "-R", "-S", "-W", "-J", "-b", "-c", "-F", "-I", "-K", "-m",
        "-Q", "-q",
    ];
    let mut iter = parts.iter().skip(1).peekable();
    while let Some(part) = iter.next() {
        if part.starts_with('-') {
            let opt_name = if part.contains('=') {
                part.split('=').next().unwrap()
            } else {
                *part
            };
            if opts_with_arg.contains(&opt_name) && !part.contains('=') {
                iter.next();
            }
            continue;
        }
        return Some(part.to_string());
    }
    None
}

/// Extract the failed command from PTY output after a command error.
/// Strategy 1: Find the full command from the prompt line just before the
/// bash error (preserves pipes, args, etc.).
/// Strategy 2: Extract the command name from the bash error message.
fn extract_failed_command(output: &str) -> String {
    static ANSI_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ANSI_RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap());
    let clean = re.replace_all(output, "").to_string();
    let lines: Vec<&str> = clean.lines().collect();

    // Find the shell error line
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_shell_error = trimmed.starts_with("-bash: ")
            || trimmed.starts_with("bash: ")
            || trimmed.starts_with("-ksh: ")
            || trimmed.starts_with("ksh: ")
            || trimmed.starts_with("-zsh: ")
            || trimmed.starts_with("zsh: ");

        if is_shell_error && i > 0 {
            // Look at the line before the error — it should be the prompt + command
            let prev = lines[i - 1].trim();
            // Extract command after the last "# " or "$ " (common prompt endings)
            if let Some(idx) = prev.rfind("# ") {
                let full_cmd = prev[idx + 2..].trim();
                if !full_cmd.is_empty() {
                    return full_cmd.to_string();
                }
            }
            if let Some(idx) = prev.rfind("$ ") {
                let full_cmd = prev[idx + 2..].trim();
                if !full_cmd.is_empty() {
                    return full_cmd.to_string();
                }
            }
        }
    }

    // Fallback: extract command name from the error message itself
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("-bash: ")
            .or_else(|| trimmed.strip_prefix("bash: "))
            .or_else(|| trimmed.strip_prefix("-ksh: "))
            .or_else(|| trimmed.strip_prefix("ksh: "))
            .or_else(|| trimmed.strip_prefix("-zsh: "))
            .or_else(|| trimmed.strip_prefix("zsh: "));
        if let Some(rest) = rest {
            if let Some(colon_pos) = rest.find(": ") {
                let cmd = rest[..colon_pos].trim();
                if !cmd.is_empty() {
                    return cmd.to_string();
                }
            }
        }
    }
    "(remote command)".to_string()
}

fn extract_bash_command(text: &str) -> Option<String> {
    let marker = "```bash";
    let start = text.find(marker)?;
    let content_start = start + marker.len();
    let content_end = text[content_start..].find("```")?;
    let cmd = text[content_start..content_start + content_end]
        .trim()
        .to_string();
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
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
