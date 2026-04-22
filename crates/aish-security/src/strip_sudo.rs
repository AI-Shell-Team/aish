//! POSIX shell tokenizer for stripping sudo prefix from commands.
//!
//! This module provides functionality to detect and strip the `sudo` prefix
//! from shell commands, including handling various sudo flags and quoted arguments.

use std::collections::HashSet;

/// Strip leading sudo prefix and flags.
///
/// Returns a tuple of (stripped_command, sudo_detected, ok):
/// - `stripped_command`: The command with sudo prefix removed, or original if no sudo detected
/// - `sudo_detected`: true if sudo was found at the start
/// - `ok`: false if sudo was present but no valid command followed it
///
/// # Examples
///
/// ```
/// use aish_security::strip_sudo::strip_sudo_prefix;
///
/// // No sudo prefix
/// let (cmd, detected, ok) = strip_sudo_prefix("ls -la");
/// assert_eq!(cmd, "ls -la");
/// assert_eq!(detected, false);
/// assert_eq!(ok, true);
///
/// // Simple sudo command
/// let (cmd, detected, ok) = strip_sudo_prefix("sudo ls");
/// assert_eq!(cmd, "ls");
/// assert_eq!(detected, true);
/// assert_eq!(ok, true);
///
/// // Sudo with user flag
/// let (cmd, detected, ok) = strip_sudo_prefix("sudo -u root ls -la");
/// assert_eq!(cmd, "ls -la");
/// assert_eq!(detected, true);
/// assert_eq!(ok, true);
/// ```
pub fn strip_sudo_prefix(command: &str) -> (String, bool, bool) {
    let raw = if command.is_empty() { "" } else { command };
    let raw_l = raw.trim_start();

    // Check if it starts with "sudo "
    if !(raw_l.starts_with("sudo ") || raw_l == "sudo") {
        return (command.to_string(), false, true);
    }

    let idx = 0;
    let (token, idx) = read_token(raw_l, idx);
    if token != "sudo" {
        return (command.to_string(), false, true);
    }

    let options_with_value: HashSet<&'static str> =
        HashSet::from(["-u", "--user", "-g", "--group", "-h", "-p", "--prompt"]);

    let mut idx = idx;
    loop {
        idx = skip_ws(raw_l, idx);
        if idx >= raw_l.len() {
            return (String::new(), true, false);
        }

        let (opt, opt_end) = read_token(raw_l, idx);
        if opt.is_empty() {
            return (String::new(), true, false);
        }

        // Check for double dash separator
        if opt == "--" {
            idx = opt_end;
            break;
        }

        // Check if it's an option
        if opt.starts_with('-') {
            // Options that take a value
            if options_with_value.contains(opt.as_str()) {
                idx = opt_end;
                let (_val, new_idx) = read_token(raw_l, idx);
                idx = new_idx;
                continue;
            }

            // Combined short options: -u<value> or -g<value>
            if opt.starts_with("-u") && opt != "-u" {
                idx = opt_end;
                continue;
            }
            if opt.starts_with("-g") && opt != "-g" {
                idx = opt_end;
                continue;
            }

            // Long options with =: --user=, --group=, --prompt=
            if opt.starts_with("--user=")
                || opt.starts_with("--group=")
                || opt.starts_with("--prompt=")
            {
                idx = opt_end;
                continue;
            }

            // Other options (boolean flags)
            idx = opt_end;
            continue;
        }

        // Not an option, so this must be the command
        break;
    }

    let stripped = raw_l[idx..].trim_start();
    if stripped.is_empty() {
        return (String::new(), true, false);
    }

    (stripped.to_string(), true, true)
}

/// Check if a character is whitespace.
fn is_space(ch: char) -> bool {
    ch.is_whitespace()
}

/// Skip whitespace characters starting from idx.
/// Returns the new index after skipping whitespace.
fn skip_ws(s: &str, mut idx: usize) -> usize {
    let bytes = s.as_bytes();
    while idx < bytes.len() {
        let ch = bytes[idx];
        if ch <= 0x7f && is_space(ch as char) {
            idx += 1;
        } else if ch > 0x7f {
            // Multi-byte UTF-8 — these are never ASCII whitespace.
            // Advance by one byte is wrong for multi-byte, but ASCII whitespace
            // is all we care about, so just stop.
            break;
        } else {
            break;
        }
    }
    idx
}

