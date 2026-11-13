//! C/S remote extensions: SSH-backed remote session access, policy sync,
//! and fleet diagnostics (issues #586, #587, #588).
//!
//! Design constraints:
//! - No resident central server: every remote operation runs `ssh <host> aish
//!   <hidden-subcommand>` on demand; the remote aish binary is located via
//!   `AISH_REMOTE_BIN` (env override) or by probing common install paths.
//! - Local behavior is untouched: all new code paths live behind explicit
//!   subcommands; the single-machine shell/daemon flow never calls into here.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Environment variable overriding the remote aish binary path.
const REMOTE_BIN_ENV: &str = "AISH_REMOTE_BIN";

/// Candidate paths probed on the remote host when `AISH_REMOTE_BIN` is unset.
const REMOTE_BIN_CANDIDATES: &[&str] = &["/usr/local/bin/aish", "/usr/bin/aish"];

/// POSIX-shell quote a single argument (single-quote wrapping, with the
/// classic embedded-quote dance). Keeps arguments with spaces, quotes or
/// `$` expansions intact across the ssh boundary.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_alphanumeric() || "-_./=:,@+".contains(c))
    {
        return arg.to_string(); // safe to pass through bare
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// A live ssh child process whose stdin/stdout carry the frame stream.
pub struct SshChannel {
    child: Child,
}

impl SshChannel {
    /// Spawn `ssh <host> -- <remote aish invocation>` with piped stdio.
    /// stderr is inherited so ssh errors stay visible.
    pub fn spawn(host: &str, remote_args: &[String]) -> Result<Self, String> {
        let mut cmd = Command::new("ssh");
        // BatchMode: never prompt for passwords (fail fast instead); the
        // remote command runs without a TTY so its stdio is a clean frame
        // channel. ServerAliveInterval detects dead peers quickly.
        cmd.arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ServerAliveInterval=10")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("--")
            .arg(host)
            // ssh joins remote_args with spaces into one remote shell string,
            // so any argument containing spaces must be shell-quoted here.
            .args(remote_args.iter().map(|a| shell_quote(a)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let child = cmd.spawn().map_err(|e| format!("ssh spawn failed: {e}"))?;
        Ok(Self { child })
    }

    /// Split the channel into buffered stdout reader + stdin handle for
    /// concurrent frame read/write.
    pub fn split(
        &mut self,
    ) -> Result<
        (
            BufReader<std::process::ChildStdout>,
            std::process::ChildStdin,
        ),
        String,
    > {
        let stdout = self
            .child
            .stdout
            .take()
            .ok_or_else(|| "ssh stdout not piped".to_string())?;
        let stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| "ssh stdin not piped".to_string())?;
        Ok((BufReader::new(stdout), stdin))
    }

    /// Wait for the ssh process to exit, returning its status code.
    pub fn wait(&mut self) -> Result<Option<i32>, String> {
        match self.child.wait() {
            Ok(status) => Ok(status.code()),
            Err(e) => Err(format!("ssh wait failed: {e}")),
        }
    }

    /// Kill the ssh child process (used on timeout).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

/// Run a remote aish subcommand and collect its stdout lines as JSON values.
/// Used by list-style remote queries (sessions, events, diagnose results).
/// When `timeout` is set, the ssh child is killed if it outlives the budget.
/// Map the first `--remote-*` flag in remote args to its canonical action
/// name (must stay in sync with `agent_action_name`).
pub fn agent_action_from_args(remote_args: &[String]) -> &'static str {
    for a in remote_args {
        match a.as_str() {
            "--remote-sessions" => return "sessions.list",
            "--remote-attach" => return "session.attach",
            "--remote-policy-show" => return "policy.show",
            "--remote-policy-install" | "--remote-policy-install-stdin" => return "policy.install",
            "--remote-session-rename" => return "session.rename",
            "--remote-session-kill" => return "session.kill",
            "--remote-session-replay" => return "session.replay",
            "--remote-audit-events" => return "audit.events",
            "--remote-diagnose" => return "diagnose.run",
            _ => {}
        }
    }
    "unknown"
}

pub(crate) fn ssh_json_lines(
    host: &str,
    remote_args: &[String],
    timeout: Option<Duration>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut ch = SshChannel::spawn(host, remote_args)?;
    let (reader, mut stdin) = ch.split()?;
    // Operator identity: when the target enforces trust, the first stdin
    // line is the signed challenge response. Harmless in open mode.
    let action = agent_action_from_args(remote_args);
    if action != "unknown" {
        match crate::remote_identity::identity_request_json(action) {
            Ok(identity) => {
                use std::io::Write;
                if let Err(e) = writeln!(stdin, "{identity}") {
                    eprintln!("[identity] write to ssh stdin failed: {e}");
                }
                let _ = stdin.flush();
            }
            Err(e) => eprintln!("[identity] cannot sign challenge: {e}"),
        }
    }
    drop(stdin); // signal EOF so the remote command finishes

    // Parse lines on a detached thread that owns the reader; the parent
    // polls with a deadline and kills the ssh child on timeout, which
    // unblocks the blocking read. The thread must not be joined on the
    // timeout path: the remote side may keep stdout open past the kill.
    let host_owned = host.to_string();
    let deadline = timeout.map(|d| Instant::now() + d);
    let mut timed_out = false;
    let values = {
        let worker = std::thread::spawn(move || -> Vec<serde_json::Value> {
            let mut values = Vec::new();
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(v) => values.push(v),
                    // Non-JSON lines (banner, warnings) are surfaced verbatim.
                    Err(_) => eprintln!("[remote {}] {}", host_owned, line),
                }
            }
            values
        });
        loop {
            if worker.is_finished() {
                break worker.join().unwrap_or_default();
            }
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    timed_out = true;
                    break Vec::new();
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    if timed_out {
        ch.kill();
        return Err(format!(
            "ssh timed out after {:?}",
            timeout.unwrap_or_default()
        ));
    }
    ch.wait()?;
    Ok(values)
}

/// Probe that the remote host has a usable aish binary. Returns the resolved
/// path on success. `AISH_REMOTE_BIN` (if set) wins without probing.
pub(crate) fn probe_remote_bin(host: &str) -> Result<String, String> {
    if let Ok(path) = std::env::var(REMOTE_BIN_ENV) {
        if !path.is_empty() {
            return Ok(path);
        }
    }
    // Try each candidate path; the first that exists and is executable wins.
    for candidate in REMOTE_BIN_CANDIDATES {
        let output = Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=5")
            .arg("--")
            .arg(host)
            .arg("test")
            .arg("-x")
            .arg(candidate)
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                return Ok(candidate.to_string());
            }
        }
    }
    Err(format!(
        "no usable aish binary found on {host} (checked {}).\n\
         Fix: install aish on {host} into one of those paths, or set\n\
         {REMOTE_BIN_ENV}=<remote path>, or add the host with\n\
         'aish remote host add <alias> {host} --remote-bin <path>'",
        REMOTE_BIN_CANDIDATES.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// Coordinator-side command implementations
// ---------------------------------------------------------------------------

/// `aish remote session list --host <h>`: list remote sessions with a
/// four-state model and the current controller (#586). The remote agent
/// reports both daemon liveness and whether the shell is still running.
pub fn cmd_session_list(host: &str, json: bool) -> Result<(), String> {
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-sessions".to_string()];
    let sessions = ssh_json_lines(host, &args, None)?;

    if json {
        for s in &sessions {
            println!("{}", serde_json::to_string(s).unwrap_or_default());
        }
        return Ok(());
    }
    if sessions.is_empty() {
        println!("No sessions on {host}.");
        println!("Tip: start one with 'aish' on {host} (or attach here to create one).");
        return Ok(());
    }
    println!("Sessions on {host}:");
    println!(
        "{:<22} {:<10} {:<12} {:<28} NAME",
        "SESSION", "STATE", "CONTROLLER", "CWD"
    );
    for s in &sessions {
        let id = s.get("session_id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let cwd = s.get("cwd").and_then(|v| v.as_str()).unwrap_or("?");
        let daemon_alive = s.get("alive").and_then(|v| v.as_bool()).unwrap_or(false);
        let shell_running = s.get("shell_running").and_then(|v| v.as_bool());
        // Four-state model:
        //   attached  — daemon alive, shell alive (usable right now)
        //   exited    — daemon alive but the shell exited (only cleanup is
        //               possible; the session cannot be resumed)
        //   unreachable — daemon socket did not answer (host down or daemon
        //               died; session state is unknown, not necessarily dead)
        //   detached  — daemon alive, nobody attached (reserved; the agent
        //               currently reports alive regardless of attach state)
        let state = if !daemon_alive {
            "unreachable"
        } else if shell_running == Some(false) {
            "exited"
        } else {
            "attached"
        };
        let display_id = if id.len() >= 8 { &id[..8] } else { id };
        let controller = s
            .get("controller")
            .and_then(|v| v.as_str())
            .map(|c| c.to_string())
            .unwrap_or_else(|| {
                if s.get("controller_stale").and_then(|v| v.as_bool()) == Some(true) {
                    "(lost)".to_string()
                } else {
                    "—".to_string()
                }
            });
        println!(
            "{:<22} {:<10} {:<12} {:<28} {}",
            display_id, state, controller, cwd, name
        );
    }
    Ok(())
}

/// Resolve a session selector (full UUID, unique name, or unique prefix)
/// against the remote session list. Returns the full UUID.
pub fn resolve_session(host: &str, selector: &str) -> Result<String, String> {
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-sessions".to_string()];
    let sessions = ssh_json_lines(host, &args, None)?;
    if selector.is_empty() {
        return Err("no session specified.\nTip: run 'aish remote session list --host ...' to see what is available".to_string());
    }
    // Exact id, exact name, then unique prefix.
    let exact_id = sessions
        .iter()
        .find(|s| s.get("session_id").and_then(|v| v.as_str()) == Some(selector));
    if let Some(s) = exact_id {
        return Ok(s
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(selector)
            .to_string());
    }
    let exact_name = sessions
        .iter()
        .find(|s| s.get("name").and_then(|v| v.as_str()) == Some(selector));
    if let Some(s) = exact_name {
        return Ok(s
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(selector)
            .to_string());
    }
    let prefix: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|s| {
            s.get("session_id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with(selector))
                .unwrap_or(false)
        })
        .collect();
    match prefix.len() {
        1 => Ok(prefix[0].get("session_id").and_then(|v| v.as_str()).unwrap_or(selector).to_string()),
        0 => Err(format!(
            "no session matches '{selector}' on {host}.\nTip: run 'aish remote session list --host {host}' to see available sessions"
        )),
        _ => {
            let ids: Vec<&str> = prefix
                .iter()
                .filter_map(|s| s.get("session_id").and_then(|v| v.as_str()))
                .collect();
            Err(format!(
                "'{selector}' is ambiguous, matches {} sessions: {}.\nTip: use a longer prefix",
                prefix.len(),
                ids.join(", ")
            ))
        }
    }
}

/// `aish remote session attach --host <h> [session]` (#586). Without a
/// session selector, list the remote sessions and prompt for a number.
pub fn cmd_session_attach(host: &str, session: &str) -> Result<(), String> {
    let session_id = if session.is_empty() {
        pick_remote_session(host)?
    } else {
        resolve_session(host, session)?
    };
    crate::attach_remote::run(host, &session_id)
}

/// Numbered picker over the remote sessions (works over plain ssh, no TUI
/// required — honest about the remote context, unlike reusing the local TUI).
fn pick_remote_session(host: &str) -> Result<String, String> {
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-sessions".to_string()];
    let sessions = ssh_json_lines(host, &args, None)?;
    if sessions.is_empty() {
        return Err(format!(
            "no sessions on {host}.\nTip: start one with 'aish' on {host}, or use 'aish run --continue' locally"
        ));
    }
    println!("Sessions on {host}:");
    for (idx, s) in sessions.iter().enumerate() {
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let cwd = s.get("cwd").and_then(|v| v.as_str()).unwrap_or("?");
        let id = s.get("session_id").and_then(|v| v.as_str()).unwrap_or("?");
        let short = if id.len() >= 8 { &id[..8] } else { id };
        println!("  [{}] {}  {cwd}  {name}", idx + 1, short);
    }
    print!("attach to session [1-{}]: ", sessions.len());
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let choice: usize = line.trim().parse().map_err(|_| {
        format!(
            "invalid selection '{}'.\nTip: enter a number between 1 and {}",
            line.trim(),
            sessions.len()
        )
    })?;
    if choice == 0 || choice > sessions.len() {
        return Err(format!(
            "selection {choice} out of range (1-{})",
            sessions.len()
        ));
    }
    Ok(sessions[choice - 1]
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// `aish remote session rename --host <h> <session> --to <name>`.
pub fn cmd_session_rename(host: &str, session: &str, to: &str) -> Result<(), String> {
    let session_id = resolve_session(host, session)?;
    let bin = probe_remote_bin(host)?;
    // Combined single arg: ssh joins remote argv with spaces, which would
    // split a name containing spaces into separate clap subcommands.
    let combined = format!("{session_id} {to}");
    let args = vec![bin, "--remote-session-rename".to_string(), combined];
    ssh_json_lines(host, &args, None)?;
    println!("{host}: renamed {session_id} -> {to}");
    Ok(())
}

/// `aish remote session kill --host <h> <session>`.
pub fn cmd_session_kill(host: &str, session: &str) -> Result<(), String> {
    let session_id = resolve_session(host, session)?;
    let bin = probe_remote_bin(host)?;
    let args = vec![
        bin,
        "--remote-session-kill".to_string(),
        session_id.to_string(),
    ];
    ssh_json_lines(host, &args, None)?;
    println!("{host}: killed session {session_id}");
    Ok(())
}

/// `aish remote session replay`: stream the session's recorded output over
/// ssh. Replay bytes go to stdout verbatim; stderr carries ssh errors.
pub fn cmd_session_replay(host: &str, session: &str, tail: Option<usize>) -> Result<(), String> {
    let session_id = resolve_session(host, session)?;
    let bin = probe_remote_bin(host)?;
    let mut args = vec![
        bin,
        "--remote-session-replay".to_string(),
        session_id.to_string(),
    ];
    if let Some(n) = tail {
        args.push("--replay-tail".to_string());
        args.push(n.to_string());
    }
    let mut child = std::process::Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("--")
        .arg(host)
        .args(args.iter().map(|a| shell_quote(a)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh spawn failed: {e}"))?;
    // Operator identity: the first stdin line satisfies the target's auth
    // gate (no-op in open mode). Then EOF so the agent streams the replay.
    {
        let mut stdin = child.stdin.take().ok_or("ssh stdin not piped")?;
        if let Ok(identity) = crate::remote_identity::identity_request_json("session.replay") {
            use std::io::Write;
            let _ = writeln!(stdin, "{identity}");
            let _ = stdin.flush();
        }
    } // stdin dropped → EOF
    let output = child
        .wait_with_output()
        .map_err(|e| format!("ssh wait failed: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        let reason = if err.trim().is_empty() {
            out.trim()
        } else {
            err.trim()
        };
        return Err(format!("replay failed: {reason}"));
    }
    use std::io::Write;
    std::io::stdout()
        .write_all(&output.stdout)
        .map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

/// `aish remote policy push`: build the envelope locally (version
/// auto-increment per host), optionally preview the diff, then send it over
/// stdin (ssh concatenates command args, which would split an envelope
/// containing spaces) (#587).
pub fn cmd_policy_push(
    host: &str,
    policy_file: &str,
    targets: &str,
    version_override: Option<u64>,
    show_diff: bool,
    yes: bool,
) -> Result<(), String> {
    let content = std::fs::read_to_string(policy_file)
        .map_err(|e| format!("cannot read policy file {policy_file}: {e}"))?;
    // Validate locally BEFORE any ssh traffic so bad YAML fails fast.
    // Guard: reject envelope files passed as policy (an envelope is a JSON
    // object with version/signature/policy keys — and JSON is valid YAML,
    // so parse validation alone would accept it).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
        if v.is_object() && (v.get("signature").is_some() || v.get("policy").is_some()) {
            return Err(format!(
                "{policy_file} looks like a policy ENVELOPE (has version/signature/policy keys), not a policy file.\nTip: pass the policy YAML itself; the envelope is built by this command"
            ));
        }
    }
    if let Err(e) = aish_security::policy_from_yaml_str(&content) {
        return Err(format!("invalid policy YAML: {e}.\nTip: check the file syntax, or run 'aish remote policy show --host {host}' to see the installed policy"));
    }

    // Auto version: max(local counter, remote installed version) + 1, so a
    // fresh local checkout never regresses against what the remote has.
    let version = match version_override {
        Some(v) => v,
        None => {
            let remote_v = remote_policy_installed(host)
                .ok()
                .flatten()
                .map(|(v, _)| v)
                .unwrap_or(0);
            next_policy_version(host).max(remote_v + 1)
        }
    };
    let targets_list: Vec<String> = targets
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let mut envelope = serde_json::json!({
        "version": version,
        "issuer": whoami_local(),
        "targets": targets_list,
        "policy": content,
    });
    // Sign when an admin keypair exists; otherwise push unsigned (target
    // hosts configured with signers will reject it).
    if let Ok(signature) = crate::policy_signing::sign_envelope(version, &targets_list, &content) {
        envelope["signature"] = serde_json::Value::String(signature);
    }
    let envelope_str = serde_json::to_string(&envelope).unwrap_or_default();

    // Optional diff against the remote's installed policy.
    if show_diff {
        match remote_policy_installed(host)? {
            Some((installed_version, installed_yaml)) => {
                println!("--- remote policy (v{installed_version})");
                println!("+++ local  policy (v{version})");
                print_unified_diff(&installed_yaml, &content);
            }
            None => println!(
                "remote has no installed remote policy yet; pushing v{version} will create it"
            ),
        }
        if !yes {
            print!("push this policy to {host}? [y/N] ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            let answer = line.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                println!("aborted, nothing pushed");
                return Ok(());
            }
        }
    }

    // Envelope travels on stdin, preceded by the operator identity line
    // (consumed by the target's auth gate when enforcement is active).
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-policy-install-stdin".to_string()];
    let mut ch = SshChannel::spawn(host, &args)?;
    let (mut reader, mut stdin) = ch.split()?;
    use std::io::Write as _;
    if let Ok(identity) = crate::remote_identity::identity_request_json("policy.install") {
        let _ = writeln!(stdin, "{identity}");
    }
    stdin
        .write_all(envelope_str.as_bytes())
        .map_err(|e| format!("cannot send envelope: {e}"))?;
    drop(stdin); // EOF: remote parses the envelope

    let mut reply_line = String::new();
    reader
        .read_line(&mut reply_line)
        .map_err(|e| format!("ssh read failed: {e}"))?;
    let _ = ch.wait();
    let reply: serde_json::Value = serde_json::from_str(reply_line.trim())
        .map_err(|e| format!("malformed remote reply: {e}"))?;
    let accepted = reply
        .get("accepted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let version = reply.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    let reason = reply.get("reason").and_then(|v| v.as_str()).unwrap_or("");
    if accepted {
        bump_local_version(host, version)?;
        println!("{host}: policy v{version} accepted and effective");
        Ok(())
    } else {
        println!("{host}: policy v{version} REJECTED: {reason}");
        Err(format!(
            "{host}: policy rejected. Tip: 'aish remote policy show --host {host}' shows the installed policy"
        ))
    }
}

/// Local per-host version counter: <config dir>/aish/remote-sync/<host>.ver
fn version_file(host: &str) -> Result<std::path::PathBuf, String> {
    let base = dirs::config_dir().ok_or("cannot resolve config dir")?;
    let dir = base.join("aish").join("remote-sync");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir.join(format!("{safe}.ver")))
}

fn next_policy_version(host: &str) -> u64 {
    version_file(host)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
        + 1
}

fn bump_local_version(host: &str, v: u64) -> Result<(), String> {
    let path = version_file(host)?;
    // Never regress the local counter.
    let current = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if v > current {
        std::fs::write(&path, v.to_string()).map_err(|e| format!("cannot write version: {e}"))?;
    }
    Ok(())
}

fn whoami_local() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Fetch the remote's installed policy (version + YAML) for diffing.
fn remote_policy_installed(host: &str) -> Result<Option<(u64, String)>, String> {
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-policy-show".to_string()];
    let lines = ssh_json_lines(host, &args, None)?;
    match lines.into_iter().next() {
        Some(v) => {
            let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
            let yaml = v
                .get("policy")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if yaml.is_empty() {
                Ok(None)
            } else {
                Ok(Some((version, yaml)))
            }
        }
        None => Ok(None),
    }
}

/// Minimal line-based diff for the confirm-before-push preview.
fn print_unified_diff(old: &str, new: &str) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut oi = 0usize;
    let mut ni = 0usize;
    while oi < old_lines.len() && ni < new_lines.len() {
        if old_lines[oi] == new_lines[ni] {
            oi += 1;
            ni += 1;
        } else {
            removed.push(old_lines[oi]);
            added.push(new_lines[ni]);
            oi += 1;
            ni += 1;
        }
    }
    while oi < old_lines.len() {
        removed.push(old_lines[oi]);
        oi += 1;
    }
    while ni < new_lines.len() {
        added.push(new_lines[ni]);
        ni += 1;
    }
    for l in &removed {
        println!("-{l}");
    }
    for l in &added {
        println!("+{l}");
    }
    if removed.is_empty() && added.is_empty() {
        println!("(no textual difference)");
    }
}

/// `aish remote policy show`: what is currently effective on the remote.
pub fn cmd_policy_show(host: &str) -> Result<(), String> {
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-policy-show".to_string()];
    let lines = ssh_json_lines(host, &args, None)?;
    let Some(v) = lines.into_iter().next() else {
        println!("{host}: no remote-installed policy (the local policy applies)");
        return Ok(());
    };
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
    let issuer = v.get("issuer").and_then(|x| x.as_str()).unwrap_or("—");
    let installed = v
        .get("installed_at")
        .and_then(|x| x.as_str())
        .unwrap_or("—");
    let rules = v.get("rules").and_then(|x| x.as_u64()).unwrap_or(0);
    let risk = v
        .get("default_risk_level")
        .and_then(|x| x.as_str())
        .unwrap_or("—");
    println!("{host}: policy v{version}");
    println!("  issuer:       {issuer}");
    println!("  installed:    {installed}");
    println!("  rules:        {rules}");
    println!("  default_risk: {risk}");
    Ok(())
}

/// `aish remote policy push --host group:<tag>`: push to every host carrying
/// the tag. Each host gets its own envelope version (the per-host counter
/// handles that). Stops at the first hard error; per-host rejections are
/// reported and counted. `yes` skips the diff confirmation per host.
pub fn cmd_policy_push_group(
    tag: &str,
    policy_file: &str,
    targets: &str,
    yes: bool,
) -> Result<(), String> {
    let hosts = crate::remote_hosts::expand_group(tag)?;
    println!(
        "pushing {policy_file} to {} host(s) in group '{tag}'",
        hosts.len()
    );
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for (target, remote_bin) in &hosts {
        if let Some(bin) = remote_bin {
            std::env::set_var("AISH_REMOTE_BIN", bin);
        }
        // Per-host push without the interactive diff (group mode is already
        // an explicit bulk action; failures are collected, not fatal).
        match cmd_policy_push(target, policy_file, targets, None, false, yes) {
            Ok(()) => ok.push(target.clone()),
            Err(e) => failed.push(format!("{target}: {e}")),
        }
    }
    println!(
        "=== group '{tag}' push: {} ok, {} failed",
        ok.len(),
        failed.len()
    );
    for f in &failed {
        println!("  FAILED {f}");
    }
    if ok.len() < hosts.len() {
        Err(format!(
            "{} of {} hosts failed. Tip: 'remote policy status --host group:{tag}' shows per-host state",
            failed.len(),
            hosts.len()
        ))
    } else {
        Ok(())
    }
}

/// `aish remote policy status`: drift detection across hosts. Compares each
/// host's installed policy against the local expectation (per-host version
/// counter). States: `current` (matches expectation), `drifted` (remote is
/// NEWER than expected — changed elsewhere), `stale` (remote is OLDER —
/// missed an update), `none` (no remote policy installed), `unreachable`.
pub fn cmd_policy_status(targets: &[(String, Option<String>)], json: bool) -> Result<(), String> {
    let mut results = Vec::new();
    for (target, remote_bin) in targets {
        if let Some(bin) = remote_bin {
            std::env::set_var("AISH_REMOTE_BIN", bin);
        }
        let expected = next_policy_version(target).saturating_sub(1);
        let state = match remote_policy_installed(target) {
            Err(_) => "unreachable".to_string(),
            Ok(None) => {
                if expected == 0 {
                    "none".to_string()
                } else {
                    "stale".to_string()
                }
            }
            Ok(Some((installed, _))) => {
                if installed >= expected && expected > 0 {
                    "current".to_string()
                } else if installed > expected {
                    "drifted".to_string()
                } else {
                    "stale".to_string()
                }
            }
        };
        results.push(serde_json::json!({
            "host": target,
            "expected_version": expected,
            "state": state,
        }));
    }
    if json {
        for r in &results {
            println!("{}", r);
        }
        return Ok(());
    }
    println!("{:<24} {:<10} EXPECTED", "HOST", "STATE");
    for r in &results {
        println!(
            "{:<24} {:<10} v{}",
            r["host"].as_str().unwrap_or(""),
            r["state"].as_str().unwrap_or(""),
            r["expected_version"].as_u64().unwrap_or(0)
        );
    }
    let drifted = results.iter().filter(|r| r["state"] == "drifted").count();
    let stale = results.iter().filter(|r| r["state"] == "stale").count();
    if drifted + stale > 0 {
        println!(
            "\n{drifted} drifted (changed outside this control host), {stale} stale (missed an update).\nTip: push again with 'remote policy push --host ...' to converge"
        );
    } else {
        println!("\nAll hosts match the local expectation.");
    }
    Ok(())
}

/// `aish remote audit-sync --host <host>`: pull remote audit events newer
/// than the local sync cursor and merge them into the local audit store,
/// deduplicating by (host, ts, event_type, command) (#587).
pub fn cmd_audit_pull(host: &str, db: Option<&str>) -> Result<(), String> {
    let cursor = load_sync_cursor(host);
    let bin = probe_remote_bin(host)?;
    let args = vec![
        bin,
        "--remote-audit-events".to_string(),
        "--since".to_string(),
        cursor.to_rfc3339(),
    ];
    let events = ssh_json_lines(host, &args, None)?;

    let store = aish_session::AuditStore::open(db.map(std::path::Path::new)).map_err(|e| {
        format!(
            "cannot open local audit store: {e}.\nTip: pass --db <path> to use a dedicated file"
        )
    })?;

    let mut imported = 0usize;
    let mut duplicates = 0usize;
    for ev in &events {
        match insert_remote_event(&store, host, ev) {
            Ok(true) => imported += 1,
            Ok(false) => duplicates += 1,
            Err(e) => eprintln!("{host}: skipped malformed event: {e}"),
        }
    }
    // Cursor = now: the next sync only fetches events newer than this pull.
    let now = chrono::Utc::now();
    save_sync_cursor(host, now)?;

    println!("{host}: imported {imported}, duplicates skipped {duplicates}");
    println!("cursor: {} -> {}", cursor.to_rfc3339(), now.to_rfc3339());
    if imported == 0 && duplicates == 0 {
        println!("(no new events since the last pull; delete the cursor file to re-pull)");
    }
    Ok(())
}

/// Insert one remote event into the local store; returns false when an
/// identical event (dedup key) already exists.
fn insert_remote_event(
    store: &aish_session::AuditStore,
    host: &str,
    ev: &serde_json::Value,
) -> Result<bool, String> {
    use aish_core::{AuditEvent, AuditSink};

    let ts = ev.get("ts").and_then(|v| v.as_str()).ok_or("missing ts")?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(ts)
        .map_err(|e| format!("bad ts: {e}"))?
        .with_timezone(&chrono::Utc);
    let event_type = ev
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("command");
    let command = ev.get("command").and_then(|v| v.as_str());

    // Dedup key check: same host + ts + type + command already imported.
    let query = aish_session::AuditQuery {
        user: None,
        host: Some(host.to_string()),
        event_type: event_type.parse().ok(),
        since: Some(timestamp - chrono::Duration::seconds(1)),
        until: Some(timestamp + chrono::Duration::seconds(1)),
        session_uuid: ev
            .get("session_uuid")
            .and_then(|v| v.as_str())
            .map(String::from),
        limit: 1000,
    };
    let existing = store.query(&query).map_err(|e| e.to_string())?;
    let duplicate = existing
        .iter()
        .any(|r| r.ts == timestamp && r.command.as_deref() == command);
    if duplicate {
        return Ok(false);
    }

    let event = AuditEvent {
        timestamp,
        session_uuid: ev
            .get("session_uuid")
            .and_then(|v| v.as_str())
            .map(String::from),
        user: ev.get("user").and_then(|v| v.as_str()).map(String::from),
        host: Some(host.to_string()),
        event_type: event_type
            .parse()
            .map_err(|_| format!("unknown event_type {event_type}"))?,
        command: command.map(String::from),
        source: ev.get("source").and_then(|v| v.as_str()).map(String::from),
        return_code: ev
            .get("return_code")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        ai_tool: ev.get("ai_tool").and_then(|v| v.as_str()).map(String::from),
        ai_args: ev.get("ai_args").and_then(|v| v.as_str()).map(String::from),
        ai_result: ev
            .get("ai_result")
            .and_then(|v| v.as_str())
            .map(String::from),
        decision: ev
            .get("decision")
            .and_then(|v| v.as_str())
            .map(String::from),
        user_choice: ev
            .get("user_choice")
            .and_then(|v| v.as_str())
            .map(String::from),
        matched_rule: ev
            .get("matched_rule")
            .and_then(|v| v.as_str())
            .map(String::from),
        risk_level: ev
            .get("risk_level")
            .and_then(|v| v.as_str())
            .map(String::from),
        duration_ms: ev.get("duration_ms").and_then(|v| v.as_u64()),
        outcome: ev.get("outcome").and_then(|v| v.as_str()).map(String::from),
    };
    store.record(event);
    Ok(true)
}

/// Cursor file: <config dir>/aish/remote-sync/<sanitized host>.ts
fn cursor_path(host: &str) -> Result<std::path::PathBuf, String> {
    let base = dirs::config_dir().ok_or("cannot resolve config dir")?;
    let dir = base.join("aish").join("remote-sync");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir.join(format!("{safe}.ts")))
}

fn load_sync_cursor(host: &str) -> chrono::DateTime<chrono::Utc> {
    cursor_path(host)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::hours(24))
}

fn save_sync_cursor(host: &str, at: chrono::DateTime<chrono::Utc>) -> Result<(), String> {
    let path = cursor_path(host)?;
    std::fs::write(&path, at.to_rfc3339()).map_err(|e| format!("cannot write cursor: {e}"))
}

/// `aish remote diagnose --hosts h1,h2 --checks svc,ports,logs` (#588).
/// `aish remote diag run ...` (#588): execute and persist the task.
pub fn diag_run(hosts: &[String], checks: &[String], json: bool) -> Result<(), String> {
    crate::diagnose::run(hosts, checks, json)
}

/// `aish remote diag list`: show saved tasks, newest first.
pub fn diag_list() -> Result<(), String> {
    let tasks = crate::diag_tasks::list()?;
    if tasks.is_empty() {
        println!("No diagnostic tasks saved.");
        println!("Tip: run 'aish remote diag run --hosts ... --checks ...' to create one");
        return Ok(());
    }
    println!("{:<14} {:<8} {:<24} CHECKS", "TASK", "HOSTS", "STARTED");
    for t in &tasks {
        let ok = t
            .outcomes
            .values()
            .filter(|o| **o == crate::diag_tasks::HostOutcome::Success)
            .count();
        let status = format!("{ok}/{}", t.outcomes.len());
        println!(
            "{:<14} {:<8} {:<24} {}",
            t.id,
            status,
            t.started_at,
            t.checks.join(",")
        );
    }
    Ok(())
}

/// `aish remote diag status <task>`: per-host outcome + evidence.
pub fn diag_status(task: &str, json: bool) -> Result<(), String> {
    let t = crate::diag_tasks::load(task)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&t).unwrap_or_default());
        return Ok(());
    }
    println!("task {} (started {})", t.id, t.started_at);
    println!("checks: {}", t.checks.join(", "));
    for h in &t.hosts {
        let outcome = t.outcomes.get(h).map(|o| o.label()).unwrap_or("pending");
        println!("  {h}: {outcome}");
    }
    let failed: Vec<&String> = t
        .hosts
        .iter()
        .filter(|h| t.outcomes.get(*h).map(|o| o.needs_retry()).unwrap_or(true))
        .collect();
    if !failed.is_empty() {
        println!(
            "Tip: 'aish remote diag retry {}' re-runs the {} failed host(s)",
            t.id,
            failed.len()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent-side hidden entries (run on the remote host, invoked over ssh)
// ---------------------------------------------------------------------------

/// Canonical action name for the agent-side identity gate. Each `--remote-*`
/// entry maps to one stable action string; the client signs exactly this
/// string, the agent verifies against it.
pub fn agent_action_name(
    sessions: bool,
    attach: bool,
    policy_show: bool,
    policy_install: bool,
    session_rename: bool,
    session_kill: bool,
    session_replay: bool,
    audit_events: bool,
    diagnose: bool,
) -> &'static str {
    if sessions {
        "sessions.list"
    } else if attach {
        "session.attach"
    } else if policy_show {
        "policy.show"
    } else if policy_install {
        "policy.install"
    } else if session_rename {
        "session.rename"
    } else if session_kill {
        "session.kill"
    } else if session_replay {
        "session.replay"
    } else if audit_events {
        "audit.events"
    } else if diagnose {
        "diagnose.run"
    } else {
        "unknown"
    }
}

/// `--remote-sessions`: print local PTY sessions as JSON lines with a
/// liveness flag per session.
pub fn agent_list_sessions() {
    for s in aish_pty::discover_sessions() {
        let alive = aish_pty::check_daemon_alive(&s.socket_path);
        // The shell (bash inside the PTY) is a child of the daemon: its
        // /proc entry distinguishes "attached/exited" states.
        let shell_running = if s.child_pid == 0 {
            None
        } else {
            Some(std::path::Path::new(&format!("/proc/{}", s.child_pid)).exists())
        };
        let record = serde_json::json!({
            "session_id": s.session_id,
            "name": s.name,
            "cwd": s.cwd,
            "model": s.model,
            "daemon_pid": s.daemon_pid,
            "child_pid": s.child_pid,
            "started_at": s.started_at,
            "alive": alive,
            "shell_running": shell_running,
            "controller": s.controller,
            "controller_stale": s.controller_stale,
        });
        println!("{}", serde_json::to_string(&record).unwrap_or_default());
    }
}

/// `--remote-session-rename <id> <name>`: rename a local session.
pub fn agent_session_rename(session_id: &str, name: &str) -> Result<(), String> {
    aish_pty::rename_session(session_id, name).map_err(|e| e.to_string())
}

/// `--remote-session-kill <id>`: kill a local session and its worker tree.
pub fn agent_session_kill(session_id: &str) -> Result<(), String> {
    let sessions = aish_pty::discover_sessions();
    let info = sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .ok_or_else(|| format!("session {session_id} not found"))?;
    aish_pty::kill_session(&info.socket_path).map_err(|e| e.to_string())
}

/// Resolve a session selector (full UUID, name, or unique prefix) to a UUID.
pub fn resolve_session_id(selector: &str) -> Result<String, String> {
    let sessions = aish_pty::discover_sessions();
    if let Some(s) = sessions.iter().find(|s| s.session_id == selector) {
        return Ok(s.session_id.clone());
    }
    let named: Vec<&aish_pty::DaemonSessionInfo> = sessions
        .iter()
        .filter(|s| s.name.as_deref() == Some(selector))
        .collect();
    if named.len() == 1 {
        return Ok(named[0].session_id.clone());
    }
    let prefixed: Vec<&aish_pty::DaemonSessionInfo> = sessions
        .iter()
        .filter(|s| s.session_id.starts_with(selector))
        .collect();
    if prefixed.len() == 1 {
        return Ok(prefixed[0].session_id.clone());
    }
    Err(if sessions.is_empty() {
        "no sessions on this host".to_string()
    } else if named.len() > 1 || prefixed.len() > 1 {
        format!("session selector '{selector}' is ambiguous")
    } else {
        format!("session '{selector}' not found")
    })
}

/// `--remote-session-replay <id> [--tail N]`: stream a session's recorded
/// terminal output. Each cast line is decoded (preferring raw_b64 for
/// non-UTF-8 chunks) and printed verbatim; the consumer sees exactly the
/// bytes the shell wrote, in order.
pub fn agent_session_replay(session_id: &str, tail: Option<usize>) -> Result<(), String> {
    let uuid = resolve_session_id(session_id)?;
    let path = aish_pty::session_recording::SessionRecorder::cast_path(&uuid)
        .ok_or("cannot resolve session directory")?;
    if !path.exists() {
        return Err(format!(
            "no recording for session {uuid} (recording starts when a session is created by this build)"
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read recording: {e}"))?;
    let mut chunks: Vec<Vec<u8>> = text
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            if let Some(b64) = v.get("raw_b64").and_then(|x| x.as_str()) {
                aish_pty::session_recording::decode_raw_b64(b64)
            } else {
                v.get("data")
                    .and_then(|x| x.as_str())
                    .map(|s| s.as_bytes().to_vec())
            }
        })
        .collect();
    if let Some(n) = tail {
        let start = chunks.len().saturating_sub(n);
        chunks = chunks[start..].to_vec();
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for chunk in chunks {
        let _ = out.write_all(&chunk);
    }
    Ok(())
}

/// `--remote-attach <session-id>`: connect to the session's daemon socket and
/// relay the frame protocol between our stdin/stdout and the socket.
pub fn agent_attach(session_id: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    // Operator identity: in enforcement mode the auth gate in main() has
    // already verified the first stdin line; in open mode stdin starts
    // directly with frame traffic (unchanged single-host behavior). Nothing
    // extra to consume here — the gate runs before this entry is called.

    // Find the session and its socket path.
    let sessions = aish_pty::discover_sessions();
    let info = sessions
        .iter()
        .find(|s| s.session_id == session_id)
        .ok_or_else(|| format!("session {session_id} not found"))?;
    if !aish_pty::check_daemon_alive(&info.socket_path) {
        return Err(format!("session {session_id} daemon is not running"));
    }

    let mut stream = std::os::unix::net::UnixStream::connect(&info.socket_path)
        .map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("socket setup failed: {e}"))?;

    // Relay loop: any side's readable bytes go to the other. Frames pass
    // through verbatim (the ssh peer performs the attach handshake itself).
    let sock_fd = stream.as_raw_fd();
    let in_fd = std::io::stdin().as_raw_fd();
    let out_fd = std::io::stdout().as_raw_fd();
    let mut buf = [0u8; 16384];

    loop {
        let mut fds = unsafe { std::mem::zeroed::<libc::fd_set>() };
        // SAFETY: [Category 8 — FFI] FD_ZERO/FD_SET on stack fd_set; fds are
        // small integers (stdin/stdout/socket).
        unsafe {
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(sock_fd, &mut fds);
            libc::FD_SET(in_fd, &mut fds);
        }
        let ret = unsafe {
            libc::select(
                sock_fd.max(in_fd) + 1,
                &mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("select failed: {err}"));
        }
        // SAFETY: [Category 8 — FFI] FD_ISSET on initialized fd_set.
        if unsafe { libc::FD_ISSET(sock_fd, &fds) } {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                return Ok(()); // daemon closed
            }
            // SAFETY: [Category 8 — FFI] write() to stdout (valid fd).
            if unsafe { libc::write(out_fd, buf.as_ptr() as *const _, n) } < 0 {
                return Ok(());
            }
        }
        // SAFETY: [Category 8 — FFI] FD_ISSET on initialized fd_set.
        if unsafe { libc::FD_ISSET(in_fd, &fds) } {
            let n = unsafe { libc::read(in_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                // ssh stdin closed: forward a Detach so the session survives.
                let enc = aish_pty::daemon_protocol::encode_frame(
                    &aish_pty::daemon_protocol::Frame::detach(),
                );
                stream.write_all(&enc).ok();
                return Ok(());
            }
            stream
                .write_all(&buf[..n as usize])
                .map_err(|_| "socket write failed".to_string())?;
        }
    }
}

/// `--remote-policy-install <envelope-json>`: validate and install a policy
/// envelope. Prints one JSON verdict line: {accepted, version, reason}.
///
/// First-phase trust model: the envelope must carry a version newer than the
/// installed remote version and parse as a valid policy. A signature field
/// that is absent or empty is accepted (unsigned first-phase envelopes); a
/// non-empty one is rejected outright because there is no key material to
/// verify against yet — ed25519 verification lands with the admin keypair
/// distribution. Until then version/target checks still prevent stale or
/// mismatched installs.
pub fn agent_policy_install(envelope: &str) {
    let verdict = install_policy_envelope(envelope);
    println!("{}", serde_json::to_string(&verdict).unwrap_or_default());
}

fn install_policy_envelope(envelope: &str) -> serde_json::Value {
    let env: serde_json::Value = match serde_json::from_str(envelope) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({"accepted": false, "version": 0, "reason": format!("malformed envelope: {e}")})
        }
    };
    let version = env.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    let targets = env
        .get("targets")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Target match: empty targets means "any host"; otherwise the local
    // hostname must match at least one entry (exact or trailing-* glob).
    if !targets.is_empty() {
        let hostname = std::fs::read_to_string("/etc/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let matched = targets.iter().any(|t| {
            t == &hostname
                || t.strip_suffix('*')
                    .map(|prefix| hostname.starts_with(prefix))
                    .unwrap_or(false)
        });
        if !matched {
            return serde_json::json!({"accepted": false, "version": version, "reason": format!("target mismatch: local host '{hostname}' not in {targets:?}")});
        }
    }

    // Version monotonicity: never regress below the installed version.
    let version_file = aish_security::remote_policy_version_path();
    let current = std::fs::read_to_string(&version_file)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if let Err(reason) = check_policy_version(version, current) {
        return serde_json::json!({"accepted": false, "version": version, "reason": reason});
    }

    // The policy body must exist before signature verification (the
    // signed payload includes the body).
    let Some(policy_yaml) = env.get("policy").and_then(|p| p.as_str()) else {
        return serde_json::json!({"accepted": false, "version": version, "reason": "missing policy body"});
    };

    // Signature gate: if any signer is configured, the signature is
    // mandatory and verified; in unsigned mode, empty signatures are
    // accepted and non-empty ones rejected.
    let signature = env.get("signature").and_then(|v| v.as_str()).unwrap_or("");
    let has_signers = !crate::policy_signing::list_signers()
        .unwrap_or_default()
        .is_empty();
    if has_signers {
        if signature.is_empty() {
            return serde_json::json!({"accepted": false, "version": version,
                "reason": "unsigned envelope rejected: this host has policy signers configured"});
        }
        if let Err(reason) =
            crate::policy_signing::verify_envelope(version, &targets, policy_yaml, signature)
        {
            return serde_json::json!({"accepted": false, "version": version, "reason": reason});
        }
    } else if let Err(reason) =
        check_policy_signature(env.get("signature").and_then(|v| v.as_str()))
    {
        return serde_json::json!({"accepted": false, "version": version, "reason": reason});
    }

    // Reject envelope-shaped bodies: a real policy never carries
    // signature/policy keys at top level.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(policy_yaml) {
        if v.is_object() && (v.get("signature").is_some() || v.get("policy").is_some()) {
            return serde_json::json!({"accepted": false, "version": version,
                "reason": "policy body looks like an envelope, not a policy"});
        }
    }
    if let Err(e) = aish_security::policy_from_yaml_str(policy_yaml) {
        return serde_json::json!({"accepted": false, "version": version, "reason": format!("policy parse failed: {e}")});
    }

    // Install atomically: write staged file, then rename over the target.
    let Some(target_path) = aish_security::writable_security_policy_path() else {
        return serde_json::json!({"accepted": false, "version": version, "reason": "cannot resolve policy path"});
    };
    let staged = target_path.with_extension("yaml.remote.tmp");
    if let Err(e) = std::fs::write(&staged, policy_yaml) {
        return serde_json::json!({"accepted": false, "version": version, "reason": format!("staging failed: {e}")});
    }
    if let Err(e) = std::fs::rename(&staged, &target_path) {
        let _ = std::fs::remove_file(&staged);
        return serde_json::json!({"accepted": false, "version": version, "reason": format!("install failed: {e}")});
    }
    let _ = std::fs::write(&version_file, version.to_string());
    // Metadata sidecar for 'aish remote policy show': issuer + install time.
    let issuer = env
        .get("issuer")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let installed_at = chrono::Utc::now().to_rfc3339();
    let meta = serde_json::json!({
        "version": version,
        "issuer": issuer,
        "installed_at": installed_at,
    });
    let _ = std::fs::write(
        target_path.with_extension("yaml.remote-meta"),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );

    serde_json::json!({
        "accepted": true,
        "version": version,
        "reason": "installed",
        "issuer": issuer,
        "installed_at": installed_at,
    })
}

/// Version monotonicity rule for remote policy installs: a candidate must be
/// strictly newer than the installed version. An absent marker parses as 0,
/// so version 1 is the first installable version. Kept pure for tests.
fn check_policy_version(candidate: u64, installed: u64) -> Result<(), String> {
    if candidate <= installed {
        Err(format!(
            "version {candidate} not newer than installed {installed}"
        ))
    } else {
        Ok(())
    }
}

/// Signature gate for remote policy installs (first phase): an absent or
/// empty signature is accepted so unsigned envelopes still install; a
/// non-empty one is rejected outright because there is no verification key
/// material yet. ed25519 verification lands with admin keypair distribution —
/// at that point this becomes a real verify against the issuer key instead of
/// a reject. Kept pure for tests.
fn check_policy_signature(signature: Option<&str>) -> Result<(), String> {
    match signature {
        None => Ok(()),
        Some(s) if s.trim().is_empty() => Ok(()),
        Some(_) => Err("signature present but verification is not supported yet".to_string()),
    }
}

/// `--remote-policy-show`: report the installed remote policy (version,
/// issuer, install time, rule count) for 'aish remote policy show'.
pub fn agent_policy_show() {
    use aish_security::remote_policy_version_path;
    let version = std::fs::read_to_string(remote_policy_version_path())
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let policy_path = aish_security::writable_security_policy_path();
    let yaml = policy_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let meta_path = policy_path
        .as_ref()
        .map(|p| p.with_extension("yaml.remote-meta"));
    let meta = meta_path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let rules = aish_security::policy_from_yaml_str(&yaml)
        .map(|p| p.rules.len())
        .unwrap_or(0);
    let risk = aish_security::policy_from_yaml_str(&yaml)
        .ok()
        .map(|p| p.default_risk_level.to_string())
        .unwrap_or_default();
    let record = serde_json::json!({
        "version": version,
        "issuer": meta.get("issuer").cloned().unwrap_or(serde_json::json!(null)),
        "installed_at": meta.get("installed_at").cloned().unwrap_or(serde_json::json!(null)),
        "policy": if yaml.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(yaml) },
        "rules": rules,
        "default_risk_level": risk,
    });
    println!("{}", serde_json::to_string(&record).unwrap_or_default());
}

