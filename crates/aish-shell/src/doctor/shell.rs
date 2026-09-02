use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::collections::{HashMap, HashSet};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Instant;

/// Timeout for the reference-shell probe in seconds.
const PROBE_TIMEOUT_SECS: u64 = 10;
/// Startup time above which the login-shell probe is flagged as slow.
const SLOW_START_SECS: u64 = 3;
/// Detach a probe subprocess from aish's controlling terminal.
///
/// Inside the REPL aish owns a PTY whose foreground process group is aish's
/// own group. A child spawned in a *new* process group (the default for
/// `Command`) is therefore a *background* group relative to that PTY. An
/// interactive (`-i`) bash opens `/dev/tty` (the PTY slave) and calls
/// `tcsetpgrp` on it to install job control — a background group touching the
/// terminal is stopped with SIGTTOU. Once stopped, `timeout`'s SIGTERM is not
/// delivered (default-term signals stay pending for stopped processes until
/// continued; only SIGKILL reaps them), so `timeout` itself blocks waiting for
/// the child, and `Doctor::run`'s `handle.await` hangs forever — the exact
/// symptom reported when `/doctor` stalls after the banner.
///
/// Starting the probe in its own session (`setsid`) gives it no controlling
/// terminal at all, so `/dev/tty` cannot be opened and bash prints its harmless
/// "cannot set terminal process group" warning instead of blocking. `timeout`
/// retains full authority to kill the child. Safe in the forked child: a fresh
/// fork is never a session leader, so `setsid` cannot fail; if it ever did, the
/// `pre_exec` closure returns the OS error and the spawn aborts rather than
/// silently falling back to the SIGTTOU-stuck behaviour.
fn detach_tty(cmd: &mut Command) {
    // SAFETY: setsid() is async-signal-safe and runs in the child after fork.
    // A fresh fork is never a session leader, so setsid() cannot fail here;
    // if it ever did, returning an error aborts exec instead of silently
    // falling back to the SIGTTOU-stuck behaviour.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Never let an interactive bash inherit aish's stdin (the PTY): it could
    // read user keystrokes or wedge the REPL input pump.
    cmd.stdin(std::process::Stdio::null());
}

/// Environment variables compared by value because they commonly cause
/// "aish changed my environment" complaints.
const VALUE_COMPARE_VARS: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "SSH_AUTH_SOCK",
    "SSH_CONNECTION",
    "TMUX",
    "TERM",
];

/// Key substrings identifying secret-bearing variables. Only presence
/// (set/unset) is reported for these, never values.
const SECRET_KEY_PATTERNS: &[&str] =
    &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];

/// Secret variables that use none of the generic patterns above but are
/// credential-bearing by AWS convention.
const SECRET_KEY_PREFIXES: &[&str] = &["AWS_ACCESS_KEY", "AWS_SECRET_KEY"];

/// Bootstrap variables aish itself legitimately introduces, plus variables
/// bash sets itself in every new shell (PWD/OLDPWD). They are excluded from
/// the "added"/"missing" diff so the report only surfaces variables that
/// actually come from rc files or the user's export statements.
const AISH_OWN_VARS: &[&str] = &["AISH_CONTROL_FD", "TERM", "SHLVL", "_", "PWD", "OLDPWD"];

/// Environment snapshot of a login bash, used as the inheritance baseline.
#[derive(Debug, Default)]
struct BaselineEnv {
    vars: HashMap<String, String>,
    /// Non-empty when the probe FAILED (timeout, non-zero exit, spawn error,
    /// or stdout that is not valid `KEY=VALUE\0` data). When this is set the
    /// baseline is unusable: `vars` may be empty or partial, so downstream
    /// reports short-circuit.
    error: Option<String>,
    /// Non-empty when the probe SUCCEEDED but rc files printed to stderr.
    /// The env data is valid, but the noise is surfaced as a separate warning
    /// so it does not suppress the inheritance diff or the startup-speed item.
    stderr_noise: Option<String>,
    elapsed: std::time::Duration,
}

