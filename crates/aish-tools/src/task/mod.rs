//! Background long-running task management.
//!
//! MVP model: a process-global registry (`TaskRegistry`) holds task state for
//! the lifetime of the aish process. Each task runs in a watcher thread that
//! owns a `setsid` child process; its stdout/stderr are streamed to per-task
//! log files under the aish temp dir so `task_output` can read incrementally
//! even after the AI turn that spawned it has ended.

pub mod tools;

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Hard cap on concurrent background tasks (LRU-style rejection beyond this).
pub const MAX_CONCURRENT_TASKS: usize = 16;

/// Default tail size returned by `task_output` when `max_bytes` is omitted.
pub const DEFAULT_READ_BYTES: usize = 4096;

/// Grace period between SIGTERM and SIGKILL in `task_stop`.
const STOP_GRACE: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Task state
// ---------------------------------------------------------------------------

/// Lifecycle state of a background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
            TaskStatus::Stopped => write!(f, "stopped"),
        }
    }
}

/// A tracked background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub task_id: String,
    pub command: String,
    pub status: TaskStatus,
    pub exit_code: Option<i32>,
    pub output_path: String,
    /// Byte offset up to which output has already been read via task_output.
    pub cursor: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RegistryInner {
    tasks: HashMap<String, TaskInfo>,
    /// Live child handles keyed by task_id (removed once reaped).
    children: HashMap<String, Arc<Mutex<Option<Child>>>>,
}

/// Process-global registry so task state outlives individual AI turns.
static REGISTRY: LazyLock<Arc<Mutex<RegistryInner>>> =
    LazyLock::new(|| Arc::new(Mutex::new(RegistryInner::default())));

fn registry() -> Arc<Mutex<RegistryInner>> {
    REGISTRY.clone()
}

static TASK_SEQ: AtomicU64 = AtomicU64::new(1);

fn new_task_id() -> String {
    let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("t{ts:x}{seq:04x}")
}

/// Directory holding per-task output logs.
fn task_log_dir() -> PathBuf {
    std::env::temp_dir().join("aish-bg-tasks")
}

fn task_log_path(task_id: &str) -> PathBuf {
    task_log_dir().join(format!("{task_id}.log"))
}

/// Spawn a command as a background task.
///
/// Returns the task id. The command runs in its own session (setsid) so a
/// whole process group can be signalled later. A watcher thread streams
/// stdout+stderr into the task log file and updates registry state on exit.
pub fn spawn_background(command: &str) -> Result<String, String> {
    let reg = registry();
    let mut reg = reg.lock();

    // Purge finished tasks beyond a sane history to bound memory.
    prune_finished(&mut reg.tasks, 64);

    if reg.children.len() >= MAX_CONCURRENT_TASKS {
        return Err(format!(
            "background task limit reached ({MAX_CONCURRENT_TASKS} running); stop one with task_stop first"
        ));
    }

    let task_id = new_task_id();
    let log_path = task_log_path(&task_id);
    fs::create_dir_all(task_log_dir())
        .map_err(|e| format!("failed to create task log dir: {e}"))?;
    let log_file = fs::File::create(&log_path)
        .map_err(|e| format!("failed to create task log file: {e}"))?;
    let log_file_err = log_file
        .try_clone()
        .map_err(|e| format!("failed to clone log file handle: {e}"))?;

    // pre_exec setsid: new session + process group led by the child pid.
    let child = Command::new("/bin/bash")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("failed to spawn background command: {e}"))?;

    let started_at_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let info = TaskInfo {
        task_id: task_id.clone(),
        command: command.to_string(),
        status: TaskStatus::Running,
        exit_code: None,
        output_path: log_path.to_string_lossy().to_string(),
        cursor: 0,
        started_at_ms,
        finished_at_ms: None,
    };
    reg.tasks.insert(task_id.clone(), info);
    let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
    reg.children.insert(task_id.clone(), child_slot.clone());
    drop(reg);

    // Watcher thread: reap the child and finalize registry state.
    let watch_id = task_id.clone();
    std::thread::spawn(move || {
        let exit_code: i32 = loop {
            let outcome = {
                let mut slot = child_slot.lock();
                match slot.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => {
                            use std::os::unix::process::ExitStatusExt;
                            let code = status
                                .code()
                                .or_else(|| status.signal().map(|s| 128 + s))
                                .unwrap_or(-1);
                            slot.take();
                            Some(code)
                        }
                        Ok(None) => None,
                        Err(_) => {
                            slot.take();
                            Some(-1)
                        }
                    },
                    // Slot already emptied (e.g. task_stop reaped it).
                    None => Some(-1),
                }
            };
            if let Some(code) = outcome {
                break code;
            }
            std::thread::sleep(Duration::from_millis(200));
        };

        let status = if exit_code == 0 {
            TaskStatus::Completed
        } else {
            TaskStatus::Failed
        };
        let finished = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        {
            let reg = registry();
            let mut reg = reg.lock();
            if let Some(info) = reg.tasks.get_mut(&watch_id) {
                info.status = status;
                info.exit_code = Some(exit_code);
                info.finished_at_ms = Some(finished);
            }
            reg.children.remove(&watch_id);
        }
    });

    Ok(task_id)
}

