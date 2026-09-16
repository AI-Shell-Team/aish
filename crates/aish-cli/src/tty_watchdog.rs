// Terminal-death watchdog.
//
// crossterm's event source (0.28/0.29, upstream issue #793) spins at 100%
// CPU when the controlling terminal dies while the process keeps its TTY fd
// open: `poll(2)` reports permanent readability (POLLIN|POLLERR|POLLHUP) and
// every `read(2)` returns 0/EIO immediately, so the internal retry loops in
// `mio.rs` never block again. Orphaned `aish` processes therefore burn a
// full core forever after their terminal window is closed.
//
// This module runs a background thread that detects a dead controlling TTY
// and exits the process. Detection never reads the fd (a read could swallow
// user keystrokes); it polls with the `POLLHUP`/`POLLERR`/`POLLNVAL` bits
// only, so a healthy terminal reports zero events and the thread sleeps.
//
// Not armed for: `--pty-daemon` (must outlive every terminal), the sandbox
// daemon/worker (same), and non-TTY stdio (piped `aish --help`, CI).

use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often the watchdog re-checks the controlling terminal.
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Set once the watchdog has been started; guards against double-spawn.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Arm the terminal-death watchdog for this process.
///
/// Does nothing when stdin is not a terminal (piped input, CI), so
/// non-interactive invocations keep their current behavior. Call once from
/// `main` before entering any interactive loop.
pub fn arm_for_interactive() {
    use std::io::IsTerminal;
    // stdin (fd 0) is what crossterm reads events from; that is the fd whose
    // death strands the event loop. Only a real TTY needs a watchdog.
    if !std::io::stdin().is_terminal() {
        return;
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("tty-watchdog".into())
        .spawn(spawn_watchdog)
        .expect("failed to spawn tty watchdog thread");
}

/// Explicitly do NOT arm the watchdog: used by daemon entries that must
/// outlive any terminal (PTY daemon, sandbox daemon/worker).
pub fn disarm() {
    STARTED.store(true, Ordering::SeqCst);
}

fn spawn_watchdog() {
    loop {
        match probe_controlling_tty() {
            TtyState::Alive => {}
            TtyState::Dead(reason) => {
                tracing::warn!(
                    "Terminal is gone ({}); exiting aish to avoid a busy event loop",
                    reason
                );
                // Give tracing a moment to flush the line before exiting.
                flush_stderr();
                std::process::exit(0);
            }
        }
        std::thread::sleep(WATCHDOG_POLL_INTERVAL);
    }
}

enum TtyState {
    Alive,
    /// Human-readable reason: hangup/error reported, or fd no longer a TTY.
    Dead(&'static str),
}

/// Inspect stdin without reading from it.
///
/// Uses `poll(2)` with POLLIN requested but classifies the fd as dead only on
/// hangup-class revents (POLLHUP/POLLERR/POLLNVAL). A readable-but-empty
/// condition alone (POLLIN with no hangup bits) can legitimately happen on a
/// live terminal, so it is treated as alive; the crossterm loop will block
/// again on the next iteration.
fn probe_controlling_tty() -> TtyState {
    const POLLIN: libc::c_short = 0x001;
    const POLLERR: libc::c_short = 0x008;
    const POLLHUP: libc::c_short = 0x010;
    const POLLNVAL: libc::c_short = 0x020;

    let mut fds = [libc::pollfd {
        fd: std::io::stdin().as_raw_fd(),
        events: POLLIN,
        revents: 0,
    }];
    // Timeout 0: non-blocking peek. The watchdog thread re-probes on its own
    // cadence, so no sleeping happens inside poll itself.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) };
    if rc < 0 {
        // poll itself failed (EINTR etc.); assume alive, retry next tick.
        return TtyState::Alive;
    }
    if rc == 0 {
        // No events: healthy, nothing readable.
        return TtyState::Alive;
    }
    let revents = fds[0].revents;
    if revents & (POLLHUP | POLLERR | POLLNVAL) != 0 {
        return TtyState::Dead("tty hangup");
    }
    // POLLIN without hangup bits: treated as alive. A live terminal can
    // report readable input at any time; crossterm consumes it and blocks
    // again. False "alive" verdicts here only cost one extra 2 s tick.
    TtyState::Alive
}

/// Best-effort flush of the tracing writer so the exit reason is visible.
fn flush_stderr() {
    use std::io::Write;
    let _ = std::io::stderr().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disarm_prevents_double_spawn() {
        // The atomic guard is process-global; assert idempotence of the flag.
        disarm();
        assert!(STARTED.load(Ordering::SeqCst));
    }
}
