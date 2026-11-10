//! C/S remote extensions: SSH-backed remote session access, policy sync,
//! and fleet diagnostics (issues #586, #587, #588).
//!
//! Design constraints:
//! - No resident central server: every remote operation runs `ssh <host> aish
//!   <hidden-subcommand>` on demand; the remote aish binary is located via
//!   `AISH_REMOTE_BIN` (env override) or by probing common install paths.
//! - Local behavior is untouched: all new code paths live behind explicit
//!   subcommands; the single-machine shell/daemon flow never calls into here.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// Environment variable overriding the remote aish binary path.
const REMOTE_BIN_ENV: &str = "AISH_REMOTE_BIN";

/// Candidate paths probed on the remote host when `AISH_REMOTE_BIN` is unset.
const REMOTE_BIN_CANDIDATES: &[&str] = &["/usr/local/bin/aish", "/usr/bin/aish"];

/// Resolve the remote aish binary path: env override wins, otherwise probe.
fn remote_bin() -> String {
    if let Ok(path) = std::env::var(REMOTE_BIN_ENV) {
        if !path.is_empty() {
            return path;
        }
    }
    REMOTE_BIN_CANDIDATES
        .first()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "aish".to_string())
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
            .arg(remote_bin())
            .args(remote_args)
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
    ) -> Result<(BufReader<std::process::ChildStdout>, std::process::ChildStdin), String> {
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
}

/// Run a remote aish subcommand and collect its stdout lines as JSON values.
/// Used by list-style remote queries (sessions, events, diagnose results).
pub(crate) fn ssh_json_lines(
    host: &str,
    remote_args: &[String],
) -> Result<Vec<serde_json::Value>, String> {
    let mut ch = SshChannel::spawn(host, remote_args)?;
    let (reader, mut stdin) = ch.split()?;
    drop(stdin); // signal EOF so the remote command finishes

    let mut values = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|e| format!("ssh read failed: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(v) => values.push(v),
            // Non-JSON lines (banner, warnings) are surfaced verbatim.
            Err(_) => eprintln!("[remote {}] {}", host, line),
        }
    }
    ch.wait()?;
    Ok(values)
}

/// Probe that the remote host has a usable aish binary. Returns the resolved
/// path on success.
pub(crate) fn probe_remote_bin(host: &str) -> Result<String, String> {
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
        "no usable aish binary found on {host}; set {REMOTE_BIN_ENV} to the remote path"
    ))
}

// ---------------------------------------------------------------------------
// Coordinator-side command implementations
// ---------------------------------------------------------------------------

/// `aish remote sessions --host <host>`: list remote PTY sessions with
/// liveness state (#586).
pub fn cmd_remote_sessions(host: &str, json: bool) -> Result<(), String> {
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-sessions".to_string()];
    let sessions = ssh_json_lines(host, &args)?;

    if json {
        for s in &sessions {
            println!("{}", serde_json::to_string(s).unwrap_or_default());
        }
        return Ok(());
    }
    if sessions.is_empty() {
        println!("No active PTY sessions on {host}.");
        return Ok(());
    }
    println!("Remote PTY sessions on {host}:");
    for s in &sessions {
        let id = s
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let cwd = s.get("cwd").and_then(|v| v.as_str()).unwrap_or("?");
        let alive = s.get("alive").and_then(|v| v.as_bool()).unwrap_or(false);
        let state = if alive { "running" } else { "exited" };
        println!("  {id}  [{state}]  {cwd}  {name}");
    }
    Ok(())
}

/// `aish remote attach --host <h> <session>` (#586).
pub fn cmd_remote_attach(host: &str, session_id: &str) -> Result<(), String> {
    crate::attach_remote::run(host, session_id)
}

