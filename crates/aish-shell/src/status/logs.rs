use aish_pty::PersistentPty;
use std::time::Duration;

pub fn collect_error_count(pty: &mut PersistentPty) -> Option<usize> {
    let result = pty.execute_command(
        "journalctl --no-pager -p err --since today -q 2>/dev/null | wc -l",
        Duration::from_secs(5),
        None,
        false,
    );
    let output = result.ok()?.0;
    output.trim().parse().ok()
}
