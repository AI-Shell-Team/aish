//! Startup update check (omp-style async notice).
//!
//! Probes `cdn.aishell.ai/download/latest` for a newer version on a background
//! thread. When one exists, the REPL prints a one-shot notice. Cache-backed
//! (24h TTL) so a typical day triggers at most one network request, and silent
//! on every failure — the probe never blocks or interrupts startup.

use std::path::PathBuf;
use std::time::Duration;

use aish_core::AishError;
use semver::Version;
use serde::{Deserialize, Serialize};

const LATEST_VERSION_URL: &str = "https://cdn.aishell.ai/download/latest";
/// How long a cached probe result stays valid.
const CACHE_TTL_SECS: i64 = 24 * 3600;
/// Hard cap on the network fetch so a slow CDN cannot stall the check.
const FETCH_TIMEOUT_SECS: u64 = 5;

/// Resolved update info ready to display.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest: String,
}

/// REPL-side state machine. Drained exactly once per session: `Pending` while
/// the background probe is in flight, `Available` once a newer version lands,
/// then `Done` forever after (shown, up-to-date, or failed).
#[derive(Debug)]
pub enum UpdateNotice {
    Pending,
    Available(UpdateInfo),
    Done,
}

/// On-disk cache of the last probe outcome.
#[derive(Serialize, Deserialize)]
struct CacheEntry {
    last_check_ts: i64,
    /// Local version at probe time. A version change invalidates the cache so
    /// a just-upgraded binary re-probes instead of trusting a stale verdict.
    last_current: String,
    /// `None` when the probe found no newer version (or failed).
    latest: Option<String>,
}

/// Probe for a newer release. Cache-first (within TTL + same local version);
/// falls back to a short network fetch otherwise. Returns `None` on any
/// failure or when the running version is already current.
///
/// This runs on a `spawn_blocking` thread, so blocking `reqwest` is fine.
pub fn probe_update(current: &str) -> Option<UpdateInfo> {
    let cache_path = cache_file_path();

    // Cache hit: reuse the last verdict without touching the network.
    if let Some(entry) = read_cache(&cache_path) {
        if cache_fresh(&entry, current) {
            return entry
                .latest
                .filter(|l| is_newer(l, current))
                .map(|l| UpdateInfo { latest: l });
        }
    }

    // Cache miss/stale/expired — fetch fresh.
    let latest = match fetch_latest_tag() {
        Ok(tag) => tag,
        // Network/parse failure: do not touch the cache so the next launch
        // retries instead of trusting a 24h negative verdict.
        Err(_) => return None,
    };
    let newer = is_newer(&latest, current);
    write_cache(
        &cache_path,
        &CacheEntry {
            last_check_ts: now_ts(),
            last_current: current.to_string(),
            latest: newer.then_some(latest.clone()),
        },
    );
    newer.then_some(UpdateInfo { latest })
}

fn latest_version_url() -> String {
    // Mirrors aish-cli's AISH_LATEST_URL override so tests/enterprise mirrors
    // can point both the manual `aish update` and this startup probe at one host.
    std::env::var("AISH_LATEST_URL").unwrap_or_else(|_| LATEST_VERSION_URL.to_string())
}

fn fetch_latest_tag() -> Result<String, AishError> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("aish-update-checker")
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| AishError::Config(e.to_string()))?;
    let resp = client
        .get(latest_version_url())
        .send()
        .map_err(|e| AishError::Config(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(AishError::Config(format!(
            "latest endpoint returned status {}",
            resp.status()
        )));
    }
    let body = resp.text().map_err(|e| AishError::Config(e.to_string()))?;
    normalize_tag(&body)
}

/// Trim whitespace, strip an optional leading `v`, and validate as SemVer.
fn normalize_tag(raw: &str) -> Result<String, AishError> {
    let cleaned = raw.trim();
    let stripped = cleaned.strip_prefix('v').unwrap_or(cleaned);
    if stripped.is_empty() {
        return Err(AishError::Config(
            "invalid latest version metadata: empty".into(),
        ));
    }
    Version::parse(stripped)
        .map_err(|_| AishError::Config(format!("invalid latest version: {cleaned}")))?;
    Ok(stripped.to_string())
}

