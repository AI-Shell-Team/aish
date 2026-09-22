use std::future::Future;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aish_i18n;
use aish_llm::{CancellationToken, LlmSession, Tool, ToolResult};
use futures::future::FutureExt;

use super::prompt;

/// Maximum output length (matches main branch's 1000 chars).
const MAX_OUTPUT_CHARS: usize = 1000;

/// Wall-clock budget for one snippet. The prompt steers the model to short
/// snippets; this only guards against a runaway loop holding the whole turn.
const PYTHON_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the wait loop polls the child and the cancel token.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Tool for executing Python code.
pub struct PythonTool;

impl Default for PythonTool {
    fn default() -> Self {
        Self::new()
    }
}

impl PythonTool {
    pub fn new() -> Self {
        Self
    }
}

/// Outcome of the supervised child run.
struct ChildOutcome {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Process exit code, or None when killed by cancel/timeout.
    exit_code: Option<i32>,
    /// User cancelled the session while the child ran.
    cancelled: bool,
    /// The wall-clock budget elapsed while the child ran.
    timed_out: bool,
    /// How long the child ran before it was stopped or exited.
    elapsed: Duration,
}

impl Tool for PythonTool {
    fn name(&self) -> &str {
        "python_exec"
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
        run_python(&args, None)
    }

    /// Runs the blocking child wait on a dedicated thread so a stuck python
    /// process cannot freeze the async runtime, and kills the child's process
    /// group when the session is cancelled (issue #547).
    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let token = session.cancellation_token_arc();
        async move {
            let res = tokio::task::spawn_blocking(move || run_python(&args, Some(&token))).await;
            match res {
                Ok(r) => r,
                Err(e) => ToolResult::error(format!("Error: python task failed: {e}")),
            }
        }
        .boxed()
    }
}

fn run_python(args: &serde_json::Value, cancel: Option<&CancellationToken>) -> ToolResult {
    let code = match args.get("code").and_then(|c| c.as_str()) {
        Some(c) => c,
        None => return ToolResult::error(aish_i18n::t("tools.python.missing_code")),
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));

    // Spawn python3 with cwd set via Command::current_dir so non-UTF-8
    // paths are handled correctly (no lossy string conversion needed).
    let indented_code = indent_each_line(code, " ");
    let wrapper = format!(
        "import sys\ntry:\n{indented_code}\nexcept Exception as e:\n print(f'Error: {{e}}',file=sys.stderr)\n sys.exit(1)",
    );

    let mut cmd = Command::new("python3");
    cmd.current_dir(&cwd)
        .arg("-c")
        .arg(&wrapper)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group: killpg() can then reap the whole tree (imports that
    // spawn helpers) without touching aish or unrelated processes.
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ToolResult::error(aish_i18n::t("tools.python.not_installed"))
        }
        Err(e) => {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("error".to_string(), e.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.python.execute_failed",
                &args_map,
            ));
        }
    };

    let outcome = wait_with_cancel(&mut child, cancel);
    finish_result(child, outcome)
}

