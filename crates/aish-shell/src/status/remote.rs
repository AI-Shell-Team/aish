//! Remote status collection for SSH sessions.

use super::MONITORED_SERVICES;

pub struct RemoteStatus {
    pub hostname: Option<String>,
    pub os_info: Option<String>,
    pub ip: Option<String>,
    pub uptime_secs: Option<u64>,
    pub cpu_percent: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub services: Option<Vec<(String, bool)>>,
    pub error_count: Option<usize>,
}

pub fn collect_remote(exec: &mut dyn FnMut(&str) -> String) -> RemoteStatus {
    let hostname = parse_hostname(&exec("command hostname 2>/dev/null"));
    let os_info = parse_os_info(&exec("command cat /etc/os-release 2>/dev/null"));
    let ip = parse_ip(&exec("command ip -4 addr show 2>/dev/null"));
    let uptime_secs = parse_uptime(&exec("command cat /proc/uptime 2>/dev/null"));
    let cpu_percent = collect_cpu(exec);
    let (mem_used, mem_total) = parse_memory(&exec("LC_ALL=C command free -b 2>/dev/null"));
    let services = parse_services(
        &exec("command systemctl list-units --type=service --all --no-pager --no-legend 2>/dev/null | awk '{print $1\":\"$3}'"),
    );
    let error_count = parse_error_count(&exec(
        "command journalctl --no-pager -p err --since today -q 2>/dev/null | wc -l",
    ));

    RemoteStatus {
        hostname,
        os_info,
        ip,
        uptime_secs,
        cpu_percent,
        mem_used,
        mem_total,
        services,
        error_count,
    }
}

fn parse_hostname(output: &str) -> Option<String> {
    let s = output.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '\'')
}

fn parse_os_info(output: &str) -> Option<String> {
    let mut name = None;
    let mut version = None;
    for line in output.lines() {
        if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
            return Some(strip_quotes(val).to_string());
        }
        if let Some(val) = line.strip_prefix("NAME=") {
            name = Some(strip_quotes(val).to_string());
        }
        if let Some(val) = line.strip_prefix("VERSION=") {
            version = Some(strip_quotes(val).to_string());
        }
    }
    match (name, version) {
        (Some(n), Some(v)) => Some(format!("{} {}", n, v)),
        (Some(n), None) => Some(n),
        _ => None,
    }
}

fn parse_ip(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if line.contains("inet ") && !line.contains("127.0.0.1") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let cidr = parts[1];
                let ip = cidr.split('/').next()?;
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
        }
    }
    None
}

fn parse_uptime(output: &str) -> Option<u64> {
    let first = output.split_whitespace().next()?;
    let secs: f64 = first.parse().ok()?;
    Some(secs as u64)
}

fn collect_cpu(exec: &mut dyn FnMut(&str) -> String) -> Option<f32> {
    let sample1 = exec("command cat /proc/stat 2>/dev/null");
    std::thread::sleep(std::time::Duration::from_millis(200));
    let sample2 = exec("command cat /proc/stat 2>/dev/null");
    let vals1 = parse_cpu_line(&sample1)?;
    let vals2 = parse_cpu_line(&sample2)?;
    // idle = column 3, iowait = column 4
    let d_idle = (vals2[3] + vals2.get(4).copied().unwrap_or(0)) as f64
        - (vals1[3] + vals1.get(4).copied().unwrap_or(0)) as f64;
    let d_total: f64 = vals2
        .iter()
        .zip(vals1.iter())
        .map(|(a, b)| *a as f64 - *b as f64)
        .sum();
    if d_total > 0.0 {
        Some(((1.0 - d_idle / d_total) * 100.0) as f32)
    } else {
        None
    }
}

fn parse_cpu_line(output: &str) -> Option<Vec<u64>> {
    // /proc/stat has multiple lines; only parse the aggregate "cpu " line
    for line in output.lines() {
        let trimmed = line.trim();
        // "cpu  " (with trailing space) is the aggregate line; "cpu0" etc. are per-core
        if trimmed.starts_with("cpu ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 5 {
                return parts[1..].iter().map(|s| s.parse().ok()).collect();
            }
        }
    }
    None
}

fn parse_memory(output: &str) -> (Option<u64>, Option<u64>) {
    // Find the "Mem:" line to skip the header row
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Mem:") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            // Mem: total used free shared buff/cache available
            if parts.len() >= 3 {
                let total = parts[1].parse().ok();
                let used = parts[2].parse().ok();
                return (used, total);
            }
        }
    }
    (None, None)
}

fn parse_services(output: &str) -> Option<Vec<(String, bool)>> {
    if output.trim().is_empty() {
        return None;
    }
    let mut services = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some((unit, status)) = line.split_once(':') {
            if let Some(name) = unit.strip_suffix(".service") {
                if MONITORED_SERVICES.contains(&name) {
                    services.push((name.to_string(), status == "active"));
                }
            }
        }
    }
    Some(services)
}

fn parse_error_count(output: &str) -> Option<usize> {
    output.trim().parse().ok()
}
