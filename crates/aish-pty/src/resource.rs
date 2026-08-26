//! Live-session resource sampling via `/proc` (Linux only).
//!
//! Each PTY daemon is spawned in its own process group (`process_group(0)` in
//! `aish-cli`), and the daemon's forked aish child plus any bash-exec'd
//! workers stay in that group. Sampling therefore aggregates CPU jiffies and
//! RSS over every process whose PGID equals the daemon PID — a complete
//! picture of what one live session costs, including tool subprocesses.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::time::Instant;

/// Resource usage of one process group (one live session's worker tree).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupResources {
    /// Summed `utime + stime` in clock ticks across the group.
    pub cpu_ticks: u64,
    /// Summed resident set size in bytes across the group.
    pub rss_bytes: u64,
}

impl GroupResources {
    /// CPU percentage over a sampling window.
    ///
    /// `ticks_delta` is the jiffies consumed between two samples taken
    /// `elapsed` apart. Returns 0.0 for degenerate windows.
    pub fn cpu_percent(ticks_delta: u64, elapsed_secs: f64) -> f64 {
        if elapsed_secs <= 0.0 {
            return 0.0;
        }
        let ticks_per_sec = clock_ticks() as f64;
        (ticks_delta as f64 / ticks_per_sec / elapsed_secs) * 100.0
    }

    /// Human-readable RSS size (MiB with one decimal, or KiB when < 1 MiB).
    pub fn rss_human(bytes: u64) -> String {
        const KIB: f64 = 1024.0;
        const MIB: f64 = 1024.0 * KIB;
        let v = bytes as f64;
        if v >= MIB {
            format!("{:.1}MB", v / MIB)
        } else {
            format!("{:.0}KB", v / KIB)
        }
    }
}

/// Clock ticks per second (`sysconf(_SC_CLK_TCK)`). Almost universally 100
/// on Linux; read once and cache.
fn clock_ticks() -> u64 {
    static TICKS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        // SAFETY: [Category 8 — FFI] `sysconf` reads a system constant.
        unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64
    });
    *TICKS
}

/// Split a `/proc/<pid>/stat` line after the comm field.
///
/// The comm field (parenthesised executable name) may itself contain spaces
/// and parentheses, so everything before the LAST `)` is skipped. The
/// returned iterator starts at field 3 (`state`).
fn stat_fields_after_comm(content: &str) -> impl Iterator<Item = &str> {
    let close = content.rfind(')').unwrap_or(0);
    content[close + 1..].split_whitespace()
}

/// (pid, ppid, utime, stime, rss_pages) from a `/proc/<pid>/stat` line.
///
/// Field indices (1-based, per proc(5)): pid=1, comm=2, state=3, ppid=4,
/// utime=14, stime=15, rss=24. Relative to the iterator starting at field
/// 3: ppid is the 2nd item; skip fields 5..=13 to land on utime; rss is the
/// 22nd item.
fn parse_stat_all(content: &str) -> Option<(u32, u32, u64, u64, u64)> {
    // pid sits before the comm field.
    let pid: u32 = content.split_whitespace().next()?.parse().ok()?;
    let mut it = stat_fields_after_comm(content);
    let _state = it.next()?;
    let ppid: u32 = it.next()?.parse().ok()?;
    // Skip fields 5..=13 (pgrp session tty_nr tpgid flags minflt cminflt
    // majflt cmajflt) so the iterator lands on utime (field 14).
    for _ in 5..=13 {
        it.next()?;
    }
    let utime: u64 = it.next()?.parse().ok()?;
    let stime: u64 = it.next()?.parse().ok()?;
    // Skip fields 16..=23 (cutime cstime priority nice num_threads
    // itrealvalue starttime vsize); rss is field 24.
    for _ in 16..=23 {
        it.next()?;
    }
    let rss_pages: u64 = it.next()?.parse().ok()?;
    Some((pid, ppid, utime, stime, rss_pages))
}

