use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::ipc::protocol::{
    decode_request_line, encode_failure_response_line, encode_success_response_line,
};
use crate::sandbox::runtime::worker::spawn_worker_process;
use crate::sandbox::types::{
    RequestIdentity, SandboxLimits, SandboxResult, SandboxRunContext, SandboxRunRequest,
};

pub(crate) const DEFAULT_SANDBOX_SOCKET_PATH: &str = "/run/aish/sandbox.sock";
const DEFAULT_READ_BUFFER_BYTES: usize = 64 * 1024;
const SYSTEMD_SOCKET_ACTIVATION_FD: RawFd = 3;
const DEFAULT_SOCKET_MODE: u32 = 0o666;
const ACCEPT_TO_REQUEST_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub(crate) struct SandboxDaemonOptions {
    pub(crate) socket_path: PathBuf,
    pub(crate) limits: SandboxLimits,
    pub(crate) worker_program: Option<PathBuf>,
}

impl Default for SandboxDaemonOptions {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from(DEFAULT_SANDBOX_SOCKET_PATH),
            limits: SandboxLimits::default(),
            worker_program: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SandboxDaemonRequest {
    pub(crate) request: SandboxRunRequest,
    pub(crate) identity: RequestIdentity,
}

pub(crate) fn bind_listener(socket_path: &Path) -> Result<UnixListener, SandboxError> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxUnavailable, error.to_string())
        })?;
    }

    if socket_path.exists() {
        fs::remove_file(socket_path).map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxUnavailable, error.to_string())
        })?;
    }

    let listener = UnixListener::bind(socket_path).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxUnavailable, error.to_string())
    })?;

    let permissions = fs::Permissions::from_mode(DEFAULT_SOCKET_MODE);
    fs::set_permissions(socket_path, permissions).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxUnavailable, error.to_string())
    })?;

    Ok(listener)
}

pub(crate) fn serve_once(
    listener: &UnixListener,
    options: &SandboxDaemonOptions,
) -> Result<SandboxDaemonRequest, SandboxError> {
    let (mut stream, _addr) = listener.accept().map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxUnavailable, error.to_string())
    })?;

    set_stream_timeouts(&stream, accept_to_request_timeout())?;
    let request = read_request(&mut stream, options.limits)?;
    set_stream_timeouts(&stream, request_timeout(options.limits, request.timeout_s))?;
    let identity = peer_credentials(&stream)?;

    let response = execute_request(&request, identity, options)?;
    log_request_succeeded(&request, &identity, &response);
    let raw = encode_success_response_line(&request.id, &response, options.limits)?;
    stream.write_all(&raw).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
    })?;

    Ok(SandboxDaemonRequest { request, identity })
}

pub(crate) fn run_forever(options: &SandboxDaemonOptions) -> Result<(), SandboxError> {
    let (listener, socket_activated) = match activated_listener_from_env()? {
        Some(listener) => (listener, true),
        None => (bind_listener(&options.socket_path)?, false),
    };
    log_daemon_started(options, socket_activated);

    loop {
        let (stream, _addr) = listener.accept().map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxUnavailable, error.to_string())
        })?;

        let options = options.clone();
        thread::spawn(move || {
            let _ = serve_connection(stream, &options);
        });
    }
}

fn activated_listener_from_env() -> Result<Option<UnixListener>, SandboxError> {
    let use_activation = should_use_systemd_socket(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    )?;
    if !use_activation {
        return Ok(None);
    }

    let listener = unsafe { UnixListener::from_raw_fd(SYSTEMD_SOCKET_ACTIVATION_FD) };
    Ok(Some(listener))
}

fn should_use_systemd_socket(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    current_pid: u32,
) -> Result<bool, SandboxError> {
    let Some(listen_fds) = listen_fds else {
        return Ok(false);
    };
    let Some(listen_pid) = listen_pid else {
        return Ok(false);
    };

    if listen_pid.parse::<u32>().ok() != Some(current_pid) {
        return Ok(false);
    }

    let listen_fds = listen_fds.parse::<u32>().map_err(|error| {
        SandboxError::with_details(
            SandboxReason::SandboxUnavailable,
            format!("invalid LISTEN_FDS: {error}"),
        )
    })?;

    match listen_fds {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(SandboxError::with_details(
            SandboxReason::SandboxUnavailable,
            format!("unsupported LISTEN_FDS={value}"),
        )),
    }
}

