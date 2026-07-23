use std::fs;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aish_llm::{Tool, ToolResult};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::prompt;

/// Default readiness poll budget for `start`.
const DEFAULT_READY_TIMEOUT_SECS: u64 = 15;
/// Grace period after SIGTERM before escalating to SIGKILL.
const STOP_GRACE_SECS: u64 = 3;
/// Default number of trailing log lines returned by `logs`.
const DEFAULT_LOG_LINES: usize = 50;
/// Polling interval while waiting for readiness.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Bytes of recent log output examined for `ready_log` matching.
const READY_LOG_TAIL_BYTES: usize = 8 * 1024;

/// Persisted state for a single supervised service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceState {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    /// Unix epoch seconds of the most recent successful spawn; 0 when stopped.
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub restart_count: u32,
    #[serde(default = "default_restart_policy")]
    pub restart_policy: String,
    #[serde(default)]
    pub ready_log: Option<String>,
    #[serde(default)]
    pub ready_port: Option<u16>,
}

fn default_restart_policy() -> String {
    "no".to_string()
}

impl ServiceState {
    fn new(name: &str, command: &str) -> Self {
        Self {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            cwd: None,
            pid: None,
            started_at: 0,
            ready: false,
            restart_count: 0,
            restart_policy: default_restart_policy(),
            ready_log: None,
            ready_port: None,
        }
    }
}

/// Tool that supervises long-running background services.
///
/// State lives at `~/.local/share/aish/services/<name>.json` and logs at
/// `~/.local/share/aish/services/<name>.log`. The tool itself is stateless; all
/// per-service bookkeeping is persisted on disk so invocations are independent.
pub struct ServiceSupervisorTool;

impl Default for ServiceSupervisorTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceSupervisorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ServiceSupervisorTool {
    fn name(&self) -> &str {
        "service_supervisor"
    }

    fn description(&self) -> &str {
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let action = match get_str(&args, "action") {
            Some(a) => a,
            None => return ToolResult::error("Missing required parameter: action"),
        };
        let name = match get_str(&args, "name") {
            Some(n) => n,
            None => return ToolResult::error("Missing required parameter: name"),
        };
        if name.trim().is_empty() {
            return ToolResult::error("Parameter `name` must not be empty");
        }

        let dir = services_dir();
        match action.as_str() {
            "start" => start_service(&dir, &args, &name),
            "status" => status_service(&dir, &name),
            "stop" => stop_service(&dir, &name),
            "logs" => {
                let lines = get_u64(&args, "log_lines")
                    .map(|n| n as usize)
                    .unwrap_or(DEFAULT_LOG_LINES)
                    .max(1);
                logs_service(&dir, &name, lines)
            }
            "restart" => restart_service(&dir, &args, &name),
            other => ToolResult::error(format!(
                "Unknown action `{}`; expected one of start|status|stop|logs|restart",
                other
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Base directory for all supervised-service state and logs.
pub fn services_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aish")
        .join("services")
}

/// Restrict a service name to path-safe characters to prevent traversal.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "service".to_string()
    } else {
        cleaned
    }
}

fn state_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize_name(name)))
}

fn log_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.log", sanitize_name(name)))
}

// ---------------------------------------------------------------------------
// State persistence (pure w.r.t. the directory passed in)
// ---------------------------------------------------------------------------

