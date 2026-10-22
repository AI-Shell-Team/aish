//! Classify clear APT failures for the post-command hint.
//!
//! These hints explain a failure and leave the next action to the user. They
//! never retry a command, add privilege, or start an AI request.

const GENERIC_HINT: &str = "shell.error_correction.press_semicolon_hint";
const APT_PERMISSION_HINT: &str = "shell.error_correction.apt_permission_hint";
const PACKAGE_MANAGER_LOCK_HINT: &str = "shell.error_correction.package_manager_lock_hint";

/// Return a more specific hint only when the failed APT command and its output
/// identify a reason that supports a safe, concrete next step.
pub(crate) fn hint_key(command: &str, output: &str) -> &'static str {
    let Some((program, subcommand)) = apt_command(command) else {
        return GENERIC_HINT;
    };

    let output = output.to_lowercase();
    if is_package_manager_lock_error(&output) {
        return PACKAGE_MANAGER_LOCK_HINT;
    }
    if matches!(program, "apt" | "apt-get")
        && subcommand == "update"
        && is_permission_error(&output)
    {
        return APT_PERMISSION_HINT;
    }
    GENERIC_HINT
}

fn apt_command(command: &str) -> Option<(&str, &str)> {
    let words: Vec<_> = command.split_whitespace().collect();
    let mut index = 0;
    if words.first().copied() == Some("sudo") {
        index += 1;
        while matches!(
            words.get(index).copied(),
            Some("-n" | "--non-interactive" | "-E")
        ) {
            index += 1;
        }
    }
    let program = *words.get(index)?;
    if !matches!(program, "apt" | "apt-get") {
        return None;
    }
    let subcommand_index = apt_subcommand_index(&words, index + 1)?;
    Some((program, words[subcommand_index]))
}

/// Find the first APT subcommand after its leading options.
fn apt_subcommand_index(words: &[&str], mut index: usize) -> Option<usize> {
    while let Some(option) = words.get(index).copied() {
        if option == "--" {
            return (index + 1 < words.len()).then_some(index + 1);
        }
        if !option.starts_with('-') {
            return Some(index);
        }

        if matches!(
            option,
            "-c" | "--config-file"
                | "-o"
                | "--option"
                | "-t"
                | "--target-release"
                | "-a"
                | "--host-architecture"
                | "-S"
                | "--snapshot"
                | "--cli-version"
        ) {
            // APT accepts these options with the value in the next token.
            // Do not mistake that value for the subcommand.
            index += 2;
        } else {
            // Boolean flags, combined short flags, and options whose value is
            // attached with '=' or directly after a short option are one token.
            index += 1;
        }
    }
    None
}

fn is_permission_error(output: &str) -> bool {
    [
        "permission denied",
        "permission is denied",
        "permission not granted",
        "权限不够",
        "权限不足",
        "权限被拒绝",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

fn is_package_manager_lock_error(output: &str) -> bool {
    [
        "held by process",
        "held by another process",
        "被另一个进程占用",
        "由另一个进程持有",
        "锁正由进程",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::{hint_key, APT_PERMISSION_HINT, GENERIC_HINT, PACKAGE_MANAGER_LOCK_HINT};

    #[test]
    fn apt_update_permission_failure_gets_a_manual_permission_hint() {
        assert_eq!(
            hint_key(
                "apt update",
                "E: Could not open lock file (13: Permission denied)"
            ),
            APT_PERMISSION_HINT
        );
        assert_eq!(
            hint_key("sudo -n apt-get update", "open (13: 权限不够)"),
            APT_PERMISSION_HINT
        );
    }

    #[test]
    fn apt_update_recognizes_leading_options_and_their_values() {
        for command in [
            "apt-get -q update",
            "apt-get -o APT::Get::Assume-Yes=true update",
            "apt --config-file /tmp/apt.conf -q update",
        ] {
            assert_eq!(
                hint_key(
                    command,
                    "E: Could not open lock file (13: Permission denied)"
                ),
                APT_PERMISSION_HINT,
                "command: {command}"
            );
        }
    }

    #[test]
    fn apt_lock_contention_takes_priority_over_permission_text() {
        assert_eq!(
            hint_key(
                "apt update",
                "Could not get lock. It is held by another process (Permission denied)."
            ),
            PACKAGE_MANAGER_LOCK_HINT
        );
        assert_eq!(
            hint_key(
                "apt-get install curl",
                "Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 1234 (apt-get)"
            ),
            PACKAGE_MANAGER_LOCK_HINT
        );
        assert_eq!(
            hint_key(
                "apt-get install curl",
                "无法获得锁 /var/lib/dpkg/lock-frontend。锁正由进程 1234（apt-get）持有"
            ),
            PACKAGE_MANAGER_LOCK_HINT
        );
    }

    #[test]
    fn lock_errors_without_holder_evidence_keep_the_generic_hint() {
        assert_eq!(
            hint_key(
                "apt-get install curl",
                "Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend), are you root?"
            ),
            GENERIC_HINT
        );
        assert_eq!(
            hint_key(
                "apt-get install curl",
                "Could not get lock /var/lib/dpkg/lock-frontend"
            ),
            GENERIC_HINT
        );
    }

    #[test]
    fn uncertain_permission_errors_keep_the_generic_diagnosis_hint() {
        assert_eq!(
            hint_key("touch /root/file", "Permission denied"),
            GENERIC_HINT
        );
        assert_eq!(
            hint_key("apt install curl", "Permission denied"),
            GENERIC_HINT
        );
    }

    #[test]
    fn unknown_apt_errors_keep_the_generic_diagnosis_hint() {
        assert_eq!(
            hint_key("apt update", "Temporary failure resolving host"),
            GENERIC_HINT
        );
        assert_eq!(
            hint_key("sudo rm -rf /", "Operation not permitted"),
            GENERIC_HINT
        );
    }
}