pub(crate) fn serve_connection(
    mut stream: UnixStream,
    options: &SandboxDaemonOptions,
) -> Result<SandboxDaemonRequest, SandboxError> {
    set_stream_timeouts(&stream, accept_to_request_timeout())?;
    let identity = peer_credentials(&stream)?;
    match read_request(&mut stream, options.limits) {
        Ok(request) => {
            set_stream_timeouts(&stream, request_timeout(options.limits, request.timeout_s))?;
            let raw = match execute_request(&request, identity, options) {
                Ok(response) => {
                    log_request_succeeded(&request, &identity, &response);
                    encode_success_response_line(&request.id, &response, options.limits)?
                }
                Err(error) => {
                    log_request_failed(&request, &identity, error.reason(), error.details());
                    encode_failure_response_line(
                        &request.id,
                        error.reason(),
                        error.details(),
                        options.limits,
                    )?
                }
            };
            stream.write_all(&raw).map_err(|error| {
                SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
            })?;
            Ok(SandboxDaemonRequest { request, identity })
        }
        Err(error) => {
            log_bad_request(&identity, &error);
            let raw =
                encode_failure_response_line("", error.reason(), error.details(), options.limits)?;
            stream.write_all(&raw).map_err(|write_error| {
                SandboxError::with_details(SandboxReason::SandboxIpcFailed, write_error.to_string())
            })?;
            Err(error)
        }
    }
}

pub(crate) fn make_run_context(
    request: SandboxRunRequest,
    identity: RequestIdentity,
    options: &SandboxDaemonOptions,
) -> SandboxRunContext {
    SandboxRunContext {
        request,
        limits: options.limits,
        socket_path: Some(options.socket_path.clone()),
        request_identity: Some(identity),
        payload_identity: None,
    }
}

fn execute_request(
    request: &SandboxRunRequest,
    identity: RequestIdentity,
    options: &SandboxDaemonOptions,
) -> Result<SandboxResult, SandboxError> {
    match &options.worker_program {
        Some(worker_program) => {
            let context = make_run_context(request.clone(), identity, options);
            spawn_worker_process(worker_program, &context)
        }
        None => Ok(fake_result_for(request)),
    }
}

fn read_request(
    stream: &mut UnixStream,
    limits: SandboxLimits,
) -> Result<SandboxRunRequest, SandboxError> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; DEFAULT_READ_BUFFER_BYTES];

    loop {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                return Err(SandboxError::with_details(
                    SandboxReason::SandboxIpcTimeout,
                    error.to_string(),
                ));
            }
            Err(error) => {
                return Err(SandboxError::with_details(
                    SandboxReason::SandboxIpcFailed,
                    error.to_string(),
                ));
            }
        };
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.len() > limits.request_bytes {
            return Err(SandboxError::from_reason(SandboxReason::RequestTooLarge));
        }
        if buf.contains(&b'\n') {
            break;
        }
    }

    decode_request_line(&buf, limits)
}

fn accept_to_request_timeout() -> Duration {
    Duration::from_secs(ACCEPT_TO_REQUEST_TIMEOUT_SECS)
}

fn request_timeout(limits: SandboxLimits, timeout_s: f64) -> Duration {
    Duration::from_secs_f64(limits.clamp_timeout_s(timeout_s).max(0.001))
}

fn set_stream_timeouts(stream: &UnixStream, timeout: Duration) -> Result<(), SandboxError> {
    stream.set_read_timeout(Some(timeout)).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
    })?;
    stream.set_write_timeout(Some(timeout)).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
    })?;
    Ok(())
}

fn fake_result_for(request: &SandboxRunRequest) -> SandboxResult {
    SandboxResult {
        exit_code: 0,
        stdout: String::new(),
        stderr: format!("sandbox daemon skeleton: {}", request.command),
        changes: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        changes_truncated: false,
    }
}

fn peer_credentials(stream: &UnixStream) -> Result<RequestIdentity, SandboxError> {
    let fd = stream.as_raw_fd();
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if rc != 0 {
        return Err(SandboxError::with_details(
            SandboxReason::SandboxUnavailable,
            std::io::Error::last_os_error().to_string(),
        ));
    }

    Ok(RequestIdentity::from_peer_credentials(
        cred.pid as u32,
        cred.uid,
        cred.gid,
    ))
}

fn log_daemon_started(options: &SandboxDaemonOptions, socket_activated: bool) {
    eprintln!("{}", format_daemon_started_log(options, socket_activated));
}

fn log_request_succeeded(
    request: &SandboxRunRequest,
    identity: &RequestIdentity,
    result: &SandboxResult,
) {
    eprintln!(
        "{}",
        format_request_succeeded_log(request, identity, result)
    );
}