/// `--remote-audit-events --since <rfc3339>`: print audit events newer than
/// the timestamp as JSON lines (summary fields only).
pub fn agent_audit_events(since: Option<&str>) {
    let since_ts = since
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::hours(24));

    let store = match aish_session::AuditStore::open(None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("audit store open failed: {e}");
            return;
        }
    };
    let query = aish_session::AuditQuery {
        user: None,
        host: None,
        event_type: None,
        since: Some(since_ts),
        until: None,
        session_uuid: None,
        limit: 10_000,
    };
    let events = match store.query(&query) {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!("audit query failed: {e}");
            return;
        }
    };
    for ev in events {
        let record = serde_json::json!({
            "ts": ev.ts.to_rfc3339(),
            "session_uuid": ev.session_uuid,
            "user": ev.user,
            "event_type": ev.event_type.to_string(),
            "command": ev.command,
            "source": ev.source,
            "return_code": ev.return_code,
            "decision": ev.decision,
            "risk_level": ev.risk_level,
            "outcome": ev.outcome,
        });
        println!("{}", serde_json::to_string(&record).unwrap_or_default());
    }
}

/// `--remote-diagnose --checks <list>`: run whitelisted read-only checks and
/// print one JSON line per check result. The whitelist is enforced here
/// regardless of what the coordinator requested.
pub fn agent_diagnose(checks: &str, enforce_read_only: bool) {
    let _ = enforce_read_only; // read-only is structural via the whitelist
    for check in checks.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let command = match check {
            "svc" => {
                "systemctl list-units --type=service --state=running --no-pager --no-legend | head -50"
            }
            "ports" => "ss -tlnp | head -50",
            "logs" => "journalctl --no-pager -n 100 --since '24 hours ago' | tail -100",
            "disk" => "df -hP | head -25",
            "load" => "uptime; vmstat 1 2 | tail -1",
            other => {
                // Unknown check: report denial, never execute.
                println!(
                    "{}",
                    serde_json::json!({"check": other, "denied": true, "reason": "check not in whitelist"})
                );
                continue;
            }
        };

        // Double-guard: even whitelisted commands are classified read-only
        // before execution (defense in depth against future edits).
        if !aish_tools::bash::is_read_only(command) {
            println!(
                "{}",
                serde_json::json!({"check": check, "denied": true, "reason": "command classified not read-only"})
            );
            continue;
        }

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output();
        match output {
            Ok(out) => {
                // Merge stdout and stderr so remote warnings stay visible in
                // the evidence instead of vanishing into the frame stream.
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(stderr.trim_end());
                }
                let truncated = text.len() > 8192;
                let body: String = text.chars().take(8192).collect();
                let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
                println!(
                    "{}",
                    serde_json::json!({
                        "check": check,
                        "exit_code": out.status.code(),
                        "truncated": truncated,
                        "lines": lines,
                        "summary": body.trim_end(),
                    })
                );
            }
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({"check": check, "denied": false, "error": format!("spawn failed: {e}")})
                );
            }
        }
    }
}