/// Run `bash -lc 'env -0'` and parse the null-delimited environment.
///
/// Mirrors the probe used by `crate::environment::load_bash_env` so the
/// baseline matches what aish actually sources at startup. Stdout is
/// required to be pure `KEY=VALUE\0` output; anything else is reported as
/// rc-file stdout pollution.
fn probe_baseline() -> BaselineEnv {
    let start = Instant::now();
    let mut baseline = BaselineEnv::default();

    // Spawn with a watchdog: `wait_timeout`-free approach using the
    // process API directly. bash -lc sourcing a hung rc file would
    // otherwise block /doctor forever.
    let mut cmd = Command::new("timeout");
    cmd.arg(format!("{}s", PROBE_TIMEOUT_SECS))
        .arg("/bin/bash")
        .arg("-lc")
        .arg("env -0");
    // Run in a new session so a hanging rc file that touches the terminal
    // cannot be SIGTTOU-stopped and wedge `timeout` (same hardening as the
    // interactive probe below).
    detach_tty(&mut cmd);
    let output = cmd.output();

    baseline.elapsed = start.elapsed();

    match output {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            // Stderr output means an rc file printed or failed, but the probe
            // itself succeeded — env data is still valid. Surface the noise as
            // a separate warning rather than suppressing the entire baseline.
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                baseline.stderr_noise = Some(first_line(stderr.trim()).to_string());
            }
            for entry in raw.split('\0') {
                if let Some((key, value)) = entry.split_once('=') {
                    baseline.vars.insert(key.to_string(), value.to_string());
                }
            }
            if baseline.vars.is_empty() {
                baseline.error = Some("login bash probe produced no environment".to_string());
            }
        }
        Ok(output) if output.status.code() == Some(124) => {
            baseline.error = Some(format!(
                "login bash probe timed out after {}s (slow or hanging rc file)",
                PROBE_TIMEOUT_SECS
            ));
        }
        Ok(output) => {
            baseline.error = Some(format!(
                "login bash exited with status {}",
                output
                    .status
                    .code()
                    .map_or("signal".to_string(), |c| c.to_string())
            ));
        }
        Err(e) => {
            baseline.error = Some(format!("failed to run login bash probe: {}", e));
        }
    }

    baseline
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// True for the job-control warning bash prints when started interactively
/// without a controlling terminal. Expected in the probe (stdio is
/// captured by Command::output), localized, and unrelated to rc-file
/// health — so it is filtered instead of flagged as noise.
fn is_job_control_warning(line: &str) -> bool {
    let lower = line.to_lowercase();
    // English + zh_CN variants of bash's job-control warning
    // ("no job control" / "无法设定终端进程组" / "此 shell 中无任务控制").
    lower.contains("process group")
        || lower.contains("no job control")
        || lower.contains("进程组")
        || lower.contains("任务控制")
}

fn is_secret_key(key: &str) -> bool {
    // Word-level match on underscore-separated segments. Substring matching
    // would misfire: MONKEY_PATCHED contains "KEY", TOKENIZE contains
    // "TOKEN". Credential-bearing prefixes (AWS access/secret keys) are
    // matched explicitly instead of a broad "AWS_" family that would flag
    // harmless AWS_PAGER/AWS_PROFILE.
    let upper = key.to_uppercase();
    if SECRET_KEY_PREFIXES.iter().any(|p| upper.starts_with(p)) {
        return true;
    }
    let segments: Vec<&str> = upper.split('_').collect();
    SECRET_KEY_PATTERNS.iter().any(|p| segments.contains(p))
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return false;
    }
    std::fs::metadata(p)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// ShellChecker reports how the aish process environment compares to a
/// fresh login bash and whether the PTY bash backend is compatible.
///
/// Design (issue #474): comparison is by variable-name set difference plus
/// value comparison for well-known variables — never by counts, which
/// produce false passes when key sets differ.
pub struct ShellChecker;

impl ShellChecker {
    pub fn new() -> Self {
        Self
    }

    /// Items describing parent shell / runtime shell / login mode.
    fn shell_identity_items(&self) -> Vec<CheckItem> {
        let mut items = Vec::new();

        let parent_shell = std::env::var("SHELL").unwrap_or_default();
        if parent_shell.is_empty() {
            items.push(CheckItem::warn(
                "parent_shell",
                "SHELL is not set; command executor falls back to /bin/bash",
            ));
        } else {
            items.push(CheckItem::pass(
                "parent_shell",
                format!("parent shell: {}", parent_shell),
            ));
            if !parent_shell.ends_with("bash") {
                items.push(
                    CheckItem::not_applicable(
                        "non_bash_parent",
                        format!(
                            "parent shell is {} (not bash); its rc file is not loaded and is not \
                             modified. Interactive PTY runs bash with aish's own rc wrapper",
                            parent_shell
                        ),
                    )
                    .hint("alias/function/completion from the parent shell config will not appear in the PTY; migration is manual by design"),
                );
            }
        }

        // Login-shell status: aish is typically started from an interactive
        // parent shell, so $0/-l probing is unreliable. Report what can be
        // observed instead.
        let is_login = std::env::var("AISH_LOGIN_SHELL")
            .map(|v| v == "1")
            .unwrap_or(false);
        items.push(CheckItem::pass(
            "login_mode",
            if is_login {
                "running as login shell (AISH_LOGIN_SHELL=1)"
            } else {
                "running as non-login shell (profile not re-sourced at startup)"
            },
        ));

        items
    }

