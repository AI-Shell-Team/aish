//! Session-scoped approval memory for tool preflight confirmations.
//!
//! When the sandbox is enabled, the shell remembers a user's "allow and don't
//! ask again" decision for the same host + command within the current session.
//! This avoids re-prompting on repeated equivalent commands (e.g. restarting
//! the same service) while still confirming every distinct command.
//!
//! Design decisions locked with the user:
//! - Only `Allow` decisions are remembered. Denials are never persisted, so a
//!   refused command is re-confirmed if the AI retries it. Repeated denials are
//!   avoided in practice by offering "reply to AI" feedback that steers the
//!   model toward a different approach.
//! - Memory lives in-process for the session only. Nothing is written to disk.
//! - Path arguments are kept intact during normalization: the memory key is
//!   the full command text, so `rm /a` and `rm /b` are distinct keys and a
//!   remembered approval never replays against a different path.

use std::collections::HashSet;

/// The user's choice when asked to confirm a tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    /// Allow this single execution; do not remember.
    Once,
    /// Allow and remember for the rest of the session (same host + command).
    RememberSession,
    /// Deny and ask the AI to try a different approach.
    ReplyToAi,
    /// Deny this execution only; do not remember.
    Deny,
}

/// Session-scoped memory of approved operations.
///
/// The key is `(tool_name, host_key, normalized_target)`. Scoping by tool
/// name keeps a remembered web-fetch host from ever auto-approving a shell
/// command of the same text. Looking up or recording an entry always
/// re-derives its key from the raw text, so callers never normalize
/// themselves.
#[derive(Debug, Default)]
pub struct ApprovalMemory {
    allowed: HashSet<(String, String, String)>,
}

impl ApprovalMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive the memory key for a command: normalize once, drop empty, and
    /// compute the host from the *normalized* text so a leading wrapper such
    /// as `nohup` does not fragment remote commands between localhost and
    /// their real host.
    fn key(tool_name: &str, command: &str) -> Option<(String, String, String)> {
        let normalized = normalize_command(command);
        if normalized.is_empty() {
            return None;
        }
        Some((
            tool_name.to_string(),
            command_host_key(&normalized),
            normalized,
        ))
    }

    /// Returns true if this `(tool, command)` was previously approved for the
    /// session on its target host.
    pub fn is_allowed(&self, tool_name: &str, command: &str) -> bool {
        match Self::key(tool_name, command) {
            Some(key) => self.allowed.contains(&key),
            None => false,
        }
    }

    /// Record an approval so subsequent equivalent operations skip the prompt.
    /// Empty commands are ignored.
    pub fn remember(&mut self, tool_name: &str, command: &str) {
        if let Some(key) = Self::key(tool_name, command) {
            self.allowed.insert(key);
        }
    }

    /// Forget all remembered approvals for this session.
    pub fn clear(&mut self) {
        self.allowed.clear();
    }

    /// Number of remembered approvals (mainly for diagnostics/tests).
    pub fn len(&self) -> usize {
        self.allowed.len()
    }

    /// Whether no approvals are currently remembered.
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

/// Identify the host a command targets.
///
/// Remote commands (`ssh user@host ...`, `telnet host`, ...) target the
/// extracted host; everything else targets the local machine. This mirrors
/// aish-shell's `extract_remote_host` but is duplicated here so this crate
/// stays free of a downstream dependency.
pub fn command_host_key(command: &str) -> String {
    extract_remote_host(command).unwrap_or_else(|| "localhost".to_string())
}

/// Normalize a command so wrapper differences don't fragment the memory,
/// while keeping every meaningful byte intact.
///
/// Strips leading/trailing whitespace and a leading bare `nohup` wrapper,
/// then preserves the rest **verbatim**. Whitespace is deliberately NOT
/// collapsed: collapsing it would make two distinct quoted paths such as
/// `rm '/tmp/a  b'` (two spaces) and `rm '/tmp/a b'` (one space) share a
/// key, letting a remembered approval skip confirmation for a different
/// command.
///
/// `sudo` is intentionally NOT stripped: a non-root approval must never
/// auto-authorize the sudo (root) variant of the same command, since sudo
/// elevates privileges.
pub fn normalize_command(command: &str) -> String {
    let mut rest = command.trim();
    while let Some(after) = rest.strip_prefix("nohup") {
        // Bare trailing `nohup` with nothing after it: no real command.
        if after.is_empty() {
            return String::new();
        }
        // `nohup` must be followed by whitespace to be the wrapper, not a
        // command whose name starts with "nohup" (e.g. `nohupyter`).
        match after.strip_prefix([' ', '\t']) {
            Some(followed) => rest = followed.trim_start(),
            None => break,
        }
    }
    rest.to_owned()
}

