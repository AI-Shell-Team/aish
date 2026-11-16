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
const HOST_TIMEOUT: Duration = Duration::from_secs(20);

/// Entry point for `aish remote diagnose`.
pub fn run(hosts: &[String], checks: &[String], json: bool) -> Result<(), String> {
    let task_id = crate::diag_tasks::new_task_id();
    let results = run_inner(hosts, checks, json)?;

    // Persist the task so it can be listed/inspected/retried later.
    let mut outcomes = std::collections::BTreeMap::new();
    for (h, r) in &results {
        let outcome = match r {
            Ok(_) => crate::diag_tasks::HostOutcome::Success,
            Err(s) => match s {
                HostStatus::Unreachable => crate::diag_tasks::HostOutcome::Unreachable,
                HostStatus::Denied => crate::diag_tasks::HostOutcome::Denied,
                HostStatus::Timeout => crate::diag_tasks::HostOutcome::Timeout,
                HostStatus::EvidenceInsufficient => {
                    crate::diag_tasks::HostOutcome::EvidenceInsufficient
                }
                HostStatus::Success => crate::diag_tasks::HostOutcome::Success,
            },
        };
        outcomes.insert(h.clone(), outcome);
    }
    // Persist the raw per-host check results so `diag status` can replay
    // the details later, not just the outcome states.
    let mut results_map = std::collections::BTreeMap::new();
    for (h, r) in &results {
        let value = match r {
            Ok(items) => serde_json::json!({ "status": "success", "results": items }),
            Err(status) => serde_json::json!({
                "status": status.label(),
                "results": [],
            }),
        };
        results_map.insert(h.clone(), value);
    }
    let record = crate::diag_tasks::DiagTask {
        id: task_id.clone(),
        hosts: hosts.to_vec(),
        checks: checks.to_vec(),
        started_at: chrono::Utc::now().to_rfc3339(),
        outcomes,
        results: results_map,
    };
    let _ = crate::diag_tasks::save(&record);
    if !json {
        println!("task: {task_id} (saved; see 'aish remote diag status {task_id}')");
    }
    Ok(())
}

/// Run the fan-out and render. Returns the per-host results for the
/// caller to persist.
fn run_inner(
    hosts: &[String],
    checks: &[String],
    json: bool,
) -> Result<HashMap<String, Result<Vec<serde_json::Value>, HostStatus>>, String> {
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
    let mut results: HashMap<String, Result<Vec<serde_json::Value>, HostStatus>> = HashMap::new();

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

    render(hosts_vec, checks_vec, &results, json)?;
    Ok(results)
}

