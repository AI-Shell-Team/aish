use aish_pty::PersistentPty;
use std::time::Duration;

use super::{ServiceStatus, MONITORED_SERVICES};

pub fn collect(pty: &mut PersistentPty) -> Option<Vec<ServiceStatus>> {
    let result = pty.execute_command(
        "systemctl list-units --type=service --all --no-pager --no-legend 2>/dev/null",
        Duration::from_secs(5),
        None,
        false,
    );
    let output = result.ok()?.0;
    if output.trim().is_empty() {
        return None;
    }

    let mut services = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let unit = parts[0]; // e.g. "sshd.service"
        let active = parts[2]; // "active" or "inactive" or "failed"

        if let Some(name) = unit.strip_suffix(".service") {
            if MONITORED_SERVICES.contains(&name) {
                services.push(ServiceStatus {
                    name: name.to_string(),
                    active: active == "active",
                });
            }
        }
    }
    Some(services)
}