/// Keep at most `keep` finished tasks (drop oldest by started time).
fn prune_finished(tasks: &mut HashMap<String, TaskInfo>, keep: usize) {
    let mut finished: Vec<(String, u64)> = tasks
        .iter()
        .filter(|(_, t)| t.status != TaskStatus::Running)
        .map(|(id, t)| (id.clone(), t.started_at_ms))
        .collect();
    if finished.len() <= keep {
        return;
    }
    finished.sort_by_key(|(_, started)| *started);
    let excess = finished.len() - keep;
    for (id, _) in finished.into_iter().take(excess) {
        tasks.remove(&id);
    }
}

// ---------------------------------------------------------------------------
// Queries and control
// ---------------------------------------------------------------------------

/// Snapshot of all tasks (running first, then newest finished).
pub fn list_tasks() -> Vec<TaskInfo> {
    let reg = registry();
    let reg = reg.lock();
    let mut all: Vec<TaskInfo> = reg.tasks.values().cloned().collect();
    all.sort_by(|a, b| {
        let a_run = a.status == TaskStatus::Running;
        let b_run = b.status == TaskStatus::Running;
        b_run.cmp(&a_run).then(b.started_at_ms.cmp(&a.started_at_ms))
    });
    all
}

/// Read a task's output since `cursor`. Returns (text, new_cursor, status).
///
/// Reads at most `max_bytes` from the current cursor position so a chatty
/// task cannot blow up the tool result; the caller can poll again to catch up.
pub fn task_output(task_id: &str, max_bytes: usize) -> Result<(String, u64, TaskStatus), String> {
    let info = {
        let reg = registry();
        let reg = reg.lock();
        reg.tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| format!("unknown task_id: {task_id}"))?
    };

    let path = PathBuf::from(&info.output_path);
    let file = fs::File::open(&path).map_err(|e| format!("failed to open task log: {e}"))?;
    let mut handle = file;
    use std::io::Seek;
    handle
        .seek(std::io::SeekFrom::Start(info.cursor))
        .map_err(|e| format!("failed to seek task log: {e}"))?;

    let mut buf = vec![0u8; max_bytes];
    let n = handle.read(&mut buf).map_err(|e| format!("read failed: {e}"))?;
    buf.truncate(n);
    let (text_bytes, _truncated) = aish_pty::truncate_utf8_safe(&buf, n);
    let text = String::from_utf8_lossy(&text_bytes).to_string();
    let new_cursor = info.cursor + n as u64;

    // Persist cursor so subsequent calls are incremental.
    {
        let reg = registry();
        let mut reg = reg.lock();
        if let Some(t) = reg.tasks.get_mut(task_id) {
            t.cursor = new_cursor;
        }
    }

    let status = {
        let reg = registry();
        let reg = reg.lock();
        reg.tasks
            .get(task_id)
            .map(|t| t.status)
            .unwrap_or(TaskStatus::Failed)
    };
    Ok((text, new_cursor, status))
}

