//! Run a user line in the shell session and copy shell state back.
//!
//! The shell session is the source of truth. After a line that can change
//! the current directory, the exported environment, or the directory stack,
//! AISH replaces its copy from the session. A failed read leaves the
//! previous copy in place.

use crate::commands::{has_unquoted_shell_syntax, plan_privilege_command};
use crate::types::ShellState;
use aish_pty::PersistentPty;
use std::collections::HashMap;
use std::time::Duration;

/// Read dirs and exported env. The backend pager prefix has already saved the
/// real `PAGER` / `SYSTEMD_PAGER` / `GIT_PAGER` in `__AISH_PV0`..`2` and
/// forced them to `cat` (see `backend_pager_override`). Put the real values
/// back before `compgen -e`. Leave `__AISH_PV*` for the prefix's restore line.
/// Values are quoted with bash `printf %q`, which is a builtin.
const PROBE: &str = r#"if [ "$__AISH_PV0" = __AISH_UNSET__ ]; then unset PAGER; else export PAGER="$__AISH_PV0"; fi; if [ "$__AISH_PV1" = __AISH_UNSET__ ]; then unset SYSTEMD_PAGER; else export SYSTEMD_PAGER="$__AISH_PV1"; fi; if [ "$__AISH_PV2" = __AISH_UNSET__ ]; then unset GIT_PAGER; else export GIT_PAGER="$__AISH_PV2"; fi; printf '%s\n' '@@AISH_DIRS@@'; dirs -l -p; printf '%s\n' '@@AISH_ENV@@'; while IFS= read -r __aish_n; do printf '%s\t' "$__aish_n"; printf '%q' "${!__aish_n}"; printf '\n'; done < <(compgen -e); unset -v __aish_n; printf '%s\n' '@@AISH_END@@'"#;

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
    let Some(word) = state_word(trimmed) else {
        return false;
    };
    if is_bare_assignment(word) {
        return true;
    }
    match word {
        "export" | "unset" | "pushd" | "popd" | "source" | "." | "declare" | "typeset"
        | "readonly" | "eval" => true,
        // `dirs -c` clears the stack. Other `dirs` forms only print it.
        "dirs" => dirs_clears_stack(trimmed),
        _ => false,
    }
}

/// The command `line` would run, after a leading `builtin` or `command`.
fn state_word(line: &str) -> Option<&str> {
    let mut words = line.split_whitespace();
    let first = words.next()?;
    match first {
        "builtin" => words.next(),
        "command" => {
            while let Some(word) = words.next() {
                if word == "--" {
                    return words.next();
                }
                if word.starts_with('-') && word != "-" {
                    continue;
                }
                return Some(word);
            }
            None
        }
        other => Some(other),
    }
}

