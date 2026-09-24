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

/// Commands that modify shell state (cd, pushd, popd, export, unset, dirs).
pub const STATE_MODIFYING_COMMANDS: &[&str] =
    &["cd", "pushd", "popd", "export", "unset", "dirs", "pwd"];

/// Commands that require a PTY for interactive input.
pub const PTY_REQUIRING_COMMANDS: &[&str] = &["su", "sudo"];

/// Commands that should be intercepted (not passed through).
pub const REJECTED_COMMANDS: &[&str] = &["exit", "logout"];

/// Check whether a command is a state-modifying builtin.
pub fn is_state_modifying(cmd: &str) -> bool {
    STATE_MODIFYING_COMMANDS.contains(&cmd)
}

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

/// Check whether a command should be rejected (intercepted).
pub fn is_rejected(cmd: &str) -> bool {
    REJECTED_COMMANDS.contains(&cmd)
}

impl ShellState {
    /// Dispatch one submitted line.
    ///
    /// `sudo` / `su` keep the original text in `pty_command`. Other builtins
    /// still parse arguments with whitespace splitting.
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
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        match parts.first().copied() {
            Some(cmd) => self.handle_builtin(cmd, &parts[1..]),
            None => BuiltinResult::not_handled(),
        }
    }

    /// Dispatch a built-in command by name.
    pub fn handle_builtin(&mut self, cmd: &str, args: &[&str]) -> BuiltinResult {
        match cmd {
            "cd" => self.handle_cd(args),
            "pwd" => self.handle_pwd(args),
            "export" => self.handle_export(args),
            "unset" => self.handle_unset(args),
            "pushd" => self.handle_pushd(args),
            "popd" => self.handle_popd(),
            "dirs" => self.handle_dirs(args),
            "help" => self.handle_help(args),
            "clear" => self.handle_clear(),
            "exit" | "quit" => self.handle_exit(),
            "setup" => self.handle_setup(args),
            _ => BuiltinResult::not_handled(),
        }
    }

    // -- cd ------------------------------------------------------------------

    fn handle_cd(&mut self, args: &[&str]) -> BuiltinResult {
        let mut physical = false;
        let mut target_arg = None;
        let mut i = 0;

        while i < args.len() {
            match args[i] {
                "-P" => physical = true,
                "-L" => physical = false,
                "-e" => {
                    // -e: exit with error if CWD cannot be determined (ignored for now)
                }
                "--" => {
                    // Rest is the target
                    if i + 1 < args.len() {
                        target_arg = Some(args[i + 1]);
                    }
                    break;
                }
                arg if arg.starts_with('-') && arg != "-" => {
                    // Parse combined flags like -PL
                    for ch in arg[1..].chars() {
                        match ch {
                            'P' => physical = true,
                            'L' => physical = false,
                            'e' => {}
                            _ => {
                                return BuiltinResult::handled(format!(
                                    "cd: invalid option -- '{}'",
                                    ch
                                ))
                            }
                        }
                    }
                }
                _ => {
                    target_arg = Some(args[i]);
                }
            }
            i += 1;
        }

        let target = if let Some(arg) = target_arg {
            if arg == "-" {
                match &self.prev_cwd {
                    Some(p) => {
                        println!("{}", p);
                        p.clone()
                    }
                    None => return BuiltinResult::handled("cd: OLDPWD not set"),
                }
            } else if arg.starts_with('~') {
                let home = dirs::home_dir()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_default();
                if let Some(rest) = arg.strip_prefix('~') {
                    format!("{}{}", home, rest)
                } else {
                    arg.to_string()
                }
            } else {
                arg.to_string()
            }
        } else {
            dirs::home_dir()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|| self.cwd.clone())
        };

        // Resolve relative paths against cwd
        let target_path = if std::path::Path::new(&target).is_absolute() {
            std::path::PathBuf::from(&target)
        } else {
            std::path::Path::new(&self.cwd).join(&target)
        };

        match std::env::set_current_dir(&target_path) {
            Ok(()) => {
                let new_cwd = if physical {
                    target_path
                        .canonicalize()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| target)
                } else {
                    // Logical: use the path as given (resolve ../symlinks logically)
                    target_path
                        .canonicalize()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| target)
                };
                self.prev_cwd = Some(self.cwd.clone());
                self.cwd = new_cwd;
                BuiltinResult::handled_no_output()
            }
            Err(e) => BuiltinResult::handled(format!("cd: {}: {}", target, e)),
        }
    }

    // -- pwd -----------------------------------------------------------------

    fn handle_pwd(&mut self, args: &[&str]) -> BuiltinResult {
        let mut physical = false;

        for &arg in args {
            match arg {
                "-P" => physical = true,
                "-L" => physical = false,
                "--" => break,
                _ => {
                    return BuiltinResult::handled(format!(
                        "pwd: invalid option -- '{}'",
                        arg.trim_start_matches('-')
                    ))
                }
            }
        }

        if physical {
            match std::env::current_dir().and_then(|p| p.canonicalize()) {
                Ok(p) => BuiltinResult::handled(p.to_string_lossy().to_string()),
                Err(e) => BuiltinResult::handled(format!("pwd: {}", e)),
            }
        } else {
            BuiltinResult::handled(self.cwd.clone())
        }
    }

    // -- export --------------------------------------------------------------

    fn handle_export(&mut self, args: &[&str]) -> BuiltinResult {
        let mut list_mode = false;
        let mut skip_next = false;
        let mut assignments = Vec::new();
        let mut names_to_export = Vec::new();

        for (i, arg) in args.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == &"--" {
                // Rest are treated as arguments
                for &a in &args[i + 1..] {
                    if a.contains('=') {
                        assignments.push(a);
                    } else {
                        names_to_export.push(a);
                    }
                }
                break;
            }
            if arg == &"-p" {
                list_mode = true;
            } else if arg == &"-n" {
                // -n: next arg is the name to remove export from
                if i + 1 < args.len() {
                    // Remove from our tracking (but can't actually un-export in Rust)
                    let name = args[i + 1];
                    self.env_vars.remove(name);
                    skip_next = true;
                }
            } else if arg == &"-f" {
                // -f: export functions — not supported
                return BuiltinResult::handled("export: -f: functions not supported");
            } else if arg.starts_with('-') {
                // Parse combined flags
                for ch in arg[1..].chars() {
                    match ch {
                        'p' => list_mode = true,
                        'n' => {
                            // Next arg after combined flags
                        }
                        'f' => {
                            return BuiltinResult::handled("export: -f: functions not supported")
                        }
                        _ => {
                            return BuiltinResult::handled(format!(
                                "export: invalid option -- '{}'",
                                ch
                            ))
                        }
                    }
                }
            } else if arg.contains('=') {
                assignments.push(arg);
            } else {
                // export NAME (mark for export, value may come later)
                names_to_export.push(arg);
            }
        }

        if list_mode && assignments.is_empty() && names_to_export.is_empty() {
            let mut vars: Vec<(&String, &String)> = self.env_vars.iter().collect();
            vars.sort_by_key(|(k, _)| *k);
            let output = vars
                .iter()
                .map(|(k, v)| format!("declare -x {}=\"{}\"", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            return BuiltinResult::handled(output);
        }

        // Process NAME=VALUE assignments
        for assignment in &assignments {
            if let Some(eq_pos) = assignment.find('=') {
                let name = assignment[..eq_pos].trim().to_string();
                let value = assignment[eq_pos + 1..].trim().to_string();
                if !name.is_empty() {
                    std::env::set_var(&name, &value);
                    self.env_vars.insert(name, value);
                }
            }
        }

        BuiltinResult::handled_no_output()
    }

    // -- unset ---------------------------------------------------------------

    fn handle_unset(&mut self, args: &[&str]) -> BuiltinResult {
        let mut past_options = false;

        for &arg in args {
            if past_options || !arg.starts_with('-') || arg == "-" {
                // Treat as a variable name to unset
                if !arg.starts_with('-') || arg == "-" {
                    self.env_vars.remove(arg);
                    std::env::remove_var(arg);
                }
            } else if arg == "--" {
                past_options = true;
            } else if arg == "-v" {
                // -v: unset variables (default behavior)
            } else if arg == "-f" {
                // -f: unset functions — not supported, ignore
            } else if arg == "-n" {
                // Unset name reference — not supported
                return BuiltinResult::handled("unset: -n: name references not supported");
            } else {
                // Check for combined flags
                for ch in arg[1..].chars() {
                    match ch {
                        'v' | 'f' => {}
                        'n' => {
                            return BuiltinResult::handled(
                                "unset: -n: name references not supported",
                            )
                        }
                        _ => {
                            return BuiltinResult::handled(format!(
                                "unset: invalid option -- '{}'",
                                ch
                            ))
                        }
                    }
                }
            }
        }

        BuiltinResult::handled_no_output()
    }

    // -- pushd ---------------------------------------------------------------

    fn handle_pushd(&mut self, args: &[&str]) -> BuiltinResult {
        if args.is_empty() {
            // No args: swap cwd and top of stack (like bash)
            if self.dir_stack.is_empty() {
                return BuiltinResult::handled("pushd: no other directory");
            }
            let top = self.dir_stack.pop().unwrap();
            self.dir_stack.push(self.cwd.clone());
            let path = std::path::PathBuf::from(&top);
            if let Ok(()) = std::env::set_current_dir(&path) {
                self.prev_cwd = Some(self.cwd.clone());
                self.cwd = top;
            }
            return self.print_dir_stack();
        }

        let arg = args[0];
        let target = if arg.starts_with('~') {
            let home = dirs::home_dir()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Some(rest) = arg.strip_prefix('~') {
                format!("{}{}", home, rest)
            } else {
                arg.to_string()
            }
        } else {
            arg.to_string()
        };

        let target_path = if std::path::Path::new(&target).is_absolute() {
            std::path::PathBuf::from(&target)
        } else {
            std::path::Path::new(&self.cwd).join(&target)
        };

        if target_path.is_dir() {
            self.dir_stack.push(self.cwd.clone());
            let _ = std::env::set_current_dir(&target_path);
            self.prev_cwd = Some(self.cwd.clone());
            self.cwd = target_path.to_string_lossy().to_string();
            self.print_dir_stack()
        } else {
            BuiltinResult::handled(format!("pushd: {}: Not a directory", target))
        }
    }

    // -- popd ----------------------------------------------------------------

    fn handle_popd(&mut self) -> BuiltinResult {
        match self.dir_stack.pop() {
            Some(dir) => {
                let path = std::path::PathBuf::from(&dir);
                if path.is_dir() {
                    if let Ok(()) = std::env::set_current_dir(&path) {
                        self.prev_cwd = Some(self.cwd.clone());
                        self.cwd = dir;
                    }
                    self.print_dir_stack()
                } else {
                    BuiltinResult::handled(format!("popd: {}: Not a directory", dir))
                }
            }
            None => BuiltinResult::handled("popd: directory stack empty"),
        }
    }

    // -- dirs ----------------------------------------------------------------

    fn handle_dirs(&mut self, args: &[&str]) -> BuiltinResult {
        if args.contains(&"-c") {
            self.dir_stack.clear();
            return BuiltinResult::handled_no_output();
        }

        // Verbose mode: show index numbers
        if args.contains(&"-v") || args.contains(&"-l") {
            let home = dirs::home_dir()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut lines = Vec::new();
            lines.push(format!(" 0  {}", shorten_path(&self.cwd, &home)));
            for (i, d) in self.dir_stack.iter().rev().enumerate() {
                lines.push(format!("{:>2}  {}", i + 1, shorten_path(d, &home)));
            }
            return BuiltinResult::handled(lines.join("\n"));
        }

        self.print_dir_stack()
    }

    /// Helper: format and return the directory stack.
    fn print_dir_stack(&self) -> BuiltinResult {
        let home = dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut parts = vec![shorten_path(&self.cwd, &home)];
        for d in self.dir_stack.iter().rev() {
            parts.push(shorten_path(d, &home));
        }
        BuiltinResult::handled(parts.join(" "))
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

    // -- clear ---------------------------------------------------------------

    fn handle_clear(&self) -> BuiltinResult {
        print!("\x1b[2J\x1b[H");
        let _ = std::io::Write::flush(&mut std::io::stdout());
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

/// Replace the home directory prefix with `~` for display.
fn shorten_path(path: &str, home: &str) -> String {
    if !home.is_empty() && path.starts_with(home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_state_modifying() {
        assert!(is_state_modifying("cd"));
        assert!(is_state_modifying("export"));
        assert!(!is_state_modifying("ls"));
    }

    #[test]
    fn test_is_pty_requiring() {
        assert!(is_pty_requiring("sudo"));
        assert!(is_pty_requiring("su"));
        assert!(is_pty_requiring("/usr/bin/sudo"));
        assert!(is_pty_requiring("/bin/su"));
        assert!(!is_pty_requiring("ls"));
    }

    #[test]
    fn test_is_rejected() {
        assert!(is_rejected("exit"));
        assert!(is_rejected("logout"));
        assert!(!is_rejected("ls"));
    }

    #[test]
    fn test_shorten_path() {
        assert_eq!(
            shorten_path("/home/user/projects", "/home/user"),
            "~/projects"
        );
        assert_eq!(shorten_path("/tmp/test", "/home/user"), "/tmp/test");
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
