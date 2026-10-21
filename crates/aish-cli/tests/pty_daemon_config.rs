#![cfg(unix)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct TestWorkspace {
    root: std::path::PathBuf,
    workdir: std::path::PathBuf,
    config_path: std::path::PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "aish-pty-config-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("create isolated test directory");
        let root = fs::canonicalize(root).expect("canonicalize isolated test directory");
        let workdir = root.join("workdir");
        fs::create_dir_all(&workdir).expect("create isolated working directory");
        let workdir = fs::canonicalize(workdir).expect("canonicalize isolated working directory");
        let config_path = root.join("selected.yaml");
        Self {
            root,
            workdir,
            config_path,
        }
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        for session in aish_pty::discover_sessions()
            .into_iter()
            .filter(|session| session.cwd == self.workdir.to_string_lossy())
        {
            let _ = aish_pty::kill_session(&session.socket_path);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct PtyProcess {
    child: Child,
    master: File,
}

impl PtyProcess {
    fn spawn(workspace: &TestWorkspace, args: &[&str]) -> Self {
        // SAFETY: `openpty` initializes both output descriptors and the
        // supplied winsize; the null name and termios pointers request defaults.
        let (master_fd, slave_fd) = unsafe {
            let mut master_fd = -1;
            let mut slave_fd = -1;
            let mut size: libc::winsize = std::mem::zeroed();
            size.ws_row = 40;
            size.ws_col = 120;
            let result = libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            );
            assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
            (master_fd, slave_fd)
        };

        // SAFETY: `openpty` returned two distinct owned descriptors above.
        let master = unsafe { File::from_raw_fd(master_fd) };
        // SAFETY: `openpty` returned two distinct owned descriptors above.
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let mut command = Command::new(env!("CARGO_BIN_EXE_aish"));
        command
            .args(args)
            .current_dir(&workspace.workdir)
            .env("AISH_CONFIG_DIR", workspace.root.join("default-config"))
            .env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8")
            .stdin(Stdio::from(
                slave.try_clone().expect("clone PTY slave for stdin"),
            ))
            .stdout(Stdio::from(
                slave.try_clone().expect("clone PTY slave for stdout"),
            ))
            .stderr(Stdio::from(slave));
        let child = command.spawn().expect("spawn aish on PTY");
        Self { child, master }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write to PTY master");
        self.master.flush().expect("flush PTY master");
    }

    fn read_until(&mut self, needle: &[u8], timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut buffer = [0u8; 4096];
        while Instant::now() < deadline {
            let mut descriptor = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `descriptor` points to one initialized pollfd and lives
            // for the duration of the call.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
            assert!(
                ready >= 0,
                "poll PTY master: {}",
                std::io::Error::last_os_error()
            );
            if ready > 0 {
                match self.master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => output.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("read PTY master: {error}"),
                }
                if contains_bytes(&output, needle) {
                    return output;
                }
            }
            if let Some(status) = self.child.try_wait().expect("check aish process") {
                panic!(
                    "aish exited before PTY output matched {:?} (status {status}): {}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&output)
                );
            }
        }
        panic!(
            "timed out waiting for PTY output {:?}: {}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&output)
        );
    }

    fn read_for(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        let mut buffer = [0u8; 4096];
        while Instant::now() < deadline {
            let mut descriptor = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `descriptor` points to one initialized pollfd and lives
            // for the duration of the call.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
            assert!(
                ready >= 0,
                "poll PTY master: {}",
                std::io::Error::last_os_error()
            );
            if ready > 0 {
                match self.master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("read PTY master: {error}"),
                }
            }
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct CapturedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: serde_json::Value,
}

fn capture_one_request(listener: TcpListener) -> Result<CapturedRequest, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("set listener nonblocking: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("timed out waiting for aish to connect".to_string());
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("accept request: {error}")),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("set request read timeout: {error}"))?;

    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let (header_end, content_length) = loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("read request headers: {error}"))?;
        if count == 0 {
            return Err("request ended before complete headers".to_string());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = find_subslice(&bytes, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .ok_or_else(|| "request did not include Content-Length".to_string())?;
            break (header_end, content_length);
        }
    };
    while bytes.len() < header_end + 4 + content_length {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("read request body: {error}"))?;
        if count == 0 {
            return Err("request ended before complete body".to_string());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }

    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let mut request_line = headers
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let authorization = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_string())
    });
    let body_start = header_end + 4;
    let body = serde_json::from_slice(&bytes[body_start..body_start + content_length])
        .map_err(|error| format!("parse request JSON: {error}"))?;

    let response = b"{\"error\":\"test captured request\"}";
    write!(
        stream,
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    )
    .map_err(|error| format!("write stub response headers: {error}"))?;
    stream
        .write_all(response)
        .map_err(|error| format!("write stub response body: {error}"))?;

    Ok(CapturedRequest {
        method,
        path,
        authorization,
        body,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || find_subslice(haystack, needle).is_some()
}

fn sessions_for(workdir: &std::path::Path) -> Vec<aish_pty::DaemonSessionInfo> {
    aish_pty::discover_sessions()
        .into_iter()
        .filter(|session| session.cwd == workdir.to_string_lossy())
        .collect()
}

fn wait_for_session_count(
    workdir: &std::path::Path,
    expected: usize,
    timeout: Duration,
) -> Vec<aish_pty::DaemonSessionInfo> {
    let deadline = Instant::now() + timeout;
    loop {
        let sessions = sessions_for(workdir);
        if sessions.len() >= expected {
            return sessions;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected} PTY sessions; found {}",
            sessions.len()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn new_session_uses_selected_config_and_cli_overrides_for_llm_request() {
    let workspace = TestWorkspace::new();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local LLM stub");
    let port = listener.local_addr().unwrap().port();
    let request_thread = thread::spawn(move || capture_one_request(listener));
    fs::write(
        &workspace.config_path,
        "model: config-model\napi_base: http://127.0.0.1:1/wrong/v1\napi_key: config-key\ntemperature: 0.73\npty_daemon_enabled: true\ninline_completion:\n  enabled: false\n",
    )
    .expect("write selected config");

    let api_base = format!("http://127.0.0.1:{port}/override/v1");
    let mut process = PtyProcess::spawn(
        &workspace,
        &[
            "--config",
            workspace.config_path.to_str().unwrap(),
            "--model",
            "cli-model",
            "--api-base",
            &api_base,
            "--api-key",
            "cli-key",
        ],
    );
    let initial_sessions = wait_for_session_count(&workspace.workdir, 1, Duration::from_secs(15));
    assert_eq!(
        initial_sessions.len(),
        1,
        "PTY session path should be isolated"
    );
    process.read_for(Duration::from_millis(500));

    process.write(b"/live_sessions\n");
    let _ = process.read_until(b"Live PTY Sessions", Duration::from_secs(10));
    process.write(b"\x1b[F\r");
    let sessions = wait_for_session_count(&workspace.workdir, 2, Duration::from_secs(15));
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id != initial_sessions[0].session_id),
        "selecting Create new session should start another daemon"
    );
    process.read_for(Duration::from_millis(700));

    process.write(b";verify daemon configuration\n");
    let request = request_thread
        .join()
        .expect("local LLM request capture thread")
        .expect("capture LLM request from new session");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/override/v1/chat/completions");
    assert_eq!(request.authorization.as_deref(), Some("Bearer cli-key"));
    assert_eq!(request.body["model"], "cli-model");
    assert_eq!(request.body["temperature"], 0.73);
}