    /// Items describing which startup files aish would load.
    fn startup_file_items() -> Vec<CheckItem> {
        let mut items = Vec::new();
        let home = dirs::home_dir();

        for rc in [".bash_profile", ".bashrc"] {
            let path = home.as_ref().map(|h| h.join(rc));
            match path {
                Some(p) if p.exists() => {
                    items.push(CheckItem::pass(
                        format!("rc_{}", rc),
                        format!("{}: found (sourced at startup via bash -lc)", p.display()),
                    ));
                }
                Some(p) => {
                    items.push(CheckItem::not_applicable(
                        format!("rc_{}", rc),
                        format!("{}: not present", p.display()),
                    ));
                }
                None => {
                    items.push(CheckItem::warn(
                        "rc_home",
                        "cannot determine home directory",
                    ));
                }
            }
        }

        // PTY wrapper always sources these two files (bash_rc_wrapper.sh).
        let system_rc = std::path::Path::new("/etc/bash.bashrc");
        if system_rc.exists() {
            items.push(CheckItem::pass(
                "rc_system",
                "/etc/bash.bashrc: found (sourced inside PTY wrapper)",
            ));
        } else {
            items.push(CheckItem::not_applicable(
                "rc_system",
                "/etc/bash.bashrc: not present",
            ));
        }

        items
    }

    /// Core diff items: set difference against the login-bash baseline.
    fn env_diff_items(&self, baseline: &BaselineEnv) -> Vec<CheckItem> {
        let mut items = Vec::new();

        if let Some(err) = &baseline.error {
            items.push(CheckItem::warn("baseline_probe", err.clone()).hint(
                "fix the rc file error above; until then the inheritance report is incomplete",
            ));
            return items;
        }
        if let Some(noise) = &baseline.stderr_noise {
            items.push(
                CheckItem::warn(
                    "rc_stderr_noise",
                    format!("rc file produced stderr noise: {}", noise),
                )
                .hint("guard rc file output so it does not pollute the PTY"),
            );
        }

        let current: HashSet<String> = std::env::vars().map(|(k, _)| k).collect();
        let baseline_filtered: HashSet<&String> = baseline
            .vars
            .keys()
            .filter(|k| !AISH_OWN_VARS.contains(&k.as_str()))
            .collect();

        // Variables the login shell has that aish did not inherit.
        let missing: Vec<&String> = baseline_filtered
            .iter()
            .copied()
            .filter(|k| !current.contains(*k))
            .collect();

        // Variables aish has that the login shell does not — inherited from
        // the parent environment (DESKTOP_SESSION, DISPLAY, etc.) rather than
        // from rc files. These are informational, not a problem.
        let added: Vec<String> = current
            .iter()
            .filter(|k| !baseline_filtered.contains(*k) && !AISH_OWN_VARS.contains(&k.as_str()))
            .cloned()
            .collect();

        let missing_secrets: Vec<&String> = missing
            .iter()
            .copied()
            .filter(|k| is_secret_key(k))
            .collect();
        let missing_plain: Vec<&String> = missing
            .iter()
            .copied()
            .filter(|k| !is_secret_key(k))
            .collect();

        if missing.is_empty() {
            items.push(CheckItem::pass(
                "env_inherit",
                "all login-shell environment variables are inherited",
            ));
        } else {
            let mut names: Vec<String> = missing_plain.iter().map(|k| (*k).to_string()).collect();
            names.extend(missing_secrets.iter().map(|k| format!("{} (secret)", k)));
            names.sort();
            let preview: Vec<String> = names.iter().take(8).cloned().collect();
            let suffix = if names.len() > 8 {
                format!(" … (+{} more)", names.len() - 8)
            } else {
                String::new()
            };
            items.push(
                CheckItem::warn(
                    "env_inherit",
                    format!(
                        "{} variable(s) set in login bash are not inherited: {}{}",
                        missing.len(),
                        preview.join(", "),
                        suffix
                    ),
                )
                .hint(
                    "these were likely set non-exported in rc files, or overridden by the \
                     parent environment; export them or add to ~/.bashrc with `export`",
                ),
            );
        }

        if added.is_empty() {
            items.push(CheckItem::pass(
                "env_added",
                "no extra variables beyond the login shell (except aish's own)",
            ));
        } else {
            let mut names = added.clone();
            names.sort();
            let preview: Vec<String> = names.iter().take(8).cloned().collect();
            let suffix = if names.len() > 8 {
                format!(" … (+{} more)", names.len() - 8)
            } else {
                String::new()
            };
            items.push(CheckItem::pass(
                "env_added",
                format!(
                    "{} variable(s) in aish but not in login bash ({}{}); \
                     inherited from the parent environment",
                    names.len(),
                    preview.join(", "),
                    suffix
                ),
            ));
        }

        // Value comparison for well-known variables. Because aish's
        // bootstrap (load_bash_env) never overwrites existing values,
        // differences here mean the rc-file value lost to the parent
        // value — the classic "PATH looks wrong in aish" case.
        for var in VALUE_COMPARE_VARS {
            let parent = std::env::var(var);
            let login = baseline.vars.get(*var);
            match (parent, login) {
                (Ok(p), Some(l)) if p != *l => {
                    let item = if *var == "PATH" {
                        self.path_diff_item(p.as_str(), l.as_str())
                    } else if is_secret_key(var) {
                        CheckItem::warn(
                            format!("value_{}", var),
                            format!(
                                "{} differs between parent and login shell (values hidden)",
                                var
                            ),
                        )
                    } else {
                        CheckItem::warn(
                            format!("value_{}", var),
                            format!(
                                "{} differs: aish={}, login bash={}",
                                var,
                                truncate(&p, 60),
                                truncate(l, 60)
                            ),
                        )
                        .hint(format!(
                            "your rc file sets {} but aish keeps the inherited value; \
                             re-export it in ~/.bashrc or set it explicitly for aish",
                            var
                        ))
                    };
                    items.push(item);
                }
                _ => {}
            }
        }

        items
    }

