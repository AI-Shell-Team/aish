use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::theme;

/// Global lock that serializes animation frame rendering against ad-hoc
/// terminal writes (notably tracing log lines). Both the animation threads
/// and [`LogLineWriter`] must hold this lock while emitting bytes to stdout,
/// guaranteeing a log event can never interleave with a partial frame.
static FRAME_LOCK: Mutex<()> = Mutex::new(());

/// Whether an animation frame is currently on screen. Read by
/// [`LogLineWriter`] to decide whether the current line must be wiped
/// before a log line is emitted.
static ANIMATION_FRAME_VISIBLE: AtomicBool = AtomicBool::new(false);

/// `io::Write` adapter used by the tracing subscriber (via `MakeWriter`) so
/// log lines never glue onto an in-flight animation frame. When a frame is
/// visible, the line is cleared first; the animation redraws on its next
/// tick once the log line (terminated by `\n`) has been emitted.
pub struct LogLineWriter {
    started: bool,
}

impl LogLineWriter {
    pub fn new() -> Self {
        Self { started: false }
    }
}

impl Default for LogLineWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl io::Write for LogLineWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _guard = FRAME_LOCK.lock();
        if !self.started {
            self.started = true;
            if ANIMATION_FRAME_VISIBLE.load(Ordering::SeqCst) {
                // Wipe the partially rendered animation line so the log
                // starts on a fresh, independent line instead of gluing
                // onto "Thinking 1.7s".
                print!("\r\x1b[2K");
                ANIMATION_FRAME_VISIBLE.store(false, Ordering::SeqCst);
            }
        }
        io::stdout().write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stdout().flush();
        Ok(())
    }
}
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
                let _frame = FRAME_LOCK.lock();
                // Hide cursor
                print!("\x1b[?25l");
                let _ = io::stdout().flush();
                drop(_frame);

                while active.load(Ordering::SeqCst) {
                    // Auto-stop when a tool needs interactive terminal input
                    // (sudo password, ssh, vim, etc.) so the prompt is visible.
                    if aish_tools::bash::interactive_input_active() {
                        break;
                    }
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let time_ms = start_time.elapsed().as_millis() as u64;
                    let shimmered = theme::shimmer_text(&text, time_ms);
                    {
                        let _frame = FRAME_LOCK.lock();
                        if elapsed > 0.1 {
                            print!(
                                "\r\x1b[K{} {}",
                                shimmered,
                                theme::dim(&format!("{:.1}s", elapsed))
                            );
                        } else {
                            print!("\r\x1b[K{}", shimmered);
                        }
                        ANIMATION_FRAME_VISIBLE.store(true, Ordering::SeqCst);
                        let _ = io::stdout().flush();
                    }
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
                let _frame = FRAME_LOCK.lock();
                print!("\r\x1b[2K\x1b[?25h");
                ANIMATION_FRAME_VISIBLE.store(false, Ordering::SeqCst);
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
                {
                    let _frame = FRAME_LOCK.lock();
                    print!("\x1b[?25l");
                    let _ = io::stdout().flush();
                }

                while active.load(Ordering::SeqCst) {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let time_ms = start_time.elapsed().as_millis() as u64;
                    let shimmered = theme::shimmer_text(&label, time_ms);
                    {
                        let _frame = FRAME_LOCK.lock();
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
                        ANIMATION_FRAME_VISIBLE.store(true, Ordering::SeqCst);
                        let _ = io::stdout().flush();
                    }
                    for _ in 0..10 {
                        if !active.load(Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }

                {
                    let _frame = FRAME_LOCK.lock();
                    print!("\r\x1b[2K\x1b[?25h");
                    ANIMATION_FRAME_VISIBLE.store(false, Ordering::SeqCst);
                    let _ = io::stdout().flush();
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for issue #490: log lines written through
    /// [`LogLineWriter`] while an animation is active must not deadlock and
    /// must complete (frame lock serializes animation frames against log
    /// writes). Output goes to the test-captured stdout; we assert the
    /// interactions finish and the visibility flag is reset after stop.
    #[test]
    fn log_writer_and_animation_do_not_deadlock_or_leave_frame_visible() {
        let anim = SharedAnimation::new();
        anim.start("思考中");

        let writer_threads: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(|| {
                    for _ in 0..50 {
                        let mut w = LogLineWriter::new();
                        w.write_all(b"2026-01-01T00:00:00Z  WARN test: log line\n")
                            .unwrap();
                        w.flush().unwrap();
                    }
                })
            })
            .collect();

        // Give writers time to interleave with animation frames.
        thread::sleep(Duration::from_millis(200));
        anim.stop();

        for t in writer_threads {
            t.join().expect("writer threads must finish");
        }

        assert!(
            !ANIMATION_FRAME_VISIBLE.load(Ordering::SeqCst),
            "animation stop must clear the frame-visible flag"
        );
    }
}
