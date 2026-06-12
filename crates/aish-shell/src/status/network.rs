use aish_pty::PersistentPty;
use std::time::Duration;

pub fn collect_ip(pty: &mut PersistentPty) -> Option<String> {
    let result = pty.execute_command(
        "ip -4 addr show 2>/dev/null",
        Duration::from_secs(3),
        None,
        false,
    );
    let output = result.ok()?.0;
    parse_ip(&output)
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
