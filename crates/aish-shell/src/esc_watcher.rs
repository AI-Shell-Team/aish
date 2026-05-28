use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aish_llm::CancellationToken;
use aish_tools::bash::interactive_input_active;

/// Crossterm-based ESC key watcher using the keyboard module.
///
/// Uses `InputRawGuard` for terminal mode switching, which preserves output
/// processing (OPOST) so that `println!()` works correctly during AI streaming.
pub struct CrosstermEscWatcher {
    _guard: Option<crate::keyboard::InputRawGuard>,
    thread: Option<std::thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

impl CrosstermEscWatcher {
    /// Start watching for ESC keypresses using the keyboard module.
    ///
    /// If the terminal cannot be switched to input-raw mode (e.g. stdin is
    /// not a tty), returns a no-op watcher.
    pub fn start(token: Arc<CancellationToken>) -> Self {
        let guard = match crate::keyboard::InputRawGuard::enter() {
            Ok(g) => Some(g),
            Err(_) => {
                return Self {
                    _guard: None,
                    thread: None,
                    stop_flag: Arc::new(AtomicBool::new(false)),
                };
            }
        };

        let stop_flag = Arc::new(AtomicBool::new(false));
        let bindings = crate::keyboard::default_bindings();

        let stop_clone = stop_flag.clone();
        let token_clone = token.clone();
        let spawn_result = std::thread::Builder::new()
            .name("crossterm-esc-watcher".into())
            .spawn(move || {
                use std::time::Duration;

                loop {
                    if stop_clone.load(Ordering::Acquire) {
                        return;
                    }

                    // Yield stdin to interactive tools (ask_user, interactive
                    // bash) that hold the InteractiveInputGuard.
                    if interactive_input_active() {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }

                    match crate::keyboard::read_event(Some(Duration::from_millis(100))) {
                        Ok(Some(crate::keyboard::ShellEvent::Key(key))) => {
                            use crossterm::event::KeyEventKind;

                            // Only handle press events
                            if key.kind != KeyEventKind::Press {
                                continue;
                            }

                            // Check if this key maps to a cancel/interrupt action
                            if let Some(action) = bindings.resolve(&key) {
                                if action == "cancel" || action == "interrupt" {
                                    if !token_clone.is_cancelled() {
                                        token_clone.cancel();
                                    }
                                    return;
                                }
                                // Ctrl+O: browse collapsed output history.
                                // The browse handler runs a TUI panel
                                // (PanelRuntime) which calls crossterm
                                // enable_raw_mode/disable_raw_mode internally.
                                // Crossterm saves the current termios (our
                                // input-raw mode from InputRawGuard) and
                                // restores it when the panel closes, so the
                                // watcher loop continues normally afterwards.
                                if action == "browse" {
                                    aish_pty::ctrl_o::invoke_browse();
                                    continue;
                                }
                            }

                        }
                        Ok(Some(_)) => {
                            // Ignore mouse and resize events
                        }
                        Ok(None) => {
                            // Timeout - continue loop
                        }
                        Err(_) => {
                            // Error reading event - continue loop
                        }
                    }
                }
            });

        match spawn_result {
            Ok(handle) => Self {
                _guard: guard,
                thread: Some(handle),
                stop_flag,
            },
            Err(e) => {
                tracing::warn!("failed to spawn crossterm-esc-watcher thread: {e}");
                // Drop the guard explicitly so terminal settings are restored,
                // then return a no-op watcher.
                drop(guard);
                Self {
                    _guard: None,
                    thread: None,
                    stop_flag,
                }
            }
        }
    }

    /// Stop the listener thread and restore terminal settings.
    pub fn stop(&mut self) {
        self.cleanup();
    }

    fn cleanup(&mut self) {
        self.stop_flag.store(true, Ordering::Release);

        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }

        // InputRawGuard::drop() restores the original terminal settings.
        let _ = self._guard.take();
    }
}

impl Drop for CrosstermEscWatcher {
    fn drop(&mut self) {
        self.cleanup();
    }
}
