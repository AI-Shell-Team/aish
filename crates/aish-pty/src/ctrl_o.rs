//! Global Ctrl+O handlers.
//!
//! Two handlers are provided:
//! - **Live handler**: called from the PTY select loop with the current exec
//!   buffer to show live bash output.
//! - **Browse handler**: called from the EscWatcher when Ctrl+O is pressed
//!   during AI streaming to browse collapsed output history.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

type LiveHandlerFn = Box<dyn FnMut(&[u8]) + Send>;
type BrowseHandlerFn = Box<dyn FnMut() + Send>;

static LIVE_HANDLER: Mutex<Option<LiveHandlerFn>> = Mutex::new(None);
static BROWSE_HANDLER: Mutex<Option<BrowseHandlerFn>> = Mutex::new(None);

/// RAII guard that puts a handler back into its static slot on drop.
///
/// This guarantees the handler is restored even if the code between take and
/// put-back panics (or the binary is compiled with `panic="abort"` and a
/// later unrelated operation panics while the handler is still out).
struct HandlerGuard<H: 'static> {
    slot: &'static Mutex<Option<H>>,
    handler: Option<H>,
}

impl<H> Drop for HandlerGuard<H> {
    fn drop(&mut self) {
        let mut guard = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        *guard = self.handler.take();
    }
}

/// Set the global Ctrl+O live handler. Called once at shell startup.
pub fn set_handler(handler: LiveHandlerFn) {
    let mut guard = LIVE_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(handler);
}

/// Set the global Ctrl+O browse handler. Called once at shell startup.
pub fn set_browse_handler(handler: BrowseHandlerFn) {
    let mut guard = BROWSE_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(handler);
}

/// Invoke the Ctrl+O live handler with the current exec buffer snapshot.
/// Called from the PTY select loop when byte 0x0F is detected.
///
/// Uses take-and-put-back to avoid holding the mutex while the
/// handler runs (which blocks on TUI input).
pub fn invoke(buffer: &[u8]) {
    take_invoke_put_back(&LIVE_HANDLER, |cb| {
        cb(buffer);
    });
}

/// Invoke the Ctrl+O browse handler (no buffer — shows history).
/// Called from the EscWatcher when byte 0x0F is detected during
/// AI streaming.
///
/// Uses the same take-and-put-back pattern as `invoke()`.
pub fn invoke_browse() {
    take_invoke_put_back(&BROWSE_HANDLER, |cb| {
        cb();
    });
}

/// Temporarily takes the handler out of the mutex, invokes the closure, then
/// puts the handler back via [`HandlerGuard`]'s [`Drop`] impl.
///
/// # Panic safety
///
/// The handler execution is wrapped in [`catch_unwind`]. If the handler
/// panics, the panic is logged and the handler is still put back into the
/// mutex so it is not permanently lost. The [`HandlerGuard`] guarantees
/// put-back even if code after the handler call panics before the guard is
/// dropped.
fn take_invoke_put_back<H>(
    static_ref: &'static Mutex<Option<H>>,
    run: impl FnOnce(&mut H),
) {
    let handler = {
        let mut guard = static_ref.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };

    // HandlerGuard's Drop will put the handler back even on panic.
    let mut hg = HandlerGuard {
        slot: static_ref,
        handler,
    };

    if let Some(ref mut cb) = hg.handler {
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(cb))) {
            tracing::warn!("ctrl_o handler panicked: {:?}", payload);
        }
    }
    // Drop of `hg` puts the handler back.
}
