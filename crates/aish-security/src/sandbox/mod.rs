#![allow(dead_code)]

use std::path::Path;

pub(crate) mod assess;
pub(crate) mod degraded;
pub(crate) mod error;
pub(crate) mod ipc;
pub(crate) mod runtime;
pub(crate) mod types;

pub use ipc::client::SandboxClient;

pub fn run_sandbox_daemon(socket_path: Option<&Path>) -> std::io::Result<()> {
    let options = runtime::daemon::SandboxDaemonOptions {
        worker_program: std::env::current_exe().ok(),
        socket_path: socket_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| runtime::daemon::DEFAULT_SANDBOX_SOCKET_PATH.into()),
        ..Default::default()
    };

    runtime::daemon::run_forever(&options).map_err(|error| std::io::Error::other(error.to_string()))
}

pub fn run_sandbox_worker() -> std::io::Result<()> {
    runtime::worker::run_sandbox_worker_stdio()
}