#[cfg(test)]
mod policy_install_tests {
    use super::{check_policy_signature, check_policy_version};

    #[test]
    fn version_check_requires_strictly_newer() {
        assert!(check_policy_version(1, 0).is_ok());
        assert!(check_policy_version(8, 7).is_ok());
        assert_eq!(
            check_policy_version(7, 7).unwrap_err(),
            "version 7 not newer than installed 7"
        );
        assert_eq!(
            check_policy_version(6, 7).unwrap_err(),
            "version 6 not newer than installed 7"
        );
    }

    #[test]
    fn signature_check_accepts_absent_and_empty() {
        assert!(check_policy_signature(None).is_ok());
        assert!(check_policy_signature(Some("")).is_ok());
        assert!(check_policy_signature(Some("   ")).is_ok());
    }

    #[test]
    fn signature_check_rejects_non_empty() {
        assert_eq!(
            check_policy_signature(Some("abc")).unwrap_err(),
            "signature present but verification is not supported yet"
        );
    }
}

/// `aish remote doctor --host <h>`: self-check the remote C/S setup and
/// print actionable results (binary present, version, sessions readable,
/// audit store readable).
pub fn cmd_remote_doctor(host: &str) -> Result<(), String> {
    println!("Checking remote C/S setup on {host}...");
    let mut failures = 0usize;

    // 1. ssh connectivity + binary
    let bin = match probe_remote_bin(host) {
        Ok(b) => {
            println!("  ✓ remote aish binary: {b}");
            b
        }
        Err(e) => {
            println!("  ✗ remote aish binary: {e}");
            return Ok(()); // everything else depends on this
        }
    };

    // 2. version parity (informational)
    let args = vec![bin.clone(), "--version".to_string()];
    match ssh_raw_lines(host, &args) {
        Ok(lines) => {
            println!(
                "  ✓ remote version: {}",
                lines.first().unwrap_or(&"?".to_string())
            );
        }
        Err(e) => {
            println!("  ✗ remote version check failed: {e}");
            failures += 1;
        }
    }

    // 3. sessions listing
    let args = vec![bin.clone(), "--remote-sessions".to_string()];
    match ssh_json_lines(host, &args, None) {
        Ok(sessions) => {
            println!("  ✓ session discovery works ({} sessions)", sessions.len());
        }
        Err(e) => {
            println!("  ✗ session discovery failed: {e}");
            failures += 1;
        }
    }

    // 4. audit store readable
    let args = vec![
        bin,
        "--remote-audit-events".to_string(),
        "--since".to_string(),
        (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
    ];
    match ssh_json_lines(host, &args, None) {
        Ok(_) => println!("  ✓ audit store readable"),
        Err(e) => {
            println!("  ✗ audit store unreadable: {e}");
            failures += 1;
        }
    }

    if failures == 0 {
        println!("All checks passed.");
    } else {
        println!("{failures} check(s) failed. Fix the ✗ items above and re-run.");
    }
    Ok(())
}

/// Run a remote command and collect plain stdout lines (no JSON parsing).
fn ssh_raw_lines(host: &str, remote_args: &[String]) -> Result<Vec<String>, String> {
    let mut ch = SshChannel::spawn(host, remote_args)?;
    let (reader, stdin) = ch.split()?;
    drop(stdin);
    let lines = reader
        .lines()
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| format!("ssh read failed: {e}"))?;
    ch.wait()?;
    Ok(lines)
}

