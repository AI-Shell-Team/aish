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
    let mut options = runtime::daemon::SandboxDaemonOptions::default();
    options.worker_program = std::env::current_exe().ok();
    if let Some(socket_path) = socket_path {
        options.socket_path = socket_path.to_path_buf();
    }

    runtime::daemon::run_forever(&options).map_err(|error| std::io::Error::other(error.to_string()))
}

pub fn run_sandbox_worker() -> std::io::Result<()> {
    runtime::worker::run_sandbox_worker_stdio()
}