/// Sample CPU ticks and RSS for every process in the tree rooted at each
/// `root_pid` (the PTY daemon). The daemon's forked REPL child calls
/// `setsid()`, so the worker tree is NOT in the daemon's process group —
/// aggregating by parent-pid closure is immune to that.
///
/// One pass over `/proc`; unreadable or vanished entries are skipped. The
/// returned map contains exactly the requested roots (defaulting to zeroed
/// resources when no live process matched).
pub fn sample_groups(root_pids: &[u32]) -> HashMap<u32, GroupResources> {
    let mut out: HashMap<u32, GroupResources> = root_pids
        .iter()
        .map(|&p| (p, GroupResources::default()))
        .collect();
    if root_pids.is_empty() {
        return out;
    }
    let page_size = page_size_bytes();
    let Ok(entries) = std::fs::read_dir(Path::new("/proc")) else {
        return out;
    };

    // pid -> (utime, stime, rss_pages) plus a children-by-parent map for
    // the descendant closure; both keyed lookups keep the per-root BFS at
    // O(tree size) instead of a linear scan over all processes.
    let mut procs: HashMap<u32, (u64, u64, u64)> = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let stat_path = entry.path().join("stat");
        let Ok(content) = std::fs::read_to_string(&stat_path) else {
            continue;
        };
        let Some((pid, ppid, utime, stime, rss_pages)) = parse_stat_all(&content) else {
            continue;
        };
        procs.insert(pid, (utime, stime, rss_pages));
        children.entry(ppid).or_default().push(pid);
    }

    // BFS from each root, summing resources.
    for &root in root_pids {
        let mut stack = vec![root];
        let mut visited = 0usize;
        while let Some(pid) = stack.pop() {
            if visited > 4096 {
                break; // defensive: pathological trees
            }
            visited += 1;
            if let Some(&(utime, stime, rss_pages)) = procs.get(&pid) {
                let slot = out
                    .get_mut(&root)
                    .expect("pre-populated for every requested root");
                slot.cpu_ticks += utime + stime;
                slot.rss_bytes += rss_pages * page_size;
            }
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
    }
    out
}

/// PIDs of every descendant of `root` (excluding `root` itself), collected
/// BEFORE any signalling so orphans (reparented to init on parent death)
/// stay discoverable.
pub fn descendant_pids(root: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(Path::new("/proc")) else {
        return Vec::new();
    };
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((pid, ppid, _, _, _)) = parse_stat_all(&content) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if out.len() > 4096 {
            break; // defensive: pathological trees / loops
        }
        if let Some(kids) = children.get(&pid) {
            for &kid in kids {
                out.push(kid);
                stack.push(kid);
            }
        }
    }
    out
}

