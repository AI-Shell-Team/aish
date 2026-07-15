use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::ipc::protocol::{decode_response_line, encode_request_line, SandboxIpcRequest};
use crate::sandbox::types::{SandboxLimits, SandboxResult, SandboxRunRequest};

const DEFAULT_READ_BUFFER_BYTES: usize = 64 * 1024;

pub(crate) trait SandboxRunner: Send + Sync {
    fn simulate(&self, request: &SandboxRunRequest) -> Result<SandboxResult, SandboxError>;
}

#[derive(Debug, Clone)]
pub struct SandboxClient {
    socket_path: PathBuf,
    limits: SandboxLimits,
}

impl SandboxClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            limits: SandboxLimits::default(),
        }
    }

    pub(crate) fn with_limits(mut self, limits: SandboxLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn connect(&self, request: &SandboxRunRequest) -> Result<UnixStream, SandboxError> {
        let stream = UnixStream::connect(&self.socket_path).map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxIpcUnavailable, error.to_string())
        })?;

        let timeout = Duration::from_secs_f64(self.limits.clamp_timeout_s(request.timeout_s));
        stream.set_read_timeout(Some(timeout)).map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
        })?;
        stream.set_write_timeout(Some(timeout)).map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
        })?;

        Ok(stream)
    }

    fn read_response(&self, stream: &mut UnixStream) -> Result<Vec<u8>, SandboxError> {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; DEFAULT_READ_BUFFER_BYTES];

        loop {
            let read = match stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
                {
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
            if buf.len() > self.limits.response_bytes {
                return Err(SandboxError::with_details(
                    SandboxReason::SandboxIpcProtocolError,
                    "response_too_large",
                ));
            }
            if buf.contains(&b'\n') {
                break;
            }
        }

        Ok(buf)
    }
}

impl SandboxRunner for SandboxClient {
    fn simulate(&self, request: &SandboxRunRequest) -> Result<SandboxResult, SandboxError> {
        let mut stream = self.connect(request)?;
        let raw = encode_request_line(&SandboxIpcRequest::from(request.clone()), self.limits)?;

        stream.write_all(&raw).map_err(|error| match error.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => {
                SandboxError::with_details(SandboxReason::SandboxIpcTimeout, error.to_string())
            }
            _ => SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string()),
        })?;

        let response = self.read_response(&mut stream)?;
        decode_response_line(&response, &request.id, self.limits)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{SandboxClient, SandboxRunner};
    use crate::sandbox::error::SandboxReason;
    use crate::sandbox::ipc::protocol::{decode_request_line, encode_success_response_line};
    use crate::sandbox::runtime::daemon::{bind_listener, serve_once, SandboxDaemonOptions};
    use crate::sandbox::types::{
        FsChange, FsChangeKind, SandboxLimits, SandboxResult, SandboxRunRequest,
    };

    fn sample_request() -> SandboxRunRequest {
        SandboxRunRequest {
            id: "req-1".to_string(),
            command: "echo hi".to_string(),
            cwd: PathBuf::from("/tmp/repo"),
            repo_root: PathBuf::from("/"),
            client_pid: 4242,
            timeout_s: 1.0,
        }
    }

    use std::path::PathBuf;

    #[test]
    fn simulate_round_trips_with_fake_server() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("sandbox.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let request = sample_request();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..read]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            let decoded = decode_request_line(&buf, SandboxLimits::default()).unwrap();
            assert_eq!(decoded.id, "req-1");

            let result = SandboxResult {
                exit_code: 0,
                stdout: "ok".to_string(),
                stderr: String::new(),
                changes: vec![FsChange {
                    path: "/tmp/repo/file.txt".to_string(),
                    kind: FsChangeKind::Created,
                    detail: None,
                }],
                stdout_truncated: false,
                stderr_truncated: false,
                changes_truncated: false,
            };
            let raw =
                encode_success_response_line("req-1", &result, SandboxLimits::default()).unwrap();
            stream.write_all(&raw).unwrap();
        });

        let client = SandboxClient::new(&socket_path);
        let result = client.simulate(&request).unwrap();
        assert_eq!(result.stdout, "ok");
        assert_eq!(result.changes[0].kind, FsChangeKind::Created);

        handle.join().unwrap();
    }

    #[test]
    fn simulate_maps_connect_failure_to_ipc_unavailable() {
        let client = SandboxClient::new("/tmp/does-not-exist/aish.sock");
        let error = client.simulate(&sample_request()).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcUnavailable);
    }

    #[test]
    fn simulate_maps_read_timeout_to_ipc_timeout() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("sandbox.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            // Sleep well beyond the clamped timeout (1s minimum) so the
            // client read reliably times out regardless of CI scheduling.
            thread::sleep(Duration::from_secs(3));
        });

        let request = SandboxRunRequest {
            timeout_s: 0.01,
            ..sample_request()
        };
        let client = SandboxClient::new(&socket_path);
        let error = client.simulate(&request).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcTimeout);

        handle.join().unwrap();
    }

    #[test]
    fn simulate_surfaces_id_mismatch_as_protocol_error() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("sandbox.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let request = sample_request();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).unwrap();
            stream
				.write_all(
					b"{\"id\":\"wrong-id\",\"ok\":true,\"result\":{\"exit_code\":0,\"stdout\":\"\",\"stderr\":\"\",\"changes\":[]}}\n",
				)
				.unwrap();
        });

        let client = SandboxClient::new(&socket_path);
        let error = client.simulate(&request).unwrap_err();
        assert_eq!(error.reason(), SandboxReason::SandboxIpcProtocolError);
        assert_eq!(error.details(), Some("id_mismatch"));

        handle.join().unwrap();
    }

    #[test]
    fn simulate_round_trips_with_daemon_skeleton() {
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

        let client = SandboxClient::new(&socket_path);
        let result = client.simulate(&sample_request()).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stderr.contains("sandbox daemon skeleton: echo hi"));

        let served = handle.join().unwrap().unwrap();
        assert_eq!(served.request.command, "echo hi");
        assert!(served.identity.pid > 0);
    }
}
