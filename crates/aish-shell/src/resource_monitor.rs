//! Periodic live-session resource monitoring for the REPL.
//!
//! Every [`Monitor::check`] call takes ONE instantaneous `/proc` sample of
//! all live PTY daemon process trees and warns in the current session when
//! another session exceeds the configured CPU / RSS thresholds. CPU percent
//! is derived from the tick delta against the previous check, so no sampling
//! sleep ever blocks the REPL. Alerts are throttled per session (5 min
//! cooldown) and reset when usage drops back below the thresholds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aish_pty::{sample_groups, DaemonSessionInfo, GroupResources};

/// Per-session monitoring state.
struct SessionState {
    /// Cumulative CPU ticks seen at the previous check; the delta between
    /// checks yields CPU percent without any sampling sleep.
    last_ticks: Option<u64>,
    /// When the previous check ran; denominates the tick delta.
    last_sample_at: Instant,
    /// Last alert emission, for the cooldown.
    last_alert: Option<Instant>,
}

/// Alert cooldown per session — one warning every 5 minutes while the
/// session stays above its thresholds.
const ALERT_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Minimum interval (seconds) between two samples before a CPU delta is
/// trusted. Below this, tick granularity over a near-zero window produces
/// wild percentages (e.g. one jiffy over 1 ms), so CPU is reported as
/// unknown instead.
const MIN_CPU_WINDOW_SECS: f64 = 1.0;

/// Threshold configuration extracted from `ConfigModel`.
#[derive(Debug, Clone)]
pub struct Thresholds {
    pub cpu_percent: f64,
    pub rss_mb: u64,
}

/// Stateful monitor keyed by session id.
pub struct Monitor {
    thresholds: Thresholds,
    states: HashMap<String, SessionState>,
}

/// A threshold violation to be reported for one session.
pub struct Alert {
    pub session: DaemonSessionInfo,
    pub cpu: Option<f64>,
    pub rss_bytes: u64,
}

impl Monitor {
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            thresholds,
            states: HashMap::new(),
        }
    }
    pub fn check(&mut self, sessions: &[DaemonSessionInfo]) -> Vec<Alert> {
        // Forget sessions that disappeared so ids are not leaked and a
        // re-appearing session starts without stale cooldown state.
        let live: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        self.states.retain(|id, _| live.contains(&id.as_str()));

        if sessions.is_empty()
            || (self.thresholds.cpu_percent <= 0.0 && self.thresholds.rss_mb == 0)
        {
            return Vec::new();
        }

        // One instantaneous /proc pass for every session tree; CPU comes
        // from the delta against the previous check, never from a sleep.
        let pids: Vec<u32> = sessions.iter().map(|s| s.daemon_pid).collect();
        let groups = sample_groups(&pids);

        let now = Instant::now();
        let rss_threshold_bytes = self.thresholds.rss_mb.saturating_mul(1024 * 1024);
        let mut alerts: Vec<Alert> = Vec::new();

        for session in sessions {
            let Some(res) = groups.get(&session.daemon_pid) else {
                continue;
            };
            let state = self
                .states
                .entry(session.session_id.clone())
                .or_insert(SessionState {
                    last_ticks: None,
                    last_sample_at: now,
                    last_alert: None,
                });

            // CPU averaged over the whole interval since the previous check
            // (default 30 s) — the long window also smooths short spikes.
            // `None` on the first check or when the window is too short for
            // tick granularity to be meaningful.
            let elapsed = now.duration_since(state.last_sample_at).as_secs_f64();
            let cpu_percent = match state.last_ticks {
                Some(prev) if elapsed >= MIN_CPU_WINDOW_SECS && res.cpu_ticks >= prev => {
                    Some(GroupResources::cpu_percent(res.cpu_ticks - prev, elapsed))
                }
                _ => None,
            };
            state.last_ticks = Some(res.cpu_ticks);
            state.last_sample_at = now;

            let cpu_exceeded = self.thresholds.cpu_percent > 0.0
                && cpu_percent.is_some_and(|cpu| cpu >= self.thresholds.cpu_percent);
            let rss_exceeded = self.thresholds.rss_mb > 0 && res.rss_bytes >= rss_threshold_bytes;

            if !cpu_exceeded && !rss_exceeded {
                // Recovered: clear so a new violation episode alerts
                // immediately instead of waiting out the old cooldown.
                state.last_alert = None;
                continue;
            }

            let due = match state.last_alert {
                None => true,
                Some(last) => now.duration_since(last) >= ALERT_COOLDOWN,
            };
            if due {
                state.last_alert = Some(now);
                alerts.push(Alert {
                    session: session.clone(),
                    cpu: cpu_percent,
                    rss_bytes: res.rss_bytes,
                });
            }
        }

        alerts
    }
}

/// Format `(cpu, rss_bytes)` for the panel detail line via i18n.
pub fn format_resource_detail(cpu: Option<f64>, rss_bytes: u64) -> String {
    let cpu_str = match cpu {
        Some(c) => format!("{c:.0}%"),
        None => "--".to_string(),
    };
    let rss_str = GroupResources::rss_human(rss_bytes);
    let mut args = std::collections::HashMap::new();
    args.insert("cpu".to_string(), cpu_str);
    args.insert("rss".to_string(), rss_str);
    aish_i18n::t_with_args("shell.live_sessions.resource_detail", &args)
}