/// Poll the child every [`WAIT_POLL_INTERVAL`], forwarding stdout/stderr so
/// pipes never fill up, until it exits, the cancel token fires, or the
/// [`PYTHON_TIMEOUT`] budget elapses. Always reaps the child.
fn wait_with_cancel(child: &mut Child, cancel: Option<&CancellationToken>) -> ChildOutcome {
    let started = Instant::now();

    // Reader threads keep the pipes drained; the wait loop owns the deadline.
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_jh = std::thread::spawn(move || {
        if let Some(p) = stdout_pipe.as_mut() {
            drain_to(p, &out_tx);
        }
    });
    let err_jh = std::thread::spawn(move || {
        if let Some(p) = stderr_pipe.as_mut() {
            drain_to(p, &err_tx);
        }
    });
    let mut cancelled = false;
    let mut timed_out = false;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                let cancelled_now = cancel.is_some_and(|t| t.is_cancelled());
                let expired = started.elapsed() >= PYTHON_TIMEOUT;
                if cancelled_now || expired {
                    kill_process_group(child.id());
                    cancelled = cancelled_now;
                    timed_out = expired && !cancelled_now;
                    // Give the group a moment to die, then force-kill.
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(WAIT_POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };
    out_jh.join().ok();
    err_jh.join().ok();

    let stdout: Vec<u8> = out_rx.into_iter().flatten().collect();
    let stderr: Vec<u8> = err_rx.into_iter().flatten().collect();

    ChildOutcome {
        stdout,
        stderr,
        exit_code: exit_status.and_then(|s| s.code()),
        cancelled,
        timed_out,
        elapsed: started.elapsed(),
    }
}

/// Drain one pipe in small chunks so a chatty child cannot fill the 64 KiB
/// pipe buffer and deadlock while we wait.
fn drain_to(reader: &mut dyn Read, tx: &mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// SIGTERM the child's process group, escalating to SIGKILL after a grace
/// period. The child was spawned with `process_group(0)`, so `-pid` targets
/// exactly its tree — never aish itself.
fn kill_process_group(pid: u32) {
    let pgid = -(pid as i32);
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(50));
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
}

/// Map the child outcome to the tool result, mirroring the previous output
/// shape and adding explicit cancelled/timed_out wording (issue #547).
fn finish_result(_child: Child, outcome: ChildOutcome) -> ToolResult {
    let stdout = String::from_utf8_lossy(&outcome.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&outcome.stderr).into_owned();

    // Truncate stdout like the main branch (1000 chars).
    let stdout = if let Some((end, _)) = stdout.char_indices().nth(MAX_OUTPUT_CHARS) {
        let mut s = stdout[..end].to_string();
        s.push_str("\n[Output truncated due to length]");
        s
    } else {
        stdout
    };

    if outcome.cancelled {
        // Success with a note, like glob: an error would make the agent
        // retry, and the retry gets cancelled again.
        let mut text = if stdout.is_empty() {
            aish_i18n::t("tools.python.no_output")
        } else {
            stdout.clone()
        };
        text.push_str(&format!(
            "\n(cancelled after {:.1}s)",
            outcome.elapsed.as_secs_f64()
        ));
        return ToolResult::success(text);
    }
    if outcome.timed_out {
        let mut text = if stdout.is_empty() {
            aish_i18n::t("tools.python.no_output")
        } else {
            stdout.clone()
        };
        if !stderr.is_empty() {
            text.push('\n');
            text.push_str(&stderr);
        }
        text.push_str(&format!(
            "\n(timed out after {:.0}s, process terminated)",
            PYTHON_TIMEOUT.as_secs()
        ));
        return ToolResult {
            ok: false,
            output: text,
            meta: Some(serde_json::json!({"timed_out": true})),
        };
    }

    let exit_code = outcome.exit_code.unwrap_or(-1);
    if exit_code == 0 && stdout.is_empty() {
        return ToolResult::success(aish_i18n::t("tools.python.no_output"));
    }
    if exit_code == 0 {
        return ToolResult::success(stdout);
    }

    let mut result_text = stdout;
    if !stderr.is_empty() {
        if !result_text.is_empty() {
            result_text.push('\n');
        }
        result_text.push_str(&stderr);
    }
    ToolResult {
        ok: false,
        output: result_text,
        meta: Some(serde_json::json!({"exit_code": exit_code})),
    }
}

/// Indent each line for embedding inside a try block (1 space, matching the
/// single-space indentation used in the wrapper format string).
fn indent_each_line(code: &str, indent: &str) -> String {
    let mut out = String::with_capacity(code.len());
    for line in code.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(indent);
            out.push_str(line);
            out.push('\n');
        }
    }
    // Strip the trailing newline added above to match the previous format!
    // behavior, which joined lines with \n without a trailing one.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_code_errors() {
        let r = run_python(&json!({}), None);
        assert!(!r.ok);
    }

    #[test]
    fn quick_snippet_returns_output() {
        let r = run_python(&json!({"code": "print(1+1)"}), None);
        assert!(r.ok, "out: {}", r.output);
        assert_eq!(r.output.trim(), "2");
    }

    #[test]
    fn cancel_kills_child_and_descendants_quickly() {
        // Issue #547 regression, strengthened per #551 review: the snippet
        // forks a descendant that STAYS in the child's process group (no
        // setsid — a detached daemon would be outside killpg's reach by
        // POSIX contract). The liveness assert below would fail if the
        // implementation killed only the direct child instead of the
        // whole group.
        let marker = format!("aish_descendant_{}", std::process::id());
        let code = format!(
            "import os, time, sys\n\
             pid = os.fork()\n\
             if pid == 0:\n\
             \x20   with open('/tmp/{marker}', 'w') as f:\n\
             \x20       f.write(str(os.getpid()))\n\
             \x20   time.sleep(60)\n\
             \x20   os._exit(0)\n\
             time.sleep(60)\n\
             print('done')"
        );
        let _ = std::fs::remove_file(format!("/tmp/{marker}"));

        let token = std::sync::Arc::new(CancellationToken::new());
        let marker_path = std::path::PathBuf::from(format!("/tmp/{marker}"));

        let jh = {
            let token = std::sync::Arc::clone(&token);
            let marker_path = marker_path.clone();
            std::thread::spawn(move || {
                // Bound the race: cancel only after the descendant has
                // actually written its PID marker, otherwise the liveness
                // check below would vacuously pass on a missing marker
                // (issue #551 review round 2). Fail fast if the snippet
                // never even forked.
                let deadline = Instant::now() + Duration::from_secs(10);
                while !marker_path.exists() {
                    assert!(
                        Instant::now() < deadline,
                        "descendant never wrote its PID marker"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::thread::sleep(Duration::from_millis(300));
                token.cancel();
            })
        };
        let started = Instant::now();
        let r = run_python(&json!({"code": code}), Some(&token));
        jh.join().unwrap();
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:.1?}");
        assert!(r.ok);
        assert!(r.output.contains("cancelled"), "out: {}", r.output);
        // The group member forked by the child must be gone too. Pure Rust
        // liveness probe (no shell): `kill -0 <pid>` succeeds only while
        // the process still exists.
        std::thread::sleep(Duration::from_millis(200));
        let pid_str = std::fs::read_to_string(&marker_path).unwrap_or_default();
        let descendant_alive = !pid_str.trim().is_empty()
            && std::process::Command::new("kill")
                .arg("-0")
                .arg(pid_str.trim())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        let _ = std::fs::remove_file(&marker_path);
        assert!(
            !descendant_alive,
            "group-member descendant survived cancellation — killpg must reap the whole group"
        );
    }

    #[test]
    fn long_snippet_times_out() {
        // 1s budget override is not exposed; use the real constant via a
        // short snippet that outlives nothing — instead verify the timeout
        // path indirectly through a busy loop that would exceed 120s only
        // if the budget did not exist. To keep tests fast, this check runs
        // the child against a tiny manually driven wait: spawn + immediate
        // timeout is not reachable without injecting the deadline, so this
        // test asserts the timed_out branch formatting via the helper.
        let outcome = ChildOutcome {
            stdout: b"partial".to_vec(),
            stderr: Vec::new(),
            exit_code: None,
            cancelled: false,
            timed_out: true,
            elapsed: Duration::from_secs(120),
        };
        let r = finish_result(unused_child(), outcome);
        assert!(!r.ok);
        assert!(r.output.contains("timed out"), "out: {}", r.output);
    }

    /// A finished `sleep 0` child for finish_result tests.
    fn unused_child() -> Child {
        Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn true")
    }
}