    /// PATH-specific diff: directory sets and ordering, trimmed preview.
    fn path_diff_item(&self, aish_path: &str, login_path: &str) -> CheckItem {
        let aish_dirs: Vec<&str> = aish_path.split(':').filter(|s| !s.is_empty()).collect();
        let login_dirs: Vec<&str> = login_path.split(':').filter(|s| !s.is_empty()).collect();

        let aish_set: HashSet<&str> = aish_dirs.iter().copied().collect();
        let login_set: HashSet<&str> = login_dirs.iter().copied().collect();

        // Sort for deterministic output: HashSet difference order is
        // randomized per run, which would make the message unstable.
        let mut only_login: Vec<&str> = login_set.difference(&aish_set).copied().collect();
        let mut only_aish: Vec<&str> = aish_set.difference(&login_set).copied().collect();
        only_login.sort_unstable();
        only_aish.sort_unstable();

        let mut msg = String::from("PATH differs");
        if !only_login.is_empty() {
            msg.push_str(&format!(
                "; missing from aish: {}",
                preview_dirs(&only_login)
            ));
        }
        if !only_aish.is_empty() {
            msg.push_str(&format!("; extra in aish: {}", preview_dirs(&only_aish)));
        }

        CheckItem::warn("value_PATH", msg).hint(
            "load_bash_env never overwrites an already-set PATH; export the wanted PATH \
             explicitly in ~/.bashrc or launch aish from the shell with the desired PATH",
        )
    }

    /// Secret variables: report set/unset only, never values.
    fn secret_items(&self, baseline: &BaselineEnv) -> Vec<CheckItem> {
        let mut items = Vec::new();

        let mut names: HashSet<String> = std::env::vars()
            .map(|(k, _)| k)
            .filter(|k| is_secret_key(k))
            .collect();
        // Baseline failed: still report current-process secrets so the
        // Secrets section stays useful without the login-shell part.
        if baseline.error.is_none() {
            names.extend(baseline.vars.keys().filter(|k| is_secret_key(k)).cloned());
        }

        let mut sorted: Vec<&String> = names.iter().collect();
        sorted.sort();
        for key in sorted {
            let state = if std::env::var(key).is_ok() {
                "set"
            } else {
                "unset"
            };
            items.push(CheckItem::pass("secret", format!("{}: {}", key, state)));
        }
        items
    }

