//! RAII guard for AI operation cancellation.
//!
//! Wraps `CrosstermEscWatcher` + `tokio::select!` + `poll_cancelled` into a
//! single guard that manages the keyboard-watcher lifecycle and provides a
//! `run()` method for executing async AI futures with cancellation support.

use std::future::Future;
use std::sync::Arc;

use aish_core::AishError;
use aish_llm::CancellationToken;

use crate::esc_watcher::CrosstermEscWatcher;

/// Guard that manages keyboard-based cancellation for an AI operation.
///
/// On creation, starts a `CrosstermEscWatcher` that monitors stdin for ESC
/// and Ctrl+C. On drop (or explicit `stop()`), the watcher is stopped and
/// the terminal is restored.
///
/// Use `run()` to execute an async AI future that can be cancelled by the
/// watcher or SIGINT.
pub struct AiCancelGuard {
    watcher: CrosstermEscWatcher,
    token: Arc<CancellationToken>,
}

impl AiCancelGuard {
    /// Create a new guard, starting the ESC/Ctrl+C watcher.
    pub fn new(token: Arc<CancellationToken>) -> Self {
        let watcher = CrosstermEscWatcher::start(token.clone());
        Self { watcher, token }
    }

    /// Execute an async AI future with cancellation support.
    ///
    /// Uses `tokio::select!` to race the future against a poll loop that
    /// detects when the cancellation token has been set (by ESC, Ctrl+C, or
    /// SIGINT). Returns `Err(AishError::Cancelled)` on cancellation.
    pub fn run<F, R>(&mut self, runtime: &tokio::runtime::Runtime, future: F) -> Result<R, AishError>
    where
        F: Future<Output = Result<R, AishError>>,
    {
        let token = self.token.clone();
        runtime.block_on(async {
            tokio::select! {
                r = future => r,
                _ = poll_cancelled(token) => Err(AishError::Cancelled),
            }
        })
    }

    /// Stop the watcher and restore terminal state.
    pub fn stop(&mut self) {
        self.watcher.stop();
    }
}

impl Drop for AiCancelGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Poll the cancellation token until it is set.
async fn poll_cancelled(token: Arc<CancellationToken>) {
    while !token.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
