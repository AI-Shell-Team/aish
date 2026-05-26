use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use richrs::spinner::Spinner;

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

    /// Start a spinner with the given label text.
    pub fn start(&self, text: &str) {
        self.stop();

        let active = self.active.clone();
        active.store(true, Ordering::SeqCst);

        let text = text.to_string();
        let start_time = Instant::now();

        let handle = thread::Builder::new()
            .name("aish-animation".into())
            .spawn(move || {
                let mut spinner = Spinner::new("dots").expect("dots spinner should exist");

                // Hide cursor
                print!("\x1b[?25l");
                let _ = io::stdout().flush();

                while active.load(Ordering::SeqCst) {
                    let frame = spinner.next_frame();
                    let elapsed = start_time.elapsed().as_secs_f64();
                    if elapsed > 0.1 {
                        print!(
                            "\r\x1b[K\x1b[34m{} {} ... {:.1}s\x1b[0m",
                            frame, text, elapsed
                        );
                    } else {
                        print!("\r\x1b[K\x1b[34m{} {}\x1b[0m", frame, text);
                    }
                    let _ = io::stdout().flush();
                    thread::sleep(Duration::from_millis(150));
                }

                // Clear spinner line and restore cursor
                print!("\r\x1b[2K\x1b[?25h");
                let _ = io::stdout().flush();
            })
            .ok();

        *self.handle.lock().unwrap() = handle;
    }

    /// Stop the spinner and clear the line.
    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().unwrap().take() {
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
