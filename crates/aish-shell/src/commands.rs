use crate::types::ShellState;
use aish_i18n::{t, t_with_args};

/// Result of handling a built-in command.
pub struct BuiltinResult {
    pub handled: bool,
    pub output: Option<String>,
    pub should_exit: bool,
    pub route_to_pty: bool,
    pub pty_command: Option<String>,
}

impl BuiltinResult {
    pub fn handled(output: impl Into<String>) -> Self {
        Self {
            handled: true,
            output: Some(output.into()),
            should_exit: false,
            route_to_pty: false,
            pty_command: None,
        }
    }

    pub fn handled_no_output() -> Self {
        Self {
            handled: true,
            output: None,
            should_exit: false,
            route_to_pty: false,
            pty_command: None,
        }
    }

    pub fn not_handled() -> Self {
        Self {
            handled: false,
            output: None,
            should_exit: false,
            route_to_pty: false,
            pty_command: None,
        }
    }

    pub fn exit() -> Self {
        Self {
            handled: true,
            output: None,
            should_exit: true,
            route_to_pty: false,
            pty_command: None,
        }
    }
}

/// Commands that require a PTY for interactive input.
pub const PTY_REQUIRING_COMMANDS: &[&str] = &["su", "sudo"];

/// Basename of a command token (`/usr/bin/sudo` → `sudo`).
pub fn command_basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// True when `token` is `su` or `sudo`, including an absolute path.
pub fn is_privilege_command_token(token: &str) -> bool {
    PTY_REQUIRING_COMMANDS.contains(&command_basename(token))
}

/// Check whether a command requires a PTY (interactive terminal).
pub fn is_pty_requiring(cmd: &str) -> bool {
    is_privilege_command_token(cmd)
}

/// Text shown at the security confirmation and the text submitted to the PTY.
///
/// Both fields are the original line. Privilege commands are not rebuilt
/// from whitespace tokens, so quotes, escaped spaces, and newlines survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegeCommandPlan {
    pub confirm_text: String,
    pub execute_text: String,
}

/// True when `line` contains shell syntax that bash would act on.
///
/// Quoted text is not syntax: `export MSG='a && b'` is one assignment.
/// `$` and backticks still count inside double quotes (`export FOO="$(date)"`).
/// Issue #574: without this check, `cd /tmp && pwd` is whitespace-split and
/// `pwd` becomes the `cd` target.
pub fn has_unquoted_shell_syntax(line: &str) -> bool {
    let mut chars = line.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }
        if ch == '\\' {
            let escaped = chars.peek().copied();
            if in_double {
                if matches!(escaped, Some('$' | '`' | '"' | '\\' | '\n')) {
                    chars.next();
                }
            } else {
                chars.next();
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = true;
            continue;
        }
        if ch == '"' {
            in_double = !in_double;
            continue;
        }
        if in_double {
            if ch == '$' || ch == '`' {
                return true;
            }
            continue;
        }
        if matches!(
            ch,
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '`' | '$' | '\n'
        ) {
            return true;
        }
    }
    false
}

/// Plan a `sudo` / `su` submission. `None` when the first token is not one.
pub fn plan_privilege_command(line: &str) -> Option<PrivilegeCommandPlan> {
    let trimmed = line.trim();
    let first = trimmed.split_whitespace().next()?;
    if !is_privilege_command_token(first) {
        return None;
    }
    let text = trimmed.to_string();
    Some(PrivilegeCommandPlan {
        confirm_text: text.clone(),
        execute_text: text,
    })
}

/// SHA-256 hex digest of a command line, used to compare confirm and execute text.
pub fn command_text_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