fn dirs_clears_stack(line: &str) -> bool {
    line.split_whitespace().skip(1).any(|arg| {
        arg == "-c" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c'))
    })
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
    if let Some(baseline) = state.env_baseline.take() {
        // First read. The session is the source of truth for values it
        // changed. Variables it still has at their startup value are left
        // alone, so a later process-only update is not written back.
        for (key, value) in &probed.env {
            match baseline.get(key) {
                Some(original) if original == value => {}
                _ => std::env::set_var(key, value),
            }
        }
    } else {
        for key in state.env_vars.keys() {
            if !probed.env.contains_key(key) {
                std::env::remove_var(key);
            }
        }
        for (key, value) in &probed.env {
            if state.env_vars.get(key) != Some(value) {
                std::env::set_var(key, value);
            }
        }
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
        let (name, quoted) = line.split_once('\t')?;
        if name.starts_with("__AISH_") || name.starts_with("__aish_") {
            continue;
        }
        if !is_env_name(name) {
            return None;
        }
        let value = unquote_bash_q(quoted)?;
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

/// Undo bash `printf %q`. A value that is not in that form rejects the probe.
fn unquote_bash_q(raw: &str) -> Option<String> {
    if raw == "''" || raw == "$''" {
        return Some(String::new());
    }
    if let Some(body) = raw.strip_prefix("$'") {
        let body = body.strip_suffix('\'')?;
        return unescape_ansi_c(body);
    }
    unescape_backslash(raw)
}

fn unescape_backslash(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push(chars.next()?);
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn unescape_ansi_c(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            return None;
        }
        let escaped = bytes[i];
        match escaped {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'e' | b'E' => out.push(0x1b),
            b'f' => out.push(0x0c),
            b'v' => out.push(0x0b),
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'x' => {
                i += 1;
                let start = i;
                while i < bytes.len() && i < start + 2 && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[start..i]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                continue;
            }
            b'0'..=b'7' => {
                let start = i;
                while i < bytes.len() && i < start + 3 && (b'0'..=b'7').contains(&bytes[i]) {
                    i += 1;
                }
                let oct = std::str::from_utf8(&bytes[start..i]).ok()?;
                let value = u32::from_str_radix(oct, 8).ok()?;
                if value > 255 {
                    return None;
                }
                out.push(value as u8);
                continue;
            }
            b'u' | b'U' => {
                let width = if escaped == b'u' { 4 } else { 8 };
                i += 1;
                let start = i;
                while i < bytes.len() && i < start + width && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if i - start != width {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[start..i]).ok()?;
                let code = u32::from_str_radix(hex, 16).ok()?;
                let ch = char::from_u32(code)?;
                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                continue;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
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
        assert!(needs_full_shell_state("command -p export A=1"));
        assert!(needs_full_shell_state("dirs -c"));
        assert!(!needs_full_shell_state("cd /tmp"));
        assert!(!needs_full_shell_state("command cd /tmp"));
        assert!(!needs_full_shell_state("builtin cd /tmp"));
        assert!(!needs_full_shell_state("dirs"));
        assert!(!needs_full_shell_state("dirs -l"));
        assert!(!needs_full_shell_state("ls"));
        assert!(!needs_full_shell_state("git status"));
        assert!(!needs_full_shell_state("pwd"));
        assert!(!needs_full_shell_state("sudo id && export A=1"));
        assert!(aish_handles_line("exit"));
        assert!(!aish_handles_line("cd /tmp; exit"));
    }

    #[test]
    fn directory_stack_is_oldest_first() {
        let output = "noise\n@@AISH_DIRS@@\n/b\n/a\n/orig\n@@AISH_ENV@@\nFOO\tOK\n@@AISH_END@@\n";
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
        let output = "@@AISH_DIRS@@\n/tmp\n@@AISH_ENV@@\nEMPTY\t''\nFOO\tOK\n@@AISH_END@@\n";
        let probed = parse_probe(output, "/tmp").expect("probe");
        assert_eq!(probed.env.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(probed.env.get("FOO").map(String::as_str), Some("OK"));
    }

    #[test]
    fn internal_probe_names_are_ignored() {
        let output = "@@AISH_DIRS@@\n/tmp\n@@AISH_ENV@@\n__AISH_PV0\tcat\nFOO\tOK\n@@AISH_END@@\n";
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

        let code = run_line(
            &mut pty,
            &mut state,
            &format!("cd {} && true", shell_quote(&dir_a.to_string_lossy())),
        )
        .expect("cd");
        assert_eq!(code, 0);
        assert_eq!(state.cwd, dir_a.to_string_lossy());

        let key = "AISH_SHELL_STATE_COPY";
        let code = run_line(
            &mut pty,
            &mut state,
            &format!("export {key}=$'{value}' && true", value = "a &&\\nb"),
        )
        .expect("export");
        assert_eq!(code, 0);
        assert_eq!(state.env_vars.get(key).map(String::as_str), Some("a &&\nb"));
        assert_eq!(std::env::var(key).ok().as_deref(), Some("a &&\nb"));
        assert_eq!(state.cwd, dir_a.to_string_lossy());

        let code = run_line(
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

        let code = run_line(
            &mut pty,
            &mut state,
            &format!("cd {} && false", shell_quote(&dir_a.to_string_lossy())),
        )
        .expect("cd && false");
        assert_ne!(code, 0);
        assert_eq!(state.cwd, dir_a.to_string_lossy());

        let code = run_line(&mut pty, &mut state, &format!("unset {key} && true")).expect("unset");
        assert_eq!(code, 0);
        assert!(!state.env_vars.contains_key(key));
        assert!(std::env::var(key).is_err());

        let code = run_line(&mut pty, &mut state, "popd && true").expect("popd");
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

    fn run_line(
        pty: &mut PersistentPty,
        state: &mut ShellState,
        line: &str,
    ) -> aish_core::Result<i32> {
        let (_output, code, cwd) =
            pty.execute_command(line, Duration::from_secs(8), None, false)?;
        if !cwd.is_empty() && cwd != state.cwd {
            state.prev_cwd = Some(state.cwd.clone());
            state.cwd = cwd;
            let _ = std::env::set_current_dir(&state.cwd);
        }
        if needs_full_shell_state(line) && pty.is_running() {
            if let Some(probed) = probe(pty) {
                adopt(state, probed);
            }
        }
        Ok(code)
    }

    #[test]
    fn bash_q_quoting_round_trips() {
        assert_eq!(unquote_bash_q("''").as_deref(), Some(""));
        assert_eq!(unquote_bash_q("OK").as_deref(), Some("OK"));
        assert_eq!(unquote_bash_q(r"a\ \&\&\ b").as_deref(), Some("a && b"));
        assert_eq!(unquote_bash_q(r"a\ b").as_deref(), Some("a b"));
        assert_eq!(
            unquote_bash_q(r"$'line1\nline2'").as_deref(),
            Some("line1\nline2")
        );
        assert_eq!(
            unquote_bash_q(r#"quote\"here"#).as_deref(),
            Some("quote\"here")
        );
        assert_eq!(unquote_bash_q(r"$'tab\tx'").as_deref(), Some("tab\tx"));
        assert_eq!(unquote_bash_q(r"\$HOME").as_deref(), Some("$HOME"));
        assert_eq!(unquote_bash_q(r"it\'s").as_deref(), Some("it's"));
        assert_eq!(unquote_bash_q(r"$'a\001b'").as_deref(), Some("a\u{1}b"));
        assert!(unquote_bash_q(r"trailing\").is_none());
    }

    #[test]
    fn adopt_leaves_process_vars_the_session_did_not_change() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_env: HashMap<String, String> = std::env::vars().collect();
        let _restore = EnvRestore {
            cwd: None,
            env: saved_env,
        };
        let mut state = ShellState::new();
        let path = std::env::var("PATH").expect("PATH");
        std::env::set_var("PATH", format!("{path}-rust"));
        std::env::set_var("AISH_RUST_ONLY", "kept");

        let cwd = state.cwd.clone();
        let mut probed = HashMap::new();
        probed.insert("PATH".to_string(), path.clone());
        probed.insert("AISH_FROM_SESSION".to_string(), "1".to_string());
        adopt(
            &mut state,
            ProbedShellState {
                cwd: cwd.clone(),
                dir_stack: Vec::new(),
                env: probed,
            },
        );
        assert_eq!(std::env::var("PATH").unwrap(), format!("{path}-rust"));
        assert_eq!(std::env::var("AISH_RUST_ONLY").unwrap(), "kept");
        assert_eq!(std::env::var("AISH_FROM_SESSION").unwrap(), "1");

        let mut next = state.env_vars.clone();
        next.remove("AISH_FROM_SESSION");
        adopt(
            &mut state,
            ProbedShellState {
                cwd,
                dir_stack: Vec::new(),
                env: next,
            },
        );
        assert!(std::env::var("AISH_FROM_SESSION").is_err());
        assert_eq!(std::env::var("AISH_RUST_ONLY").unwrap(), "kept");
        assert_eq!(std::env::var("PATH").unwrap(), format!("{path}-rust"));
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
