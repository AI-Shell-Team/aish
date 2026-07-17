use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::theme;

/// Thread-safe animation wrapper that displays a spinner with elapsed time
/// in a background thread.
///
/// Usage: `start(text)` to begin, `stop()` to end. Thread-safe via interior
/// mutability — wrap in `Arc` for sharing across threads.
pub struct SharedAnimation {
    active: Arc<AtomicBool>,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SharedAnimation {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        }
    }

    /// Start a shimmer animation with the given label text.
    pub fn start(&self, text: &str) {
        self.stop();

        let active = self.active.clone();
        active.store(true, Ordering::SeqCst);

        let text = text.to_string();
        let start_time = Instant::now();

        let handle = thread::Builder::new()
            .name("aish-animation".into())
            .spawn(move || {
                // Hide cursor
                print!("\x1b[?25l");
                let _ = io::stdout().flush();

                while active.load(Ordering::SeqCst) {
                    // Auto-stop when a tool needs interactive terminal input
                    // (sudo password, ssh, vim, etc.) so the prompt is visible.
                    if aish_tools::bash::interactive_input_active() {
                        break;
                    }
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let time_ms = start_time.elapsed().as_millis() as u64;
                    let shimmered = theme::shimmer_text(&text, time_ms);
                    if elapsed > 0.1 {
                        print!(
                            "\r\x1b[K{} {}",
                            shimmered,
                            theme::dim(&format!("{:.1}s", elapsed))
                        );
                    } else {
                        print!("\r\x1b[K{}", shimmered);
                    }
                    let _ = io::stdout().flush();
                    for _ in 0..10 {
                        if !active.load(Ordering::SeqCst)
                            || aish_tools::bash::interactive_input_active()
                        {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }

                // Clear animation line and restore cursor
                print!("\r\x1b[2K\x1b[?25h");
                let _ = io::stdout().flush();
            })
            .ok();

        *self.handle.lock() = handle;
    }

    /// Stop the animation and clear the line.
    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().take() {
            let _ = h.join();
        }
    }
}

impl Default for SharedAnimation {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SharedAnimation {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Shimmer animation for sub-agent thinking lines (`  └─ explore · 思考中 2.1s`).
///
/// Separate from [`SharedAnimation`] so the main-loop animation can stay off
/// while sub-agent progress remains visibly alive.
pub struct SubAgentThinkingAnimation {
    active: Arc<AtomicBool>,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SubAgentThinkingAnimation {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        }
    }

    /// Animate `prefix` (dimmed) + shimmer sweep on `label`.
    pub fn start(&self, prefix: &str, label: &str) {
        self.stop();

        let active = self.active.clone();
        active.store(true, Ordering::SeqCst);

        let prefix = theme::dim(prefix);
        let label = label.to_string();
        let start_time = Instant::now();

        let handle = thread::Builder::new()
            .name("aish-subagent-animation".into())
            .spawn(move || {
                print!("\x1b[?25l");
                let _ = io::stdout().flush();

                while active.load(Ordering::SeqCst) {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let time_ms = start_time.elapsed().as_millis() as u64;
                    let shimmered = theme::shimmer_text(&label, time_ms);
                    if elapsed > 0.1 {
                        print!(
                            "\r\x1b[K{}{} {}",
                            prefix,
                            shimmered,
                            theme::dim(&format!("{:.1}s", elapsed))
                        );
                    } else {
                        print!("\r\x1b[K{}{}", prefix, shimmered);
                    }
                    let _ = io::stdout().flush();
                    for _ in 0..10 {
                        if !active.load(Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }

                print!("\r\x1b[2K\x1b[?25h");
                let _ = io::stdout().flush();
            })
            .ok();

        *self.handle.lock() = handle;
    }

    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().take() {
            let _ = h.join();
        }
    }
}

impl Default for SubAgentThinkingAnimation {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SubAgentThinkingAnimation {
    fn drop(&mut self) {
        self.stop();
    }
}