/// Read a single token from the string, handling quotes and escapes.
/// Returns (token_content, new_idx).
fn read_token(s: &str, idx: usize) -> (String, usize) {
    let idx = skip_ws(s, idx);
    if idx >= s.len() {
        return (String::new(), idx);
    }

    let mut out = String::new();
    let mut in_squote = false;
    let mut in_dquote = false;

    // Build char_indices starting from current byte offset
    let mut iter = s[idx..].char_indices().peekable();

    while let Some(&(_, ch)) = iter.peek() {
        // Break on whitespace if not in quotes
        if !in_squote && !in_dquote && is_space(ch) {
            break;
        }

        // Handle single quotes
        if ch == '\'' && !in_dquote {
            in_squote = !in_squote;
            iter.next();
            continue;
        }

        // Handle double quotes
        if ch == '"' && !in_squote {
            in_dquote = !in_dquote;
            iter.next();
            continue;
        }

        // Handle backslash escapes (not in single quotes)
        if ch == '\\' && !in_squote {
            iter.next(); // consume backslash
            if let Some(&(_, next_ch)) = iter.peek() {
                out.push(next_ch);
                iter.next(); // consume escaped char
            }
            continue;
        }

        out.push(ch);
        iter.next();
    }

    // Compute final byte position
    let final_pos = if let Some(&(offset, _)) = iter.peek() {
        idx + offset
    } else {
        s.len()
    };

    (out, final_pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_sudo() {
        let (cmd, detected, ok) = strip_sudo_prefix("ls -la");
        assert_eq!(cmd, "ls -la");
        assert!(!detected);
        assert!(ok);
    }

    #[test]
    fn test_simple_sudo() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_flags() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -u root ls -la");
        assert_eq!(cmd, "ls -la");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_double_dash() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -u root -- ls -la");
        assert_eq!(cmd, "ls -la");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_alone() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo");
        assert_eq!(cmd, "");
        assert!(detected);
        assert!(!ok);
    }

    #[test]
    fn test_sudo_incomplete_flag() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -u");
        assert_eq!(cmd, "");
        assert!(detected);
        assert!(!ok);
    }

    #[test]
    fn test_sudo_with_equals() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo --user=admin bash -c 'rm -rf /'");
        assert_eq!(cmd, "bash -c 'rm -rf /'");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_quoted_value() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -u 'root user' cmd");
        assert_eq!(cmd, "cmd");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_double_dash_separator() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -- ls && echo hi");
        assert_eq!(cmd, "ls && echo hi");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_combined_flag() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -uroot ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_group_flag() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -g admin -u user ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_empty_string() {
        let (cmd, detected, ok) = strip_sudo_prefix("");
        assert_eq!(cmd, "");
        assert!(!detected);
        assert!(ok);
    }

    #[test]
    fn test_whitespace_only() {
        let (cmd, detected, ok) = strip_sudo_prefix("   ");
        assert_eq!(cmd, "   ");
        assert!(!detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_prompt_flag() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -p 'Password: ' ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_multiple_flags() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -u root -g wheel -H ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_escaped_spaces() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo -u root\\ user ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_double_quoted_command() {
        let (cmd, detected, ok) = strip_sudo_prefix("sudo bash -c \"echo hello\"");
        assert_eq!(cmd, "bash -c \"echo hello\"");
        assert!(detected);
        assert!(ok);
    }

    #[test]
    fn test_not_sudo_at_start() {
        let (cmd, detected, ok) = strip_sudo_prefix("echo sudo ls");
        assert_eq!(cmd, "echo sudo ls");
        assert!(!detected);
        assert!(ok);
    }

    #[test]
    fn test_sudo_with_leading_ws() {
        let (cmd, detected, ok) = strip_sudo_prefix("  sudo ls");
        assert_eq!(cmd, "ls");
        assert!(detected);
        assert!(ok);
    }
}