/// `true` only when `remote` is strictly newer than `local`. Any parse failure
/// is conservative (`false`) — never offer an update on an unparseable tag.
fn is_newer(remote: &str, local: &str) -> bool {
    let parse = |v: &str| Version::parse(v.strip_prefix('v').unwrap_or(v));
    match (parse(remote), parse(local)) {
        (Ok(r), Ok(l)) => r > l,
        _ => false,
    }
}

fn cache_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aish")
        .join("update-check.json")
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn cache_fresh(entry: &CacheEntry, current: &str) -> bool {
    entry.last_current == current && now_ts().saturating_sub(entry.last_check_ts) < CACHE_TTL_SECS
}

fn read_cache(path: &PathBuf) -> Option<CacheEntry> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(path: &PathBuf, entry: &CacheEntry) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string(entry) else {
        return;
    };
    // Atomic write: stage to a sibling temp, then rename, so a crash mid-write
    // cannot leave a half-written JSON for the next launch to choke on.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_strictly_greater() {
        assert!(is_newer("0.3.5", "0.3.4"));
        assert!(!is_newer("0.3.4", "0.3.4"));
        assert!(!is_newer("0.3.3", "0.3.4"));
    }

    #[test]
    fn is_newer_strips_v_prefix() {
        assert!(is_newer("v0.4.0", "0.3.4"));
        assert!(is_newer("0.4.0", "v0.3.4"));
        assert!(is_newer("v1.0.0", "v0.9.9"));
    }

    #[test]
    fn is_newer_garbage_is_false() {
        assert!(!is_newer("garbage", "0.3.4"));
        assert!(!is_newer("0.3.5", "garbage"));
    }

    #[test]
    fn normalize_strips_and_validates() {
        assert_eq!(normalize_tag("v0.3.4\n").unwrap(), "0.3.4");
        assert_eq!(normalize_tag(" 0.3.4 ").unwrap(), "0.3.4");
        assert_eq!(normalize_tag("0.3.4").unwrap(), "0.3.4");
        assert!(normalize_tag("").is_err());
        assert!(normalize_tag("abc").is_err());
    }

    #[test]
    fn cache_fresh_within_ttl() {
        let entry = CacheEntry {
            last_check_ts: now_ts(),
            last_current: "0.3.4".into(),
            latest: Some("0.3.5".into()),
        };
        assert!(cache_fresh(&entry, "0.3.4"));
    }

    #[test]
    fn cache_stale_after_ttl() {
        let entry = CacheEntry {
            last_check_ts: now_ts() - CACHE_TTL_SECS - 10,
            last_current: "0.3.4".into(),
            latest: Some("0.3.5".into()),
        };
        assert!(!cache_fresh(&entry, "0.3.4"));
    }

    #[test]
    fn cache_invalidated_when_local_version_changed() {
        let entry = CacheEntry {
            last_check_ts: now_ts(),
            last_current: "0.3.3".into(),
            latest: Some("0.3.5".into()),
        };
        // User just upgraded; the cached verdict was for the old binary.
        assert!(!cache_fresh(&entry, "0.3.4"));
    }

    #[test]
    fn cache_roundtrip_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        let entry = CacheEntry {
            last_check_ts: 12345,
            last_current: "0.3.4".into(),
            latest: Some("0.3.5".into()),
        };
        write_cache(&path, &entry);
        let read = read_cache(&path).unwrap();
        assert_eq!(read.last_check_ts, 12345);
        assert_eq!(read.last_current, "0.3.4");
        assert_eq!(read.latest.as_deref(), Some("0.3.5"));
    }

    #[test]
    fn cache_none_latest_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        let entry = CacheEntry {
            last_check_ts: 1,
            last_current: "0.3.4".into(),
            latest: None,
        };
        write_cache(&path, &entry);
        assert!(read_cache(&path).unwrap().latest.is_none());
    }

    #[test]
    fn read_cache_missing_file_is_none() {
        assert!(read_cache(&PathBuf::from("/nonexistent/update-check.json")).is_none());
    }

    #[test]
    fn read_cache_corrupt_json_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        std::fs::write(&path, "not json {").unwrap();
        assert!(read_cache(&path).is_none());
    }
}