/// Stop a running task: SIGTERM the process group, then SIGKILL after grace.
pub fn task_stop(task_id: &str) -> Result<i32, String> {
    // Take the child handle out so the watcher stops polling it.
    let child_slot = {
        let reg = registry();
        let mut reg = reg.lock();
        reg.children.remove(task_id)
    }
    .ok_or_else(|| format!("task not running: {task_id}"))?;

    let mut guard = child_slot.lock();
    let Some(mut child) = guard.take() else {
        return Err(format!("task not running: {task_id}"));
    };
    let pid = child.id() as i32;

    // Terminate the whole process group (child is its own session leader).
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }

    let deadline = std::time::Instant::now() + STOP_GRACE;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                use std::os::unix::process::ExitStatusExt;
                break status
                    .code()
                    .or_else(|| status.signal().map(|s| 128 + s))
                    .unwrap_or(-1);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    match child.wait() {
                        Ok(status) => {
                            use std::os::unix::process::ExitStatusExt;
                            break status
                                .code()
                                .or_else(|| status.signal().map(|s| 128 + s))
                                .unwrap_or(-1);
                        }
                        Err(_) => break -1,
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break -1,
        }
    };

    // Mark stopped so task_list reflects reality even before the watcher
    // notices (watcher will also see the exit and try to finalize; harmless).
    {
        let reg = registry();
        let mut reg = reg.lock();
        if let Some(info) = reg.tasks.get_mut(task_id) {
            info.status = TaskStatus::Stopped;
            info.exit_code = Some(exit_code);
            info.finished_at_ms = Some(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            );
        }
    }
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_completes_and_collects_exit_code() {
        let id = spawn_background("echo bg-ok").unwrap();
        // Wait for completion (bounded).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let info = list_tasks().into_iter().find(|t| t.task_id == id).unwrap();
            if info.status != TaskStatus::Running {
                assert_eq!(info.status, TaskStatus::Completed);
                assert_eq!(info.exit_code, Some(0));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "task did not finish");
            std::thread::sleep(Duration::from_millis(50));
        }
        let (out, _, status) = task_output(&id, DEFAULT_READ_BYTES).unwrap();
        assert_eq!(status, TaskStatus::Completed);
        assert!(out.contains("bg-ok"), "output was: {out}");
    }

    #[test]
    fn failing_command_marks_failed() {
        let id = spawn_background("exit 7").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let info = list_tasks().into_iter().find(|t| t.task_id == id).unwrap();
            if info.status != TaskStatus::Running {
                assert_eq!(info.status, TaskStatus::Failed);
                assert_eq!(info.exit_code, Some(7));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn task_output_is_incremental() {
        let id = spawn_background("printf 'aaaa\\n'; sleep 0.4; printf 'bbbb\\n'").unwrap();
        // First read immediately: likely only 'aaaa' (or empty).
        let (out1, cursor1, _) = task_output(&id, DEFAULT_READ_BYTES).unwrap();
        // Wait for the rest.
        std::thread::sleep(Duration::from_millis(900));
        let (out2, _, status) = task_output(&id, DEFAULT_READ_BYTES).unwrap();
        assert_eq!(status, TaskStatus::Completed);
        // No duplication: 'bbbb' must appear exactly once across the two reads.
        let combined = format!("{out1}{out2}");
        assert_eq!(combined.matches("bbbb").count(), 1);
        assert!(cursor1 <= task_output(&id, DEFAULT_READ_BYTES).unwrap().1);
    }

    #[test]
    fn task_stop_terminates_running_task() {
        let id = spawn_background("sleep 30").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let code = task_stop(&id).unwrap();
        // SIGTERM'd bash reports 128+15.
        assert_eq!(code, 143);
        let info = list_tasks().into_iter().find(|t| t.task_id == id).unwrap();
        assert_eq!(info.status, TaskStatus::Stopped);
    }

    #[test]
    fn unknown_task_output_errors() {
        assert!(task_output("nope", 100).is_err());
    }

    #[test]
    fn unknown_task_stop_errors() {
        assert!(task_stop("nope").is_err());
    }

    #[test]
    fn prune_keeps_recent_finished() {
        let mut tasks = HashMap::new();
        for i in 0..80u64 {
            tasks.insert(
                format!("t{i}"),
                TaskInfo {
                    task_id: format!("t{i}"),
                    command: String::new(),
                    status: TaskStatus::Completed,
                    exit_code: Some(0),
                    output_path: String::new(),
                    cursor: 0,
                    started_at_ms: i,
                    finished_at_ms: Some(i),
                },
            );
        }
        prune_finished(&mut tasks, 64);
        assert_eq!(tasks.len(), 64);
        assert!(tasks.contains_key("t79"), "newest must survive");
        assert!(!tasks.contains_key("t0"), "oldest must be pruned");
    }
}