fn extract_remote_host(command: &str) -> Option<String> {
    let mut parts = command.split_whitespace();
    let cmd = parts.next()?;
    if !matches!(cmd, "ssh" | "telnet" | "mosh" | "sftp" | "nc" | "netcat") {
        return None;
    }
    const OPTS_WITH_ARG: &[&str] = &[
        "-D", "-E", "-F", "-I", "-J", "-L", "-O", "-Q", "-R", "-S", "-W", "-b", "-c", "-e", "-i",
        "-l", "-m", "-o", "-p", "-w",
    ];
    let mut iter = parts.peekable();
    while let Some(part) = iter.next() {
        if part.starts_with('-') {
            let opt_name = if let Some(eq) = part.find('=') {
                &part[..eq]
            } else {
                part
            };
            if OPTS_WITH_ARG.contains(&opt_name) && !part.contains('=') {
                iter.next();
            }
            continue;
        }
        return Some(part.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_memory_allows_nothing() {
        let mem = ApprovalMemory::new();
        assert!(mem.is_empty());
        assert!(!mem.is_allowed("bash", "systemctl restart nginx"));
    }

    #[test]
    fn remembers_equivalent_commands() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "systemctl restart nginx");
        // Identical text matches.
        assert!(mem.is_allowed("bash", "systemctl restart nginx"));
        // nohup wrapper is stripped, so it matches the bare command.
        assert!(mem.is_allowed("bash", "nohup systemctl restart nginx"));
        // Decorative whitespace is NOT collapsed (preserves quoted paths),
        // so a multi-space variant stays distinct.
        assert!(!mem.is_allowed("bash", "systemctl  restart nginx"));
        assert_eq!(mem.len(), 1);
    }

    #[test]
    fn sudo_variant_is_not_authorized_by_non_root_approval() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "systemctl restart nginx");
        // A non-root approval must NOT silently authorize the sudo (root)
        // variant — that would escalate privileges and skip the sandbox.
        assert!(!mem.is_allowed("bash", "sudo systemctl restart nginx"));
        // Reverse direction must also hold.
        let mut mem2 = ApprovalMemory::new();
        mem2.remember("bash", "sudo systemctl restart nginx");
        assert!(!mem2.is_allowed("bash", "systemctl restart nginx"));
        assert!(mem2.is_allowed("bash", "sudo systemctl restart nginx"));
    }

    #[test]
    fn distinguishes_hosts() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "ssh root@host-a systemctl restart nginx");
        assert!(mem.is_allowed("bash", "ssh root@host-a systemctl restart nginx"));
        // Same command text but a different host must not be auto-approved.
        assert!(!mem.is_allowed("bash", "ssh root@host-b systemctl restart nginx"));
    }

    #[test]
    fn nohup_wrapped_remote_matches_unwrapped() {
        // Regression: the host key must be derived from the *normalized*
        // command. Otherwise `nohup ssh root@host ...` would store localhost
        // and never match the bare `ssh root@host ...` (real host).
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "nohup ssh root@host uptime");
        assert!(mem.is_allowed("bash", "ssh root@host uptime"));
        assert!(mem.is_allowed("bash", "nohup ssh root@host uptime"));
        assert!(!mem.is_allowed("bash", "ssh root@other uptime"));
        // Normalizes-to-empty inputs (nohup-only / whitespace) record nothing.
        let mut mem2 = ApprovalMemory::new();
        mem2.remember("bash", "nohup");
        assert!(mem2.is_empty());
        mem2.remember("bash", "   ");
        assert!(mem2.is_empty());
    }

    #[test]
    fn local_commands_share_localhost_key() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "systemctl restart nginx");
        // Same command text (no sudo) matches on localhost.
        assert!(mem.is_allowed("bash", "systemctl restart nginx"));
        assert!(mem.is_allowed("bash", "nohup systemctl restart nginx"));
    }

    #[test]
    fn distinct_commands_are_not_implicitly_approved() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "systemctl restart nginx");
        assert!(!mem.is_allowed("bash", "systemctl restart mysql"));
        assert!(!mem.is_allowed("bash", "systemctl stop nginx"));
    }
    #[test]
    fn path_distinct_commands_are_not_cross_authorized() {
        // After removing the path-bearing guard, the safety argument for
        // remembering path commands rests on the memory key being the full
        // command text. Different paths must stay distinct so a remembered
        // approval never replays against a different path.
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "rm /tmp/a");
        assert!(mem.is_allowed("bash", "rm /tmp/a"));
        assert!(!mem.is_allowed("bash", "rm /tmp/b"));
        assert!(!mem.is_allowed("bash", "rm /tmp/a/x"));
        // A path prefix must not authorize a longer path either.
        mem.remember("bash", "rm /var/log");
        assert!(!mem.is_allowed("bash", "rm /var/log/app.log"));
    }

    #[test]
    fn quoted_whitespace_paths_stay_distinct() {
        // Whitespace inside quotes is semantically meaningful (part of the
        // path). normalize_command preserves it verbatim, so two paths that
        // differ only in internal spacing must NOT share an approval key.
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "rm '/tmp/a  b'");
        assert!(mem.is_allowed("bash", "rm '/tmp/a  b'"));
        assert!(!mem.is_allowed("bash", "rm '/tmp/a b'"));
        // Reverse direction.
        let mut mem2 = ApprovalMemory::new();
        mem2.remember("bash", "rm '/tmp/a b'");
        assert!(!mem2.is_allowed("bash", "rm '/tmp/a  b'"));
    }

    #[test]
    fn clear_wipes_memory() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "systemctl restart nginx");
        assert!(mem.is_allowed("bash", "systemctl restart nginx"));
        mem.clear();
        assert!(mem.is_empty());
        assert!(!mem.is_allowed("bash", "systemctl restart nginx"));
    }

    #[test]
    fn empty_command_is_ignored() {
        let mut mem = ApprovalMemory::new();
        mem.remember("bash", "");
        assert!(mem.is_empty());
        assert!(!mem.is_allowed("bash", ""));
    }

    #[test]
    fn tool_name_scopes_approvals() {
        // The tool_name dimension keeps a remembered operation from leaking
        // across tools: approving a web_fetch host must not also auto-approve
        // a shell command of the same text, and vice versa.
        let mut mem = ApprovalMemory::new();
        mem.remember("web_fetch", "example.com");
        assert!(mem.is_allowed("web_fetch", "example.com"));
        assert!(!mem.is_allowed("bash", "example.com"));
        assert_eq!(mem.len(), 1);
        // Distinct tool names each take a slot.
        mem.remember("bash", "example.com");
        assert_eq!(mem.len(), 2);
    }

    #[test]
    fn strips_leading_wrappers() {
        // nohup is stripped (no privilege change).
        assert_eq!(
            normalize_command("nohup systemctl restart nginx"),
            "systemctl restart nginx"
        );
        // sudo is NOT stripped — it changes privileges.
        assert_eq!(
            normalize_command("sudo systemctl restart nginx"),
            "sudo systemctl restart nginx"
        );
        // Whitespace is preserved verbatim (no collapsing) so quoted paths
        // with different internal spacing stay distinct keys.
        assert_eq!(
            normalize_command("systemctl  restart   nginx"),
            "systemctl  restart   nginx"
        );
        assert_eq!(normalize_command(""), "");
        assert_eq!(normalize_command("nohup"), "");
        assert_eq!(normalize_command("nohup   nohup ls"), "ls");
        // A command whose name merely starts with "nohup" is not stripped.
        assert_eq!(normalize_command("nohupyter run"), "nohupyter run");
    }

    #[test]
    fn extract_remote_host_basics() {
        assert_eq!(
            extract_remote_host("ssh root@host"),
            Some("root@host".into())
        );
        assert_eq!(
            extract_remote_host("ssh -p 2222 user@host ls"),
            Some("user@host".into())
        );
        assert_eq!(
            extract_remote_host("ssh -o StrictHostKeyChecking=no root@host uptime"),
            Some("root@host".into())
        );
        assert_eq!(extract_remote_host("systemctl restart nginx"), None);
        assert_eq!(extract_remote_host(""), None);
    }

    #[test]
    fn ssh_quiet_flag_does_not_consume_host() {
        // Regression: ssh -q is the quiet flag (no argument). Treating -q as
        // an option-with-arg caused `ssh -q host` to skip `host`, returning
        // None and misclassifying the remote command as localhost. This
        // mirrors the same regression test in aish-shell's extract_remote_host.
        assert_eq!(
            extract_remote_host("ssh -q root@host uptime"),
            Some("root@host".into())
        );
        assert_eq!(
            extract_remote_host("ssh -q user@host"),
            Some("user@host".into())
        );
    }

    #[test]
    fn ssh_argument_options_consume_their_value() {
        // -K is a flag (GSSAPI auth, no argument) — host is found.
        assert_eq!(
            extract_remote_host("ssh -K root@host uptime"),
            Some("root@host".into())
        );
        // -Q takes a query_option argument, so the following host is found.
        assert_eq!(
            extract_remote_host("ssh -Q cipher root@host"),
            Some("root@host".into())
        );
        // -D (dynamic forward) takes a port argument.
        assert_eq!(
            extract_remote_host("ssh -D 1080 root@host"),
            Some("root@host".into())
        );
    }

    #[test]
    fn host_key_falls_back_to_localhost() {
        assert_eq!(command_host_key("systemctl restart nginx"), "localhost");
        assert_eq!(
            command_host_key("ssh root@web-1 systemctl restart nginx"),
            "root@web-1"
        );
    }
}