fn log_request_failed(
    request: &SandboxRunRequest,
    identity: &RequestIdentity,
    reason: SandboxReason,
    details: Option<&str>,
) {
    eprintln!(
        "{}",
        format_request_failed_log(request, identity, reason, details)
    );
}

fn log_bad_request(identity: &RequestIdentity, error: &SandboxError) {
    eprintln!("{}", format_bad_request_log(identity, error));
}

fn format_daemon_started_log(options: &SandboxDaemonOptions, socket_activated: bool) -> String {
    format_daemon_log_message(
        format_daemon_log_header(None, None, None, None),
        DaemonLogMessage {
            state: "listen_success",
            command: None,
            cwd: None,
            repo_root: None,
            status_bits: Vec::new(),
            reason: None,
            detail: None,
            extra_lines: vec![format!(
                "  Socket: {} | activation={} | worker={}",
                options.socket_path.display(),
                socket_activated,
                options
                    .worker_program
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "fake".to_string())
            )],
        },
    )
}

fn format_request_succeeded_log(
    request: &SandboxRunRequest,
    identity: &RequestIdentity,
    result: &SandboxResult,
) -> String {
    let mut status_bits = vec![
        format!("exit_code={}", result.exit_code),
        format!("file_changes={}", result.changes.len()),
    ];
    if result.changes_truncated {
        status_bits.push("changes_truncated=true".to_string());
    }

    let mut extra_lines = Vec::new();
    if !result.stderr.trim().is_empty() {
        extra_lines.extend(format_multiline_block("STDERR", &result.stderr));
    }

    format_daemon_log_message(
        format_daemon_log_header(
            Some(identity),
            Some(request.client_pid),
            Some(&request.id),
            None,
        ),
        DaemonLogMessage {
            state: "request_success",
            command: Some(&request.command),
            cwd: Some(request.cwd.as_path()),
            repo_root: Some(request.repo_root.as_path()),
            status_bits,
            reason: None,
            detail: None,
            extra_lines,
        },
    )
}

fn format_request_failed_log(
    request: &SandboxRunRequest,
    identity: &RequestIdentity,
    reason: SandboxReason,
    details: Option<&str>,
) -> String {
    format_daemon_log_message(
        format_daemon_log_header(
            Some(identity),
            Some(request.client_pid),
            Some(&request.id),
            None,
        ),
        DaemonLogMessage {
            state: "request_failure",
            command: Some(&request.command),
            cwd: Some(request.cwd.as_path()),
            repo_root: Some(request.repo_root.as_path()),
            status_bits: Vec::new(),
            reason: Some(reason.as_str()),
            detail: details,
            extra_lines: Vec::new(),
        },
    )
}

fn format_bad_request_log(identity: &RequestIdentity, error: &SandboxError) -> String {
    format_daemon_log_message(
        format_daemon_log_header(Some(identity), None, None, None),
        DaemonLogMessage {
            state: "request_bad_request",
            command: None,
            cwd: None,
            repo_root: None,
            status_bits: Vec::new(),
            reason: Some(error.reason().as_str()),
            detail: error.details(),
            extra_lines: Vec::new(),
        },
    )
}

fn format_daemon_log_header(
    identity: Option<&RequestIdentity>,
    client_pid: Option<u32>,
    request_id: Option<&str>,
    session: Option<&str>,
) -> String {
    let mut header = format!("sandboxd(pid={})", std::process::id());
    let mut meta = Vec::new();

    if let Some(identity) = identity {
        meta.push(format!("uid={}", identity.uid));
        meta.push(format!("gid={}", identity.gid));
        meta.push(format!("peer_pid={}", identity.pid));
    }
    if let Some(client_pid) = client_pid {
        meta.push(format!("client_pid={client_pid}"));
    }
    if let Some(request_id) = request_id {
        meta.push(format!("request_id={}", log_field(request_id)));
    }
    if let Some(session) = session {
        meta.push(format!("session={}", log_field(session)));
    }

    if !meta.is_empty() {
        header.push(' ');
        header.push_str(&meta.join(", "));
    }

    header
}

struct DaemonLogMessage<'a> {
    state: &'a str,
    command: Option<&'a str>,
    cwd: Option<&'a Path>,
    repo_root: Option<&'a Path>,
    status_bits: Vec<String>,
    reason: Option<&'a str>,
    detail: Option<&'a str>,
    extra_lines: Vec<String>,
}

