//! Integration tests for #586 role arbitration.
//!
//! Scenario: a daemon session started via the normal outer-aish path (the
//! outer client becomes Controller). A second AttachedBackend client must
//! receive RoleNotify(Observer) — including the case where the notify frame
//! rides in the same flush batch as ScrollbackEnd — and its input must never
//! reach the PTY.

#![cfg(unix)]

use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use aish_pty::{AttachedBackend, PtyBackend, PtyEvent};

const TIMEOUT_SECS: u64 = 20;

struct TestWorkspace {
    root: std::path::PathBuf,
    #[allow(dead_code)]
    outer: Option<Child>,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "aish-arbitration-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("cfg")).expect("create cfg dir");
        std::fs::create_dir_all(root.join("workdir")).expect("create workdir");
        let root = std::fs::canonicalize(&root).expect("canonicalize");
        std::fs::write(
            root.join("cfg").join("config.yaml"),
            "model: test\napi_base: http://127.0.0.1:1/v1\napi_key: k\npty_daemon_enabled: true\ninline_completion:\n  enabled: false\n",
        )
        .expect("write config");
        std::fs::write(
            root.join("selected.yaml"),
            "model: test\napi_base: http://127.0.0.1:1/v1\napi_key: k\npty_daemon_enabled: true\ninline_completion:\n  enabled: false\n",
        )
        .expect("write selected config");
        // Kill leftover daemons from previous runs and clear their session
        // files, so the test attaches to the daemon it starts itself and
        // the sidecar assertions see only this run's files.
        for s in aish_pty::discover_sessions() {
            let _ = aish_pty::kill_session(&s.socket_path);
        }
        let sessions_dir = aish_pty::pty_session_dir().expect("session dir");
        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Self { root, outer: None }
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        for session in aish_pty::discover_sessions() {
            let _ = aish_pty::kill_session(&session.socket_path);
        }
        if let Some(child) = self.outer.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spawn the outer aish on a PTY (it creates the daemon and attaches as
/// Controller, exactly like a normal interactive launch).
fn spawn_outer(ws: &TestWorkspace) -> (Child, std::fs::File) {
    let bin = outer_bin();
    let (master_fd, slave_fd) = unsafe {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut size: libc::winsize = std::mem::zeroed();
        size.ws_row = 40;
        size.ws_col = 120;
        assert_eq!(
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size
            ),
            0,
            "openpty"
        );
        (master_fd, slave_fd)
    };
    let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let child = Command::new(&bin)
        .arg("--config")
        .arg(ws.root.join("selected.yaml"))
        .current_dir(ws.root.join("workdir"))
        .env("AISH_CONFIG_DIR", ws.root.join("cfg"))
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::from(slave.try_clone().expect("clone slave")))
        .stdout(Stdio::from(slave.try_clone().expect("clone slave")))
        .stderr(Stdio::from(slave.try_clone().expect("clone slave")))
        .spawn()
        .expect("spawn outer aish");
    (child, master)
}

fn outer_bin() -> String {
    // The integration test lives in aish-pty; the aish binary is built by
    // aish-cli. Prefer the workspace target dir relative to this crate.
    // Prefer an explicitly provided binary (set by the caller or CI), then
    // the workspace target dir relative to this crate.
    if let Ok(p) = std::env::var("AISH_TEST_BIN") {
        return p;
    }
    let ws_target = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/aish");
    if std::path::Path::new(ws_target).exists() {
        return ws_target.to_string();
    }
    panic!("aish binary not found; build aish-cli first or set AISH_TEST_BIN")
}