/// `aish remote policy-push --host <host> --policy <file>`: send a policy
/// envelope and report whether the remote accepted it (#587).
pub fn cmd_policy_push(host: &str, policy_file: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(policy_file)
        .map_err(|e| format!("cannot read policy file {policy_file}: {e}"))?;
    // The envelope is passed as a single argument; the remote side validates
    // version/targets before installing.
    let bin = probe_remote_bin(host)?;
    let args = vec![bin, "--remote-policy-install".to_string(), content];
    let replies = ssh_json_lines(host, &args)?;
    match replies.into_iter().next() {
        Some(reply) => {
            let accepted = reply
                .get("accepted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let version = reply.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
            let reason = reply.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            if accepted {
                println!("{host}: policy v{version} accepted and effective");
                Ok(())
            } else {
                println!("{host}: policy v{version} REJECTED: {reason}");
                Err(format!("{host}: policy rejected"))
            }
        }
        None => Err(format!("{host}: no reply from remote policy install")),
    }
}

/// `aish remote audit-sync --host <host>`: pull remote audit events newer
/// than the local sync cursor and merge them into the local audit store,
/// deduplicating by (host, ts, event_type, command) (#587).
pub fn cmd_audit_sync(host: &str, db: Option<&str>) -> Result<(), String> {
    let cursor = load_sync_cursor(host);
    let bin = probe_remote_bin(host)?;
    let args = vec![
        bin,
        "--remote-audit-events".to_string(),
        "--since".to_string(),
        cursor.to_rfc3339(),
    ];
    let events = ssh_json_lines(host, &args)?;

    let store = aish_session::AuditStore::open(db.map(std::path::Path::new))
        .map_err(|e| format!("cannot open local audit store: {e}"))?;

    let mut imported = 0usize;
    let mut duplicates = 0usize;
    for ev in &events {
        match insert_remote_event(&store, host, ev) {
            Ok(true) => imported += 1,
            Ok(false) => duplicates += 1,
            Err(e) => eprintln!("{host}: skip event: {e}"),
        }
    }
    // Cursor = now: the next sync only fetches events newer than this pull.
    let now = chrono::Utc::now();
    save_sync_cursor(host, now)?;

    println!("{host}: imported {imported}, duplicates skipped {duplicates}");
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
        ai_result: ev.get("ai_result").and_then(|v| v.as_str()).map(String::from),
        decision: ev.get("decision").and_then(|v| v.as_str()).map(String::from),
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
pub fn cmd_remote_diagnose(
    hosts: &[String],
    checks: &[String],
    json: bool,
) -> Result<(), String> {
    crate::diagnose::run(hosts, checks, json)
}

// ---------------------------------------------------------------------------
// Agent-side hidden entries (run on the remote host, invoked over ssh)
// ---------------------------------------------------------------------------

/// `--remote-sessions`: print local PTY sessions as JSON lines with a
/// liveness flag per session.
pub fn agent_list_sessions() {
    for s in aish_pty::discover_sessions() {
        let alive = aish_pty::check_daemon_alive(&s.socket_path);
        let record = serde_json::json!({
            "session_id": s.session_id,
            "name": s.name,
            "cwd": s.cwd,
            "model": s.model,
            "daemon_pid": s.daemon_pid,
            "child_pid": s.child_pid,
            "started_at": s.started_at,
            "alive": alive,
        });
        println!("{}", serde_json::to_string(&record).unwrap_or_default());
    }
}

/// `--remote-attach <session-id>`: connect to the session's daemon socket and
/// relay the frame protocol between our stdin/stdout and the socket.
pub fn agent_attach(session_id: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

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
/// installed remote version and parse as a valid policy. Signature checking
/// lands with the admin keypair distribution; until then version/target
/// checks still prevent stale or mismatched installs.
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
    if version <= current {
        return serde_json::json!({"accepted": false, "version": version, "reason": format!("version {version} not newer than installed {current}")});
    }

    // The policy body must parse into a valid SecurityPolicy.
    let Some(policy_yaml) = env.get("policy").and_then(|p| p.as_str()) else {
        return serde_json::json!({"accepted": false, "version": version, "reason": "missing policy body"});
    };
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

    serde_json::json!({"accepted": true, "version": version, "reason": "installed"})
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

        let output = std::process::Command::new("sh").arg("-c").arg(command).output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let truncated = stdout.len() > 8192;
                let body: String = stdout.chars().take(8192).collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "check": check,
                        "exit_code": out.status.code(),
                        "truncated": truncated,
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