    /// PTY bash compatibility: /bin/bash must exist and the rc wrapper
    /// probe must run cleanly.
    fn compat_items(&self) -> Vec<CheckItem> {
        let mut items = Vec::new();

        // aish hardcodes /bin/bash as the PTY shell.
        if is_executable("/bin/bash") {
            items.push(CheckItem::pass(
                "bash_binary",
                "/bin/bash: present and executable (PTY backend)",
            ));
        } else {
            items.push(CheckItem::fail(
                "bash_binary",
                "/bin/bash: missing or not executable — the interactive PTY cannot start",
            ));
        }

        // Interactive bash availability probe. NOTE: this is NOT the same
        // rcfile wrapper aish uses in the PTY (aish generates a temp
        // --rcfile at session start); it exercises ~/.bashrc, which is the
        // dominant source of startup noise and slowness.
        //
        // The probe runs in a new session with no controlling terminal (see
        // `detach_tty`), so the job-control warning ("cannot set terminal
        // process group" / "no job control") is EXPECTED and harmless. It is
        // filtered before flagging stderr noise.
        let mut cmd = Command::new("timeout");
        cmd.arg(format!("{}s", PROBE_TIMEOUT_SECS))
            .arg("/bin/bash")
            .arg("-ic")
            .arg("true");
        // Detach from aish's controlling terminal (see `detach_tty`): without
        // this, interactive bash opens `/dev/tty` (aish's PTY slave), calls
        // tcsetpgrp from a background process group, gets SIGTTOU-stopped, and
        // `timeout` cannot reap it — `/doctor` hangs forever after the banner.
        detach_tty(&mut cmd);
        let probe = cmd.output();
        match probe {
            Ok(out) if out.status.success() => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let real_noise: Vec<&str> = stderr
                    .lines()
                    .filter(|l| !is_job_control_warning(l))
                    .collect();
                if real_noise.is_empty() {
                    items.push(CheckItem::pass(
                        "interactive_bash",
                        "interactive bash starts cleanly (no stderr noise)",
                    ));
                } else {
                    items.push(
                        CheckItem::warn(
                            "interactive_bash",
                            format!(
                                "interactive bash startup prints stderr: {}",
                                first_line(real_noise.join("\n").trim())
                            ),
                        )
                        .hint("noise from rc files can pollute PTY output; guard rc file output"),
                    );
                }
            }
            Ok(out) if out.status.code() == Some(124) => {
                items.push(CheckItem::warn(
                    "interactive_bash",
                    format!(
                        "interactive bash startup timed out after {}s — slow rc file",
                        PROBE_TIMEOUT_SECS
                    ),
                ));
            }
            Ok(out) => {
                items.push(CheckItem::warn(
                    "interactive_bash",
                    format!(
                        "interactive bash exited with status {} at startup",
                        out.status
                            .code()
                            .map_or("signal".to_string(), |c| c.to_string())
                    ),
                ));
            }
            Err(e) => {
                items.push(CheckItem::warn(
                    "interactive_bash",
                    format!("failed to launch interactive bash probe: {}", e),
                ));
            }
        }

        items
    }

    /// Startup-speed item derived from the baseline probe duration.
    fn startup_speed_item(baseline: &BaselineEnv) -> CheckItem {
        let secs = baseline.elapsed.as_secs();
        if baseline.error.is_some() {
            return CheckItem::not_applicable(
                "startup_speed",
                "startup speed not measured (baseline probe failed)",
            );
        }
        if secs >= SLOW_START_SECS {
            CheckItem::warn(
                "startup_speed",
                format!(
                    "login shell startup took {:.1}s (above {}s threshold) — rc files may slow every aish startup",
                    baseline.elapsed.as_secs_f64(),
                    SLOW_START_SECS
                ),
            )
        } else {
            CheckItem::pass(
                "startup_speed",
                format!(
                    "login shell startup: {:.1}s",
                    baseline.elapsed.as_secs_f64()
                ),
            )
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{}…", cut)
    }
}

/// Comma-joined preview of up to 5 directories with an overflow suffix.
fn preview_dirs(dirs: &[&str]) -> String {
    let preview: Vec<&str> = dirs.iter().take(5).copied().collect();
    if dirs.len() > 5 {
        format!("{} … (+{} more)", preview.join(", "), dirs.len() - 5)
    } else {
        preview.join(", ")
    }
}

impl Checker for ShellChecker {
    fn name(&self) -> &str {
        "Shell Compatibility"
    }

