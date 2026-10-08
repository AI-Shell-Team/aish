//! Run a user line in the shell session and copy shell state back.
//!
//! The shell session is the source of truth. After a line that can change
//! the current directory, the exported environment, or the directory stack,
//! AISH replaces its copy from the session. A failed read leaves the
//! previous copy in place.

use crate::commands::{has_unquoted_shell_syntax, plan_privilege_command};
use crate::types::ShellState;
use aish_pty::PersistentPty;
use base64::Engine;
use std::collections::HashMap;
use std::time::Duration;

/// Read dirs and exported env. The backend pager prefix has already saved the
/// real `PAGER` / `SYSTEMD_PAGER` / `GIT_PAGER` in `__AISH_PV0`..`2` and
/// forced them to `cat` (see `backend_pager_override`). Put the real values
/// back before `compgen -e`. Leave `__AISH_PV*` for the prefix's restore line.
const PROBE: &str = r#"if [ "$__AISH_PV0" = __AISH_UNSET__ ]; then unset PAGER; else export PAGER="$__AISH_PV0"; fi; if [ "$__AISH_PV1" = __AISH_UNSET__ ]; then unset SYSTEMD_PAGER; else export SYSTEMD_PAGER="$__AISH_PV1"; fi; if [ "$__AISH_PV2" = __AISH_UNSET__ ]; then unset GIT_PAGER; else export GIT_PAGER="$__AISH_PV2"; fi; printf '%s\n' '@@AISH_DIRS@@'; dirs -l -p; printf '%s\n' '@@AISH_ENV@@'; while IFS= read -r __aish_n; do printf '%s\t' "$__aish_n"; printf '%s' "${!__aish_n}" | base64 -w0; printf '\n'; done < <(compgen -e); unset -v __aish_n; printf '%s\n' '@@AISH_END@@'"#;

const DIRS_MARK: &str = "@@AISH_DIRS@@\n";
const ENV_MARK: &str = "@@AISH_ENV@@\n";
const END_MARK: &str = "@@AISH_END@@";

/// Exported environment and directory stack read from a shell session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedShellState {
    pub cwd: String,
    /// Oldest directory first. The current directory is not included.
    pub dir_stack: Vec<String>,
    pub env: HashMap<String, String>,
}

/// True when this line can change the exported environment or the directory stack.
///
/// Ordinary commands and `sudo` / `su` are false: only the current directory
/// is refreshed from the session's prompt event.
pub fn needs_full_shell_state(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || plan_privilege_command(trimmed).is_some() {
        return false;
    }
    if has_unquoted_shell_syntax(trimmed) {
        return true;
    }
    let Some(first) = trimmed.split_whitespace().next() else {
        return false;
    };
    if first == "builtin" || first == "command" || is_bare_assignment(first) {
        return true;
    }
    matches!(
        first,
        "cd" | "export"
            | "unset"
            | "pushd"
            | "popd"
            | "dirs"
            | "source"
            | "."
            | "declare"
            | "typeset"
            | "readonly"
            | "eval"
    )
}

/// `exit` / `help` / `setup` with no shell syntax stay in AISH.
pub fn aish_handles_line(line: &str) -> bool {
    let trimmed = line.trim();
    if has_unquoted_shell_syntax(trimmed) {
        return false;
    }
    let Some(first) = trimmed.split_whitespace().next() else {
        return false;
    };
    matches!(first, "exit" | "quit" | "logout" | "help" | "setup")
}