fn wait_for_session(ws: &TestWorkspace) -> (String, String) {
    let deadline = std::time::Instant::now() + Duration::from_secs(TIMEOUT_SECS);
    loop {
        // Pick the NEWEST session in the workdir: stale daemons from
        // previous runs can linger and would otherwise be selected.
        let mut sessions: Vec<_> = aish_pty::discover_sessions()
            .into_iter()
            .filter(|s| {
                std::fs::canonicalize(&s.cwd)
                    .map(|c| c == ws.root.join("workdir"))
                    .unwrap_or(false)
            })
            .collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        if let Some(s) = sessions.into_iter().next() {
            return (s.socket_path.display().to_string(), s.session_id);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session file never appeared"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn drain(backend: &mut dyn PtyBackend, secs: u64) -> Vec<PtyEvent> {
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(evts) = backend.drain_events() {
            events.extend(evts);
        }
    }
    events
}

fn output_bytes(events: &[PtyEvent]) -> Vec<u8> {
    events
        .iter()
        .flat_map(|e| match e {
            PtyEvent::Output(b) => b.clone(),
            _ => Vec::new(),
        })
        .collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn second_client_gets_observer_role_and_input_is_gated() {
    let ws = TestWorkspace::new();
    let (outer_child, mut _master) = spawn_outer(&ws);
    let (socket_path, session_id) = wait_for_session(&ws);
    let _ = outer_child;

    // Give the outer client time to complete its own attach (it becomes
    // Controller). Verify it is really attached: typed input must echo.
    std::thread::sleep(Duration::from_secs(2));
    {
        use std::io::Write as _;
        let _ = _master.write_all(b"echo PROBE_ATTACH\r");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut probe_ok = false;
        while std::time::Instant::now() < deadline {
            let mut buf = [0u8; 65536];
            // Blocking read with timeout via fcntl would be nicer; poll().
            let mut set = unsafe { std::mem::zeroed::<libc::fd_set>() };
            unsafe {
                libc::FD_ZERO(&mut set);
                libc::FD_SET(_master.as_raw_fd(), &mut set);
            }
            let mut tv = libc::timeval {
                tv_sec: 0,
                tv_usec: 200_000,
            };
            let r = unsafe {
                libc::select(
                    _master.as_raw_fd() + 1,
                    &mut set,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut tv,
                )
            };
            if r > 0 {
                let n =
                    unsafe { libc::read(_master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                if contains(&buf[..n.max(0) as usize], b"PROBE_ATTACH") {
                    probe_ok = true;
                    break;
                }
            }
        }
        assert!(
            probe_ok,
            "outer client did not attach to the daemon (PROBE_ATTACH not echoed)"
        );
    }

    // Second client: must be told it is an Observer.
    let mut c2 = AttachedBackend::attach(std::path::Path::new(&socket_path), &session_id, 30, 100)
        .expect("second client attach");

    let ev2 = drain(&mut c2, 4);
    let has_observer = ev2.iter().any(|e| matches!(
        e,
        PtyEvent::RoleNotice(role, _) if *role == aish_pty::daemon_protocol::ClientRole::Observer
    ));
    let kinds: Vec<String> = ev2
        .iter()
        .map(|e| match e {
            PtyEvent::Output(b) => format!("Output({})", b.len()),
            PtyEvent::Control(_) => "Control".to_string(),
            PtyEvent::RoleNotice(r, _) => format!("RoleNotice({:?})", r),
        })
        .collect();
    assert!(
        has_observer,
        "second client must receive RoleNotify(Observer); events={:?}",
        kinds
    );

    // Observer input must be dropped: no echo on any client.
    c2.write_input(b"echo BAD_OBSERVER\r").expect("c2 write");
    let ev2b = drain(&mut c2, 2);
    let out2 = output_bytes(&ev2b);
    assert!(
        !contains(&out2, b"BAD_OBSERVER"),
        "observer input must not be echoed back to the observer"
    );

    // The daemon updates the controller fields in the session JSON on role
    // changes; poll for the controller field to appear (the outer attach can
    // take a while due to shell startup).
    let session_file = aish_pty::pty_session_dir()
        .expect("session dir")
        .join(format!("{session_id}.json"));
    let mut controller_named = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&session_file) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if v.get("controller").and_then(|c| c.as_str()).is_some() {
                    controller_named = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        controller_named,
        "session JSON must name the controller after role changes"
    );

    let _ = c2.detach();
}
