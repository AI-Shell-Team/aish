use aish_llm::{Tool, ToolResult};
use aish_security::SecurityDecision;

/// Type of the security check callback.
type SecurityCheckFn = Box<dyn Fn(&str) -> SecurityDecision + Send + Sync>;

/// Strip sudo prefix (with optional flags like -u, -g, -E, etc.) from a command.
/// Returns the command with the sudo portion removed.
fn strip_sudo(command: &str) -> &str {
    let trimmed = command.trim();
    // Check if command starts with "sudo" followed by whitespace or end of string
    if !trimmed.starts_with("sudo") {
        return trimmed;
    }
    // Make sure it's actually "sudo" not e.g. "sudoedit"
    let after_sudo = trimmed.get(4..).unwrap_or("");
    if !after_sudo.is_empty() && !after_sudo.starts_with(char::is_whitespace) {
        return trimmed;
    }
    let mut rest = after_sudo.trim_start();
    // Skip sudo flags (-u root, -E, -i, -S, -s, etc.)
    // Flags that take an argument: -u, -g, -U, -p, -r, -t, -T, -A, -C, -D, -K
    while rest.starts_with('-') {
        // Find end of flag token
        let flag_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let flag = &rest[..flag_end];
        rest = rest[flag_end..].trim_start();
        // Some flags take an argument value (next token)
        // e.g., -u root, -g group, -C 3
        if flag.contains('u')
            || flag.contains('g')
            || flag.contains('p')
            || flag.contains('r')
            || flag.contains('t')
            || flag.contains('C')
            || flag.contains('T')
            || flag.contains('D')
            || flag.contains('A')
        {
            // Skip the next token (argument value)
            if let Some(space) = rest.find(char::is_whitespace) {
                rest = rest[space..].trim_start();
            } else {
                rest = "";
            }
        }
    }
    rest
}

/// Bash tool wrapper that adds security confirmation before execution.
///
/// When the security check returns `Confirm`, the user is prompted on stdin.
/// When it returns `Block`, execution is denied immediately.
pub struct SecureBashTool {
    inner: crate::bash::BashTool,
    security_check: Option<SecurityCheckFn>,
}

impl SecureBashTool {
    /// Create a new secure bash tool without a security check (allows all).
    pub fn new() -> Self {
        Self {
            inner: crate::bash::BashTool::new(),
            security_check: None,
        }
    }

    /// Create a secure bash tool with the given security check function.
    pub fn with_security_check(
        security_check: impl Fn(&str) -> SecurityDecision + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: crate::bash::BashTool::new(),
            security_check: Some(Box::new(security_check)),
        }
    }
}

impl Default for SecureBashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for SecureBashTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    fn preflight(&self, args: &serde_json::Value) -> aish_llm::PreflightResult {
        let command = match args.get("command").and_then(|c| c.as_str()) {
            Some(cmd) => cmd,
            None => return aish_llm::PreflightResult::Allow,
        };

        // Strip sudo prefix before security check
        let effective_command = strip_sudo(command);

        if let Some(ref check) = self.security_check {
            match check(effective_command) {
                SecurityDecision::Allow => aish_llm::PreflightResult::Allow,
                SecurityDecision::Confirm { reason } => {
                    aish_llm::PreflightResult::Confirm { message: reason }
                }
                SecurityDecision::Block { reason } => {
                    aish_llm::PreflightResult::Block { message: reason }
                }
            }
        } else {
            aish_llm::PreflightResult::Allow
        }
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        // Security check is now handled by preflight()
        self.inner.execute(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_security_check(cmd: &str) -> aish_security::SecurityDecision {
        if cmd.starts_with("rm") {
            aish_security::SecurityDecision::Block {
                reason: "rm is blocked".into(),
            }
        } else {
            aish_security::SecurityDecision::Allow
        }
    }

    #[test]
    fn test_strip_sudo_basic() {
        assert_eq!(strip_sudo("sudo rm -rf /"), "rm -rf /");
    }

    #[test]
    fn test_strip_sudo_no_sudo() {
        assert_eq!(strip_sudo("ls -la"), "ls -la");
    }

    #[test]
    fn test_strip_sudo_with_user_flag() {
        assert_eq!(strip_sudo("sudo -u root rm -rf /"), "rm -rf /");
    }

    #[test]
    fn test_strip_sudo_multiple_flags() {
        assert_eq!(strip_sudo("sudo -E -u root rm -rf /"), "rm -rf /");
    }

    #[test]
    fn test_strip_sudo_just_sudo() {
        assert_eq!(strip_sudo("sudo"), "");
    }

    #[test]
    fn test_strip_sudo_not_sudo_command() {
        assert_eq!(strip_sudo("sudoedit /etc/hosts"), "sudoedit /etc/hosts");
    }

    #[test]
    fn test_preflight_strips_sudo() {
        let tool = SecureBashTool::with_security_check(test_security_check);
        let args = serde_json::json!({"command": "sudo rm -rf /"});
        let result = tool.preflight(&args);
        assert!(matches!(result, aish_llm::PreflightResult::Block { .. }));
    }

    #[test]
    fn test_preflight_sudo_with_allowed_command() {
        let tool = SecureBashTool::with_security_check(test_security_check);
        let args = serde_json::json!({"command": "sudo ls -la"});
        let result = tool.preflight(&args);
        assert_eq!(result, aish_llm::PreflightResult::Allow);
    }

    #[test]
    fn test_preflight_no_sudo_blocks() {
        let tool = SecureBashTool::with_security_check(test_security_check);
        let args = serde_json::json!({"command": "rm -rf /"});
        let result = tool.preflight(&args);
        assert!(matches!(result, aish_llm::PreflightResult::Block { .. }));
    }
}