fn is_bare_assignment(token: &str) -> bool {
    let Some(eq) = token.find('=') else {
        return false;
    };
    if eq == 0 {
        return false;
    }
    let name = if token.as_bytes()[eq - 1] == b'+' {
        &token[..eq - 1]
    } else {
        &token[..eq]
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Run `line` in `pty` and, when the line can change shell state, copy it back.
///
/// The exit code is the session's. A failed copy leaves the environment and
/// directory stack as they were; the current directory still follows the
/// session when the command itself reported one.
pub fn run_line_in_session(
    pty: &mut PersistentPty,
    state: &mut ShellState,
    line: &str,
) -> aish_core::Result<i32> {
    let (_output, code, cwd) = pty.execute_command(line, Duration::from_secs(8), None, false)?;
    apply_cwd(state, &cwd);
    if needs_full_shell_state(line) && pty.is_running() {
        if let Some(probed) = probe(pty) {
            adopt(state, probed);
        }
    }
    Ok(code)
}

/// Read shell state from a session that has already run the user's line.
///
/// The read is a second command. `$?` is saved first and restored after, so
/// the session status stays the user's line.
pub fn probe(pty: &mut PersistentPty) -> Option<ProbedShellState> {
    if pty.run_unwrapped_line("__AISH_SAVED_EC=$?").is_err() {
        return None;
    }
    let parsed = (|| {
        let (output, code, cwd) = pty
            .execute_command(PROBE, Duration::from_secs(8), None, false)
            .ok()?;
        if code != 0 {
            return None;
        }
        parse_probe(&output, &cwd)
    })();
    // Expand the saved status before unsetting it. `(exit N)` is the last
    // command, so the session's `$?` becomes N.
    let _ = pty.run_unwrapped_line(
        r#"eval "unset -v __aish_n __AISH_SAVED_EC __AISH_ACTIVE_COMMAND_SEQ __AISH_ACTIVE_COMMAND_TEXT; (exit $__AISH_SAVED_EC)""#,
    );
    parsed
}

pub fn adopt(state: &mut ShellState, probed: ProbedShellState) {
    apply_cwd(state, &probed.cwd);
    state.dir_stack = probed.dir_stack;
    for key in state.env_vars.keys() {
        if !probed.env.contains_key(key) {
            std::env::remove_var(key);
        }
    }
    for (key, value) in &probed.env {
        std::env::set_var(key, value);
    }
    state.env_vars = probed.env;
}

fn apply_cwd(state: &mut ShellState, cwd: &str) {
    if cwd.is_empty() || cwd == state.cwd {
        return;
    }
    state.prev_cwd = Some(state.cwd.clone());
    state.cwd = cwd.to_string();
    let _ = std::env::set_current_dir(cwd);
}

fn parse_probe(output: &str, cwd_from_pty: &str) -> Option<ProbedShellState> {
    let text = output.replace('\r', "");
    let dirs_at = text.find(DIRS_MARK)?;
    let after_dirs = &text[dirs_at + DIRS_MARK.len()..];
    let env_rel = after_dirs.find(ENV_MARK)?;
    let dirs_body = &after_dirs[..env_rel];
    let after_env = &after_dirs[env_rel + ENV_MARK.len()..];
    let end_rel = after_env.find(END_MARK)?;
    let env_body = &after_env[..end_rel];

    let dir_lines: Vec<String> = dirs_body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let cwd = if !cwd_from_pty.is_empty() {
        cwd_from_pty.to_string()
    } else {
        dir_lines.first()?.clone()
    };
    let stacked = if dir_lines.first().is_some_and(|top| top == &cwd) {
        &dir_lines[1..]
    } else if dir_lines.is_empty() {
        &dir_lines[..]
    } else {
        &dir_lines[1..]
    };
    let dir_stack: Vec<String> = stacked.iter().rev().cloned().collect();

    let mut env = HashMap::new();
    for line in env_body.lines() {
        // Do not trim: an empty value is `NAME\t`, and trim would drop the tab.
        if line.is_empty() {
            continue;
        }
        let (name, encoded) = line.split_once('\t')?;
        if name.starts_with("__AISH_") || name.starts_with("__aish_") {
            continue;
        }
        if !is_env_name(name) {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        let value = String::from_utf8(bytes).ok()?;
        env.insert(name.to_string(), value);
    }
    if env.is_empty() {
        return None;
    }
    Some(ProbedShellState {
        cwd,
        dir_stack,
        env,
    })
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn full_state_lines_are_the_agreed_set() {
        assert!(needs_full_shell_state("cd /tmp && true"));
        assert!(needs_full_shell_state("export A=1 && true"));
        assert!(needs_full_shell_state("pushd /tmp && true"));
        assert!(needs_full_shell_state("cd /tmp && false"));
        assert!(needs_full_shell_state("declare A=1"));
        assert!(needs_full_shell_state("FOO=1"));
        assert!(needs_full_shell_state("builtin export A=1"));
        assert!(needs_full_shell_state("command cd /tmp"));
        assert!(!needs_full_shell_state("ls"));
        assert!(!needs_full_shell_state("git status"));
        assert!(!needs_full_shell_state("pwd"));
        assert!(!needs_full_shell_state("sudo id && export A=1"));
        assert!(aish_handles_line("exit"));
        assert!(!aish_handles_line("cd /tmp; exit"));
    }

    #[test]
    fn directory_stack_is_oldest_first() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("OK");
        let output = format!(
            "noise\n@@AISH_DIRS@@\n/b\n/a\n/orig\n@@AISH_ENV@@\nFOO\t{encoded}\n@@AISH_END@@\n"
        );
        let probed = parse_probe(&output, "/b").expect("probe");
        assert_eq!(probed.cwd, "/b");
        assert_eq!(
            probed.dir_stack,
            vec!["/orig".to_string(), "/a".to_string()]
        );
        assert_eq!(probed.env.get("FOO").map(String::as_str), Some("OK"));
    }

    #[test]
    fn garbage_probe_is_rejected() {
        assert!(parse_probe("no markers", "/tmp").is_none());
    }

    #[test]
    fn empty_env_value_is_kept() {
        let output = "@@AISH_DIRS@@\n/tmp\n@@AISH_ENV@@\nEMPTY\t\nFOO\tT0s=\n@@AISH_END@@\n";
        let probed = parse_probe(output, "/tmp").expect("probe");
        assert_eq!(probed.env.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(probed.env.get("FOO").map(String::as_str), Some("OK"));
    }

    #[test]
    fn internal_probe_names_are_ignored() {
        let output =
            "@@AISH_DIRS@@\n/tmp\n@@AISH_ENV@@\n__AISH_PV0\tY2F0\nFOO\tT0s=\n@@AISH_END@@\n";
        let probed = parse_probe(output, "/tmp").expect("probe");
        assert!(!probed.env.contains_key("__AISH_PV0"));
        assert_eq!(probed.env.get("FOO").map(String::as_str), Some("OK"));
    }

    #[test]
    fn session_copy_matches_after_state_lines() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_cwd = std::env::current_dir().ok();
        let saved_env: HashMap<String, String> = std::env::vars().collect();
        let _restore = EnvRestore {
            cwd: saved_cwd.clone(),
            env: saved_env.clone(),
        };

        let root = std::env::temp_dir().join(format!("aish-shell-state-{}", std::process::id()));
        let dir_a = root.join("a");
        let dir_b = root.join("b");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();
        let root = root.canonicalize().unwrap();
        let dir_a = dir_a.canonicalize().unwrap();
        let dir_b = dir_b.canonicalize().unwrap();

        let mut pty = start_pty(root.to_str().unwrap());
        let mut state = ShellState::new();
        state.cwd = root.to_string_lossy().to_string();

        let code = run_line_in_session(
            &mut pty,
            &mut state,
            &format!("cd {} && true", shell_quote(&dir_a.to_string_lossy())),
        )
        .expect("cd");
        assert_eq!(code, 0);
        assert_eq!(state.cwd, dir_a.to_string_lossy());

        let key = "AISH_SHELL_STATE_COPY";
        let code = run_line_in_session(&mut pty, &mut state, &format!("export {key}=OK && true"))
            .expect("export");
        assert_eq!(code, 0);
        assert_eq!(state.env_vars.get(key).map(String::as_str), Some("OK"));
        assert_eq!(std::env::var(key).ok().as_deref(), Some("OK"));
        assert_eq!(state.cwd, dir_a.to_string_lossy());

        let code = run_line_in_session(
            &mut pty,
            &mut state,
            &format!("pushd {} && true", shell_quote(&dir_b.to_string_lossy())),
        )
        .expect("pushd");
        assert_eq!(code, 0);
        assert_eq!(state.cwd, dir_b.to_string_lossy());
        assert!(
            state
                .dir_stack
                .iter()
                .any(|d| d.as_str() == dir_a.to_string_lossy()),
            "stack {:?} missing {}",
            state.dir_stack,
            dir_a.display()
        );

        let code = run_line_in_session(
            &mut pty,
            &mut state,
            &format!("cd {} && false", shell_quote(&dir_a.to_string_lossy())),
        )
        .expect("cd && false");
        assert_ne!(code, 0);
        assert_eq!(state.cwd, dir_a.to_string_lossy());

        let code = run_line_in_session(&mut pty, &mut state, &format!("unset {key} && true"))
            .expect("unset");
        assert_eq!(code, 0);
        assert!(!state.env_vars.contains_key(key));
        assert!(std::env::var(key).is_err());

        let code = run_line_in_session(&mut pty, &mut state, "popd && true").expect("popd");
        assert_eq!(code, 0);
        assert!(
            state.dir_stack.is_empty(),
            "stack after popd: {:?}",
            state.dir_stack
        );

        pty.stop();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn probe_keeps_an_exported_pager() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut pty = start_pty("/tmp");
        pty.run_unwrapped_line("export PAGER=aish-pager-sentinel")
            .expect("export");
        let probed = probe(&mut pty).expect("probe");
        assert_eq!(
            probed.env.get("PAGER").map(String::as_str),
            Some("aish-pager-sentinel")
        );
        pty.stop();
    }

    #[test]
    fn failed_line_keeps_session_status() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut pty = start_pty("/tmp");
        pty.run_unwrapped_line("false").expect("false");
        assert!(probe(&mut pty).is_some());
        let code = pty
            .run_unwrapped_line("test $? -ne 0")
            .expect("status check");
        assert_eq!(code, 0, "session $? was cleared by the readback");
        pty.stop();
    }

    struct EnvRestore {
        cwd: Option<std::path::PathBuf>,
        env: HashMap<String, String>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(cwd) = self.cwd.take() {
                let _ = std::env::set_current_dir(cwd);
            }
            restore_env(&self.env);
        }
    }

    fn start_pty(cwd: &str) -> PersistentPty {
        let mut last = None;
        for _ in 0..5 {
            match PersistentPty::start(cwd, 24, 80) {
                Ok(pty) => return pty,
                Err(err) => {
                    last = Some(err);
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
        }
        panic!("PersistentPty::start failed: {last:?}");
    }

    fn shell_quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    fn restore_env(saved: &HashMap<String, String>) {
        let current: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
        for key in current {
            if !saved.contains_key(&key) {
                std::env::remove_var(&key);
            }
        }
        for (key, value) in saved {
            std::env::set_var(key, value);
        }
    }
}
