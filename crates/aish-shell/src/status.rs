//! Environment status display for /status command.

mod logs;
mod network;
mod remote;
mod services;
mod system_info;

use std::sync::Mutex;

use crate::theme;

/// System information collected via sysinfo crate.
struct SystemInfo {
    hostname: String,
    os_version: String,
    uptime_secs: u64,
    cpu_percent: f32,
    mem_used: u64,
    mem_total: u64,
}

/// Status of a monitored service.
struct ServiceStatus {
    name: String,
    active: bool,
}

const MONITORED_SERVICES: &[&str] = &[
    "sshd",
    "ssh",
    "nginx",
    "apache2",
    "httpd",
    "docker",
    "containerd",
    "mysql",
    "mysqld",
    "postgresql",
    "redis",
    "systemd-resolved",
    "cron",
];

/// Run the environment status scan and display results.
pub fn run_status(
    pty: &Mutex<aish_pty::PersistentPty>,
    version: &str,
    session_id: &str,
    live_session_id: Option<&str>,
    model: &str,
) {
    let sys = system_info::collect();

    let (ip, svcs, errors) = {
        let mut pty_guard = pty.lock().unwrap();
        let ip = network::collect_ip(&mut pty_guard);
        let svcs = services::collect(&mut pty_guard);
        let errors = logs::collect_error_count(&mut pty_guard);
        (ip, svcs, errors)
    };

    render(
        version,
        session_id,
        live_session_id,
        model,
        &sys,
        &ip,
        &svcs,
        &errors,
    );
}

fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= GB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0}M", bytes as f64 / MB as f64)
    } else {
        format!("{}K", bytes / 1024)
    }
}

fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d {}h", days, hours)
    } else {
        format!("{}h {}m", hours, minutes)
    }
}

fn render(
    version: &str,
    session_id: &str,
    live_session_id: Option<&str>,
    model: &str,
    sys: &SystemInfo,
    ip: &Option<String>,
    svcs: &Option<Vec<ServiceStatus>>,
    errors: &Option<usize>,
) {
    use aish_i18n::t;
    let mut lines: Vec<String> = Vec::new();

    // Line 1: aish info
    let session_short = &session_id[..session_id.len().min(6)];
    let mut line1 = format!(
        "aish {} │ {}: {} │ {}: {}",
        version,
        t("status.session"),
        session_short,
        t("status.model"),
        model,
    );
    if let Some(live) = live_session_id {
        let live_short = &live[..live.len().min(8)];
        line1.push_str(&format!(" │ {}: {}", t("status.live_session"), live_short));
    }
    lines.push(line1);

    // Line 2: host info
    let na = t("status.na");
    let ip_str = ip.as_deref().unwrap_or(&na);
    lines.push(format!(
        "{}: {} ({}) │ {} │ {} {}",
        t("status.host"),
        sys.hostname,
        ip_str,
        sys.os_version,
        t("status.up"),
        format_uptime(sys.uptime_secs)
    ));

    // Line 3: resources
    let mem_pct = (sys.mem_used * 100).checked_div(sys.mem_total).unwrap_or(0);
    lines.push(format!(
        "CPU: {:.0}% │ Mem: {}/{} ({}%)",
        sys.cpu_percent,
        format_bytes(sys.mem_used),
        format_bytes(sys.mem_total),
        mem_pct,
    ));

    // Line 4: services (optional)
    if let Some(svc_list) = svcs {
        if !svc_list.is_empty() {
            let parts: Vec<String> = svc_list
                .iter()
                .map(|s| {
                    if s.active {
                        theme::success(&format!("{} {}", s.name, theme::ICON_SUCCESS))
                    } else {
                        theme::faint(&format!("{} {}", s.name, theme::ICON_DONE))
                    }
                })
                .collect();
            lines.push(format!("{}: {}", t("status.services"), parts.join(" ")));
        }
    }

    // Line 5: errors (optional)
    if let Some(count) = errors {
        lines.push(format!("{}: {}", t("status.errors_today"), count));
    }

    // Render with box drawing
    let total = lines.len();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            println!("{} {}", theme::TREE_CORNER, line);
        } else if i == total - 1 {
            println!("{} {}", theme::TREE_LAST, line);
        } else {
            println!("{} {}", theme::TREE_BRANCH, line);
        }
    }
}

/// Callback for remote /status. Returns rendered output string.
pub fn run_status_remote(
    exec: &mut dyn FnMut(&str) -> String,
    version: &str,
    session_id: &str,
    model: &str,
) -> String {
    let rs = remote::collect_remote(exec);
    render_to_string(version, session_id, model, &rs)
}

fn render_to_string(
    version: &str,
    session_id: &str,
    model: &str,
    rs: &remote::RemoteStatus,
) -> String {
    use aish_i18n::t;
    let mut lines: Vec<String> = Vec::new();

    let session_short = &session_id[..session_id.len().min(6)];
    lines.push(format!(
        "aish {} │ {}: {} │ {}: {}",
        version,
        t("status.session"),
        session_short,
        t("status.model"),
        model
    ));

    let na = t("status.na");
    let ip_str = rs.ip.as_deref().unwrap_or(&na);
    let os_str = rs.os_info.as_deref().unwrap_or("?");
    let up_str = rs
        .uptime_secs
        .map(|s| format_uptime(s))
        .unwrap_or_else(|| "?".into());
    lines.push(format!(
        "{}: {} ({}) │ {} │ {} {}",
        t("status.host"),
        rs.hostname.as_deref().unwrap_or("?"),
        ip_str,
        os_str,
        t("status.up"),
        up_str
    ));

    let mem_pct = match (rs.mem_used, rs.mem_total) {
        (Some(u), Some(t)) if t > 0 => (u * 100).checked_div(t).unwrap_or(0),
        _ => 0,
    };
    let cpu_str = rs
        .cpu_percent
        .map(|p| format!("{:.0}%", p))
        .unwrap_or_else(|| "?".into());
    let mem_str = match (rs.mem_used, rs.mem_total) {
        (Some(u), Some(t)) => format!("{}/{} ({}%)", format_bytes(u), format_bytes(t), mem_pct),
        _ => "?/?".into(),
    };
    lines.push(format!("CPU: {} │ Mem: {}", cpu_str, mem_str));

    if let Some(ref svcs) = rs.services {
        if !svcs.is_empty() {
            let parts: Vec<String> = svcs
                .iter()
                .map(|(name, active)| {
                    if *active {
                        theme::success(&format!("{} {}", name, theme::ICON_SUCCESS))
                    } else {
                        theme::faint(&format!("{} {}", name, theme::ICON_DONE))
                    }
                })
                .collect();
            lines.push(format!("{}: {}", t("status.services"), parts.join(" ")));
        }
    }

    if let Some(count) = rs.error_count {
        lines.push(format!("{}: {}", t("status.errors_today"), count));
    }

    let total = lines.len();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            out.push_str(&format!("{} {}\n", theme::TREE_CORNER, line));
        } else if i == total - 1 {
            out.push_str(&format!("{} {}\n", theme::TREE_LAST, line));
        } else {
            out.push_str(&format!("{} {}\n", theme::TREE_BRANCH, line));
        }
    }
    out
}