/// Run the remote diagnose subcommand on one host.
fn diagnose_host(host: &str, checks: &[String]) -> Result<Vec<serde_json::Value>, HostStatus> {
    let bin = crate::remote_cmd::probe_remote_bin(host).map_err(|_| HostStatus::Unreachable)?;
    let mut args = vec![bin, "--remote-diagnose".to_string()];
    args.push("--checks".to_string());
    args.push(checks.join(","));
    args.push("--enforce-read-only".to_string());

    let values = match std::thread::scope(|scope| {
        // Password for literal targets comes from the environment
        // (AISH_REMOTE_PASSWORD); ssh_json_lines reads it via env_password.
        let handle = scope.spawn(|| ssh_json_lines(host, &args, Some(HOST_TIMEOUT)));
        handle.join()
    }) {
        Ok(Ok(values)) => values,
        Ok(Err(_)) => return Err(HostStatus::Unreachable),
        Err(_) => return Err(HostStatus::Unreachable),
    };

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

/// Aggregate status counts for the JSON report.
fn summary_of(
    hosts: &[String],
    results: &HashMap<String, Result<Vec<serde_json::Value>, HostStatus>>,
) -> serde_json::Value {
    let total = hosts.len();
    let mut success = 0usize;
    let mut by_status: HashMap<&'static str, usize> = HashMap::new();
    for h in hosts {
        let label = match results.get(h) {
            Some(Ok(_)) => {
                success += 1;
                HostStatus::Success.label()
            }
            Some(Err(status)) => status.label(),
            None => HostStatus::Timeout.label(),
        };
        *by_status.entry(label).or_insert(0) += 1;
    }
    let failed = total - success;
    let mut by_status: serde_json::Map<String, serde_json::Value> = by_status
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
        .collect();
    by_status.sort_keys();
    serde_json::json!({
        "total": total,
        "success": success,
        "failed": failed,
        "by_status": serde_json::Value::Object(by_status),
    })
}

/// Render the aggregate report. Coverage statement comes first; hosts with
/// no evidence are never shown as healthy.
fn render(
    hosts: Vec<String>,
    checks: Vec<String>,
    results: &HashMap<String, Result<Vec<serde_json::Value>, HostStatus>>,
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
        out.insert("summary".to_string(), summary_of(&hosts, &results));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_results() -> HashMap<String, Result<Vec<serde_json::Value>, HostStatus>> {
        let mut results = HashMap::new();
        results.insert(
            "ok-host".to_string(),
            Ok(vec![
                serde_json::json!({"check": "disk", "summary": "/dev 90%"}),
            ]),
        );
        results.insert("down-host".to_string(), Err(HostStatus::Unreachable));
        results.insert("slow-host".to_string(), Err(HostStatus::Timeout));
        results
    }

    #[test]
    fn summary_counts_match_host_statuses() {
        let hosts = vec![
            "ok-host".to_string(),
            "down-host".to_string(),
            "slow-host".to_string(),
        ];
        let s = summary_of(&hosts, &sample_results());
        assert_eq!(s["total"], 3);
        assert_eq!(s["success"], 1);
        assert_eq!(s["failed"], 2);
        assert_eq!(s["by_status"]["success"], 1);
        assert_eq!(s["by_status"]["unreachable"], 1);
        assert_eq!(s["by_status"]["timeout"], 1);
    }

    #[test]
    fn missing_host_counts_as_timeout() {
        let hosts = vec!["ok-host".to_string(), "no-result".to_string()];
        let s = summary_of(&hosts, &sample_results());
        assert_eq!(s["total"], 2);
        assert_eq!(s["success"], 1);
        assert_eq!(s["failed"], 1);
        assert_eq!(s["by_status"]["timeout"], 1);
    }

    #[test]
    fn run_rejects_unknown_check_before_any_traffic() {
        let err = run(
            &["h".to_string()],
            &["disk".to_string(), "rm-rf".to_string()],
            false,
        )
        .unwrap_err();
        assert!(err.contains("unknown check 'rm-rf'"), "got: {err}");
    }

    #[test]
    fn run_rejects_empty_hosts_and_checks() {
        assert!(run(&[], &["disk".to_string()], false).is_err());
        assert!(run(&["h".to_string()], &[], false).is_err());
    }

    #[test]
    fn ssh_json_lines_timeout_kills_stuck_child() {
        // The test needs a working passwordless `ssh localhost`: it proves
        // the timeout path kills a stuck child instead of waiting for it.
        // CI runners have no localhost ssh credentials, so ssh exits with
        // an empty stdout immediately (Ok([])) — skip instead of failing.
        let probe = std::process::Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=3",
                "--",
                "localhost",
                "echo",
                "ok",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        match probe {
            Ok(out) if out.status.success() => {}
            _ => {
                eprintln!("skipping: no passwordless ssh to localhost");
                return;
            }
        }

        // Local sh sleep exceeds the budget; ssh_json_lines must kill it and
        // return an error well under the sleep duration.
        let start = std::time::Instant::now();
        let err = ssh_json_lines(
            "localhost",
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo '{\"a\":1}'; sleep 30".to_string(),
            ],
            Some(Duration::from_millis(500)),
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "got: {err}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
