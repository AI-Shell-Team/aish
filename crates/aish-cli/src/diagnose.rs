//! `aish remote diagnose`: fan out bounded read-only checks across hosts and
//! aggregate partial results (#588).
//!
//! Per host: run `ssh <host> aish --remote-diagnose --checks ...` and parse
//! JSON lines. Hosts are attempted with bounded concurrency; failures are
//! recorded per host (unreachable / denied / timeout) and never hide results
//! from successful hosts.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crate::remote_cmd::ssh_json_lines;

/// Per-host status vocabulary mirrors the issue's acceptance criteria.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostStatus {
    Success,
    Unreachable,
    Denied,
    Timeout,
    EvidenceInsufficient,
}

impl HostStatus {
    fn label(&self) -> &'static str {
        match self {
            HostStatus::Success => "success",
            HostStatus::Unreachable => "unreachable",
            HostStatus::Denied => "denied",
            HostStatus::Timeout => "timeout",
            HostStatus::EvidenceInsufficient => "evidence-insufficient",
        }
    }
}

/// Known read-only check names. Any unknown name is rejected before any
/// ssh traffic — the whitelist is the structural guarantee against write
/// or privilege-escalation commands.
const KNOWN_CHECKS: &[&str] = &["svc", "ports", "logs", "disk", "load"];

/// Max hosts probed concurrently.
const MAX_CONCURRENCY: usize = 4;

/// Per-host remote execution timeout.
const HOST_TIMEOUT: Duration = Duration::from_secs(30);

/// Entry point for `aish remote diagnose`.
pub fn run(hosts: &[String], checks: &[String], json: bool) -> Result<(), String> {
    if hosts.is_empty() {
        return Err("no hosts given; use --hosts h1,h2,...".to_string());
    }
    if checks.is_empty() {
        return Err("no checks given; use --checks svc,ports,logs,disk,load".to_string());
    }
    for c in checks {
        if !KNOWN_CHECKS.contains(&c.as_str()) {
            return Err(format!(
                "unknown check '{c}'; allowed: {}",
                KNOWN_CHECKS.join(", ")
            ));
        }
    }

    // Fan out with bounded concurrency via a small worker pool.
    let (tx, rx) = mpsc::channel::<(String, Result<Vec<serde_json::Value>, HostStatus>)>();
    let hosts_vec: Vec<String> = hosts.to_vec();
    let checks_vec: Vec<String> = checks.to_vec();

    let mut pending: Vec<String> = hosts_vec.iter().rev().cloned().collect();
    let mut results: HashMap<String, Result<Vec<serde_json::Value>, HostStatus>> =
        HashMap::new();

    std::thread::scope(|scope| {
        let mut in_flight = 0usize;
        let mut handles = Vec::new();
        loop {
            // Launch up to MAX_CONCURRENCY.
            while in_flight < MAX_CONCURRENCY {
                let Some(host) = pending.pop() else {
                    break;
                };
                let tx = tx.clone();
                let checks = checks_vec.clone();
                in_flight += 1;
                handles.push(scope.spawn(move || {
                    let outcome = diagnose_host(&host, &checks);
                    let _ = tx.send((host, outcome));
                }));
            }
            if in_flight == 0 {
                break;
            }
            // Collect one result.
            match rx.recv_timeout(HOST_TIMEOUT) {
                Ok((host, outcome)) => {
                    in_flight -= 1;
                    results.insert(host, outcome);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    break; // remaining in-flight hosts reported as timeout below
                }
                Err(_) => break,
            }
        }
        for h in handles {
            let _ = h.join();
        }
    });

    // Any host without a recorded result is marked timed out.
    for h in &hosts_vec {
        results
            .entry(h.clone())
            .or_insert_with(|| Err(HostStatus::Timeout));
    }

    render(hosts_vec, checks_vec, results, json)
}

/// Run the remote diagnose subcommand on one host.
fn diagnose_host(
    host: &str,
    checks: &[String],
) -> Result<Vec<serde_json::Value>, HostStatus> {
    let bin = crate::remote_cmd::probe_remote_bin(host).map_err(|_| HostStatus::Unreachable)?;
    let mut args = vec![bin, "--remote-diagnose".to_string()];
    args.push("--checks".to_string());
    args.push(checks.join(","));
    args.push("--enforce-read-only".to_string());

    let values = std::thread::scope(|scope| {
        let handle = scope.spawn(|| ssh_json_lines(host, &args));
        handle.join().map_err(|_| HostStatus::Unreachable)?
    })
    .map_err(|_| HostStatus::Unreachable)?;

    if values.is_empty() {
        return Err(HostStatus::EvidenceInsufficient);
    }
    // The remote marks denial explicitly.
    let denied = values
        .iter()
        .any(|v| v.get("denied").and_then(|d| d.as_bool()) == Some(true));
    if denied {
        return Err(HostStatus::Denied);
    }
    Ok(values)
}

/// Render the aggregate report. Coverage statement comes first; hosts with
/// no evidence are never shown as healthy.
fn render(
    hosts: Vec<String>,
    checks: Vec<String>,
    results: HashMap<String, Result<Vec<serde_json::Value>, HostStatus>>,
    json: bool,
) -> Result<(), String> {
    if json {
        let mut out = serde_json::Map::new();
        out.insert(
            "coverage".to_string(),
            serde_json::json!({
                "requested_hosts": hosts,
                "checks": checks,
            }),
        );
        let mut host_reports = Vec::new();
        for h in &hosts {
            let report = match results.get(h) {
                Some(Ok(items)) => serde_json::json!({
                    "host": h,
                    "status": "success",
                    "results": items,
                }),
                Some(Err(status)) => serde_json::json!({
                    "host": h,
                    "status": status.label(),
                    "results": [],
                }),
                None => serde_json::json!({
                    "host": h,
                    "status": "timeout",
                    "results": [],
                }),
            };
            host_reports.push(report);
        }
        out.insert("hosts".to_string(), serde_json::Value::Array(host_reports));
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap_or_default()
        );
        return Ok(());
    }

    let mut ok = 0usize;
    let mut failed = Vec::new();
    println!("=== Fleet diagnose report ===");
    println!("Checks requested: {}", checks.join(", "));
    for h in &hosts {
        match results.get(h) {
            Some(Ok(items)) => {
                ok += 1;
                println!("\n[{h}] success ({} items)", items.len());
                for item in items {
                    let check = item.get("check").and_then(|v| v.as_str()).unwrap_or("?");
                    let summary = item.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                    let truncated = item
                        .get("truncated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let marker = if truncated { " [TRUNCATED]" } else { "" };
                    println!("  {check}: {summary}{marker}");
                }
            }
            Some(Err(status)) => {
                failed.push((h.clone(), status.label().to_string()));
            }
            None => {
                failed.push((h.clone(), HostStatus::Timeout.label().to_string()));
            }
        }
    }

    println!("\n=== Coverage ===");
    println!("Successful: {ok}/{} hosts", hosts.len());
    if !failed.is_empty() {
        println!("Missing/failed hosts (NOT included in conclusions):");
        for (h, s) in &failed {
            println!("  {h}: {s}");
        }
    }
    Ok(())
}
