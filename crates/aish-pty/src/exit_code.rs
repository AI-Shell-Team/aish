//! Heuristics for command exit codes when PTY/bash reporting is unreliable
//! (e.g. polkit password prompts, stale prompt_ready events).

/// True when captured output shows polkit (or similar) auth still in progress.
pub fn polkit_auth_in_progress(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    let auth_started = output.contains("AUTHENTICATING FOR")
        || output.contains("Authenticating as:")
        || output.contains("请输入密码")
        || lower.contains("password:");
    if !auth_started {
        return false;
    }
    let auth_finished =
        output.contains("AUTHENTICATION COMPLETE") || output.contains("AUTHENTICATION FAILED");
    !auth_finished
}

/// Adjust a bash-reported exit code using captured command output.
pub fn infer_exit_code_from_output(reported: i32, output: &str) -> i32 {
    if reported != 0 {
        return reported;
    }
    if output_indicates_failure(output) {
        1
    } else {
        reported
    }
}

/// Common failure signatures visible in stderr/stdout despite exit code 0.
pub fn output_indicates_failure(output: &str) -> bool {
    const MARKERS: &[&str] = &[
        "Failed to restart",
        "Failed to start",
        "Failed to stop",
        "Failed to reload",
        "could not be found",
        "Access denied",
        "Permission denied",
        "command not found",
        "未找到命令",
        "No such file or directory",
        "AUTHENTICATION FAILED",
    ];
    if MARKERS.iter().any(|m| output.contains(m)) {
        return true;
    }
    // systemd unit missing — require the combined phrase, not "Unit " alone.
    output.contains(".service not found")
        || output.contains(".socket not found")
        || output.contains(".target not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polkit_in_progress_while_password_pending() {
        let out =
            "==== AUTHENTICATING FOR org.freedesktop.systemd1.manage-units ====\n请输入密码\n";
        assert!(polkit_auth_in_progress(out));
    }

    #[test]
    fn polkit_not_in_progress_after_complete() {
        let out = "==== AUTHENTICATION COMPLETE ====\nFailed to restart foo.service: Unit foo not found.\n";
        assert!(!polkit_auth_in_progress(out));
    }

    #[test]
    fn infer_failure_from_systemctl_output() {
        let out = "Failed to restart sssh.service: Unit sssh.service not found.\n";
        assert_eq!(infer_exit_code_from_output(0, out), 1);
        assert_eq!(infer_exit_code_from_output(5, out), 5);
    }

    #[test]
    fn infer_success_unchanged() {
        assert_eq!(infer_exit_code_from_output(0, "hello\n"), 0);
    }

    #[test]
    fn infer_success_systemctl_status_active() {
        let out = "● nginx.service - Web server\n   Active: active (running)\n";
        assert_eq!(infer_exit_code_from_output(0, out), 0);
    }
}