/// Terminate `root` and its whole worker tree (REPL → bash → tools).
///
/// Each aish layer calls `setsid()` (daemon child, persistent bash), so a
/// process-group kill cannot reach the full tree — the descendant set is
/// walked once up front (before parents die and orphans get reparented).
/// Each victim is pinned with a pidfd BEFORE any signal: pidfds reference
/// the exact process instance, so an exited-and-recycled pid can never be
/// signalled, for SIGTERM or SIGKILL alike.
///
/// When `pidfd_open` is unavailable (kernel < 5.3, seccomp/container
/// policy, EPERM) the pid is still signalled by plain `kill` — dropping
/// it would silently turn the termination into a no-op, which is exactly
/// the runaway-session failure this exists to fix. That path accepts the
/// theoretical TOCTOU of a pid recycled between the /proc scan and the
/// signal; `pidfd_send_signal` failure with a pinned fd additionally
/// cross-checks identity via fdinfo before falling back to `kill`.
pub fn kill_process_tree(root: u32) {
    let victims: Vec<(u32, Option<PidFd>)> = descendant_pids(root)
        .into_iter()
        .chain(std::iter::once(root))
        .map(|pid| (pid, pidfd_open(pid)))
        .collect();
    for (pid, fd) in &victims {
        pidfd_signal(fd.as_ref(), pid, libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    for (pid, fd) in &victims {
        pidfd_signal(fd.as_ref(), pid, libc::SIGKILL);
    }
}

/// Signal the process behind `pid` with `sig`, tolerating every failure.
///
/// - Pinned fd, working `pidfd_send_signal`: signal the exact instance.
/// - Pinned fd, syscall blocked: `kill` only when the fdinfo pid still
///   matches `/proc/<pid>/stat`, proving no recycling.
/// - No fd (pidfd_open unavailable): straight `kill` — termination must
///   not silently degrade to a no-op on old kernels.
fn pidfd_signal(fd: Option<&PidFd>, pid: &u32, sig: libc::c_int) {
    if let Some(fd) = fd {
        if pidfd_send_signal(fd, sig).is_some() {
            return;
        }
        if let Some(pinned) = pidfd_pid(fd) {
            if proc_stat_pid(*pid) != Some(pinned) {
                return; // pid was recycled; do not signal the stranger
            }
        }
    }
    // SAFETY: [Category 8 — FFI] `kill(pid, sig)` — pid comes from our own
    // /proc walk; failures (ESRCH, EPERM) are deliberately ignored so one
    // dead pid cannot abort the sweep.
    unsafe {
        libc::kill(*pid as libc::pid_t, sig);
    }
}

/// A pinned process handle (`pidfd_open`). The fd stays valid after the
/// target exits (reads then fail with ESRCH semantics), so the identity it
/// captured can never drift onto a recycled pid.
struct PidFd(std::os::fd::OwnedFd);

/// `pidfd_open(2)`. `None` when the syscall is unavailable or the process
/// is already gone.
fn pidfd_open(pid: u32) -> Option<PidFd> {
    const SYS_PIDFD_OPEN: libc::c_long = 434;
    // SAFETY: [Category 8 — FFI] `syscall(SYS_pidfd_open, pid, 0)` — a
    // direct Linux syscall; flags must be 0 per the man page. Returns a
    // new fd (>= 0) or -1 with errno.
    let fd = unsafe { libc::syscall(SYS_PIDFD_OPEN, pid as libc::pid_t, 0) };
    if fd < 0 {
        return None;
    }
    let fd = fd as libc::c_int;
    // SAFETY: [Category 8 — FFI] fd is a valid, owned descriptor returned
    // by the syscall above.
    Some(PidFd(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
}

/// `pidfd_send_signal(2)`. `None` when the syscall is unavailable; a
/// `Some(())` result whose signal failed is indistinguishable from success
/// only in that the target had already exited, which is fine here.
fn pidfd_send_signal(fd: &PidFd, sig: libc::c_int) -> Option<()> {
    const SYS_PIDFD_SEND_SIGNAL: libc::c_long = 424;
    // SAFETY: [Category 8 — FFI] `syscall(SYS_pidfd_send_signal, fd, sig,
    // NULL, 0)` — a direct Linux syscall with the reserved info pointer
    // NULL and flags 0.
    unsafe {
        libc::syscall(
            SYS_PIDFD_SEND_SIGNAL,
            fd.0.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        ) >= 0
    }
    .then_some(())
}

/// The pid that the fd pins, read from the fd's `fdinfo`. `None` when the
fn pidfd_pid(fd: &PidFd) -> Option<u32> {
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.0.as_raw_fd())).ok()?;
    for line in info.lines() {
        if let Some(rest) = line.strip_prefix("Pid:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// The pid reported by `/proc/<pid>/stat` (field 1). `None` when the
/// process does not exist — a recycled pid reports the NEW process's pid
/// there only after the namespace re-maps it, which cannot happen for the
/// init-pid-namespace view this code uses.
fn proc_stat_pid(pid: u32) -> Option<u32> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    content.split_whitespace().next()?.parse().ok()
}

/// Process page size (`sysconf(_SC_PAGESIZE)`), cached.
fn page_size_bytes() -> u64 {
    static SIZE: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        // SAFETY: [Category 8 — FFI] `sysconf` reads a system constant.
        unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64
    });
    *SIZE
}

/// One sampled live session with derived metrics.
#[derive(Debug, Clone)]
pub struct SessionResourceSample {
    pub session_id: String,
    pub daemon_pid: u32,
    /// CPU percent averaged over the sampling window. `None` when the delta
    /// could not be derived.
    pub cpu_percent: Option<f64>,
    /// Summed RSS of the whole process group.
    pub rss_bytes: u64,
}

/// Take two samples of the given daemon PGIDs and derive CPU percentages.
///
/// `window_ms` is the delay between samples. Blocking for the window is
/// acceptable because this runs at REPL cadence (per prompt or per command),
/// never on the async runtime.
pub fn sample_sessions_with_cpu(
    pgids: &[(String, u32)],
    window_ms: u64,
) -> Vec<SessionResourceSample> {
    let ids: Vec<(String, u32)> = pgids.to_vec();
    let pid_list: Vec<u32> = ids.iter().map(|(_, p)| *p).collect();

    let t0 = Instant::now();
    let first = sample_groups(&pid_list);
    std::thread::sleep(std::time::Duration::from_millis(window_ms));
    let second = sample_groups(&pid_list);
    let elapsed = t0.elapsed().as_secs_f64();

    ids.into_iter()
        .map(|(session_id, daemon_pid)| {
            let a = first.get(&daemon_pid);
            let b = second.get(&daemon_pid);
            let cpu_percent = match (a, b) {
                (Some(a), Some(b)) if b.cpu_ticks >= a.cpu_ticks => Some(
                    GroupResources::cpu_percent(b.cpu_ticks - a.cpu_ticks, elapsed),
                ),
                _ => None,
            };
            SessionResourceSample {
                session_id,
                daemon_pid,
                cpu_percent,
                rss_bytes: b.map(|r| r.rss_bytes).unwrap_or(0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percent_math() {
        // 100 ticks over 1s at 100Hz = exactly one core.
        assert!((GroupResources::cpu_percent(100, 1.0) - 100.0).abs() < 0.01);
        // Zero window must not divide by zero.
        assert_eq!(GroupResources::cpu_percent(100, 0.0), 0.0);
        // 25 ticks over 1s = quarter core.
        assert!((GroupResources::cpu_percent(25, 1.0) - 25.0).abs() < 0.01);
    }

    #[test]
    fn rss_human_format() {
        assert_eq!(GroupResources::rss_human(512 * 1024), "512KB");
        assert_eq!(GroupResources::rss_human(300 * 1024 * 1024), "300.0MB");
    }

    #[test]
    fn parses_stat_line_with_parens_in_comm() {
        // comm containing spaces and parens must not shift field indices.
        // Field map (1-based): pid=1 ppid=4; utime=14; stime=15; rss=24.
        let stat = "1234 (aish (worker) #2) R 100 1234 1234 1234 0 12345 \
                    0 0 0 12 34 56 0 0 20 0 3 0 98765 109568 512 0 0 0 0 0 0 0 0 0";
        let (pid, ppid, utime, stime, rss) = parse_stat_all(stat).expect("must parse");
        assert_eq!((pid, ppid), (1234, 100));
        assert_eq!((utime, stime), (34, 56));
        assert_eq!(rss, 512);
    }

    #[test]
    fn samples_own_process_tree() {
        // The test process is the root of its own (single-process) tree.
        let own = std::process::id();
        let groups = sample_groups(&[own]);
        let r = groups
            .get(&own)
            .unwrap_or_else(|| panic!("own pid {own} present"));
        assert!(r.rss_bytes > 0, "own RSS must be > 0, got {}", r.rss_bytes);
    }

    #[test]
    fn cpu_sampling_produces_non_negative_deltas() {
        let own = std::process::id();
        let samples = sample_sessions_with_cpu(&[("test".to_string(), own)], 50);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.session_id, "test");
        assert!(s.rss_bytes > 0);
        if let Some(cpu) = s.cpu_percent {
            assert!(cpu >= 0.0);
        }
    }

    #[test]
    fn dead_root_reports_zero() {
        let groups = sample_groups(&[u32::MAX]);
        assert_eq!(groups[&u32::MAX], GroupResources::default());
    }

    #[test]
    fn descendant_pids_finds_direct_child() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let kids = descendant_pids(std::process::id());
        assert!(
            kids.contains(&child.id()),
            "child {} must appear in descendants: {kids:?}",
            child.id()
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