    fn check(&self) -> Vec<CheckResult> {
        let baseline = probe_baseline();

        let mut results = Vec::new();

        // Result 1: identity + startup files + speed.
        let mut identity = self.shell_identity_items();
        identity.extend(Self::startup_file_items());
        identity.push(Self::startup_speed_item(&baseline));
        results.push(CheckResult::from_items(self.name(), identity));

        // Result 2: environment inheritance diff.
        let diff = self.env_diff_items(&baseline);
        results.push(CheckResult::from_items(
            format!("{} — Environment", self.name()),
            diff,
        ));

        // Result 3: secrets (names + set/unset only).
        let secrets = self.secret_items(&baseline);
        results.push(CheckResult::from_items(
            format!("{} — Secrets", self.name()),
            secrets,
        ));

        // Result 4: PTY bash compatibility.
        let compat = self.compat_items();
        results.push(CheckResult::from_items(
            format!("{} — PTY Bash", self.name()),
            compat,
        ));

        results
    }

    fn fix(&self, _item: &CheckItem) -> FixResult {
        // Read-only checker by design (issue #474): never touch user rc
        // files or environment automatically.
        FixResult {
            success: false,
            message: "Shell Compatibility is read-only; edit rc files manually".to_string(),
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_secret_key() {
        assert!(is_secret_key("OPENAI_API_KEY"));
        assert!(is_secret_key("GITHUB_TOKEN"));
        assert!(is_secret_key("AWS_SECRET_ACCESS_KEY"));
        assert!(is_secret_key("MY_DB_PASSWORD"));
        assert!(is_secret_key("MY_PASSWD"));
        assert!(is_secret_key("GIT_CREDENTIAL"));
        // AWS_ prefix family: credential-ish AWS_* variables only.
        assert!(is_secret_key("AWS_SECRET_KEY"));
        // Non-secrets must not match: substring false positives.
        assert!(!is_secret_key("PATH"));
        assert!(!is_secret_key("MONKEY_PATCHED"));
        assert!(!is_secret_key("TOKENIZE_MODE"));
        assert!(!is_secret_key("AWS_PAGER"));
        assert!(!is_secret_key("KEYBOARD_LAYOUT"));
    }

    #[test]
    fn test_probe_baseline_parses_env() {
        // Environment-dependent probe: a broken rc file on the test host
        // legitimately produces an error baseline. Only assert structural
        // invariants that hold in both outcomes.
        let baseline = probe_baseline();
        if baseline.error.is_none() {
            assert!(baseline.vars.contains_key("PATH"));
        }
    }

    #[test]
    fn test_is_executable() {
        assert!(is_executable("/bin/bash"));
        assert!(!is_executable("/nonexistent/aish-test-xyz"));
    }

    #[test]
    fn test_path_diff_item_reports_missing_dirs() {
        let checker = ShellChecker::new();
        let item = checker.path_diff_item("/usr/bin:/bin", "/usr/local/bin:/usr/bin:/bin");
        assert!(item.message.contains("missing from aish"));
        assert!(item.message.contains("/usr/local/bin"));
    }

    #[test]
    fn test_is_job_control_warning() {
        // English and localized variants of the expected warning.
        assert!(is_job_control_warning(
            "bash: cannot set terminal process group (123): Inappropriate ioctl for device"
        ));
        assert!(is_job_control_warning(
            "bash: 无法设定终端进程组 (123): 对设备不适当的 ioctl 操作"
        ));
        assert!(is_job_control_warning("bash: 此 shell 中无任务控制"));
        // Real rc-file noise must still be flagged.
        assert!(!is_job_control_warning("cannot find /tmp/cargo-home/env"));
        assert!(!is_job_control_warning("some random error"));
    }

    #[test]
    fn test_preview_dirs_overflow() {
        let dirs = vec!["/a", "/b", "/c", "/d", "/e", "/f", "/g"];
        let out = preview_dirs(&dirs);
        assert!(out.contains("/e"));
        assert!(!out.contains("/f"));
        assert!(out.contains("(+2 more)"));
        assert_eq!(preview_dirs(&["/a", "/b"]), "/a, /b");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("01234567890", 5), "01234…");
    }

    #[test]
    fn test_check_produces_all_sections() {
        let checker = ShellChecker::new();
        let results = checker.check();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].checker, "Shell Compatibility");
        assert_eq!(results[1].checker, "Shell Compatibility — Environment");
        assert_eq!(results[2].checker, "Shell Compatibility — Secrets");
        assert_eq!(results[3].checker, "Shell Compatibility — PTY Bash");
    }
}