fn format_daemon_log_message(header: String, message: DaemonLogMessage<'_>) -> String {
    let mut lines = vec![header];

    if let Some(command) = message.command {
        lines.push(format!("  Command: {}", log_field(command)));
    }
    if let Some(cwd) = message.cwd {
        lines.push(format!("  CWD: {}", cwd.display()));
    }
    if let Some(repo_root) = message.repo_root {
        lines.push(format!("  RepoRoot: {}", repo_root.display()));
    }

    let mut bits = vec![format!("Status: {}", message.state)];
    bits.extend(message.status_bits);
    lines.push(format!("  {}", bits.join(" | ")));

    if let Some(reason) = message.reason {
        lines.push(format!("  Reason: {}", log_field(reason)));
    }

    if let Some(detail) = message.detail {
        lines.extend(format_multiline_block("Error", detail));
    }

    lines.extend(message.extra_lines);
    lines.join("\n")
}

fn format_multiline_block(title: &str, value: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let value = value.trim_end_matches('\n');
    if value.trim().is_empty() {
        return lines;
    }

    lines.push(format!("  {title}:"));
    for raw_line in value.lines() {
        if raw_line.trim().is_empty() {
            continue;
        }
        lines.push(format!("    > {}", raw_line));
    }
    lines
}

fn log_field(value: &str) -> String {
    value.replace('\n', "\\n")
}

