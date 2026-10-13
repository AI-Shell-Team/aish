//! Cancellation bridge between aish's [`aish_llm::CancellationToken`] and the
//! async MCP transports.
//!
//! aish cancels work by setting a flag on a shared token and polls it (see
//! `aish_llm::agents::spawn::forward_cancellation`, which polls every 10ms).
//! MCP follows the same convention instead of registering per-call callbacks:
//! callbacks accumulate on the session token for the lifetime of the shell,
//! while a 10ms poll costs one timer per in-flight call and keeps cancellation
//! latency below the shell's own Esc handling granularity.

use std::sync::Arc;
use std::time::Duration;

use aish_llm::CancellationToken;

/// Poll interval while waiting for cancellation.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Cheap cancellation handle handed to transports and the manager.
pub struct CancelListener {
    token: Arc<CancellationToken>,
}

impl CancelListener {
    pub fn new(token: Arc<CancellationToken>) -> Arc<Self> {
        Arc::new(Self { token })
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolve as soon as the token is cancelled.
    pub async fn cancelled(&self) {
        while !self.token.is_cancelled() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_after_cancel() {
        let token = Arc::new(CancellationToken::new());
        let listener = CancelListener::new(token.clone());
        assert!(!listener.is_cancelled());
        token.cancel();
        tokio::time::timeout(Duration::from_secs(1), listener.cancelled())
            .await
            .expect("listener must resolve after cancel");
        assert!(listener.is_cancelled());
    }
}