#[cfg(test)]
mod shell_quote_tests {
    use super::shell_quote;

    #[test]
    fn bare_tokens_pass_through() {
        assert_eq!(shell_quote("/usr/local/bin/aish"), "/usr/local/bin/aish");
        assert_eq!(shell_quote("--remote-sessions"), "--remote-sessions");
        assert_eq!(shell_quote("user@host"), "user@host");
    }

    #[test]
    fn spaces_are_quoted() {
        let q = shell_quote("uuid-1 web-1");
        assert_eq!(q, "'uuid-1 web-1'");
        // And the quoted form round-trips through a split on spaces when the
        // remote shell parses it back as one word.
        assert!(q.contains(' '));
        assert!(q.starts_with('\''));
    }

    #[test]
    fn embedded_quotes_are_escaped() {
        let q = shell_quote("it's");
        assert_eq!(q, "'it'\\''s'");
    }

    #[test]
    fn dollar_and_semicolon_are_quoted() {
        assert!(shell_quote("$HOME").starts_with('\''));
        assert!(shell_quote("a;b").starts_with('\''));
    }
}

#[cfg(test)]
mod policy_signing_tests {
    #[test]
    fn sign_and_verify_roundtrip() {
        // Isolated keypair via the module's own file paths would collide;
        // use direct dalek primitives to validate the canonical payload.
        use ed25519_dalek::{Signer, SigningKey, Verifier};
        let mut csprng = rand_core::OsRng;
        let key = SigningKey::generate(&mut csprng);
        let targets = ["prod-*".to_string()];
        let policy = "rules: []\n";
        let payload = format!("{}|{}|{}", 7u64, targets.join(","), policy);
        let sig = key.sign(payload.as_bytes());
        assert!(key.verifying_key().verify(payload.as_bytes(), &sig).is_ok());
        // Tampered payload must fail.
        let tampered = format!("{}|{}|{}", 7u64, targets.join(","), "rules: [x]\n");
        assert!(key
            .verifying_key()
            .verify(tampered.as_bytes(), &sig)
            .is_err());
    }
}