/// Format the threshold-alert message via i18n.
///
/// The session name (if any) is folded into the `{id}` label so unnamed
/// sessions don't render empty parentheses.
pub fn format_resource_alert(alert: &Alert) -> String {
    let cpu_str = match alert.cpu {
        Some(c) => format!("{c:.0}%"),
        None => "--".to_string(),
    };
    let rss_str = GroupResources::rss_human(alert.rss_bytes);
    let id = &alert.session.session_id;
    let short = &id[..8.min(id.len())];
    let label = match alert.session.name.as_deref() {
        Some(n) if !n.is_empty() => format!("{short} ({n})"),
        _ => short.to_string(),
    };
    let mut args = std::collections::HashMap::new();
    args.insert("id".to_string(), label);
    args.insert("cpu".to_string(), cpu_str);
    args.insert("rss".to_string(), rss_str);
    aish_i18n::t_with_args("shell.live_sessions.resource_alert", &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> Thresholds {
        Thresholds {
            cpu_percent: 80.0,
            rss_mb: 1024,
        }
    }

    #[test]
    fn empty_sessions_yield_no_alerts() {
        let mut m = Monitor::new(thresholds());
        assert!(m.check(&[]).is_empty());
    }

    #[test]
    fn disabled_thresholds_yield_no_alerts() {
        let mut m = Monitor::new(Thresholds {
            cpu_percent: 0.0,
            rss_mb: 0,
        });
        let sessions = vec![fake_session("aaaaaaaa")];
        assert!(m.check(&sessions).is_empty());
    }

    #[test]
    fn dead_daemon_below_thresholds_no_alert() {
        // The PID does not exist → zero usage → below thresholds → no alert.
        let mut m = Monitor::new(thresholds());
        let sessions = vec![fake_session_with_pid("aaaaaaaa", u32::MAX - 1)];
        assert!(m.check(&sessions).is_empty());
    }

    #[test]
    fn cooldown_prevents_repeat_alerts() {
        // Verify the state machine via our own process tree, whose RSS
        // certainly exceeds a 1 MiB threshold (CPU disabled).
        let mut m = Monitor::new(Thresholds {
            cpu_percent: 0.0,
            rss_mb: 1,
        });
        // The test process itself is the tree root.
        let pid = std::process::id();
        let sessions = vec![fake_session_with_pid("aaaaaaaa", pid)];
        let first = m.check(&sessions);
        assert_eq!(
            first.len(),
            1,
            "own tree RSS should exceed 1 MiB (test harness)"
        );
        let second = m.check(&sessions);
        assert!(
            second.is_empty(),
            "cooldown must suppress immediate repeat alert"
        );
    }

    #[test]
    fn first_check_has_no_cpu_baseline() {
        // CPU-only thresholds: the first check has no baseline and the
        // immediate second check's window is below MIN_CPU_WINDOW_SECS, so
        // no CPU alert may fire even at a near-zero threshold.
        let mut m = Monitor::new(Thresholds {
            cpu_percent: 0.0001,
            rss_mb: 0,
        });
        let sessions = vec![fake_session_with_pid("aaaaaaaa", std::process::id())];
        assert!(m.check(&sessions).is_empty());
        assert!(m.check(&sessions).is_empty());
    }

    #[test]
    fn recovery_resets_alert_state() {
        let mut m = Monitor::new(Thresholds {
            cpu_percent: 0.0,
            rss_mb: 1,
        });
        // The test process itself is the tree root.
        let pid = std::process::id();
        let hot = vec![fake_session_with_pid("aaaaaaaa", pid)];
        assert_eq!(m.check(&hot).len(), 1);

        // Session disappears from the list (e.g. killed) → state forgotten.
        let gone: Vec<DaemonSessionInfo> = Vec::new();
        m.check(&gone);
        assert!(
            m.states.is_empty(),
            "vanished session state must be dropped"
        );
    }

    #[test]
    fn formats_detail_and_alert() {
        let detail = format_resource_detail(Some(97.4), 300 * 1024 * 1024);
        assert!(detail.contains("97%"), "detail: {detail}");
        assert!(detail.contains("300.0MB"), "detail: {detail}");

        let alert = Alert {
            session: fake_session("abcdef12"),
            cpu: Some(97.0),
            rss_bytes: 512 * 1024 * 1024,
        };
        let msg = format_resource_alert(&alert);
        assert!(msg.contains("abcdef12"), "alert msg: {msg}");
        assert!(!msg.contains("()"), "no empty parens: {msg}");

        // A named session folds the name into the id label.
        let mut named = fake_session("abcdef12");
        named.name = Some("build".to_string());
        let msg = format_resource_alert(&Alert {
            session: named,
            cpu: None,
            rss_bytes: 0,
        });
        assert!(msg.contains("(build)"), "alert msg: {msg}");
        assert!(msg.contains("--"), "unknown CPU renders as --: {msg}");
    }

    fn fake_session(id: &str) -> DaemonSessionInfo {
        fake_session_with_pid(id, 1)
    }

    fn fake_session_with_pid(id: &str, pid: u32) -> DaemonSessionInfo {
        DaemonSessionInfo {
            session_id: id.to_string(),
            socket_path: std::path::PathBuf::from("/tmp/none.sock"),
            daemon_pid: pid,
            child_pid: 0,
            started_at: 0,
            cwd: "/tmp".to_string(),
            model: None,
            api_base: None,
            name: None,
        }
    }
}
