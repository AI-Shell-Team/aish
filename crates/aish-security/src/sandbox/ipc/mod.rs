pub(crate) mod client;
pub(crate) mod protocol;

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::sandbox::error::{SandboxError, SandboxReason};

/// Linux treats a zero `SO_RCVTIMEO` as "wait forever".
const MIN_SOCKET_TIMEOUT: Duration = Duration::from_millis(1);

pub(crate) fn read_within_deadline(
    stream: &mut UnixStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<usize, SandboxError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Arm the wait to whatever is left. A result already written still
        // returns from `read`; only a peer that sends nothing times out.
        let wait = if remaining < MIN_SOCKET_TIMEOUT {
            MIN_SOCKET_TIMEOUT
        } else {
            remaining
        };
        stream.set_read_timeout(Some(wait)).map_err(|error| {
            SandboxError::with_details(SandboxReason::SandboxIpcFailed, error.to_string())
        })?;

        match stream.read(buf) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == ErrorKind::Interrupted => {
                if remaining < MIN_SOCKET_TIMEOUT {
                    return Err(SandboxError::with_details(
                        SandboxReason::SandboxIpcTimeout,
                        error.to_string(),
                    ));
                }
            }
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
        }
    }
}