fn log_optional_field(value: Option<&str>) -> String {
    value.map(log_field).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{
        bind_listener, format_request_succeeded_log, make_run_context, read_request,
        serve_connection, serve_once, should_use_systemd_socket, SandboxDaemonOptions,
        DEFAULT_SOCKET_MODE,
    };
    use crate::sandbox::error::SandboxReason;
    use crate::sandbox::types::{RequestIdentity, SandboxLimits, SandboxResult, SandboxRunRequest};

    #[test]
    fn serve_connection_round_trips_request_and_fake_response() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let options = SandboxDaemonOptions::default();

        let handle = thread::spawn(move || serve_connection(server, &options));

        client
			.write_all(
				b"{\"id\":\"req-1\",\"command\":\"echo hi\",\"cwd\":\"/tmp\",\"repo_root\":\"/\",\"client_pid\":123,\"timeout_s\":12}\n",
			)
			.unwrap();
        let _ = client.shutdown(std::net::Shutdown::Write);

        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();

        let served = handle.join().unwrap().unwrap();
        assert_eq!(served.request.id, "req-1");
        assert_eq!(served.request.command, "echo hi");
        assert!(served.identity.pid > 0);
        assert!(buf.contains("\"ok\":true"));
        assert!(buf.contains("sandbox daemon skeleton: echo hi"));
    }

    #[test]
    fn serve_connection_writes_failure_for_invalid_request() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let options = SandboxDaemonOptions::default();

        let handle = thread::spawn(move || serve_connection(server, &options));

        client.write_all(b"{bad json}\n").unwrap();
        let _ = client.shutdown(std::net::Shutdown::Write);

        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();

        let error = handle.join().unwrap().unwrap_err();
        assert_eq!(error.reason(), SandboxReason::BadRequest);
        assert!(buf.contains("\"ok\":false"));
        assert!(buf.contains("\"reason\":\"bad_request\""));
    }

    #[test]
    fn read_request_maps_stalled_peer_to_ipc_timeout() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        server
            .set_read_timeout(Some(Duration::from_millis(1)))
            .unwrap();

        let error = read_request(&mut server, SandboxLimits::default()).unwrap_err();

        assert_eq!(error.reason(), SandboxReason::SandboxIpcTimeout);
    }

    #[test]
    fn bind_listener_and_serve_once_accept_unix_socket_request() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("sandbox.sock");
        let options = SandboxDaemonOptions {
            socket_path: socket_path.clone(),
            limits: SandboxLimits::default(),
            worker_program: None,
        };
        let listener = bind_listener(&socket_path).unwrap();

        let thread_options = options.clone();
        let handle = thread::spawn(move || serve_once(&listener, &thread_options));

        let mut client = UnixStream::connect(&socket_path).unwrap();
        client
			.write_all(
				b"{\"id\":\"req-2\",\"command\":\"pwd\",\"cwd\":\"/tmp/repo\",\"repo_root\":\"/\",\"client_pid\":456,\"timeout_s\":15}\n",
			)
			.unwrap();
        let _ = client.shutdown(std::net::Shutdown::Write);

        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();

        let served = handle.join().unwrap().unwrap();
        assert_eq!(served.request.id, "req-2");
        assert!(buf.contains("\"ok\":true"));

        let context = make_run_context(served.request, served.identity, &options);
        assert_eq!(context.socket_path, Some(socket_path));
        assert!(context.request_identity.is_some());
    }

    #[test]
    fn bind_listener_sets_expected_socket_mode() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("sandbox.sock");

        let _listener = bind_listener(&socket_path).unwrap();

        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, DEFAULT_SOCKET_MODE);
    }

    #[test]
    fn serve_connection_can_spawn_worker_program_when_configured() {
        let temp = tempdir().unwrap();
        let worker_path = temp.path().join("fake-worker.sh");
        std::fs::write(
            &worker_path,
            "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"result\":{\"exit_code\":0,\"stdout\":\"from-worker\",\"stderr\":\"\",\"changes\":[]}}\n'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&worker_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&worker_path, perms).unwrap();

        let (mut client, server) = UnixStream::pair().unwrap();
        let options = SandboxDaemonOptions {
            socket_path: temp.path().join("sandbox.sock"),
            limits: SandboxLimits::default(),
            worker_program: Some(worker_path),
        };

        let handle = thread::spawn(move || serve_connection(server, &options));

        client
            .write_all(
                b"{\"id\":\"req-3\",\"command\":\"echo hi\",\"cwd\":\"/tmp\",\"repo_root\":\"/\",\"client_pid\":123,\"timeout_s\":12}\n",
            )
            .unwrap();
        let _ = client.shutdown(std::net::Shutdown::Write);

        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();

        let served = handle.join().unwrap().unwrap();
        assert_eq!(served.request.id, "req-3");
        assert!(buf.contains("\"ok\":true"));
        assert!(buf.contains("from-worker"));
    }

    #[test]
    fn serve_connection_writes_failure_when_worker_execution_fails() {
        let temp = tempdir().unwrap();
        let worker_path = temp.path().join("failing-worker.sh");
        std::fs::write(
            &worker_path,
            "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":false,\"reason\":\"sandbox_unavailable\",\"error\":\"missing caps\"}\n'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&worker_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&worker_path, perms).unwrap();

        let (mut client, server) = UnixStream::pair().unwrap();
        let options = SandboxDaemonOptions {
            socket_path: temp.path().join("sandbox.sock"),
            limits: SandboxLimits::default(),
            worker_program: Some(worker_path),
        };

        let handle = thread::spawn(move || serve_connection(server, &options));

        client
            .write_all(
                b"{\"id\":\"req-4\",\"command\":\"echo hi\",\"cwd\":\"/tmp\",\"repo_root\":\"/\",\"client_pid\":123,\"timeout_s\":12}\n",
            )
            .unwrap();
        let _ = client.shutdown(std::net::Shutdown::Write);

        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();

        let served = handle.join().unwrap().unwrap();
        assert_eq!(served.request.id, "req-4");
        assert!(buf.contains("\"id\":\"req-4\""));
        assert!(buf.contains("\"ok\":false"));
        assert!(buf.contains("\"reason\":\"sandbox_unavailable\""));
        assert!(buf.contains("missing caps"));
    }

    #[test]
    fn systemd_socket_activation_env_is_strictly_scoped_to_current_process() {
        let pid = std::process::id();

        assert!(!should_use_systemd_socket(None, Some("1"), pid).unwrap());
        assert!(!should_use_systemd_socket(Some("999999"), Some("1"), pid).unwrap());
        assert!(!should_use_systemd_socket(Some(&pid.to_string()), Some("0"), pid).unwrap());
        assert!(should_use_systemd_socket(Some(&pid.to_string()), Some("1"), pid).unwrap());

        let error = should_use_systemd_socket(Some(&pid.to_string()), Some("2"), pid).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxUnavailable);
    }

    #[test]
    fn request_success_log_uses_structured_multiline_format() {
        let request = SandboxRunRequest {
            id: "req-structured".to_string(),
            command: "echo hi".to_string(),
            cwd: PathBuf::from("/tmp/repo"),
            repo_root: PathBuf::from("/"),
            client_pid: 456,
            timeout_s: 12.0,
        };
        let identity = RequestIdentity::from_peer_credentials(123, 1000, 1000);
        let result = SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: "warning: sample stderr".to_string(),
            changes: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            changes_truncated: false,
        };

        let log = format_request_succeeded_log(&request, &identity, &result);

        assert!(log.contains("sandboxd(pid="));
        assert!(log.contains("uid=1000, gid=1000, peer_pid=123, client_pid=456"));
        assert!(log.contains("request_id=req-structured"));
        assert!(log.contains("  Command: echo hi"));
        assert!(log.contains("  CWD: /tmp/repo"));
        assert!(log.contains("  RepoRoot: /"));
        assert!(log.contains("  Status: request_success | exit_code=0 | file_changes=0"));
        assert!(log.contains("  STDERR:"));
        assert!(log.contains("    > warning: sample stderr"));
    }
}