fn load_state(dir: &Path, name: &str) -> Option<ServiceState> {
    let path = state_path(dir, name);
    let data = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_state(dir: &Path, state: &ServiceState) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = state_path(dir, &state.name);
    let json = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    // Atomic-ish replace to avoid readers observing a truncated file.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Liveness / signals
// ---------------------------------------------------------------------------

/// Probe whether `pid` is still running.
///
/// Because supervised processes are spawned detached and their `Child` handle
/// is dropped (never `wait`ed), a naturally-exited child lingers as a zombie.
/// `kill(pid, 0)` reports a zombie as alive, so we first attempt a non-blocking
/// `waitpid` which both detects the exit and reaps the zombie. If the pid is
/// not our child (e.g. reparented via a future setsid), we fall back to a
/// signal-0 probe.
fn pid_alive(pid: u32) -> bool {
    let raw = pid as libc::pid_t;
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid(2) with WNOHANG never blocks; it inspects/reaps child state.
    let r = unsafe { libc::waitpid(raw, &mut status, libc::WNOHANG) };
    if r == raw {
        // Reaped: the child has exited.
        return false;
    }
    if r == -1 {
        let err = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::ECHILD {
            // Not our child; fall through to the signal-0 probe.
            let rc = unsafe { libc::kill(raw, 0) };
            if rc == 0 {
                return true;
            }
            let e = io::Error::last_os_error().raw_os_error().unwrap_or(0);
            // ESRCH = no such process; EPERM = exists but no permission -> alive.
            return e != libc::ESRCH;
        }
    }
    // r == 0 => child still running (no state change).
    true
}

fn send_signal(pid: u32, sig: libc::c_int) {
    // SAFETY: best-effort signal delivery to a known pid.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// Combine configured readiness signals. A criterion that is not configured is
/// treated as satisfied, so with no checks configured the result is `true`.
/// When both are configured, both must pass (logical AND).
fn evaluate_ready(log_content: Option<&str>, log_re: Option<&Regex>, port_open: Option<bool>) -> bool {
    let log_ok = match log_re {
        Some(re) => log_content.map(|c| re.is_match(c)).unwrap_or(false),
        None => true,
    };
    let port_ok = port_open.unwrap_or(true);
    log_ok && port_ok
}

/// Match `ready_log` regex against recent log content.
fn log_matches_ready(content: &str, re: &Regex) -> bool {
    re.is_match(content)
}

/// Attempt a short TCP connect to 127.0.0.1:`port` to test readiness.
fn port_is_open(port: u16) -> bool {
    let addr: SocketAddr = match format!("127.0.0.1:{}", port).parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// Read up to the last `bytes` of `path` as a UTF-8 (lossy) string.
fn read_log_tail(path: &Path, bytes: usize) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > bytes as u64 {
        // Seek to `bytes` before EOF so only the recent window is examined.
        file.seek(SeekFrom::End(-(bytes as i64))).ok()?;
    }
    let mut buf = Vec::with_capacity(bytes.min(READY_LOG_TAIL_BYTES));
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Poll readiness until `deadline` or the configured checks pass.
fn wait_until_ready(
    log_file: &Path,
    ready_log: Option<&str>,
    ready_port: Option<u16>,
    deadline: Instant,
) -> bool {
    let log_re = ready_log.and_then(|p| Regex::new(p).ok());
    loop {
        let log_content = if log_re.is_some() {
            read_log_tail(log_file, READY_LOG_TAIL_BYTES)
        } else {
            None
        };
        let port_open = ready_port.map(port_is_open);
        if let Some(re) = log_re.as_ref() {
            let matched = log_content
                .as_deref()
                .map(|c| log_matches_ready(c, re))
                .unwrap_or(false);
            let port_ok = port_open.unwrap_or(true);
            if matched && port_ok {
                return true;
            }
        } else {
            // Only a port criterion (or none): port_open being None means "no
            // criterion", which evaluate_ready treats as satisfied.
            if evaluate_ready(None, None, port_open) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

fn start_service(dir: &Path, args: &serde_json::Value, name: &str) -> ToolResult {
    // Resolve command: explicit arg wins, else reuse persisted config.
    let mut state = load_state(dir, name).unwrap_or_else(|| ServiceState::new(name, ""));
    let command = match get_str(args, "command") {
        Some(c) if !c.is_empty() => c,
        _ => {
            if state.command.is_empty() {
                return ToolResult::error(format!(
                    "Cannot start `{}`: `command` is required and no prior config exists",
                    name
                ));
            }
            state.command.clone()
        }
    };
    state.command = command.clone();

    if let Some(a) = get_string_array(args, "args") {
        state.args = a;
    }
    if let Some(cwd) = get_str(args, "cwd") {
        state.cwd = Some(cwd);
    }
    if let Some(rl) = get_str(args, "ready_log") {
        state.ready_log = Some(rl);
    }
    if let Some(rp) = get_u64(args, "ready_port") {
        state.ready_port = Some(rp as u16);
    }
    if let Some(p) = get_str(args, "restart_policy") {
        state.restart_policy = p;
    }

    // Idempotent: if an existing instance is still alive, report it instead of
    // spawning a duplicate (avoids port/handle conflicts).
    if let Some(pid) = state.pid {
        if pid_alive(pid) {
            return status_result(&state, true, format!("`{}` already running", name));
        }
    }

    let log_file = log_path(dir, name);
    if let Err(e) = fs::create_dir_all(dir) {
        return ToolResult::error(format!("Failed to create services dir: {e}"));
    }

    let stdout_handle = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        Ok(f) => f,
        Err(e) => return ToolResult::error(format!("Failed to open log file: {e}")),
    };
    let stderr_handle = match stdout_handle.try_clone() {
        Ok(f) => f,
        Err(e) => return ToolResult::error(format!("Failed to dup log handle: {e}")),
    };

    let mut cmd = Command::new(&command);
    cmd.args(&state.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_handle))
        .stderr(Stdio::from(stderr_handle));
    if let Some(cwd) = &state.cwd {
        cmd.current_dir(cwd);
    }
    // Detach into a new process group so a terminal SIGHUP does not reach it.
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ToolResult::error(format!("Failed to spawn `{}`: {e}", command));
        }
    };
    let pid = child.id();
    // Intentionally drop `child` WITHOUT waiting or killing: the process is now
    // supervised only via its pid + state file.

    let is_restart = state.started_at != 0 || state.pid.is_some();
    if is_restart {
        state.restart_count = state.restart_count.saturating_add(1);
    } else {
        state.restart_count = 0;
    }
    state.pid = Some(pid);
    state.started_at = now_secs();

    let ready_timeout = get_u64(args, "ready_timeout").unwrap_or(DEFAULT_READY_TIMEOUT_SECS);
    let deadline = Instant::now() + Duration::from_secs(ready_timeout);
    let ready = wait_until_ready(&log_file, state.ready_log.as_deref(), state.ready_port, deadline);
    state.ready = ready;

    if let Err(e) = save_state(dir, &state) {
        tracing::warn!("failed to persist state for `{}`: {e}", name);
    }

    let output = if ready {
        format!("Started `{}` (pid {pid}, ready)", name)
    } else {
        format!("Started `{}` (pid {pid}, readiness not confirmed)", name)
    };
    status_result(&state, true, output)
}

fn status_service(dir: &Path, name: &str) -> ToolResult {
    let state = match load_state(dir, name) {
        Some(s) => s,
        None => {
            return ok_meta(
                format!("No state for `{}`", name),
                serde_json::json!({
                    "name": name,
                    "running": false,
                    "ready": false,
                    "configured": false,
                }),
            );
        }
    };
    status_result(&state, false, format!("Status for `{}`", name))
}

fn stop_service(dir: &Path, name: &str) -> ToolResult {
    let mut state = match load_state(dir, name) {
        Some(s) => s,
        None => return ToolResult::error(format!("No state for `{}`; nothing to stop", name)),
    };
    let stopped = match state.pid {
        Some(pid) if pid_alive(pid) => {
            send_signal(pid, libc::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(STOP_GRACE_SECS);
            while Instant::now() < deadline {
                if !pid_alive(pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if pid_alive(pid) {
                send_signal(pid, libc::SIGKILL);
                std::thread::sleep(Duration::from_millis(100));
            }
            !pid_alive(pid)
        }
        Some(_) => true, // already gone
        None => false,   // nothing recorded
    };

    state.pid = None;
    state.ready = false;
    state.started_at = 0;
    if let Err(e) = save_state(dir, &state) {
        tracing::warn!("failed to persist state for `{}`: {e}", name);
    }

    let output = if stopped {
        format!("Stopped `{}`", name)
    } else {
        format!("`{}` was not running", name)
    };
    ok_meta(
        output,
        serde_json::json!({
            "name": name,
            "stopped": stopped,
            "restart_policy": state.restart_policy,
        }),
    )
}

fn logs_service(dir: &Path, name: &str, lines: usize) -> ToolResult {
    let path = log_path(dir, name);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return ToolResult::error(format!("No log file for `{}`", name));
        }
        Err(e) => return ToolResult::error(format!("Failed to read log: {e}")),
    };
    let tail: Vec<&str> = content.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect();
    let body = tail.join("\n");
    ok_meta(
        format!("Last {} log line(s) for `{}`", tail.len(), name),
        serde_json::json!({
            "name": name,
            "lines": tail.len(),
            "log": body,
        }),
    )
}

fn restart_service(dir: &Path, args: &serde_json::Value, name: &str) -> ToolResult {
    // Stop is best-effort: missing state is fine (treat as fresh start).
    if load_state(dir, name).is_some() {
        let _ = stop_service(dir, name);
    }
    start_service(dir, args, name)
}

// ---------------------------------------------------------------------------
// Output shaping
// ---------------------------------------------------------------------------

fn status_result(state: &ServiceState, running_override: bool, output: String) -> ToolResult {
    let (running, pid) = match state.pid {
        Some(pid) => (running_override || pid_alive(pid), Some(pid)),
        None => (false, None),
    };
    let uptime = if running {
        now_secs().saturating_sub(state.started_at)
    } else {
        0
    };
    ok_meta(
        output,
        serde_json::json!({
            "name": state.name,
            "command": state.command,
            "args": state.args,
            "cwd": state.cwd,
            "pid": pid,
            "running": running,
            "ready": running && state.ready,
            "uptime_secs": uptime,
            "restart_count": state.restart_count,
            "restart_policy": state.restart_policy,
            "ready_log": state.ready_log,
            "ready_port": state.ready_port,
        }),
    )
}

fn ok_meta(output: impl Into<String>, meta: serde_json::Value) -> ToolResult {
    ToolResult {
        ok: true,
        output: output.into(),
        meta: Some(meta),
    }
}

// ---------------------------------------------------------------------------
// Argument extraction
// ---------------------------------------------------------------------------

fn get_str(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)?
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn get_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key)?.as_u64()
}

fn get_string_array(args: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let arr = args.get(key)?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
    )
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aish_llm::Tool;
    use std::net::TcpListener;

    /// Service state must survive a save -> load round-trip unchanged, including
    /// optional fields and serde defaults.
    #[test]
    fn test_state_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut state = ServiceState::new("web", "/usr/bin/python3");
        state.args = vec!["-m".into(), "http.server".into(), "8080".into()];
        state.cwd = Some("/srv".into());
        state.pid = Some(4242);
        state.started_at = 1_700_000_000;
        state.ready = true;
        state.restart_count = 3;
        state.restart_policy = "always".into();
        state.ready_log = Some("Serving HTTP".into());
        state.ready_port = Some(8080);

        save_state(dir, &state).unwrap();

        let loaded = load_state(dir, "web").expect("state file should exist");
        assert_eq!(loaded, state);

        // A second service coexists in the same dir without collision.
        let other = ServiceState::new("db", "postgres");
        save_state(dir, &other).unwrap();
        assert_eq!(load_state(dir, "db").unwrap(), other);
        assert_eq!(load_state(dir, "web").unwrap(), state);
    }

    /// Default restart policy must deserialize as "no" for a minimal state file,
    /// guaranteeing forward compatibility with older state written before the
    /// field existed.
    #[test]
    fn test_state_defaults_minimal_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Hand-written minimal JSON, missing optional fields.
        fs::write(
            state_path(dir, "min"),
            r#"{"name":"min","command":"echo","pid":null,"started_at":0,"ready":false}"#,
        )
        .unwrap();
        let loaded = load_state(dir, "min").unwrap();
        assert_eq!(loaded.restart_policy, "no");
        assert!(loaded.args.is_empty());
        assert_eq!(loaded.restart_count, 0);
    }

    /// `ready_log` must match anywhere in the recent log tail (substring/regex).
    #[test]
    fn test_ready_log_matching() {
        let re = Regex::new(r"Listening on (http|https)://").unwrap();
        assert!(log_matches_ready("info: Listening on http://0.0.0.0:3000", &re));
        assert!(log_matches_ready("Listening on https://localhost", &re));
        assert!(!log_matches_ready("starting up...", &re));
        assert!(!log_matches_ready("", &re));

        // Combined readiness: regex present but unmatched -> not ready even if
        // the port probe (simulated) would succeed.
        assert!(!evaluate_ready(Some("booting"), Some(&re), Some(true)));
        // Regex matches and port open -> ready.
        assert!(evaluate_ready(
            Some("Listening on http://x"),
            Some(&re),
            Some(true)
        ));
        // No criteria configured -> optimistic ready.
        assert!(evaluate_ready(None, None, None));
        // Only port configured, open -> ready.
        assert!(evaluate_ready(None, None, Some(true)));
        // Only port configured, closed -> not ready.
        assert!(!evaluate_ready(None, None, Some(false)));
    }

    /// Port readiness must report a live local listener as open and a closed
    /// port as not open. Uses a real ephemeral listener — no network mock.
    #[test]
    fn test_ready_port_probe() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_is_open(port), "bound listener should be reachable");

        // Port 1 is unprivileged to *connect* to and refuses fast on loopback.
        assert!(!port_is_open(1), "unused port should be closed");
    }

    /// Name sanitization must neutralize path-traversal attempts.
    #[test]
    fn test_name_sanitization() {
        assert_eq!(sanitize_name("web-server_1.0"), "web-server_1.0");
        // Slashes are stripped to `_`, so the result is a flat (traversal-safe) filename.
        assert_eq!(sanitize_name("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_name("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_name(""), "service");
        // Traversal cannot escape the services dir.
        assert!(state_path(Path::new("/tmp/svc"), "../x").starts_with("/tmp/svc"));
    }

    /// `logs` returns the requested number of trailing lines, not the whole file.
    #[test]
    fn test_logs_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let body: String = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        fs::write(log_path(dir, "svc"), body).unwrap();

        let res = logs_service(dir, "svc", 3);
        assert!(res.ok);
        let meta = res.meta.unwrap();
        assert_eq!(meta["lines"], 3);
        assert_eq!(meta["log"], "line 7\nline 8\nline 9");

        // Requesting more than available returns all lines present.
        let res2 = logs_service(dir, "svc", 999);
        assert_eq!(res2.meta.unwrap()["lines"], 10);
    }

    /// `status` for an unknown service reports not configured rather than erroring.
    #[test]
    fn test_status_unknown_service() {
        let tmp = tempfile::tempdir().unwrap();
        let res = status_service(tmp.path(), "nope");
        assert!(res.ok);
        let meta = res.meta.unwrap();
        assert_eq!(meta["running"], false);
        assert_eq!(meta["configured"], false);
    }

    /// Tool wiring: name/parameters/prompt are wired and action dispatch rejects
    /// an unknown action with an error result.
    #[test]
    fn test_tool_dispatch_unknown_action() {
        let tool = ServiceSupervisorTool::new();
        assert_eq!(tool.name(), "service_supervisor");
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        let actions = params["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(actions.len(), 5);

        let res = tool.execute(serde_json::json!({"action": "frobnicate", "name": "x"}));
        assert!(!res.ok);
        assert!(res.output.contains("Unknown action"), "{}", res.output);
    }

    /// End-to-end lifecycle on a REAL detached process: start a long-running
    /// `sh` whose readiness is detected via `ready_log`, confirm status reports
    /// it running with a live pid, then stop it and confirm the pid is gone.
    /// Everything is scoped to a tempdir (no home pollution). A drop guard
    /// ensures the child is reaped even if an assertion fails mid-test.
    #[test]
    fn test_start_status_stop_real_process() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();

        let start = start_service(
            &dir,
            &serde_json::json!({
                "action": "start",
                "name": "smoke",
                "command": "sh",
                "args": ["-c", "echo SMOKE_READY; sleep 60"],
                "ready_log": "SMOKE_READY",
                "ready_timeout": 5,
            }),
            "smoke",
        );
        assert!(start.ok, "start failed: {}", start.output);
        let meta = start.meta.as_ref().expect("start meta");
        assert_eq!(meta["running"], true, "{}", start.output);
        assert_eq!(meta["ready"], true, "should be ready after log match");
        let pid = meta["pid"].as_u64().expect("pid present") as u32;
        assert!(pid > 0);

        // Drop guard: kill the child if any later assertion aborts the test.
        struct Reaper(u32);
        impl Drop for Reaper {
            fn drop(&mut self) {
                if pid_alive(self.0) {
                    send_signal(self.0, libc::SIGKILL);
                }
            }
        }
        let _reaper = Reaper(pid);

        assert!(pid_alive(pid), "detached child must be alive");

        // status reflects the persisted, running state.
        let st = status_service(&dir, "smoke");
        assert!(st.ok);
        assert_eq!(st.meta.unwrap()["running"], true);

        // A duplicate start must be idempotent (not spawn a second child).
        let dup = start_service(&dir, &serde_json::json!({"name": "smoke", "command": "sh"}), "smoke");
        assert!(dup.ok);
        assert_eq!(dup.meta.unwrap()["pid"], pid);
        assert!(pid_alive(pid), "still the same single process");

        // logs surface the readiness line the child emitted.
        let lg = logs_service(&dir, "smoke", 10);
        assert!(lg.ok);
        assert!(
            lg.meta.unwrap()["log"].as_str().unwrap().contains("SMOKE_READY"),
            "log should contain emitted line"
        );

        // stop terminates the child and clears pid from state.
        let stop = stop_service(&dir, "smoke");
        assert!(stop.ok, "{}", stop.output);
        assert_eq!(stop.meta.unwrap()["stopped"], true);
        assert!(!pid_alive(pid), "child must be gone after stop");

        // Final state has no pid.
        let after = load_state(&dir, "smoke").expect("state persisted");
        assert!(after.pid.is_none());
    }

}