impl ShellState {
    /// Dispatch one submitted line.
    ///
    /// `sudo` / `su` keep the original text. Other lines the shell session
    /// must parse (`cd`, `export`, `pwd`, compound commands) do too.
    /// `exit` / `help` / `setup` with no shell syntax stay here.
    pub fn handle_builtin_line(&mut self, line: &str) -> BuiltinResult {
        let trimmed = line.trim();
        if let Some(plan) = plan_privilege_command(trimmed) {
            return BuiltinResult {
                handled: false,
                output: None,
                should_exit: false,
                route_to_pty: true,
                pty_command: Some(plan.execute_text),
            };
        }
        if !crate::shell_session::aish_handles_line(trimmed) {
            return BuiltinResult {
                handled: false,
                output: None,
                should_exit: false,
                route_to_pty: true,
                pty_command: Some(trimmed.to_string()),
            };
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        match parts.first().copied() {
            Some(cmd) => self.handle_builtin(cmd, &parts[1..]),
            None => BuiltinResult::not_handled(),
        }
    }

    /// Dispatch a built-in command by name.
    pub fn handle_builtin(&mut self, cmd: &str, args: &[&str]) -> BuiltinResult {
        match cmd {
            "help" => self.handle_help(args),
            "exit" | "quit" | "logout" => self.handle_exit(),
            "setup" => self.handle_setup(args),
            _ => BuiltinResult::not_handled(),
        }
    }

    // -- help ----------------------------------------------------------------

    fn handle_help(&self, args: &[&str]) -> BuiltinResult {
        match args.first() {
            Some(topic) => self.render_topic_help(topic),
            None => {
                let title = t("help.general.title");
                println!("\n{}\n", crate::theme::accent(&crate::theme::bold(&title)));
                let markdown = t("help.general.markdown");
                crate::renderer::ShellRenderer::new().render_markdown(&markdown);
                BuiltinResult::handled_no_output()
            }
        }
    }

    fn render_topic_help(&self, topic: &str) -> BuiltinResult {
        let title_key = format!("help.topics.{}.title", topic);
        let title = t(&title_key);
        // If the returned value equals the key itself, the topic doesn't exist
        if title == title_key {
            let mut args = std::collections::HashMap::new();
            args.insert("topic".to_string(), topic.to_string());
            eprintln!("{}", t_with_args("help.topics.unknown", &args));
            return BuiltinResult::handled_no_output();
        }
        println!("\n{}\n", crate::theme::accent(&crate::theme::bold(&title)));
        let md_key = format!("help.topics.{}.markdown", topic);
        let markdown = t(&md_key);
        crate::renderer::ShellRenderer::new().render_markdown(&markdown);
        BuiltinResult::handled_no_output()
    }

    // -- exit ----------------------------------------------------------------

    fn handle_exit(&mut self) -> BuiltinResult {
        self.should_exit = true;
        BuiltinResult {
            handled: true,
            output: Some(t("shell.exit_goodbye")),
            should_exit: true,
            route_to_pty: false,
            pty_command: None,
        }
    }

    // -- setup ----------------------------------------------------------------

    fn handle_setup(&mut self, _args: &[&str]) -> BuiltinResult {
        // Routed through `AishShell::run_setup_wizard` in app.rs.
        BuiltinResult::not_handled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pty_requiring() {
        assert!(is_pty_requiring("sudo"));
        assert!(is_pty_requiring("su"));
        assert!(is_pty_requiring("/usr/bin/sudo"));
        assert!(is_pty_requiring("/bin/su"));
        assert!(!is_pty_requiring("ls"));
    }

    #[test]
    fn test_builtin_result_handled_includes_new_fields() {
        let result = BuiltinResult::handled("test output");
        assert!(result.handled);
        assert_eq!(result.output, Some("test output".to_string()));
        assert!(!result.should_exit);
        assert!(!result.route_to_pty);
        assert_eq!(result.pty_command, None);
    }

    #[test]
    fn test_builtin_result_handled_no_output_includes_new_fields() {
        let result = BuiltinResult::handled_no_output();
        assert!(result.handled);
        assert_eq!(result.output, None);
        assert!(!result.should_exit);
        assert!(!result.route_to_pty);
        assert_eq!(result.pty_command, None);
    }

    #[test]
    fn test_builtin_result_not_handled_includes_new_fields() {
        let result = BuiltinResult::not_handled();
        assert!(!result.handled);
        assert_eq!(result.output, None);
        assert!(!result.should_exit);
        assert!(!result.route_to_pty);
        assert_eq!(result.pty_command, None);
    }

    #[test]
    fn test_builtin_result_exit_includes_new_fields() {
        let result = BuiltinResult::exit();
        assert!(result.handled);
        assert_eq!(result.output, None);
        assert!(result.should_exit);
        assert!(!result.route_to_pty);
        assert_eq!(result.pty_command, None);
    }

    #[test]
    fn privilege_plan_keeps_quoted_runs_of_spaces() {
        let line = "sudo printf '%s\\n' 'a  b'";
        let plan = plan_privilege_command(line).expect("sudo line");
        assert_eq!(plan.execute_text, line);
        assert_eq!(plan.confirm_text, line);
        assert_eq!(plan.confirm_text.len(), line.len());
        let digest = command_text_digest(line);
        assert_eq!(command_text_digest(&plan.confirm_text), digest);
        assert_eq!(command_text_digest(&plan.execute_text), digest);
    }

    #[test]
    fn privilege_plan_keeps_escaped_spaces() {
        let line = "sudo echo a\\ \\ b";
        let plan = plan_privilege_command(line).expect("sudo line");
        assert_eq!(plan.execute_text, "sudo echo a\\ \\ b");
        assert_eq!(plan.confirm_text, plan.execute_text);
    }

    #[test]
    fn privilege_plan_keeps_sh_c_script() {
        let line = "sudo sh -c 'printf \"%s\" \"a  b\"'";
        let plan = plan_privilege_command(line).expect("sudo line");
        assert_eq!(plan.execute_text, line);
        assert_eq!(plan.confirm_text, plan.execute_text);
    }

    #[test]
    fn privilege_plan_keeps_embedded_newline() {
        let line = "sudo printf '%s' 'a\n\nb'";
        let plan = plan_privilege_command(line).expect("sudo line");
        assert_eq!(plan.execute_text, line);
        assert!(plan.execute_text.contains('\n'));
    }

    #[test]
    fn absolute_sudo_uses_the_original_line() {
        let line = "/usr/bin/sudo id";
        let plan = plan_privilege_command(line).expect("absolute sudo");
        assert_eq!(plan.execute_text, line);
        assert_eq!(plan.confirm_text, line);
        assert_eq!(
            command_text_digest(&plan.confirm_text),
            command_text_digest(&plan.execute_text)
        );
    }

    #[test]
    fn su_plan_keeps_quoted_argument() {
        let line = "su -c 'echo a  b'";
        let plan = plan_privilege_command(line).expect("su line");
        assert_eq!(plan.execute_text, line);
        assert_eq!(plan.confirm_text, plan.execute_text);
    }

    #[test]
    fn non_privilege_line_has_no_plan() {
        assert!(plan_privilege_command("ls -la").is_none());
        assert!(plan_privilege_command("echo sudo ls").is_none());
    }

    #[test]
    fn builtin_line_routes_original_sudo_text() {
        let mut state = ShellState::new();
        let line = "sudo printf '%s\\n' 'a  b'";
        let result = state.handle_builtin_line(line);
        assert!(result.route_to_pty);
        assert_eq!(result.pty_command.as_deref(), Some(line));
        assert!(!result.should_exit);
    }

    #[test]
    fn builtin_line_routes_original_su_text() {
        let mut state = ShellState::new();
        let line = "su -c 'echo a  b'";
        let result = state.handle_builtin_line(line);
        assert!(result.route_to_pty);
        assert_eq!(result.pty_command.as_deref(), Some(line));
    }

    #[test]
    fn shell_syntax_ignores_quoted_operators() {
        assert!(!has_unquoted_shell_syntax("cd /tmp"));
        assert!(!has_unquoted_shell_syntax("export FOO=bar"));
        assert!(!has_unquoted_shell_syntax("export MSG='a && b'"));
        assert!(!has_unquoted_shell_syntax("export MSG=\"a && b\""));
        assert!(!has_unquoted_shell_syntax("unset FOO"));
        assert!(has_unquoted_shell_syntax("cd && pwd"));
        assert!(has_unquoted_shell_syntax("cd /tmp; pwd"));
        assert!(has_unquoted_shell_syntax("cd /tmp | pwd"));
        assert!(has_unquoted_shell_syntax("cd /tmp || pwd"));
        assert!(has_unquoted_shell_syntax("cd /tmp > /dev/null"));
        assert!(has_unquoted_shell_syntax("unset ; -z"));
        assert!(has_unquoted_shell_syntax("export A=1 && echo hi"));
        assert!(has_unquoted_shell_syntax("export FOO=$(date)"));
        assert!(has_unquoted_shell_syntax("export FOO=\"$(date)\""));
    }

    /// Issue #574, minimised: `cd && pwd` was reported as
    /// `cd: pwd: No such file or directory`. The reported line is the same failure.
    #[test]
    fn compound_cd_is_handed_to_the_shell() {
        for line in ["cd && pwd", "cd /tmp && pwd"] {
            let mut state = ShellState::new();
            let cwd = state.cwd.clone();
            let result = state.handle_builtin_line(line);
            let output = result.output.unwrap_or_default();
            assert!(result.route_to_pty, "line {line:?} symptom: {output}");
            assert_eq!(result.pty_command.as_deref(), Some(line));
            assert!(
                !output.contains("No such file or directory"),
                "line {line:?} symptom: {output}"
            );
            assert_eq!(state.cwd, cwd);
        }
    }

    /// Issue #574, minimised: `unset ; -z` was reported as
    /// `unset: invalid option -- 'z'`.
    #[test]
    fn compound_unset_is_handed_to_the_shell() {
        let reported = "unset AISH_AUDIT_VAR; test -z \"$AISH_AUDIT_VAR\" && printf UNSET_OK";
        for line in ["unset ; -z", reported] {
            let mut state = ShellState::new();
            let result = state.handle_builtin_line(line);
            let output = result.output.unwrap_or_default();
            assert!(result.route_to_pty, "line {line:?} symptom: {output}");
            assert_eq!(result.pty_command.as_deref(), Some(line));
            assert!(!output.contains("invalid option"), "symptom: {output}");
        }
    }

    /// Same splitter, silent success: the assignment was applied and the
    /// rest of the line was dropped.
    #[test]
    fn compound_export_does_not_apply_partially() {
        let mut state = ShellState::new();
        let key = "AISH_ISSUE_574";
        let before = state.env_vars.get(key).cloned();
        let line = format!("export {key}=1 && echo hi");
        let result = state.handle_builtin_line(&line);
        assert!(result.route_to_pty);
        assert_eq!(result.pty_command.as_deref(), Some(line.as_str()));
        assert_eq!(state.env_vars.get(key), before.as_ref());
        std::env::remove_var(key);
    }

    #[test]
    fn pwd_is_handed_to_the_session() {
        let mut state = ShellState::new();
        let result = state.handle_builtin_line("pwd");
        assert!(result.route_to_pty);
        assert_eq!(result.pty_command.as_deref(), Some("pwd"));
        assert!(result.output.is_none());
    }

    #[test]
    fn test_handle_exit_has_confirmation_message() {
        let mut state = ShellState::new();
        let result = state.handle_exit();
        assert!(result.handled);
        assert!(result.should_exit);
        assert!(result.output.is_some_and(|s| !s.is_empty()));
        assert!(!result.route_to_pty);
        assert_eq!(result.pty_command, None);
    }
}
