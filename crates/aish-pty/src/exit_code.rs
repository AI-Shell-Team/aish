//! Heuristics for command exit codes when PTY/bash reporting is unreliable
//! (e.g. polkit password prompts, stale prompt_ready events).

/// True when captured output shows polkit auth still in progress.
///
/// Only matches the *polkit* pkttyagent banners. Generic password prompts
/// (`password:`, `请输入密码`) are intentionally excluded because sudo, ssh
/// and su all block bash until their auth finishes — by the time bash's
/// PromptReady fires, that auth is necessarily complete and there is
/// nothing to defer. Treating those prompts as "auth in progress" makes
/// the PromptReady event get deferred forever whenever the success line
/// does not match a hard-coded completion string (e.g. zh_CN PAM printing
/// `验证成功` instead of polkit's `==== AUTHENTICATION COMPLETE ====`),
/// which leaves `send_command_interactive` stuck and the shell without a
/// prompt or echo.
///
/// **Known gap**: pkttyagent's banner strings are wrapped in `_()`/`gettext()`
/// and translated under non-English locales. A localized polkit install may
/// emit (for example) `==== 正在为 ... 进行身份验证 ====` instead of the
/// English `==== AUTHENTICATING FOR ... ====` and slip past this check.
/// We deliberately do NOT enumerate translated variants (that's the
/// enumeration anti-pattern that re-introduced this bug once already). The
/// failure mode is bounded by the iteration cap in `send_command_interactive`
/// (see `DEFERRED_MAX_ITERS` in persistent.rs): if this heuristic stays
/// `true` longer than the budget, the deferred events are flushed regardless.
pub fn polkit_auth_in_progress(output: &str) -> bool {
    let polkit_prompted = output.contains("AUTHENTICATING FOR")
        || output.contains("Authenticating as:");
    if !polkit_prompted {
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

    /// Regression: a sync auth flow (sudo + zh_CN PAM, sudo + English PAM,
    /// ssh) must NOT be mistaken for polkit auth in progress. These flows
    /// block bash, so when PromptReady fires the auth is already done and
    /// must not be deferred. Previously the generic `请输入密码` /
    /// `password:` markers triggered the deferral and, because the success
    /// line (`验证成功` / silent) was not in the hard-coded finish list,
    /// `send_command_interactive` never returned — leaving the shell
    /// without prompt or echo.
    #[test]
    fn sync_sudo_zh_pam_not_treated_as_polkit() {
        let out = "请输入密码:\n验证成功\n";
        assert!(!polkit_auth_in_progress(out));
    }

    #[test]
    fn sync_sudo_en_not_treated_as_polkit() {
        let out = "[sudo] password for user: \n";
        assert!(!polkit_auth_in_progress(out));
    }

    #[test]
    fn sync_ssh_password_not_treated_as_polkit() {
        let out = "user@host's password: ";
        assert!(!polkit_auth_in_progress(out));
    }

    /// Known limitation: `auth_finished` is a substring match over the whole
    /// output buffer. Within a single `send_command_interactive` call that
    /// triggers polkit auth more than once (e.g. `systemctl stop A && systemctl
    /// stop B` as non-root), the first session's `AUTHENTICATION COMPLETE`
    /// remains in the buffer and masks the second session's in-progress state.
    /// This test documents the current (imperfect) behavior so a future fix
    /// that switches to last-occurrence or per-section matching updates this
    /// assertion intentionally. The `DEFERRED_MAX_ITERS` cap in persistent.rs
    /// bounds the worst case to a ~10 s delay.
    #[test]
    fn known_limitation_stale_complete_masks_new_auth() {
        let out = "\
==== AUTHENTICATING FOR org.freedesktop.systemd1.manage-units ====
Password:
==== AUTHENTICATION COMPLETE ====
==== AUTHENTICATING FOR org.freedesktop.systemd1.manage-units ====
Password:
";
        // Ideally this should be `true` (second auth in progress), but the
        // whole-buffer substring match returns `false` because the first
        // session's COMPLETE line is still present.
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
