use std::ffi::CString;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::openpty;
use nix::sys::signal::{kill, Signal};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, OutputFlags, SetArg};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{close, dup2, execvp, fork, getuid, pipe, ForkResult, Pid};

use aish_core::AishError;
use tracing::debug;

use crate::command_state::CommandState;
use crate::control::{decode_control_chunk, BackendControlEvent};
use crate::types::{CancelToken, CommandSource};

/// Bash rc wrapper script embedded at compile time.
const BASH_RC_WRAPPER: &str = include_str!("bash_rc_wrapper.sh");

// Interactive commands where Ctrl-C should be forwarded as character, not SIGINT.
const SESSION_COMMANDS: &[&str] = &["ssh", "telnet", "mosh", "nc", "netcat", "ftp", "sftp"];

// Commands that need a real terminal (PTY) for interactive use.
const INTERACTIVE_COMMANDS: &[&str] = &[
    "vim", "vi", "nano", "emacs", "ssh", "telnet", "mosh", "htop", "top", "btop", "iotop", "less",
    "more", "most", "man", "screen", "tmux", "mc", "ranger",
];

fn write_all_with_retry<F>(buf: &[u8], mut write_once: F) -> bool
where
    F: FnMut(&[u8]) -> Result<usize, i32>,
{
    // Returns true iff every byte of `buf` was handed to `write_once`
    // successfully. EINTR is retried; Ok(0) and other errnos give up early
    // and return false so the caller can decide whether to retry at a
    // higher level (e.g. re-injecting a PS1 marker on the next prompt).
    let mut remaining = buf;
    while !remaining.is_empty() {
        match write_once(remaining) {
            Ok(0) => return false,
            Ok(written) => remaining = &remaining[written.min(remaining.len())..],
            Err(errno) if errno == libc::EINTR => continue,
            Err(errno) => {
                debug!(errno, "failed to forward PTY output to stdout");
                return false;
            }
        }
    }
    true
}

fn write_stdout_all(buf: &[u8]) {
    // Return value intentionally ignored: callers fire-and-forget stdout writes.
    let _ = write_all_with_retry(buf, |remaining| {
        let rc = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                remaining.as_ptr() as *const libc::c_void,
                remaining.len(),
            )
        };
        if rc < 0 {
            Err(std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO))
        } else {
            Ok(rc as usize)
        }
    });
}

/// Strip ANSI escape sequences from a byte slice, returning the visible
/// text. Used by prompt detection so colorized prompts are recognized.
///
/// Handles the three common escape forms:
/// * CSI: `\x1b[...<letter>` (colours, cursor moves)
/// * OSC: `\x1b]...<BEL>` or `\x1b]...\x1b\\` (terminal title — emitted by
///   bash's default PS1 on most distros)
/// * Other: `\x1b<single char>` (e.g. `\x1b7` save cursor)
fn strip_ansi(data: &[u8]) -> String {
    let s = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        if chars.peek().copied() == Some('\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Heuristic: does this byte slice (assumed to be one terminal line) look
/// like a remote shell prompt?
///
/// Matches bash `[user@host dir]# `, Ubuntu `user@host:dir$ `, zsh `host% `,
/// and similar — including ANSI-coloured variants. Rejects long lines or
/// lines that lack typical prompt punctuation to avoid misclassifying
/// command output that happens to end in `# ` or `$ `.
fn looks_like_remote_prompt(line: &[u8]) -> bool {
    if line.is_empty() || line.len() > 200 {
        return false;
    }
    let stripped = strip_ansi(line);
    if stripped.is_empty() || stripped.len() > 80 {
        return false;
    }
    let ends_ok = stripped.ends_with("# ")
        || stripped.ends_with("$ ")
        || stripped.ends_with("% ")
        || stripped.ends_with("> ");
    if !ends_ok {
        return false;
    }
    // Require at least one typical prompt character so command output that
    // coincidentally ends in `# ` is rejected.
    if stripped.contains('@') || stripped.contains('[') || stripped.contains(']') {
        return true;
    }
    // zsh's default prompt is often just `host% ` — no `@`/`[`/`]`. Accept it
    // only when the stem looks like a bare hostname (alphanumeric, `.`, `_`,
    // `-`, no whitespace, at least one letter) so command output trailing in
    // `% ` (e.g. `50% ` progress lines) is not matched.
    if stripped.ends_with("% ") {
        let stem = stripped.trim_end().trim_end_matches('%').trim();
        return !stem.is_empty()
            && !stem.chars().any(char::is_whitespace)
            && stem.chars().any(|c| c.is_ascii_alphabetic())
            && stem
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    }
    false
}

/// Detect whether the last terminal line in `data` looks like a remote
/// shell prompt. Returns true when the chunk ends with a prompt-shaped
/// line (e.g. `[root@host ~]# `, `user@host:~$ `, ANSI-coloured variants).
///
/// Used to find a safe moment to inject the `PS1=...` command that bakes
/// the `[ssh:host]` marker into the remote prompt. We only inject at a
/// clean prompt boundary; injecting mid-line would interrupt the user.
fn last_line_is_remote_prompt(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let last_sep = data.iter().rposition(|&b| b == b'\n' || b == b'\r');
    let last_line_start = last_sep.map(|p| p + 1).unwrap_or(0);
    looks_like_remote_prompt(&data[last_line_start..])
}

/// Return the ANSI-stripped last line of `data` if non-empty.
///
/// Mirrors the line-splitting logic in `last_line_is_remote_prompt` so
/// callers can inspect the candidate prompt's content (e.g. to detect
/// zsh `% `-style prompts vs bash `$ `/`# `-style). Returns `None` when
/// the last line is empty (no prompt candidate to inspect).
fn stripped_last_line(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    let last_sep = data.iter().rposition(|&b| b == b'\n' || b == b'\r');
    let last_line = match last_sep {
        Some(p) => &data[p + 1..],
        None => data,
    };
    if last_line.is_empty() {
        return None;
    }
    Some(strip_ansi(last_line).into_bytes())
}

/// Build the bash command that prepends a yellow `[ssh:host]` marker to
/// the remote PS1. Uses `\[\]` readline zero-width markers so bash's own
/// cursor-column tracking accounts for the colour escapes correctly —
/// arrow keys, Ctrl-R and Tab completion all keep working.
///
/// `\e` is bash's PS1 escape for ASCII ESC (shorter than `\033`). The
/// trailing `printf '\33[A\33[J'` moves the cursor up one line (where bash
/// echoed this very `PS1=...` command) and erases from there to end of
/// screen — this reliably clears the echo even when it wraps across
/// multiple visual lines on narrow terminals.
///
/// The leading space keeps the command out of bash's history list on
/// any system with `HISTCONTROL=ignorespace` or `ignoreboth` (the
/// default on most Linux distros, including UOS). Without it, the user
/// would see this injection every time they press Up arrow.
///
/// Build the bash command that injects an environment-aware prefix into
/// the remote PS1. The prefix is split into:
///
/// - **static** (baked into the PS1 literal): `[ssh:user@host ⤴ jumps | shell | container | kube:ctx]`
///   wrapped in `\[\]` so bash's readline treats color escapes as zero-width.
/// - **live** (updated by `__aish_ctx_hook` each prompt): git branch, venv
///   name, sudo-escalation `[ROOT]` badge.
///
/// When `enable_git=false` or `shell_type != Bash`, falls back to a minimal
/// legacy-style injection with no hook (matches pre-#249 behavior).
///
/// The host string is shell-quoted before splicing into the single-quoted
/// PS1 literal. `parse_ssh_command` is regex-based and could in principle
/// produce a value containing `'`, which would otherwise break out of the
/// PS1 assignment and let arbitrary bytes run on the remote shell. Quoting
/// collapses that risk to a benignly-mangled PS1.
pub(crate) fn build_ps1_marker_command(
    info: &SshCommandInfo,
    snapshot: &RemoteContextSnapshot,
    danger: DangerLevel,
    enable_git: bool,
    show_venv: bool,
    show_container: bool,
    show_kube: bool,
) -> Vec<u8> {
    let color_code = match danger {
        DangerLevel::Danger => "31;1",
        DangerLevel::None => "33",
    };
    let user_at = match &info.user {
        Some(u) if !u.is_empty() => format!("{}@", u),
        _ => String::new(),
    };
    let jumps = info.display_jumps();
    // Legacy fallback: minimal `[ssh:host]` literal. Non-bash shells can't
    // host the PROMPT_COMMAND hook, and the DejaGnu opt-out test expects no
    // `| seg` after the marker in this path.
    if !enable_git || !matches!(snapshot.shell_type, ShellKind::Bash) {
        let static_parts = format!("[ssh:{}{}{}]", user_at, info.host, jumps);
        let prefix = format!("\\[\\e[{}m\\]{}\\[\\e[0m\\]", color_code, static_parts);
        let cmd = format!(
            " PS1={}\"$PS1\"; printf '\\33[A\\33[J'\r",
            shell_quote_escape(&prefix)
        );
        return cmd.into_bytes();
    }

    let shell_name = "bash";
    let container_seg = match &snapshot.container {
        Some(c) if show_container && !c.is_empty() => format!(" | {}", c),
        _ => String::new(),
    };
    let kube_seg = match &snapshot.kube_context {
        Some(ctx) if show_kube && !ctx.is_empty() => format!(" | kube:{}", ctx),
        _ => String::new(),
    };

    let static_parts = format!(
        "[ssh:{}{}{} | {}{}{}]",
        user_at, info.host, jumps, shell_name, container_seg, kube_seg
    );

    let prefix = format!("\\[\\e[{}m\\]{}\\[\\e[0m\\]", color_code, static_parts);

    // Live segments (venv/git/ROOT) must be plain text — no ANSI colour.
    // Bash only parses `\[ \]` from the literal PS1, not from variable
    // expansion; embedding ESC bytes in `__aish_ctx_live` would let readline
    // count them as visible columns and break Up/Down history navigation.
    //   * Leading space → HISTCONTROL=ignorespace keeps this out of history.
    //   * ${PROMPT_COMMAND[*]} collapses array form (bash 5.1+) to string.
    //   * The leading space before __aish_ctx_hook() makes the start anchor
    //     unambiguous (user input never starts with a space) — the echo
    //     strip path relies on this.
    //   * `concat!`/`format!` with explicit spaces: Rust's `\` line
    //     continuation strips leading whitespace.
    //   * `__aish_ctx_hook()` is defined FIRST so the echo-strip anchor sits
    //     at offset 0 of the injected command.
    let was_root_lit = if info.user.as_deref() == Some("root") {
        "1"
    } else {
        "0"
    };
    let venv_block = if show_venv {
        concat!(
            " local v=\"$VIRTUAL_ENV\";",
            " [ -z \"$v\" ] && v=\"$CONDA_DEFAULT_ENV\";",
            " if [ -n \"$v\" ]; then",
            " __aish_ctx_live=\"${__aish_ctx_live}|$(basename \"$v\")\";",
            " fi;",
        )
    } else {
        ""
    };
    let body = format!(
        concat!(
            " __aish_ctx_hook() {{",
            " __aish_ctx_live=\"\";",
            " {venv_block}",
            " local b;",
            " if b=$(git symbolic-ref --short HEAD 2>/dev/null); then",
            " __aish_ctx_live=\"${{__aish_ctx_live}}|\"$b;",
            " fi;",
            " if [ \"$EUID\" = 0 ] && [ \"{was_root}\" != \"1\" ]; then",
            " __aish_ctx_live=\"${{__aish_ctx_live}}[ROOT]\";",
            " fi;",
            " if [ -n \"$__aish_orig_pc\" ]; then",
            " eval \"$__aish_orig_pc\" 2>/dev/null || true;",
            " fi;",
            " }};",
            " __aish_orig_pc=\"${{PROMPT_COMMAND[*]}}\";",
            " __aish_ctx_live=\"\";",
            " PROMPT_COMMAND=__aish_ctx_hook;",
        ),
        venv_block = venv_block,
        was_root = was_root_lit
    );

    let ps1_prefix = format!("{}${{__aish_ctx_live}} ", prefix);
    let cmd = format!(
        "{} PS1={}\"$PS1\"; printf '\\33[A\\33[J'\r",
        body,
        shell_quote_escape(&ps1_prefix)
    );
    cmd.into_bytes()
}

/// Active PS1-echo suppression state. Installed whenever we write a PS1
/// marker command to `master_fd`, used to strip the resulting PTY echo
/// before it reaches the user's terminal.
///
/// `remaining` decrements per stripped echo. After it hits 0 (or after the
/// 10-second safety timeout) the suppressor becomes inert.
pub(crate) struct Ps1EchoSuppressor {
    /// The literal command bytes we wrote to master_fd (host-substituted,
    /// including the trailing `\r`).
    pub(crate) pattern: Vec<u8>,
    /// How many echoes we still expect to strip.
    pub(crate) remaining: u8,
    /// When the suppressor was installed — used for the safety timeout.
    pub(crate) started: std::time::Instant,
    /// Bytes from the trailing edge of the previous chunk that could be the
    /// start of a pattern match but couldn't be confirmed because the match
    /// would extend past the chunk boundary. Prepended to the next chunk so a
    /// pattern split across two PTY reads is still stripped. Used by the
    /// byte-exact path only.
    pub(crate) pending: Vec<u8>,
    /// When Some, an echo's `\r\n` has been stripped (ERASE_LINE emitted) but
    /// the trailing `printf '\33[A\33[J'` sequence — which bash sends as the
    /// last step of the injected command — has only partially arrived. Holds
    /// the bytes after the echo's `\n` that could still be a prefix of
    /// PRINTF_ERASE. Without this, a chunk boundary between `\n` and
    /// `\x1b[A\x1b[J` would let the cursor-positioning escapes leak to the
    /// user's terminal and corrupt cursor tracking.
    pub(crate) pending_printf_erase: Option<Vec<u8>>,
}

impl Ps1EchoSuppressor {
    /// Maximum lifetime of a suppressor. If echoes haven't arrived by this
    /// point, give up so we don't accidentally strip legitimate output
    /// containing `PS1=...` text the user might type later.
    pub(crate) const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
}

/// Construct a suppressor for a freshly-injected PS1 marker. The `pattern`
/// matches what `build_ps1_marker_command(info, snapshot, ...)` produced.
pub(crate) fn build_ps1_echo_suppressor(
    info: &SshCommandInfo,
    snapshot: &RemoteContextSnapshot,
    danger: DangerLevel,
    enable_git: bool,
    show_venv: bool,
    show_container: bool,
    show_kube: bool,
) -> Ps1EchoSuppressor {
    Ps1EchoSuppressor {
        pattern: build_ps1_marker_command(
            info,
            snapshot,
            danger,
            enable_git,
            show_venv,
            show_container,
            show_kube,
        ),
        remaining: 2,
        started: std::time::Instant::now(),
        pending: Vec::new(),
        pending_printf_erase: None,
    }
}

/// Strip the injected PS1 marker echo from `data`.
///
/// Two strategies, selected by `pattern` shape:
///
/// - **Byte-exact** (`strip_ps1_echo_exact`): for the short legacy injection
///   (~70 bytes). Pattern matches the echo byte-for-byte and tolerates
///   chunk-boundary splits via `pending`.
///
/// - **Anchor-based** (`strip_ps1_echo_anchor`): for the long git-aware
///   injection (~410 bytes). Bash readline inserts wrap artifacts (bare
///   `\r`, extra spaces) at terminal-width boundaries that break byte-exact
///   matching, so we anchor on ` __aish_ctx_hook()` plus the next `\n`
///   and strip everything in between.
pub(crate) fn strip_ps1_echo(data: &[u8], sup: &mut Ps1EchoSuppressor) -> Vec<u8> {
    if sup.remaining == 0 || sup.pattern.is_empty() {
        return data.to_vec();
    }
    let uses_anchor_strategy = find_subslice(&sup.pattern, b"__aish_ctx_hook()").is_some();
    if uses_anchor_strategy {
        strip_ps1_echo_anchor(data, sup)
    } else {
        strip_ps1_echo_exact(data, sup)
    }
}

/// Echo count armed at injection time. The suppressor is built with
/// `remaining = 2` (one for local-PTY echo, one for remote-bash echo).
/// Both strip strategies key off this constant to decide how aggressively
/// to buffer pending bytes after the first echo has already been consumed.
/// Hoisted to module scope so `strip_ps1_echo_exact` and
/// `strip_ps1_echo_anchor` cannot drift apart.
const PS1_ECHO_INITIAL_REMAINING: u8 = 2;

/// Minimum trailing-prefix length worth buffering once at least one echo
/// has been stripped. Below this threshold a partial anchor/pattern prefix
/// is treated as user input and emitted immediately. Without this gate,
/// the spacebar would appear dead for ~10s after every injection because
/// both patterns start with `' '` — a single space looks like the start of
/// a split echo. The value is conservative: the smallest realistic echo
/// fragment is far longer than 5 bytes.
const PS1_ECHO_MIN_PREFIX_AFTER_STRIP: usize = 5;

/// Byte-exact strip for the legacy short injection. See `strip_ps1_echo`.
fn strip_ps1_echo_exact(data: &[u8], sup: &mut Ps1EchoSuppressor) -> Vec<u8> {
    // `PS1_ECHO_INITIAL_REMAINING` / `PS1_ECHO_MIN_PREFIX_AFTER_STRIP` are
    // shared with `strip_ps1_echo_anchor` — see their definitions at module
    // scope for the rationale (spacebar-dead-after-injection regression).
    let combined: Vec<u8> = if sup.pending.is_empty() {
        data.to_vec()
    } else {
        let mut v = std::mem::take(&mut sup.pending);
        v.extend_from_slice(data);
        v
    };
    let mut out: Vec<u8> = Vec::with_capacity(combined.len());
    let mut cursor: usize = 0;
    while sup.remaining > 0 && cursor < combined.len() {
        let region = &combined[cursor..];
        let pos = match find_subslice(region, &sup.pattern) {
            Some(p) => p,
            None => {
                let min_prefix = if sup.remaining < PS1_ECHO_INITIAL_REMAINING {
                    PS1_ECHO_MIN_PREFIX_AFTER_STRIP
                } else {
                    1
                };
                let split = (0..region.len())
                    .find(|&i| {
                        let suffix = &region[i..];
                        suffix.len() >= min_prefix
                            && suffix.len() <= sup.pattern.len()
                            && sup.pattern.starts_with(suffix)
                    })
                    .unwrap_or(region.len());
                out.extend_from_slice(&region[..split]);
                if split < region.len() {
                    sup.pending.extend_from_slice(&region[split..]);
                }
                cursor = combined.len();
                break;
            }
        };
        out.extend_from_slice(&region[..pos]);
        cursor += pos + sup.pattern.len();
        sup.remaining = sup.remaining.saturating_sub(1);
    }
    if cursor < combined.len() {
        out.extend_from_slice(&combined[cursor..]);
    }
    out
}

/// Anchor-based strip for the long git-aware injection.
///
/// Find the start anchor ` __aish_ctx_hook()` (leading space makes it
/// specific to our injection — bash function definitions in user input
/// never start with a space), then the next `\n` (bash command terminator),
/// then optionally `\x1b[A\x1b[J` (the trailing printf's cursor-up + erase).
/// Strip everything from the previous `\n` boundary (or the anchor if none)
/// through the end of the printf erase sequence. Single-shot: the full
/// echo must be visible in the current `data` slice; if any piece is
/// missing the suppressor is dropped and `data` passes through unchanged.
/// After strip, `remaining = 0` so the caller discards the suppressor.
///
/// `ERASE_LINE` is emitted in the output so the terminal wipes the line
/// where the initial remote prompt sat (it was displayed to the user in
/// a prior chunk before we could strip it); the new prompt then arrives
/// in the next chunk on a clean line.
fn strip_ps1_echo_anchor(data: &[u8], sup: &mut Ps1EchoSuppressor) -> Vec<u8> {
    const START_ANCHOR: &[u8] = b" __aish_ctx_hook()";
    const PRINTF_ERASE: &[u8] = b"\x1b[A\x1b[J";
    const ERASE_LINE: &[u8] = b"\r\x1b[2K";
    // Upper bound on bytes buffered while waiting for the rest of a split
    // echo. The git-aware injection is ~410 bytes; 1 KiB covers that plus
    // readline wrap artifacts. Beyond this the suppressor gives up and
    // flushes pending so a stray anchor fragment can't swallow an unbounded
    // amount of real output.
    const PENDING_LIMIT: usize = 1024;
    // `PS1_ECHO_INITIAL_REMAINING` / `PS1_ECHO_MIN_PREFIX_AFTER_STRIP` are
    // shared with `strip_ps1_echo_exact` at module scope — see comments there.

    let mut out: Vec<u8> = Vec::with_capacity(data.len() + 16);

    // If the previous chunk left us with bytes that could still be a prefix
    // of PRINTF_ERASE (i.e. we committed an echo strip but the trailing
    // `\x1b[A\x1b[J` hadn't fully arrived), resolve that state first.
    // Without this branch, those bytes would leak to the user's terminal
    // and corrupt cursor tracking — bash/readline would think the cursor
    // is one line below where it visually is, causing symptoms like
    // "cursor position abnormal" and "space needs multiple presses".
    let combined: Vec<u8> = if let Some(pending) = sup.pending_printf_erase.take() {
        let mut v = pending;
        v.extend_from_slice(data);
        // We have at least 1 byte after the echo's \n. Decide whether the
        // combined buffer confirms or refutes PRINTF_ERASE.
        if v.len() >= PRINTF_ERASE.len() {
            if v.starts_with(PRINTF_ERASE) {
                // PRINTF_ERASE confirmed — strip it. This finalises the
                // echo we partially stripped last chunk.
                sup.remaining = sup.remaining.saturating_sub(1);
                v[PRINTF_ERASE.len()..].to_vec()
            } else {
                // Enough bytes and they don't match — no PRINTF_ERASE was
                // sent. Emit the held bytes (they're real output) and
                // finalise the echo.
                out.extend_from_slice(&v);
                sup.remaining = sup.remaining.saturating_sub(1);
                Vec::new()
            }
        } else if PRINTF_ERASE.starts_with(&v) {
            // Still a strict prefix of PRINTF_ERASE — keep buffering until
            // we can confirm or refute.
            sup.pending_printf_erase = Some(v);
            return out;
        } else {
            // Shorter than PRINTF_ERASE and not a prefix — definitely not
            // PRINTF_ERASE. Emit and finalise.
            out.extend_from_slice(&v);
            sup.remaining = sup.remaining.saturating_sub(1);
            Vec::new()
        }
    } else if sup.pending.is_empty() {
        data.to_vec()
    } else {
        let mut v = std::mem::take(&mut sup.pending);
        v.extend_from_slice(data);
        v
    };

    // Loop over echoes: build_ps1_echo_suppressor arms `remaining = 2`
    // because the injected command can be echoed twice (local PTY echo +
    // remote bash echo). Strip each occurrence independently so neither
    // leaks to the terminal.
    let mut cursor: usize = 0;
    while sup.remaining > 0 && cursor < combined.len() {
        let region = &combined[cursor..];
        let anchor_rel = match find_subslice(region, START_ANCHOR) {
            Some(s) => s,
            None => {
                // Once at least one echo has been stripped, the second
                // echo (when it exists at all) arrives back-to-back
                // with the first. Holding back a short trailing prefix
                // of START_ANCHOR at this point would swallow user
                // keystrokes — a literal space is the most common
                // victim since START_ANCHOR begins with `' '`, and the
                // symptom is the spacebar appearing dead for ~10s
                // (until the suppressor's safety timeout fires). Only
                // buffer prefixes long enough to plausibly be a real
                // anchor fragment.
                let min_prefix = if sup.remaining < PS1_ECHO_INITIAL_REMAINING {
                    PS1_ECHO_MIN_PREFIX_AFTER_STRIP
                } else {
                    1
                };
                let split = (0..region.len())
                    .find(|&i| {
                        let suffix = &region[i..];
                        suffix.len() >= min_prefix
                            && suffix.len() <= START_ANCHOR.len()
                            && START_ANCHOR.starts_with(suffix)
                    })
                    .unwrap_or(region.len());
                out.extend_from_slice(&region[..split]);
                if split < region.len() {
                    sup.pending.extend_from_slice(&region[split..]);
                }
                if sup.pending.len() > PENDING_LIMIT {
                    out.append(&mut sup.pending);
                    sup.remaining = 0;
                }
                return out;
            }
        };

        let abs_anchor = cursor + anchor_rel;
        let after_anchor = abs_anchor + START_ANCHOR.len();
        let newline_rel = match combined[after_anchor..].iter().position(|&b| b == b'\n') {
            Some(p) => p,
            None => {
                // Anchor found but the command echo hasn't terminated yet.
                // Hold back the line containing the anchor; emit everything
                // before it.
                let strip_start = match combined[cursor..abs_anchor]
                    .iter()
                    .rposition(|&b| b == b'\n')
                {
                    Some(p) => cursor + p + 1,
                    None => cursor,
                };
                out.extend_from_slice(&combined[cursor..strip_start]);
                sup.pending.extend_from_slice(&combined[strip_start..]);
                if sup.pending.len() > PENDING_LIMIT {
                    out.append(&mut sup.pending);
                    sup.remaining = 0;
                }
                return out;
            }
        };
        let after_newline = after_anchor + newline_rel + 1;

        let strip_start = match combined[cursor..abs_anchor]
            .iter()
            .rposition(|&b| b == b'\n')
        {
            Some(p) => cursor + p + 1,
            None => abs_anchor,
        };

        // Decide where this strip ends. Three cases:
        //   1. PRINTF_ERASE fully present → strip through it.
        //   2. PRINTF_ERASE refuted (enough bytes, no match) → strip
        //      through the echo's \n only.
        //   3. PRINTF_ERASE could still be arriving (chunk ended at or
        //      inside a prefix of PRINTF_ERASE) → emit ERASE_LINE now,
        //      buffer the bytes after \n, and DON'T decrement remaining.
        //      The next chunk resolves case 1 vs 2 via the preamble above.
        let end = if combined.len() >= after_newline + PRINTF_ERASE.len() {
            if &combined[after_newline..after_newline + PRINTF_ERASE.len()] == PRINTF_ERASE {
                after_newline + PRINTF_ERASE.len()
            } else {
                after_newline
            }
        } else {
            let tail = &combined[after_newline..];
            if PRINTF_ERASE.starts_with(tail) {
                // Case 3: emit ERASE_LINE and buffer the pending prefix.
                out.extend_from_slice(&combined[cursor..strip_start]);
                out.extend_from_slice(ERASE_LINE);
                sup.pending_printf_erase = Some(tail.to_vec());
                return out;
            }
            after_newline
        };

        out.extend_from_slice(&combined[cursor..strip_start]);
        out.extend_from_slice(ERASE_LINE);
        cursor = end;
        sup.remaining = sup.remaining.saturating_sub(1);
    }

    if cursor < combined.len() {
        out.extend_from_slice(&combined[cursor..]);
    }
    out
}

/// Find the first occurrence of `needle` in `haystack`. Equivalent to
/// `haystack.windows(needle.len()).position(|w| w == needle)` but
/// handles `needle.len() > haystack.len()` gracefully (returns None).
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Persistent PTY session managing a single long-lived bash process.
pub struct PersistentPty {
    master_fd: RawFd,
    control_fd: RawFd,
    child_pid: Pid,
    command_state: CommandState,
    control_buffer: String,
    #[allow(clippy::type_complexity)]
    output_callback: Option<Arc<dyn Fn(&[u8]) + Send + Sync>>,
    rows: u16,
    cols: u16,
    running: AtomicBool,
    /// Next backend command sequence number (decreasing negatives).
    next_backend_seq: i32,
    /// Shared output buffer for execute_command mode.
    exec_buffer: Arc<Mutex<Vec<u8>>>,
    /// Whether we are in exec mode (buffer output instead of forwarding).
    exec_mode: Arc<AtomicBool>,
    /// Monotonic id for tab-completion requests.
    next_completion_request_id: AtomicU64,
}

#[path = "aish_completion.rs"]
mod aish_completion;

impl PersistentPty {
    /// Start a new persistent bash session.
    pub fn start(cwd: &str, rows: u16, cols: u16) -> aish_core::Result<Self> {
        // Write rcfile to a temp file (bash --rcfile needs a real file path).
        let rcfile_path = write_rcfile_temp()?;

        // Create control pipe.
        let (control_read, control_write) =
            pipe().map_err(|e| AishError::Pty(format!("failed to create control pipe: {e}")))?;

        // Create PTY.
        let pty_result =
            openpty(None, None).map_err(|e| AishError::Pty(format!("failed to openpty: {e}")))?;
        let master_fd = pty_result.master;
        let slave_fd = pty_result.slave;

        // Set master non-blocking.
        set_nonblocking(&master_fd)?;

        // Set control pipe read end non-blocking.
        set_nonblocking(&control_read)?;

        // Sync terminal size.
        let stdin_fd = libc::STDIN_FILENO;
        let _ = sync_window_size(stdin_fd, master_fd.as_raw_fd());

        // Get raw fds for child.
        let slave_raw = slave_fd.as_raw_fd();
        let control_write_raw = control_write.as_raw_fd();
        let rcfile_path_clone = rcfile_path.to_string_lossy().to_string();

        // Fork.
        let child_pid =
            match unsafe { fork() }.map_err(|e| AishError::Pty(format!("fork failed: {e}")))? {
                ForkResult::Parent { child } => {
                    drop(slave_fd);
                    drop(control_write);
                    child
                }
                ForkResult::Child => {
                    child_main(slave_raw, control_write_raw, &rcfile_path_clone, cwd);
                }
            };

        debug!(pid = %child_pid, "persistent bash started");

        // Convert to raw fds.
        let master_raw = master_fd.into_raw_fd();
        let control_raw = control_read.into_raw_fd();

        // NOTE: Don't delete rcfile here -- there's a race condition where bash
        // may not have opened it yet. Delete after session_ready is received.

        let mut pty = Self {
            master_fd: master_raw,
            control_fd: control_raw,
            child_pid,
            command_state: CommandState::new(),
            control_buffer: String::with_capacity(1024),
            output_callback: None,
            rows,
            cols,
            running: AtomicBool::new(true),
            next_backend_seq: -1,
            exec_buffer: Arc::new(Mutex::new(Vec::new())),
            exec_mode: Arc::new(AtomicBool::new(false)),
            next_completion_request_id: AtomicU64::new(0),
        };

        // Wait for session_ready event.  Also returns whether the
        // initial PromptReady was seen in the same control-pipe read
        // (common case: both arrive together).
        let saw_prompt = pty.wait_for_session_ready(Duration::from_secs(5))?;

        // Consume the initial PromptReady if it wasn't already seen
        // during session_ready.  Use a short timeout — bash emits it
        // very quickly after SessionReady.
        if !saw_prompt {
            pty.wait_for_initial_prompt_ready(Duration::from_millis(500));
        }
        pty.drain_master_to_stdout();

        // Now safe to clean up rcfile -- bash has loaded it.
        let _ = std::fs::remove_file(&rcfile_path);

        Ok(pty)
    }

    /// Send a command to bash (no waiting for completion).
    pub fn send_command(&mut self, command: &str, seq: Option<i32>) -> aish_core::Result<()> {
        let source = if seq.is_some() {
            CommandSource::Backend
        } else {
            CommandSource::User
        };
        self.command_state.register_command(command, source, seq);

        // Prepend Ctrl-U (NAK) to clear stale input in the PTY line
        // discipline canonical buffer.  Keystrokes forwarded from the
        // interactive forwarding loop may linger there and corrupt the
        // next command.
        let mut payload = b"\x15".to_vec();
        if let Some(s) = seq {
            let quoted = shell_quote_escape(command);
            payload.extend_from_slice(
                format!(" __AISH_ACTIVE_COMMAND_SEQ={s}; __AISH_ACTIVE_COMMAND_TEXT={quoted}; ")
                    .as_bytes(),
            );
        }
        payload.extend_from_slice(command.as_bytes());
        payload.push(b'\n');

        self.write_master(&payload)
    }

    /// Execute a command and wait for completion with timeout.
    /// Returns cleaned output and exit code.
    /// When `cancel_token` is provided, the caller can request
    /// cancellation; on cancel the method sends SIGINT and returns
    /// exit code -1.
    pub fn execute_command(
        &mut self,
        command: &str,
        timeout: Duration,
        cancel_token: Option<&CancelToken>,
        display_output: bool,
    ) -> aish_core::Result<(String, i32, String)> {
        let seq = self.allocate_backend_seq();

        // Enter exec mode: buffer output.
        self.exec_buffer.lock().unwrap().clear();
        self.exec_mode.store(true, Ordering::SeqCst);

        self.send_command(command, Some(seq))?;

        // Save and set terminal to non-canonical mode so we can read
        // individual bytes (Ctrl+Z = 0x1a, Ctrl+C = 0x03) without the
        // terminal driver intercepting them.
        let stdin_fd = libc::STDIN_FILENO;
        let stdin_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
        let saved_termios = tcgetattr(stdin_borrowed).ok();
        let pty_raw_termios = saved_termios.as_ref().map(|saved| {
            let mut raw = saved.clone();
            use nix::sys::termios::{ControlFlags, InputFlags, LocalFlags};
            raw.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
            raw.input_flags &= !InputFlags::ISTRIP;
            raw.control_flags &= !ControlFlags::CSIZE;
            raw.control_flags |= ControlFlags::CS8;
            raw.control_chars[libc::VMIN] = 1;
            raw.control_chars[libc::VTIME] = 0;
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw);
            raw
        });

        let deadline = std::time::Instant::now() + timeout;
        let mut result_exit_code: i32 = -1;
        let mut result_cwd = String::new();
        let mut cancelled = false;
        // Select-based I/O loop.
        'select_loop: while std::time::Instant::now() < deadline {
            // Check external cancellation.
            if let Some(ref ct) = cancel_token {
                if ct.is_cancelled() {
                    let _ = self.write_master(b"\x03");
                    cancelled = true;
                    break;
                }
            }

            let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            unsafe {
                libc::FD_ZERO(&mut read_fds);
                libc::FD_SET(stdin_fd, &mut read_fds);
                libc::FD_SET(self.master_fd, &mut read_fds);
                libc::FD_SET(self.control_fd, &mut read_fds);
            }
            let max_fd = self.master_fd.max(self.control_fd).max(stdin_fd) + 1;

            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let mut tv = libc::timeval {
                tv_sec: remaining.as_secs().min(1) as libc::c_long,
                tv_usec: (remaining.subsec_micros() % 1_000_000) as libc::c_long,
            };

            let sel = unsafe {
                libc::select(
                    max_fd,
                    &mut read_fds,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut tv,
                )
            };

            if sel < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    continue;
                }
                break;
            }

            if sel == 0 {
                continue;
            }

            // Read stdin -> forward to master.
            if unsafe { libc::FD_ISSET(stdin_fd, &read_fds) } {
                let mut tmp = [0u8; 64];
                match unsafe {
                    libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        if data.contains(&0x03) {
                            // Ctrl+C: forward the byte to the PTY so the
                            // terminal driver delivers SIGINT to the
                            // *foreground* process group (the pager's group),
                            // not bash's own group. Pagers like `less` swallow
                            // SIGINT, so the SIGTERM/SIGKILL escalation for
                            // them happens in the cleanup section below after
                            // the loop breaks.
                            let _ = self.write_master(b"\x03");
                            if let Some(ref ct) = cancel_token {
                                // Mark as user interrupt so bash can abort the
                                // LLM session — raw-mode Ctrl+C is 0x03, not SIGINT.
                                ct.cancel_as_user_interrupt();
                            }
                            cancelled = true;
                            break 'select_loop;
                        } else if data.contains(&0x0f) {
                            // Ctrl+O: invoke the live output viewer (blocks until closed).
                            // Restore terminal from PTY raw mode so the panel can use it.
                            // TCSANOW: must switch immediately so invoke() gets a usable terminal.
                            if let Some(ref saved) = saved_termios {
                                let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, saved);
                            }
                            let buf = self.exec_buffer.lock().unwrap().clone();
                            crate::ctrl_o::invoke(&buf);
                            if let Some(ref raw) = pty_raw_termios {
                                let _ = tcsetattr(stdin_borrowed, SetArg::TCSADRAIN, raw);
                            }
                        } else {
                            // Forward everything else (including Ctrl+Z = 0x1a)
                            // to the PTY so bash handles it natively.
                            let _ = self.write_master(data);
                        }
                    }
                    _ => {}
                }
            }

            // Read master -> exec buffer.
            if unsafe { libc::FD_ISSET(self.master_fd, &read_fds) } {
                let mut tmp = [0u8; 8192];
                match unsafe {
                    libc::read(
                        self.master_fd,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 && self.exec_mode.load(Ordering::SeqCst) => {
                        if display_output {
                            write_stdout_all(&tmp[..n as usize]);
                        }
                        self.exec_buffer
                            .lock()
                            .unwrap()
                            .extend_from_slice(&tmp[..n as usize]);
                    }
                    n if n > 0 => {}
                    0 => {
                        self.running.store(false, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }

            // Read control pipe for events.
            if unsafe { libc::FD_ISSET(self.control_fd, &read_fds) } {
                let mut tmp = [0u8; 4096];
                match unsafe {
                    libc::read(
                        self.control_fd,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        let events =
                            decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]);
                        for event in &events {
                            if let BackendControlEvent::ShellExiting { .. } = event {
                                self.running.store(false, Ordering::SeqCst);
                            }
                            if let BackendControlEvent::PromptReady { cwd, .. } = event {
                                result_cwd = cwd.clone();
                            }
                            if let Some(r) = self.command_state.handle_event(event) {
                                if r.command_seq == Some(seq) {
                                    result_exit_code = r.exit_code;
                                    break 'select_loop;
                                }
                            }
                        }
                    }
                    0 => {
                        self.running.store(false, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Drain remaining output.
        self.drain_master_to_exec_buffer();

        // If cancelled, forcefully terminate any foreground job that survived
        // the Ctrl+C byte. Interactive pagers (e.g. `less` invoked by
        // `nmcli -p`) swallow SIGINT and keep owning the PTY foreground,
        // which would leave bash stuck and pollute subsequent commands. We
        // escalate to SIGTERM/SIGKILL against the *foreground* process group
        // (the pager's group), never against bash's own group, so the
        // long-lived session survives for the next command.
        if cancelled {
            force_cancel_pty_foreground(self.master_fd, self.child_pid);
            // Give bash a moment to reclaim the foreground after the pager
            // dies, then drain residual output (job-terminated notices, the
            // restored prompt) into the exec buffer so it is captured rather
            // than leaking into the next command.
            std::thread::sleep(Duration::from_millis(50));
            self.drain_master_to_exec_buffer();
        }

        self.exec_mode.store(false, Ordering::SeqCst);

        // Flush stale input so escape sequences don't confuse the next prompt.
        unsafe {
            libc::tcflush(stdin_fd, libc::TCIFLUSH);
        }

        // Restore terminal settings.
        if let Some(ref saved) = saved_termios {
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSADRAIN, saved);
        }

        let raw_output = self
            .exec_buffer
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<u8>>();
        let raw_str = String::from_utf8_lossy(&raw_output).to_string();

        if cancelled {
            let cleaned = clean_pty_output(&raw_str, command);
            Ok((cleaned, -1, result_cwd))
        } else {
            let cleaned = clean_pty_output(&raw_str, command);
            Ok((cleaned, result_exit_code, result_cwd))
        }
    }
}

impl PersistentPty {
    /// Send a user command and enter raw stdin forwarding mode until
    /// prompt_ready is received. Returns (exit_code, cwd, output).
    pub fn send_command_interactive(
        &mut self,
        command: &str,
        ai_callback: Option<Box<crate::AiCallback>>,
        status_callback: Option<Box<crate::StatusCallback>>,
        shared_host: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
        secret_check: Option<
            std::sync::Arc<dyn Fn(&str) -> Option<crate::SshSecretCheckResult> + Send + Sync>,
        >,
        secret_vault: Option<std::sync::Arc<std::sync::Mutex<aish_security::secret::SecretVault>>>,
        on_output: Option<Box<dyn Fn(&str) + Send>>,
        input_guard_enabled: bool,
        enable_remote_git_prompt: bool,
        remote_rich_prompt: bool,
        remote_danger_patterns: Vec<String>,
        remote_show_venv: bool,
        remote_show_container: bool,
        remote_show_kube: bool,
    ) -> aish_core::Result<(i32, String, String)> {
        let is_session = is_session_command(command);
        debug!(
            "send_command_interactive ENTER: cmd={:?}, is_session={}, master_fd={}, control_fd={}",
            command, is_session, self.master_fd, self.control_fd
        );
        let master_fd = self.master_fd;
        // Compile per call: patterns may change between calls (hot reload,
        // different profile), so a process-wide cache would be wrong.
        let compiled_danger_patterns =
            aish_config::compile_remote_danger_patterns(&remote_danger_patterns);
        let mut interceptor = if is_session {
            crate::SessionInterceptor::new(ai_callback, status_callback, input_guard_enabled)
        } else {
            crate::SessionInterceptor::new(None, None, input_guard_enabled)
        };

        // Drain stale data from both the PTY master fd and the control
        // pipe BEFORE registering the new command.  A stale PromptReady
        // left in the control pipe (e.g. from bash's initial prompt or
        // from a previous command whose event arrived late) would be
        // matched with the new command's submission, producing a wrong
        // exit code (the classic off-by-one shift).
        self.drain_master_silent();
        self.drain_control_pipe_raw();

        self.command_state
            .register_command(command, CommandSource::User, None);

        // Write command to bash.  Prepend Ctrl-U (NAK) to clear any
        // stale input in the PTY line discipline canonical buffer so
        // that leftover keystrokes from a previous interactive session
        // are not prepended to the actual command.
        let mut payload = vec![0x15];
        payload.extend_from_slice(command.as_bytes());
        payload.push(b'\n');
        self.write_master(&payload)?;

        // Save and set terminal to raw mode.
        let stdin_fd = libc::STDIN_FILENO;
        let stdin_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
        let saved_termios = tcgetattr(stdin_borrowed).ok();
        if let Some(ref saved) = saved_termios {
            let mut raw = saved.clone();
            cfmakeraw(&mut raw);
            // Re-enable output processing so that \n in PTY output is
            // converted to \r\n by the terminal driver.  Without this,
            // interactive sessions (ssh, telnet) display prompts
            // concatenated on the same line because the terminal emulator
            // only moves the cursor down for bare \n without returning to
            // column 0.
            raw.output_flags |= OutputFlags::OPOST | OutputFlags::ONLCR;
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw);
        }

        // Forwarding loop.
        let mut write_buf: Vec<u8> = Vec::new();
        let mut result_cwd = String::new();
        let mut result_exit_code: i32 = -1;
        let mut output_buf: Vec<u8> = Vec::new();
        let mut done = false;
        // After receiving PromptReady, keep draining master_fd until a full
        // select timeout passes with no new data.  The control pipe may
        // deliver PromptReady before the kernel has flushed all PTY output
        // to master_fd, causing intermittent missing output for fast
        // commands.
        let mut draining = false;
        let mut deferred_control_events: Vec<crate::control::BackendControlEvent> = Vec::new();
        // Wall-clock budget for the deferred-event replay loop. If the polkit
        // heuristic keeps returning true longer than this budget — e.g. a
        // translated polkit banner that no longer matches the hard-coded
        // English strings, or some future regression in
        // `polkit_auth_in_progress` — we abandon the deferral and flush the
        // events through `handle_event`. This converts the failure mode from
        // "shell hangs forever" (the bug this path once caused) to "shell
        // pauses for ~10 s, then proceeds".
        //
        // A wall-clock deadline (rather than an iteration count) is required
        // because `select` returns immediately when PTY/control/stdin has
        // data ready — under password entry with keystroke echoes or chatty
        // PTY output, the loop can blow through an iteration budget in a
        // fraction of the intended window and flush `PromptReady` while
        // polkit auth is genuinely still in progress.
        const DEFERRED_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(10);
        let mut deferred_since: Option<std::time::Instant> = None;
        // The PTY may emit a bare leading newline from stale prompt
        // rendering.  Only skip a leading CR-LF or LF at the very start
        // of the first chunk -- never consume actual command output.
        let mut skip_leading_newline = true;
        // When a command is injected, the remote shell echoes it back.
        // Store the command here so the echo can be stripped from output.
        let mut skip_echo_cmd: Option<String> = None;
        // Followup callback state: after AI injects a command, capture its
        // output and call the followup when the shell goes idle.
        let mut pending_followup: Option<Box<crate::FollowupCallback>> = None;
        let mut followup_captured: Vec<u8> = Vec::new();
        let mut followup_capturing = false;
        // Offloader for followup output (created when capturing starts)
        let mut followup_offloader: Option<crate::PtyOutputOffload> = None;
        // Set to true when output that looks like a remote shell prompt
        // (ending with "# " or "$ ") is seen during followup_capturing.
        // Once detected the idle threshold is reduced so the followup
        // fires quickly instead of waiting the full FOLLOWUP_IDLE_GRACE.
        let mut followup_prompt_seen = false;
        const FOLLOWUP_PROMPT_IDLE: u32 = 10; // 500 ms after prompt detected
                                              // When the user presses Ctrl+Z or Ctrl+C during followup_capturing,
                                              // start a short countdown. The followup fires once the countdown
                                              // reaches 0 (after FOLLOWUP_INTERRUPT_GRACE consecutive idle polls),
                                              // which captures any suspension/termination output but avoids waiting
                                              // for the full FOLLOWUP_IDLE_GRACE period.
        let mut followup_interrupt_countdown: Option<u32> = None;
        const FOLLOWUP_INTERRUPT_GRACE: u32 = 10; // 500 ms
                                                  // Pending AI response — shared between TriggerAi handler and
                                                  // followup handler for multi-round tool chaining.
        let mut pending_response: Option<crate::AiResponse> = None;
        // Consecutive idle poll count — require N empty polls before treating
        // the shell as truly idle (prevents premature followup triggers over
        // SSH where brief network gaps can exceed 50ms).
        let mut idle_poll_count: u32 = 0;
        const IDLE_THRESHOLD: u32 = 3;
        // Extra idle grace for bashexec followup capturing. Commands like
        // `for i in {1..N}; do echo $i; sleep 1; done` produce intermittent
        // output with multi-second gaps. Without extra grace the followup
        // fires after only 150 ms of silence (IDLE_THRESHOLD * 50 ms).
        const FOLLOWUP_IDLE_GRACE: u32 = 100; // 5 s at 50 ms per poll

        // Host probe state
        let mut probe_active = false;
        let mut probe_sections: Vec<String> = Vec::new();
        let mut probe_current_section = String::new();
        let mut probe_injected = false;
        let mut probe_start: Option<std::time::Instant> = None;
        const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        const PROBE_MARKER_COUNT: usize = 5;
        let mut remote_info_for_probe: Option<SshCommandInfo> = if is_session {
            parse_ssh_command(command)
        } else {
            None
        };
        // `remote_host_for_probe` mirrors `remote_info_for_probe.dest_raw`.
        // Kept because `ps1_marker_done_for` and the nested-SSH stack key on
        // the bare destination string.
        let mut remote_host_for_probe: Option<String> =
            remote_info_for_probe.as_ref().map(|i| i.dest_raw.clone());
        // Nested SSH tracking: scan PTY output (not keystrokes) for
        // SSH commands so history/recall/paste all work correctly. The
        // stack stores the OUTER session's full `SshCommandInfo` so a pop
        // restores user/port/jumps — previously it stored only the dest
        // string and synthesized an info with `user: None`, which dropped
        // the `root` flag on rollback and mis-rendered the danger color.
        let mut nested_host_stack: Vec<SshCommandInfo> = Vec::new();
        let mut output_ssh_scan = String::new();
        let mut nested_probe_pending = false;
        // Accumulator for ssh success-signal detection across PTY chunks.
        // Cleared on y-confirmation, appended every chunk while a nested
        // session is pending, scanned by `scan_output_for_ssh_success` at
        // PS1-inject time. Without this, a multi-chunk MOTD (where the
        // `Last login:` line arrives in an earlier chunk than the final
        // bash prompt) defeats the success-signal gate and PS1 inject
        // never fires for legitimate nested sessions.
        let mut nested_confirm_buf: String = String::new();
        // Stdin shadow: track user keystrokes to detect session commands
        // (ssh, telnet) typed character-by-character.  The PTY-output
        // scanner relies on the remote shell echo format which can be
        // unreliable (readline redraws with \r, ANSI escapes, etc.).
        // This shadow gives a clean, format-independent detection path.
        let mut stdin_shadow: Vec<u8> = Vec::with_capacity(4096);
        // Track if we're at a password prompt. SSH shows "password:" before
        // authentication. The probe command would corrupt password input
        // because the PTY line discipline buffers keystrokes until newline.
        // Clear this flag when we see a shell prompt (# or $) indicating
        // successful authentication.
        let mut at_password_prompt = false;
        // Track if we're in history search mode (Ctrl+R or Ctrl+S). The probe
        // should not be injected during search - it would corrupt the search
        // input. Clear when we see a shell prompt indicating search ended.
        let mut in_search_mode = false;
        // Tracks the remote host for which we have already baked the
        // `[ssh:host]` marker into PS1. Reset (to None) implicitly by
        // comparing against `remote_host_for_probe` — when the user enters
        // a nested SSH, the new host triggers a fresh injection.
        let mut ps1_marker_done_for: Option<String> = None;
        // Saved ps1_marker_done_for values for outer SSH sessions, parallel
        // to nested_host_stack. When the user enters a nested SSH we push
        // the current value (and clear ps1_marker_done_for so the new bash
        // gets its own injection); when they exit the nested session we
        // pop and restore. Without this, returning to the outer session
        // would look like a host change and trigger a second PS1 injection,
        // stacking two `[ssh:...]` markers in front of the prompt.
        let mut ps1_marker_done_stack: Vec<Option<String>> = Vec::new();
        // Active PS1-echo suppressor. Installed when we inject a PS1 marker
        // command, used to strip the local PTY echo + remote bash echo of the
        // command bytes before they reach the user's terminal. See Task docs
        // in `strip_ps1_echo` for the full rationale.
        let mut ps1_echo_suppressor: Option<Ps1EchoSuppressor> = None;
        // Bytes pulled out of a timed-out PS1 echo suppressor's `pending`
        // buffer. Prepended to the next PTY read so held-back output is
        // displayed instead of disappearing with the suppressor.
        let mut ps1_pending_flush: Vec<u8> = Vec::new();
        // When a probe is triggered on-demand by the AI trigger (`;`),
        // the AI question is saved here and invoked after the probe completes.
        let mut pending_ai_question: Option<String> = None;
        let mut probe_for_ai: bool = false;
        // Track when a session command was just sent (ssh, telnet, etc.).
        // When a session command is executing, SSH may show password prompts
        // after the PTY briefly goes idle. We need extra idle polls before
        // injecting the probe to give SSH time to display auth prompts.
        let mut session_command_just_sent = is_session;
        const SESSION_CMD_IDLE_GRACE: u32 = 20; // extra idle polls for session commands (1 second at 50ms)
                                                // Hard-cancel flag: Ctrl+C during confirmation sets this to true,
                                                // preventing further followup/tool chaining until the next AI trigger.
        let mut ai_cancelled: bool = false;

        // Helper: write to stdout and optionally record via on_output callback.
        let show = |text: &str| {
            write_stdout_all(text.as_bytes());
            if let Some(ref cb) = on_output {
                cb(text);
            }
        };

        while !done {
            // Build fd sets.
            let mut read_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            let mut write_fds: libc::fd_set = unsafe { std::mem::zeroed() };
            unsafe {
                libc::FD_ZERO(&mut read_fds);
                libc::FD_ZERO(&mut write_fds);
                if !draining {
                    libc::FD_SET(stdin_fd, &mut read_fds);
                    libc::FD_SET(self.control_fd, &mut read_fds);
                }
                libc::FD_SET(self.master_fd, &mut read_fds);
                if !write_buf.is_empty() {
                    libc::FD_SET(self.master_fd, &mut write_fds);
                }
            }

            let max_fd = if draining {
                self.master_fd + 1
            } else {
                self.master_fd.max(self.control_fd).max(stdin_fd) + 1
            };
            // Shorter timeout during drain phase (5ms) to avoid noticeable
            // latency after the command has already completed.
            let mut tv = libc::timeval {
                tv_sec: 0,
                tv_usec: if draining { 5_000 } else { 50_000 },
            };

            let sel = unsafe {
                libc::select(
                    max_fd,
                    &mut read_fds,
                    &mut write_fds,
                    std::ptr::null_mut(),
                    &mut tv,
                )
            };

            if sel < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    continue;
                }
                break;
            }

            if sel == 0 {
                // Timeout -- during drain phase this means all output has
                // been delivered.  During normal phase increment the idle
                // counter to require consecutive empty polls before acting.
                if idle_poll_count.checked_rem(20) == Some(0) && idle_poll_count > 0 {
                    debug!(
                        "select timeout (idle_poll={}, draining={}, done={}, running={}, \
                         nested_stack={}, remote_host={:?}, ps1_done={:?}, probe_active={}, \
                         nested_probe_pending={}",
                        idle_poll_count,
                        draining,
                        done,
                        self.running.load(Ordering::SeqCst),
                        nested_host_stack.len(),
                        remote_host_for_probe,
                        ps1_marker_done_for,
                        probe_active,
                        nested_probe_pending,
                    );
                }
                idle_poll_count += 1;

                // Safety net: abort probe if it hasn't completed in time
                if probe_active {
                    if let Some(start) = probe_start {
                        if start.elapsed() > PROBE_TIMEOUT {
                            debug!(
                                "probe: TIMEOUT after {:?}, sections={}",
                                start.elapsed(),
                                probe_sections.len(),
                            );
                            probe_active = false;
                            // Save a minimal profile so the host is tracked
                            // even if the probe data is incomplete.
                            if let Some(ref host_key) = remote_host_for_probe {
                                let mut profile = aish_hosts::get_or_create_profile(host_key);
                                profile.last_updated = chrono::Utc::now();
                                let _ = aish_hosts::save_profile(&profile);
                                debug!("probe: saved minimal profile for {} on timeout", host_key);
                            }
                            probe_sections.clear();
                            probe_current_section.clear();
                            // Refresh the prompt so the user isn't stuck.
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\r".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            skip_leading_newline = true;

                            // If this probe was triggered on-demand by
                            // AI, invoke the callback even on timeout
                            // (with minimal profile data).
                            if probe_for_ai {
                                probe_for_ai = false;
                                if let Some(question) = pending_ai_question.take() {
                                    let ec = self.command_state.last_exit_code();
                                    let resp =
                                        interceptor.call_ai(question, ec, secret_vault.as_ref());
                                    interceptor.finish_ai();
                                    if let Some(response) = resp {
                                        pending_response = Some(response);
                                    } else {
                                        let cancel_msg = format!(
                                            "\x1b[33m{}\x1b[0m\r\n",
                                            aish_i18n::t("shell.session.ai_cancelled"),
                                        );
                                        show(&cancel_msg);
                                    }
                                }
                            }
                        }
                    }
                }

                if draining {
                    debug!("drain phase complete, exiting main loop");
                    done = true;
                }
                // Only treat the shell as idle after N consecutive timeouts
                // to avoid false positives from brief SSH network gaps.
                // For session commands (ssh, telnet), we add extra grace polls
                // to give the remote server time to display auth prompts before
                // we inject the probe.
                let idle_threshold = if session_command_just_sent {
                    IDLE_THRESHOLD + SESSION_CMD_IDLE_GRACE
                } else if followup_capturing {
                    if followup_prompt_seen {
                        IDLE_THRESHOLD + FOLLOWUP_PROMPT_IDLE
                    } else {
                        IDLE_THRESHOLD + FOLLOWUP_IDLE_GRACE
                    }
                } else {
                    IDLE_THRESHOLD
                };
                // Countdown triggered by Ctrl+Z/Ctrl+C during
                // followup_capturing.  Only decrements on idle polls so
                // that suspension/termination output from the remote
                // shell is captured before the followup fires.
                if let Some(ref mut count) = followup_interrupt_countdown {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        followup_interrupt_countdown = None;
                        idle_poll_count = idle_threshold;
                    }
                }
                // Drop stale PS1 echo suppressor. If both echoes haven't
                // arrived within TIMEOUT, assume the SSH connection died
                // mid-handshake (or the remote shell never echoed) and
                // release the suppressor so it doesn't accidentally match
                // a future user-typed PS1=... line.
                if let Some(ref sup) = ps1_echo_suppressor {
                    let elapsed = sup.started.elapsed();
                    if elapsed >= Ps1EchoSuppressor::TIMEOUT {
                        debug!(
                            "ps1 echo suppressor timed out after {:?}, dropping",
                            elapsed
                        );
                        if !sup.pending.is_empty() {
                            ps1_pending_flush.extend_from_slice(&sup.pending);
                        }
                        ps1_echo_suppressor = None;
                    }
                }

                if idle_poll_count >= idle_threshold {
                    // No data for N * 50ms — the remote shell is idle and
                    // sitting at a prompt waiting for input.
                    // Mark probe as injected so we don't auto-inject on idle.
                    // Actual probing is deferred to the first AI invocation
                    // (`;` trigger) to avoid false positives from Ctrl+R
                    // history search and other SSH mentions in output.
                    if !probe_injected && !at_password_prompt && !in_search_mode {
                        debug!(
                            "idle: marking probe_injected=true, \
                             was nested_probe_pending={}, host={:?}, stack={:?}",
                            nested_probe_pending, remote_host_for_probe, nested_host_stack,
                        );
                        probe_injected = true;
                        nested_probe_pending = false;
                    }
                    // If we were capturing output for followup analysis, the
                    // command has finished — invoke the followup callback.
                    if followup_capturing {
                        // Detect stuck state: shell is showing a PS2 continuation
                        // prompt (e.g. unclosed heredoc/quote). Send Ctrl+C to
                        // cancel and skip the followup.
                        if looks_like_continuation_prompt(&followup_captured) {
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\x03".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            followup_capturing = false;
                            // Fire-and-forget so the LLM thread gets output
                            // instead of "Channel closed".
                            if let Some(followup) = pending_followup.take() {
                                std::thread::spawn(move || {
                                    let _ = followup("", None);
                                });
                            }
                            followup_captured.clear();
                            // Clean up offloader
                            if let Some(offloader) = followup_offloader.take() {
                                offloader.cancel();
                            }
                        } else {
                            followup_capturing = false;
                            // Finalize offloader and get path
                            let offload_path = if let Some(offloader) = followup_offloader.take() {
                                let result = offloader.finalize(&[], &[], 0);
                                if result.stdout.status == "offloaded" {
                                    result.stdout.path.clone()
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            if let Some(followup) = pending_followup.take() {
                                let output =
                                    String::from_utf8_lossy(&followup_captured).to_string();
                                let clean = strip_ansi_and_prompt(&output);
                                let next_response = followup(&clean, offload_path.as_deref());
                                if let Some(resp) = next_response {
                                    pending_response = Some(resp);
                                } else {
                                    unsafe {
                                        libc::write(
                                            self.master_fd,
                                            b"\r".as_ptr() as *const libc::c_void,
                                            1,
                                        );
                                    }
                                    skip_leading_newline = true;
                                }
                            }
                            followup_captured.clear();
                        }
                    }
                }
                // Process pending AI response (multi-round chaining).
                // Must happen here — the `continue` below skips the
                // normal pending_response block after the master-fd read.
                // Guard: if a previous Ctrl+C hard-aborted, skip all further
                // pending responses and return to the shell prompt.
                if ai_cancelled {
                    // Consume and discard any pending response; fire-and-forget
                    // the followup so the LLM thread receives output instead of
                    // "Channel closed" when the output_sender is dropped.
                    if let Some(response) = pending_response.take() {
                        if let Some(followup) = response.followup {
                            std::thread::spawn(move || {
                                let _ = followup("Command cancelled by user", None);
                            });
                        }
                        skip_leading_newline = true;
                        unsafe {
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                    }
                    continue;
                }
                if let Some(response) = pending_response.take() {
                    // Handle ask_user first — it may produce a new pending_response
                    if let Some((request, channel)) = response.ask_user {
                        let aborted = handle_ask_user_interaction(
                            request,
                            channel,
                            stdin_fd,
                            self.master_fd,
                            &mut pending_response,
                            &interceptor,
                        );
                        if aborted {
                            ai_cancelled = true;
                            pending_response = None;
                            skip_leading_newline = true;
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\r".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                        }
                        // If ask_user produced a final response, fall through
                        // to process it on the next iteration.
                        continue;
                    }
                    if let Some(ref cmd) = response.command {
                        let tool_text = aish_i18n::t_with_args("shell.session.tool_bash", &{
                            let mut m = std::collections::HashMap::new();
                            m.insert("command".to_string(), cmd.clone());
                            m
                        });
                        let tool_line = format!("\x1b[36m{}\x1b[0m\r\n", tool_text);
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                tool_line.as_ptr() as *const libc::c_void,
                                tool_line.len(),
                            );
                        }
                        let confirm = format!(
                            "\x1b[33m{}\x1b[0m ",
                            aish_i18n::t("shell.session.confirm_execute")
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                confirm.as_ptr() as *const libc::c_void,
                                confirm.len(),
                            );
                        }
                        let mut ans = [0u8; 1];
                        let approved = match unsafe {
                            libc::read(stdin_fd, ans.as_mut_ptr() as *mut libc::c_void, 1)
                        } {
                            1 => {
                                // Ctrl+C: hard abort — skip followup entirely
                                if ans[0] == 0x03 {
                                    show("^C\r\n");
                                    drain_stdin_trailing(stdin_fd, master_fd);
                                    ai_cancelled = true;
                                    false
                                } else {
                                    let echo = if ans[0] == b'y'
                                        || ans[0] == b'Y'
                                        || ans[0] == b'\r'
                                        || ans[0] == b'\n'
                                    {
                                        b"y\r\n"
                                    } else {
                                        b"n\r\n"
                                    };
                                    show(std::str::from_utf8(echo).unwrap_or("y\r\n"));
                                    // Drain trailing newline/CR so it doesn't leak
                                    // into the next read cycle.
                                    drain_stdin_trailing(stdin_fd, master_fd);
                                    ans[0] == b'y'
                                        || ans[0] == b'Y'
                                        || ans[0] == b'\r'
                                        || ans[0] == b'\n'
                                }
                            }
                            _ => false,
                        };
                        if approved {
                            // InputGuard: AI-generated commands must clear
                            // the same safety gate as user-typed ones,
                            // even after the generic Y/n approval. Screen
                            // the placeholder form (still contains <SECRET>
                            // tokens) so any InputGuard display message
                            // never leaks real secret values.
                            if !screen_injected_command(&interceptor, stdin_fd, self.master_fd, cmd)
                            {
                                if let Some(followup) = response.followup {
                                    std::thread::spawn(move || {
                                        let _ = followup("Command cancelled by user", None);
                                    });
                                }
                                ai_cancelled = true;
                                skip_leading_newline = true;
                                unsafe {
                                    libc::write(
                                        self.master_fd,
                                        b"\r".as_ptr() as *const libc::c_void,
                                        1,
                                    );
                                }
                                continue;
                            }
                            // Restore secret placeholders in the AI-generated command
                            let mut cmd_restored = cmd.clone();
                            if let Some(ref vault) = secret_vault {
                                let vault_guard = vault.lock().unwrap();
                                let (restored, count) = vault_guard.restore(cmd);
                                if count > 0 {
                                    let mut rargs = std::collections::HashMap::new();
                                    rargs.insert("count".to_string(), count.to_string());
                                    let msg = aish_i18n::t_with_args(
                                        "shell.security.secret.restored",
                                        &rargs,
                                    );
                                    let info = format!("\x1b[2m{}\x1b[0m\r\n", msg);
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            info.as_ptr() as *const libc::c_void,
                                            info.len(),
                                        );
                                    }
                                    cmd_restored = restored;
                                }
                            }
                            // Show "Running..." feedback
                            let running_msg = format!(
                                "\x1b[90m{}\x1b[0m\r\n",
                                aish_i18n::t("shell.session.running")
                            );
                            show(&running_msg);
                            let safe_cmd = close_unclosed_heredoc(&cmd_restored);
                            skip_echo_cmd = Some(safe_cmd.clone());
                            let mut inject = safe_cmd.as_bytes().to_vec();
                            inject.push(b'\r');
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    inject.as_ptr() as *const libc::c_void,
                                    inject.len(),
                                );
                            }
                            if response.followup.is_some() {
                                followup_captured.clear();
                                followup_capturing = true;
                                followup_prompt_seen = false;
                                pending_followup = response.followup;
                                // Create offloader for followup output
                                let session_uuid = uuid::Uuid::new_v4().to_string();
                                let base_dir =
                                    std::env::temp_dir().to_str().unwrap_or("/tmp").to_string();
                                followup_offloader = Some(crate::PtyOutputOffload::new(
                                    &safe_cmd,
                                    &session_uuid,
                                    "",
                                    1024,
                                    &base_dir,
                                ));
                                // Reset idle counter so the new command
                                // gets a fresh grace period instead of
                                // inheriting the stale count from the
                                // previous followup round.
                                idle_poll_count = 0;
                            }
                        } else if ai_cancelled {
                            // Hard abort: print cancel and return to shell.
                            // Fire-and-forget the followup so the LLM thread
                            // receives output instead of "Channel closed".
                            let cancel_msg = format!(
                                "\x1b[33m{}\x1b[0m\r\n",
                                aish_i18n::t("shell.command_cancelled")
                            );
                            show(&cancel_msg);
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\r".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            if let Some(followup) = response.followup {
                                std::thread::spawn(move || {
                                    let _ = followup("Command cancelled by user", None);
                                });
                            }
                            skip_leading_newline = true;
                        } else {
                            let cancel_msg = format!(
                                "\x1b[33m{}\x1b[0m\r\n",
                                aish_i18n::t("shell.command_cancelled")
                            );
                            show(&cancel_msg);
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\r".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            // User rejected the command — terminate the
                            // entire tool chain, not just this command.
                            // Fire-and-forget the followup so the LLM
                            // thread receives output instead of "Channel
                            // closed" when the sender is dropped.
                            if let Some(followup) = response.followup {
                                std::thread::spawn(move || {
                                    let _ = followup("Command rejected by user. Stop calling bash tools and adjust your approach.", None);
                                });
                            }
                            skip_leading_newline = true;
                        }
                    } else {
                        if !response.display_text.is_empty() {
                            let mut msg = response.display_text.clone();
                            msg.push_str("\r\n");
                            show(&msg);
                        }
                        unsafe {
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                    }
                }
                continue;
            }

            // Write buffered data.
            if unsafe { libc::FD_ISSET(self.master_fd, &write_fds) } && !write_buf.is_empty() {
                match unsafe {
                    libc::write(
                        self.master_fd,
                        write_buf.as_ptr() as *const libc::c_void,
                        write_buf.len(),
                    )
                } {
                    n if n > 0 => {
                        write_buf.drain(..n as usize);
                    }
                    _ => {
                        write_buf.clear();
                    }
                }
            }

            // Read stdin -> interceptor or master (only during normal phase).
            if !draining && unsafe { libc::FD_ISSET(stdin_fd, &read_fds) } {
                let mut tmp = [0u8; 1024];
                match unsafe {
                    libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                } {
                    n if n > 0 => {
                        let data = &tmp[..n as usize];
                        idle_poll_count = 0;

                        // Non-session: original passthrough behavior
                        if !is_session {
                            if data.contains(&0x03) {
                                let _ = kill_pg(self.child_pid, Signal::SIGINT);
                            }
                            write_buf.extend_from_slice(data);
                            continue;
                        }

                        // Session command: route through interceptor
                        for &byte in data {
                            match interceptor.feed_stdin(byte) {
                                crate::StdinAction::Blocked(reason) => {
                                    // InputGuard blocked the line.
                                    // Cancel the echoed line on the PTY
                                    // (Ctrl+C) by sending immediately —
                                    // don't queue, otherwise the byte sits
                                    // in write_buf until after display().
                                    // Also drop any forward bytes from the
                                    // same intercepted line so they don't
                                    // leak through after the cancel.
                                    if interceptor.take_cancel_pty_line() {
                                        write_buf.clear();
                                        let cancel = [0x03u8];
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                cancel.as_ptr() as *const libc::c_void,
                                                cancel.len(),
                                            );
                                        }
                                    }
                                    // Keep stdin_shadow in sync with the
                                    // interceptor's line_shadow (both just
                                    // got cleared on Block).
                                    stdin_shadow.clear();
                                    // Clear the current line first to erase
                                    // Tab/completion readline redraw artifacts
                                    // that the remote shell leaves on the
                                    // current line. Without this, the redrawn
                                    // `[root@... ~]# <cmd>` visually clashes
                                    // with the BLOCKED message below it.
                                    show(&format!("\r\x1b[2K\r\n\x1b[31m{}\x1b[0m\r\n", reason));
                                    skip_leading_newline = true;
                                    // N3: discard any remaining bytes in this
                                    // stdin batch. Typeahead like
                                    // `<destructive cmd>\n<next cmd>` would
                                    // otherwise continue through feed_stdin
                                    // after the block, leaking the next
                                    // command past the just-cancelled line.
                                    break;
                                }
                                crate::StdinAction::NeedConfirm { reason, line } => {
                                    // InputGuard wants user confirmation.
                                    // Cancel the echoed line on PTY first,
                                    // flushing Ctrl+C immediately so the
                                    // remote readline is reset before we
                                    // prompt / re-inject.
                                    if interceptor.take_cancel_pty_line() {
                                        write_buf.clear();
                                        let cancel = [0x03u8];
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                cancel.as_ptr() as *const libc::c_void,
                                                cancel.len(),
                                            );
                                        }
                                    }
                                    let confirmed = read_confirm_raw(stdin_fd, master_fd, &reason);
                                    if confirmed {
                                        // Mirror Forward-branch nested
                                        // session detection so a confirmed
                                        // ssh/telnet keeps probe targeting
                                        // up to date.
                                        let line_str = String::from_utf8_lossy(&line);
                                        if let Some(info) = parse_ssh_command(line_str.trim()) {
                                            if is_session_command(line_str.trim()) {
                                                const MAX_NESTING: usize = 8;
                                                if nested_host_stack.len() < MAX_NESTING {
                                                    if let Some(cur_info) =
                                                        remote_info_for_probe.take()
                                                    {
                                                        nested_host_stack.push(cur_info);
                                                        // Keep ps1_marker_done_stack in sync with
                                                        // nested_host_stack so disconnect can restore
                                                        // the outer marker state. Without this push
                                                        // the pop on disconnect returns None and the
                                                        // outer session's PS1 gets re-injected,
                                                        // stacking duplicate [ssh:...] markers.
                                                        ps1_marker_done_stack
                                                            .push(ps1_marker_done_for.take());
                                                    }
                                                    // Keep legacy host string and structured info
                                                    // in sync — both come from the same parse and
                                                    // are consumed by the injection block below.
                                                    let host = info.dest_raw.clone();
                                                    remote_info_for_probe = Some(info);
                                                    remote_host_for_probe = Some(host.clone());
                                                    if let Some(ref sh) = shared_host {
                                                        *sh.lock().unwrap() = Some(host);
                                                    }
                                                    probe_injected = false;
                                                    probe_active = false;
                                                    nested_probe_pending = true;
                                                    session_command_just_sent = true;
                                                    probe_sections.clear();
                                                    probe_current_section.clear();
                                                    probe_start = None;
                                                    output_ssh_scan.clear();
                                                    nested_confirm_buf.clear();
                                                    at_password_prompt = false;
                                                    in_search_mode = false;
                                                }
                                            }
                                        }
                                        // Re-inject the line into PTY.
                                        // If the confirmed line is an ssh
                                        // invocation, inject ConnectTimeout
                                        // so an unreachable target fails in
                                        // seconds rather than the kernel's
                                        // ~127s SYN retry window (during which
                                        // SIGINT can't abort the in-kernel
                                        // `connect()`). `inject_*` is a no-op
                                        // for non-ssh lines and when the user
                                        // already specified ConnectTimeout.
                                        let injected_line =
                                            if let Ok(s) = std::str::from_utf8(&line) {
                                                inject_ssh_connect_timeout(
                                                    s,
                                                    DEFAULT_SSH_CONNECT_TIMEOUT,
                                                )
                                                .into_bytes()
                                            } else {
                                                line.clone()
                                            };
                                        write_buf.extend_from_slice(&injected_line);
                                        write_buf.push(b'\r');
                                    } else {
                                        show("\r\n\x1b[33mCancelled\x1b[0m\r\n");
                                    }
                                    // Declined or confirmed, the shadow
                                    // line is fully consumed either way.
                                    stdin_shadow.clear();
                                    skip_leading_newline = true;
                                    // N3: discard remaining bytes in this
                                    // stdin batch — same rationale as the
                                    // Blocked branch above. Typeahead after
                                    // a confirmation prompt must not leak
                                    // through unscreened. NOTE: confirmed
                                    // commands also discard typeahead, so
                                    // if the user typed
                                    // `<destructive>\n<safe_next>\n` and
                                    // then approves the destructive one,
                                    // `<safe_next>` is dropped — they must
                                    // re-enter it after the confirmed
                                    // command finishes. This is the
                                    // conservative trade-off: silently
                                    // running queued commands right after a
                                    // confirmation would be surprising
                                    // (and easy to miss).
                                    break;
                                }
                                crate::StdinAction::Forward => {
                                    if followup_capturing && byte == 0x03 {
                                        // Hard abort: Ctrl+C during
                                        // followup capturing.  Cancel
                                        // the followup and return to
                                        // shell immediately — do NOT
                                        // send the partial output back
                                        // to the LLM (which causes it
                                        // to loop with retries).
                                        ai_cancelled = true;
                                        followup_capturing = false;
                                        followup_captured.clear();
                                        if let Some(followup) = pending_followup.take() {
                                            std::thread::spawn(move || {
                                                let _ = followup("Command cancelled by user", None);
                                            });
                                        }
                                        // Forward Ctrl+C to remote PTY
                                        write_buf.push(byte);
                                        let cancel_msg = format!(
                                            "\r\n\x1b[33m{}\x1b[0m\r\n",
                                            aish_i18n::t("shell.command_cancelled"),
                                        );
                                        show(&cancel_msg);
                                        skip_leading_newline = true;
                                    } else if followup_capturing && byte == 0x1b {
                                        // ESC during followup capturing —
                                        // check for standalone ESC vs
                                        // arrow/function key sequence.
                                        let mut ffds: libc::fd_set = unsafe { std::mem::zeroed() };
                                        unsafe {
                                            libc::FD_ZERO(&mut ffds);
                                            libc::FD_SET(stdin_fd, &mut ffds);
                                        }
                                        let mut ftv = libc::timeval {
                                            tv_sec: 0,
                                            tv_usec: 50_000,
                                        };
                                        let fsel = unsafe {
                                            libc::select(
                                                stdin_fd + 1,
                                                &mut ffds,
                                                std::ptr::null_mut(),
                                                std::ptr::null_mut(),
                                                &mut ftv,
                                            )
                                        };
                                        if fsel == 0 {
                                            // Standalone ESC — hard abort
                                            ai_cancelled = true;
                                            followup_capturing = false;
                                            followup_captured.clear();
                                            if let Some(followup) = pending_followup.take() {
                                                std::thread::spawn(move || {
                                                    let _ =
                                                        followup("Command cancelled by user", None);
                                                });
                                            }
                                            if let Some(offloader) = followup_offloader.take() {
                                                offloader.cancel();
                                            }
                                            let cancel_msg = format!(
                                                "\r\n\x1b[33m{}\x1b[0m\r\n",
                                                aish_i18n::t("shell.command_cancelled"),
                                            );
                                            show(&cancel_msg);
                                            skip_leading_newline = true;
                                        } else {
                                            // Follow-up bytes exist (arrow/function
                                            // key) — consume and discard them.
                                            let mut discard = [0u8; 16];
                                            unsafe {
                                                libc::read(
                                                    stdin_fd,
                                                    discard.as_mut_ptr() as *mut libc::c_void,
                                                    discard.len(),
                                                );
                                            }
                                        }
                                    } else if followup_capturing && byte == 0x1A {
                                        // Ctrl+Z: keep countdown behavior
                                        followup_interrupt_countdown =
                                            Some(FOLLOWUP_INTERRUPT_GRACE);
                                        write_buf.push(byte);
                                    } else {
                                        write_buf.push(byte);
                                        // Track stdin for nested session
                                        // command detection (ssh, telnet).
                                        // This detects commands typed
                                        // character-by-character reliably,
                                        // unlike the PTY-output scanner
                                        // which can miss readline echoes.
                                        match byte {
                                            b'\r' | b'\n' => {
                                                let line = String::from_utf8_lossy(&stdin_shadow);
                                                if let Some(info) = parse_ssh_command(line.trim()) {
                                                    if is_session_command(line.trim()) {
                                                        let host = info.dest_raw.clone();
                                                        debug!(
                                                            "stdin: nested session \
                                                             detected: {:?} -> {}",
                                                            remote_host_for_probe, host,
                                                        );
                                                        const MAX_NESTING: usize = 8;
                                                        if nested_host_stack.len() < MAX_NESTING {
                                                            if let Some(cur_info) =
                                                                remote_info_for_probe.take()
                                                            {
                                                                nested_host_stack.push(cur_info);
                                                                ps1_marker_done_stack.push(
                                                                    ps1_marker_done_for.take(),
                                                                );
                                                            }
                                                            // Mirror the line-level branch:
                                                            // update both the legacy host
                                                            // string and the structured info
                                                            // so the injection block sees
                                                            // the new SSH destination.
                                                            remote_info_for_probe = Some(info);
                                                            remote_host_for_probe =
                                                                Some(host.clone());
                                                            if let Some(ref sh) = shared_host {
                                                                *sh.lock().unwrap() = Some(host);
                                                            }
                                                            probe_injected = false;
                                                            probe_active = false;
                                                            nested_probe_pending = true;
                                                            session_command_just_sent = true;
                                                            probe_sections.clear();
                                                            probe_current_section.clear();
                                                            probe_start = None;
                                                            output_ssh_scan.clear();
                                                            at_password_prompt = false;
                                                            in_search_mode = false;
                                                        }
                                                    }
                                                }
                                                stdin_shadow.clear();
                                            }
                                            0x03 | 0x15 => {
                                                stdin_shadow.clear();
                                            }
                                            0x7F | 0x08 => {
                                                crate::pop_last_utf8_char(&mut stdin_shadow);
                                            }
                                            0x1B => {
                                                stdin_shadow.clear();
                                            }
                                            0x00..=0x1F => {}
                                            _ => {
                                                stdin_shadow.push(byte);
                                            }
                                        }
                                    }
                                }
                                crate::StdinAction::EchoLocally => unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        &byte as *const u8 as *const libc::c_void,
                                        1,
                                    );
                                },
                                crate::StdinAction::TriggerAi(mut question) => {
                                    // Reset hard-cancel flag for a fresh AI session
                                    ai_cancelled = false;
                                    // Clear stdin shadow — the AI prefix
                                    // text is not a session command.
                                    stdin_shadow.clear();
                                    // When triggered from line-level detection, the
                                    // PTY has already echoed the input line.  Send
                                    // Ctrl+C to cancel it on the remote side.
                                    if interceptor.take_cancel_pty_line() {
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                b"\x03".as_ptr() as *const libc::c_void,
                                                1,
                                            );
                                        }
                                        // Drain PTY output from Ctrl+C (^C + new prompt).
                                        // Must consume it NOW before calling the blocking
                                        // AI callback, otherwise it appears after the AI
                                        // response and confirmation prompt.
                                        let mut drain_buf = [0u8; 4096];
                                        loop {
                                            let mut rfds: libc::fd_set =
                                                unsafe { std::mem::zeroed() };
                                            unsafe {
                                                libc::FD_ZERO(&mut rfds);
                                                libc::FD_SET(self.master_fd, &mut rfds);
                                            }
                                            let mut tv = libc::timeval {
                                                tv_sec: 0,
                                                tv_usec: 100_000, // 100ms
                                            };
                                            let sel = unsafe {
                                                libc::select(
                                                    self.master_fd + 1,
                                                    &mut rfds,
                                                    std::ptr::null_mut(),
                                                    std::ptr::null_mut(),
                                                    &mut tv,
                                                )
                                            };
                                            if sel > 0
                                                && unsafe { libc::FD_ISSET(self.master_fd, &rfds) }
                                            {
                                                let n = unsafe {
                                                    libc::read(
                                                        self.master_fd,
                                                        drain_buf.as_mut_ptr() as *mut libc::c_void,
                                                        drain_buf.len(),
                                                    )
                                                };
                                                if n <= 0 {
                                                    break;
                                                }
                                                let data = &drain_buf[..n as usize];
                                                interceptor.feed_pty_output(data);
                                                continue;
                                            }
                                            break;
                                        }
                                    }

                                    // Move to a new line (preserve user's input line)
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            b"\r\n".as_ptr() as *const libc::c_void,
                                            2,
                                        );
                                    }

                                    // Security gate: secret detection in AI input
                                    if let Some(ref checker) = secret_check {
                                        if let Some(result) = checker(&question) {
                                            let choice = show_secret_dialog(
                                                &result.warning,
                                                libc::STDIN_FILENO,
                                            );
                                            match choice {
                                                SshSecretChoice::Abort => {
                                                    let abort_msg = format!(
                                                        "\x1b[33m{}\x1b[0m\r\n",
                                                        aish_i18n::t(
                                                            "shell.security.secret.aborted"
                                                        )
                                                    );
                                                    unsafe {
                                                        libc::write(
                                                            libc::STDOUT_FILENO,
                                                            abort_msg.as_ptr()
                                                                as *const libc::c_void,
                                                            abort_msg.len(),
                                                        );
                                                        libc::write(
                                                            self.master_fd,
                                                            b"\r".as_ptr() as *const libc::c_void,
                                                            1,
                                                        );
                                                    }
                                                    interceptor.finish_ai();
                                                    continue;
                                                }
                                                SshSecretChoice::Redact => {
                                                    if let Some(ref vault) = secret_vault {
                                                        let mut vault_guard = vault.lock().unwrap();
                                                        let redacted = vault_guard.redact(
                                                            &result.detected_secrets,
                                                            &question,
                                                        );
                                                        let count = result.detected_secrets.len();
                                                        let mut rargs =
                                                            std::collections::HashMap::new();
                                                        rargs.insert(
                                                            "count".to_string(),
                                                            count.to_string(),
                                                        );
                                                        let msg = aish_i18n::t_with_args(
                                                            "shell.security.secret.redacted",
                                                            &rargs,
                                                        );
                                                        let info =
                                                            format!("\x1b[33m{}\x1b[0m\r\n", msg);
                                                        unsafe {
                                                            libc::write(
                                                                libc::STDOUT_FILENO,
                                                                info.as_ptr()
                                                                    as *const libc::c_void,
                                                                info.len(),
                                                            );
                                                        }
                                                        // Replace question with redacted version
                                                        question = redacted;
                                                    } else {
                                                        // Cannot honor "Redact" safely without vault support.
                                                        interceptor.finish_ai();
                                                        continue;
                                                    }
                                                }
                                                SshSecretChoice::Allow => {
                                                    // Proceed with original question unchanged
                                                }
                                            }
                                        }
                                    }

                                    // Check for dossier commands before invoking AI
                                    let question_trimmed = question.trim().to_string();
                                    let dossier_result = handle_dossier_command(
                                        &question_trimmed,
                                        remote_host_for_probe.as_deref(),
                                    );
                                    let dossier_was_handled = dossier_result.is_some();
                                    let resp = if let Some(response) = dossier_result {
                                        // Dossier command handled, display result directly
                                        if !response.is_empty() {
                                            let msg = format!("\x1b[36m{}\x1b[0m\r\n", response);
                                            unsafe {
                                                libc::write(
                                                    libc::STDOUT_FILENO,
                                                    msg.as_ptr() as *const libc::c_void,
                                                    msg.len(),
                                                );
                                            }
                                        }
                                        None
                                    } else {
                                        // Not a dossier command — check if
                                        // we need to probe the host first.
                                        debug!(
                                            "TriggerAi: remote_host_for_probe={:?}, \
                                             probe_injected={}, probe_active={}, \
                                             nested_probe_pending={}, stack={:?}",
                                            remote_host_for_probe,
                                            probe_injected,
                                            probe_active,
                                            nested_probe_pending,
                                            nested_host_stack,
                                        );
                                        let needs_probe =
                                            remote_host_for_probe.as_ref().is_some_and(
                                                |host_key| match aish_hosts::load_profile(host_key)
                                                {
                                                    None => true,
                                                    Some(p) => p.probe_is_stale(),
                                                },
                                            );
                                        debug!(
                                            "TriggerAi: needs_probe={}, host={:?}",
                                            needs_probe, remote_host_for_probe,
                                        );
                                        if needs_probe && remote_host_for_probe.is_some() {
                                            // Defer AI: inject probe now, call
                                            // AI after probe completes.
                                            debug!("TriggerAi: injecting on-demand probe");
                                            let probe_cmd =
                                                format!("{}\n", aish_hosts::probe_command(),);
                                            let status = format!(
                                                "\r\x1b[90m{}\x1b[0m\r\n",
                                                aish_i18n::t("shell.session.probing"),
                                            );
                                            show(&status);
                                            unsafe {
                                                libc::write(
                                                    self.master_fd,
                                                    probe_cmd.as_ptr() as *const libc::c_void,
                                                    probe_cmd.len(),
                                                );
                                            }
                                            probe_active = true;
                                            probe_start = Some(std::time::Instant::now());
                                            probe_sections.clear();
                                            probe_current_section.clear();
                                            pending_ai_question = Some(question);
                                            probe_for_ai = true;
                                            // Don't call finish_ai() yet —
                                            // keep interceptor in AiProcessing
                                            // so probe output is suppressed.
                                            skip_leading_newline = true;
                                            continue;
                                        } else {
                                            // No probe needed, call AI now.
                                            let ec = self.command_state.last_exit_code();
                                            interceptor.call_ai(question, ec, secret_vault.as_ref())
                                        }
                                    };
                                    interceptor.finish_ai();
                                    skip_leading_newline = true;
                                    if let Some(response) = resp {
                                        pending_response = Some(response);
                                    } else if !dossier_was_handled {
                                        // AI returned None (cancelled or error)
                                        let cancel_msg = format!(
                                            "\x1b[33m{}\x1b[0m\r\n",
                                            aish_i18n::t("shell.session.ai_cancelled")
                                        );
                                        show(&cancel_msg);
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                b"\r".as_ptr() as *const libc::c_void,
                                                1,
                                            );
                                        }
                                    } else {
                                        // Dossier command handled — just refresh the prompt
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                b"\r".as_ptr() as *const libc::c_void,
                                                1,
                                            );
                                        }
                                    }
                                }
                                crate::StdinAction::TriggerStatus => {
                                    ai_cancelled = false;
                                    stdin_shadow.clear();

                                    // Cancel the echoed /status line on the remote side
                                    if interceptor.take_cancel_pty_line() {
                                        unsafe {
                                            libc::write(
                                                self.master_fd,
                                                b"\x03".as_ptr() as *const libc::c_void,
                                                1,
                                            );
                                        }
                                        let mut drain_buf = [0u8; 4096];
                                        loop {
                                            let mut rfds: libc::fd_set =
                                                unsafe { std::mem::zeroed() };
                                            unsafe {
                                                libc::FD_ZERO(&mut rfds);
                                                libc::FD_SET(self.master_fd, &mut rfds);
                                            }
                                            let mut tv = libc::timeval {
                                                tv_sec: 0,
                                                tv_usec: 100_000,
                                            };
                                            let sel = unsafe {
                                                libc::select(
                                                    self.master_fd + 1,
                                                    &mut rfds,
                                                    std::ptr::null_mut(),
                                                    std::ptr::null_mut(),
                                                    &mut tv,
                                                )
                                            };
                                            if sel > 0
                                                && unsafe { libc::FD_ISSET(self.master_fd, &rfds) }
                                            {
                                                let n = unsafe {
                                                    libc::read(
                                                        self.master_fd,
                                                        drain_buf.as_mut_ptr() as *mut libc::c_void,
                                                        drain_buf.len(),
                                                    )
                                                };
                                                if n <= 0 {
                                                    break;
                                                }
                                                interceptor
                                                    .feed_pty_output(&drain_buf[..n as usize]);
                                                continue;
                                            }
                                            break;
                                        }
                                    }

                                    // Newline after user's input
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            b"\r\n".as_ptr() as *const libc::c_void,
                                            2,
                                        );
                                    }

                                    // Execute remote status collection via callback
                                    if interceptor.has_status_callback() {
                                        let master_fd = self.master_fd;
                                        let mut exec_fn: Box<crate::RemoteExecFn> =
                                            Box::new(move |cmd: &str| {
                                                execute_remote_command(master_fd, cmd)
                                            });

                                        let rendered =
                                            interceptor.invoke_status_callback(&mut *exec_fn);

                                        let msg = format!("{}\r\n", rendered);
                                        unsafe {
                                            libc::write(
                                                libc::STDOUT_FILENO,
                                                msg.as_ptr() as *const libc::c_void,
                                                msg.len(),
                                            );
                                        }
                                    }

                                    interceptor.finish_ai();
                                    skip_leading_newline = true;
                                    unsafe {
                                        libc::write(
                                            self.master_fd,
                                            b"\r".as_ptr() as *const libc::c_void,
                                            1,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Read master -> stdout.
            if unsafe { libc::FD_ISSET(self.master_fd, &read_fds) } {
                let mut tmp = [0u8; 8192];
                match unsafe {
                    libc::read(
                        self.master_fd,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        idle_poll_count = 0;
                        let mut data = &tmp[..n as usize];
                        if skip_leading_newline {
                            // Only strip a bare leading CR-LF or LF that
                            // came from stale prompt rendering.  Do NOT
                            // discard actual command output.
                            if data.starts_with(b"\r\n") {
                                data = &data[2..];
                            } else if data.starts_with(b"\n") {
                                data = &data[1..];
                            }
                            skip_leading_newline = false;
                        }
                        // Strip the remote shell's echo of an injected command.
                        if let Some(ref echo_cmd) = skip_echo_cmd {
                            let pattern = format!("{}\r\n", echo_cmd).into_bytes();
                            if data.starts_with(&pattern) {
                                data = &data[pattern.len()..];
                            } else {
                                let pattern_cr = format!("{}\r", echo_cmd).into_bytes();
                                if data.starts_with(&pattern_cr) {
                                    data = &data[pattern_cr.len()..];
                                }
                            }
                            skip_echo_cmd = None;
                        }
                        // Drop stale PS1 echo suppressor on the read path so a
                        // long-running suppressor can't strip legitimate output
                        // containing `PS1=...` text the user types >10s after
                        // the injection. Idle-poll enforcement alone is not
                        // enough: under continuous PTY output the idle branch
                        // never runs.
                        if let Some(ref sup) = ps1_echo_suppressor {
                            if sup.started.elapsed() >= Ps1EchoSuppressor::TIMEOUT {
                                debug!(
                                    "ps1 echo suppressor timed out after {:?} (read path), dropping",
                                    sup.started.elapsed()
                                );
                                if !sup.pending.is_empty() {
                                    ps1_pending_flush.extend_from_slice(&sup.pending);
                                }
                                ps1_echo_suppressor = None;
                            }
                        }
                        // Strip PS1 marker echo into an owned buffer so the
                        // cleaned bytes flow through the rest of the pipeline
                        // (output_buf, followup_captured, ssh_scan, display)
                        // instead of leaking the raw `PS1=...` command into
                        // recorded output or LLM followup context. Prepend any
                        // bytes flushed from a suppressor that timed out
                        // mid-echo so they still reach the user.
                        let cleaned_data: Vec<u8> = if !ps1_pending_flush.is_empty() {
                            let mut v = std::mem::take(&mut ps1_pending_flush);
                            if let Some(ref mut sup) = ps1_echo_suppressor {
                                v.extend_from_slice(&strip_ps1_echo(data, sup));
                            } else {
                                v.extend_from_slice(data);
                            }
                            v
                        } else if let Some(ref mut sup) = ps1_echo_suppressor {
                            strip_ps1_echo(data, sup)
                        } else {
                            data.to_vec()
                        };
                        // Drop the suppressor once it has nothing left to strip.
                        if let Some(ref sup) = ps1_echo_suppressor {
                            if sup.remaining == 0 {
                                ps1_echo_suppressor = None;
                            }
                        }
                        let data: &[u8] = &cleaned_data;
                        if !data.is_empty() {
                            output_buf.extend_from_slice(data);
                            // Clear session command grace period once we receive output.
                            // The remote has responded (password prompt, shell prompt,
                            // or other output) - no need to delay probe injection further.
                            session_command_just_sent = false;
                            // Feed interceptor for line-start tracking and output buffering
                            if is_session {
                                interceptor.feed_pty_output(data);
                            }
                            // Detect password prompt regardless of probe state.
                            // SSH shows "password:" before authentication. The probe
                            // must not be injected during this phase.
                            let text = String::from_utf8_lossy(data);
                            if text.to_lowercase().contains("password") {
                                at_password_prompt = true;
                            }
                            // Capture probe output silently.
                            // The probe command uses printf to build markers so
                            // only 5 output markers appear (no echo contamination).
                            // Sections [1..] hold the real data.
                            let was_probe_active = probe_active;
                            if probe_active {
                                let text = String::from_utf8_lossy(data);
                                let marker = aish_hosts::probe_marker();
                                let mut remaining: &str = text.as_ref();
                                while let Some(pos) = remaining.find(marker) {
                                    // Capture text before this marker within the
                                    // current chunk (e.g. shell output between
                                    // two markers that arrive in the same chunk).
                                    probe_current_section.push_str(&remaining[..pos]);
                                    probe_sections.push(std::mem::take(&mut probe_current_section));
                                    remaining = &remaining[pos + marker.len()..];
                                }
                                probe_current_section.push_str(remaining);

                                // 5 markers = 6 sections (5 in probe_sections
                                // + 1 in probe_current_section after the last
                                // marker).  Data starts at [1].
                                if probe_sections.len() >= PROBE_MARKER_COUNT {
                                    probe_active = false;
                                    // Append the trailing section (text after
                                    // the last marker, e.g. locale data).
                                    probe_sections.push(std::mem::take(&mut probe_current_section));
                                    debug!(
                                        "probe: {} sections found (incl. trailing)",
                                        probe_sections.len(),
                                    );
                                    for (i, s) in probe_sections.iter().enumerate() {
                                        debug!(
                                            "probe section[{}]: {:?}",
                                            i,
                                            &s[..s.len().min(200)]
                                        );
                                    }
                                    let data_sections = &probe_sections[1..];
                                    let info = aish_hosts::parse_probe_output(data_sections);
                                    debug!(
                                        "probe parsed: os={}, kernel={}, shell={}, user={}, home={}, locale={}",
                                        info.os, info.kernel, info.shell, info.user, info.home, info.locale,
                                    );
                                    if let Some(ref host_key) = remote_host_for_probe {
                                        let mut profile =
                                            aish_hosts::get_or_create_profile(host_key);
                                        profile.system = info;
                                        profile.last_updated = chrono::Utc::now();
                                        if let Err(e) = aish_hosts::save_profile(&profile) {
                                            debug!("probe: failed to save profile: {}", e);
                                        } else {
                                            debug!("probe: profile saved for {}", host_key);
                                        }
                                    }
                                    probe_sections.clear();
                                    probe_current_section.clear();
                                    // Send CR to refresh the remote shell prompt,
                                    // since it was swallowed by the probe guard.
                                    unsafe {
                                        libc::write(
                                            self.master_fd,
                                            b"\r".as_ptr() as *const libc::c_void,
                                            1,
                                        );
                                    }
                                    skip_leading_newline = true;

                                    // If this probe was triggered on-demand
                                    // by the AI, invoke the AI callback now.
                                    if probe_for_ai {
                                        probe_for_ai = false;
                                        if let Some(question) = pending_ai_question.take() {
                                            let ec = self.command_state.last_exit_code();
                                            let resp = interceptor.call_ai(
                                                question,
                                                ec,
                                                secret_vault.as_ref(),
                                            );
                                            interceptor.finish_ai();
                                            if let Some(response) = resp {
                                                pending_response = Some(response);
                                            } else {
                                                let cancel_msg = format!(
                                                    "\x1b[33m{}\x1b[0m\r\n",
                                                    aish_i18n::t("shell.session.ai_cancelled"),
                                                );
                                                show(&cancel_msg);
                                            }
                                        }
                                    }
                                }
                            }
                            // Capture output for followup analysis
                            if followup_capturing {
                                followup_captured.extend_from_slice(data);
                                // Stream to followup offloader if active
                                if let Some(ref mut offloader) = followup_offloader {
                                    offloader
                                        .append_overflow(crate::types::StreamName::Stdout, data);
                                }
                                // Detect remote shell prompt to speed up
                                // followup completion.  After the command
                                // finishes the shell prints a prompt (e.g.
                                // "[root@host ~]# ").  Once seen we reduce
                                // the idle threshold from ~5 s to ~500 ms.
                                // Check the accumulated buffer tail (not just
                                // the current chunk) so prompt patterns split
                                // across SSH packets are still detected.
                                if !followup_prompt_seen && followup_captured.len() >= 2 {
                                    let tail = &followup_captured
                                        [followup_captured.len().saturating_sub(4)..];
                                    if tail.ends_with(b"# ")
                                        || tail.ends_with(b"$ ")
                                        || tail.ends_with(b"#\r")
                                        || tail.ends_with(b"$\r")
                                    {
                                        followup_prompt_seen = true;
                                    }
                                }
                            }
                            // Scan PTY output for nested SSH commands and
                            // connection-close messages.  This catches cases
                            // the keystroke buffer cannot (history, paste,
                            // reverse-i-search, etc.).
                            // Nested SSH detection: run when not currently in a probe
                            // (but NOT checking was_probe_active, so we detect immediately
                            // after a probe completes without waiting for another cycle).
                            if !probe_active && is_session && !nested_probe_pending {
                                let text = String::from_utf8_lossy(data);
                                output_ssh_scan.push_str(&text);
                                // Detect successful auth: shell prompt (# or $)
                                // means user has logged in, password phase is over.
                                if at_password_prompt
                                    && (text.contains("# ")
                                        || text.contains("$ ")
                                        || text.contains("#\r")
                                        || text.contains("$\r"))
                                {
                                    at_password_prompt = false;
                                }
                                // Detect history search mode: Ctrl+R shows "^R"
                                // and Ctrl+S shows "^S" in the PTY output.
                                // Don't inject probe during search - it would
                                // corrupt the search input. Clear on shell
                                // prompt (search cancelled/completed).
                                if text.contains("^R") || text.contains("^S") {
                                    in_search_mode = true;
                                }
                                if in_search_mode
                                    && (text.contains("# ")
                                        || text.contains("$ ")
                                        || text.contains("#\r")
                                        || text.contains("$\r"))
                                {
                                    in_search_mode = false;
                                }
                                // Trim to last complete line boundary to keep
                                // line-based scanning reliable.
                                if output_ssh_scan.len() > 2048 {
                                    let mut keep = output_ssh_scan.len() - 2048;
                                    // Align to a valid UTF-8 char boundary
                                    // to avoid panicking on multi-byte chars.
                                    while keep > 0 && !output_ssh_scan.is_char_boundary(keep) {
                                        keep -= 1;
                                    }
                                    let drain_end = output_ssh_scan[..keep]
                                        .rfind('\n')
                                        .map(|pos| pos + 1)
                                        .unwrap_or(keep);
                                    output_ssh_scan.drain(..drain_end);
                                }
                                if let Some(host) = scan_output_for_ssh_host(&output_ssh_scan) {
                                    debug!(
                                        "nested SSH detected in output: {} -> {}, scan_buf_len={}",
                                        remote_host_for_probe.as_deref().unwrap_or("?"),
                                        host,
                                        output_ssh_scan.len(),
                                    );
                                    const MAX_NESTING: usize = 8;
                                    if nested_host_stack.len() >= MAX_NESTING {
                                        debug!(
                                            "nested SSH: max nesting ({}) reached, ignoring",
                                            MAX_NESTING
                                        );
                                    } else {
                                        // Push the OUTER session's structured
                                        // info so a pop restores user/port/
                                        // jumps. If the outer info is missing
                                        // (e.g. scanner fired before any prior
                                        // parse populated it), fall back to a
                                        // synthesized minimal info so the stack
                                        // depth still matches ps1_marker_done_stack.
                                        let outer =
                                            remote_info_for_probe.take().unwrap_or_else(|| {
                                                SshCommandInfo {
                                                    user: None,
                                                    host: host.clone(),
                                                    jump_chain: Vec::new(),
                                                    dest_raw: host.clone(),
                                                }
                                            });
                                        nested_host_stack.push(outer);
                                        ps1_marker_done_stack.push(ps1_marker_done_for.take());
                                        // Output-scan path only knows the host
                                        // string for the NEW destination —
                                        // synthesize a minimal SshCommandInfo
                                        // so the injection block still has the
                                        // structured form it expects. The
                                        // outer info was already pushed above
                                        // with full fidelity.
                                        remote_info_for_probe = Some(SshCommandInfo {
                                            user: None,
                                            host: host.clone(),
                                            jump_chain: Vec::new(),
                                            dest_raw: host.clone(),
                                        });
                                        remote_host_for_probe = Some(host.clone());
                                        if let Some(ref sh) = shared_host {
                                            *sh.lock().unwrap() = Some(host.clone());
                                        }
                                        probe_injected = false;
                                        probe_active = false;
                                        nested_probe_pending = true;
                                        // Reset grace period for the new nested session
                                        session_command_just_sent = true;
                                        probe_sections.clear();
                                        probe_current_section.clear();
                                        probe_start = None;
                                        output_ssh_scan.clear();
                                        at_password_prompt = false;
                                        in_search_mode = false;
                                        // Do NOT inject the PS1 marker here.
                                        // Bash always shows its first prompt
                                        // BEFORE reading stdin, so the bytes
                                        // could only affect the second prompt
                                        // onwards — same as the late-injection
                                        // path below. Worse, writing here would
                                        // send the bytes across every SSH hop
                                        // in the chain (high loss probability
                                        // for nested SSH), and setting
                                        // `ps1_marker_done_for = Some(host)`
                                        // would permanently disable the
                                        // late-injection retry. Let the
                                        // late-injection block handle it: it
                                        // fires the moment the first remote
                                        // prompt arrives, and re-fires if the
                                        // first attempt's bytes are lost.
                                    }
                                } else if !output_ssh_scan.contains('\n') {
                                    // No complete line yet — skip scan
                                } else {
                                    let tail = if output_ssh_scan.len() > 500 {
                                        let start = output_ssh_scan.len() - 500;
                                        // align to char boundary
                                        let mut s = start;
                                        while s < output_ssh_scan.len()
                                            && !output_ssh_scan.is_char_boundary(s)
                                        {
                                            s += 1;
                                        }
                                        &output_ssh_scan[s..]
                                    } else {
                                        output_ssh_scan.as_str()
                                    };
                                    debug!(
                                        "SSH scan: no host found, buf_len={}, \
                                         current_host={:?}, stack_len={}, \
                                         tail={:?}",
                                        output_ssh_scan.len(),
                                        remote_host_for_probe,
                                        nested_host_stack.len(),
                                        tail,
                                    );
                                }
                                if let Some(closed_host) =
                                    scan_output_for_disconnect(&output_ssh_scan)
                                {
                                    let is_current = remote_host_for_probe
                                        .as_deref()
                                        .is_some_and(|h| h == closed_host);
                                    if is_current {
                                        if !nested_host_stack.is_empty() {
                                            debug!(
                                                "nested SSH disconnect (current: {:?}, closed: {})",
                                                remote_host_for_probe, closed_host
                                            );
                                            if let Some(prev_info) = nested_host_stack.pop() {
                                                // The stack stores the full outer
                                                // SshCommandInfo — restore it
                                                // directly so user/port/jumps
                                                // (and the root-user danger flag)
                                                // survive the round-trip. For the
                                                // resumed outer session the
                                                // ps1_marker_done_for is restored
                                                // below so no re-injection fires,
                                                // but keeping the structured info
                                                // correct matters if a later
                                                // nested-ssh re-pushes or if a
                                                // race clears ps1_marker_done_for.
                                                let prev_host = prev_info.dest_raw.clone();
                                                remote_info_for_probe = Some(prev_info);
                                                remote_host_for_probe = Some(prev_host.clone());
                                                // Restore the outer session's
                                                // ps1_marker_done_for so we do not
                                                // re-inject (the outer bash's PS1
                                                // still has the marker from the
                                                // initial injection).
                                                ps1_marker_done_for =
                                                    ps1_marker_done_stack.pop().flatten();
                                                if let Some(ref sh) = shared_host {
                                                    *sh.lock().unwrap() = Some(prev_host);
                                                }
                                                probe_injected = true;
                                                probe_active = false;
                                                nested_probe_pending = false;
                                                probe_sections.clear();
                                                probe_current_section.clear();
                                                ps1_echo_suppressor = None;
                                            }
                                        } else {
                                            // Non-nested disconnect: user ran
                                            // `aish` (no args) then typed ssh
                                            // manually, so nested_host_stack was
                                            // never pushed (only
                                            // remote_host_for_probe was set on
                                            // stdin-shadow detection). The ssh
                                            // child has exited; clear session
                                            // state so the next local-bash prompt
                                            // is treated as local (no PS1 marker
                                            // injection, no probe targeting a
                                            // dead host). Without this clear,
                                            // remote_host_for_probe stays
                                            // Some("host") and aish keeps waiting
                                            // for a remote PromptReady that will
                                            // never come — the UI hangs.
                                            debug!(
                                                "outer SSH disconnect (closed: {}), \
                                                 clearing session state and exiting loop",
                                                closed_host
                                            );
                                            remote_info_for_probe = None;
                                            remote_host_for_probe = None;
                                            ps1_marker_done_for = None;
                                            probe_injected = true;
                                            probe_active = false;
                                            nested_probe_pending = false;
                                            probe_sections.clear();
                                            probe_current_section.clear();
                                            ps1_echo_suppressor = None;
                                            if let Some(ref sh) = shared_host {
                                                *sh.lock().unwrap() = None;
                                            }
                                            // Exit the main loop. The ssh
                                            // session has ended — this
                                            // command is definitively done.
                                            // We must NOT rely on the next
                                            // PromptReady to drive
                                            // `command_state.handle_event`
                                            // → `draining = true`, because
                                            // `active_submission` may have
                                            // been consumed earlier in the
                                            // long ssh session (e.g. by a
                                            // spurious PromptReady from a
                                            // PROMPT_COMMAND replay), in
                                            // which case take_submission
                                            // returns None and the loop
                                            // hangs forever waiting for a
                                            // matching submission that
                                            // never comes.
                                            done = true;
                                        }
                                        output_ssh_scan.clear();
                                        nested_confirm_buf.clear();
                                    }
                                }
                            }
                            // Nested SSH failure detection — independent of
                            // the `!nested_probe_pending` gate above and also
                            // independent of `nested_probe_pending` itself.
                            // When a nested ssh was just confirmed (y-pressed)
                            // but the remote reports a connection failure
                            // (host unreachable, refused, timed out, auth
                            // failure, etc.), roll back the nested state so
                            // the PS1 injection block doesn't fire on the
                            // next outer-shell prompt. Without this, the
                            // state machine keeps the dead host as
                            // `remote_host_for_probe`, triggering
                            // `probe_remote_command` (5s blocking read) on the
                            // next prompt the outer bash emits after the
                            // failed ssh, and injecting the dead host's PS1
                            // marker in front of the outer host's marker
                            // (visible as `[ssh:dead] [ssh:outer] [prompt]`).
                            //
                            // Important: we cannot gate this on
                            // `nested_probe_pending` because the idle-detector
                            // at line ~1485 clears that flag after
                            // `SESSION_CMD_IDLE_GRACE` (~1.15s) regardless of
                            // whether ssh has finished — and ssh's
                            // ConnectTimeout can be 5s+ (DEFAULT_SSH_CONNECT_TIMEOUT).
                            // Run whenever we have a parent session on the
                            // stack; the recognized-failure patterns are
                            // specific enough that normal session output will
                            // not trip them.
                            if !probe_active && is_session && !nested_host_stack.is_empty() {
                                let text = String::from_utf8_lossy(data);
                                if scan_output_for_ssh_failure(&text).is_some() {
                                    debug!(
                                        "nested SSH failure detected (current: {:?}), \
                                         rolling back to outer session",
                                        remote_host_for_probe
                                    );
                                    if let Some(prev_info) = nested_host_stack.pop() {
                                        let prev_host = prev_info.dest_raw.clone();
                                        remote_info_for_probe = Some(prev_info);
                                        remote_host_for_probe = Some(prev_host.clone());
                                        ps1_marker_done_for = ps1_marker_done_stack.pop().flatten();
                                        if let Some(ref sh) = shared_host {
                                            *sh.lock().unwrap() = Some(prev_host);
                                        }
                                        probe_injected = true;
                                        probe_active = false;
                                        nested_probe_pending = false;
                                        probe_sections.clear();
                                        probe_current_section.clear();
                                        ps1_echo_suppressor = None;
                                        // Don't try to reuse output_ssh_scan
                                        // after rolling back: clear it so the
                                        // next outer-shell output doesn't
                                        // match a stale failure marker.
                                        output_ssh_scan.clear();
                                        nested_confirm_buf.clear();
                                    }
                                }
                            }
                            // When inside an SSH/telnet/... session, bake a
                            // `[ssh:host]` marker into the remote PS1 so it
                            // appears in front of every prompt on the same
                            // line. We do this by sending a `PS1=...` command
                            // to the remote shell once per host: bash's
                            // readline then renders the marker itself, so
                            // cursor-column tracking stays correct and
                            // arrow-key / Ctrl-R / Tab redraws don't garble
                            // the display.
                            //
                            // This block is intentionally OUTSIDE the
                            // `!probe_active && !was_probe_active` display
                            // gate below: the probe-completes chunk is exactly
                            // when the first remote prompt arrives, and we
                            // must inject on that chunk even though display
                            // of probe internals is suppressed.
                            //
                            // Conditions: only fire on a clean prompt-looking
                            // chunk (so we don't interrupt the user mid-type),
                            // skip during password entry and reverse-i-search,
                            // and only once per host so nested SSH gets its
                            // own marker.
                            //
                            // Nested-SSH success gate: when inside an outer
                            // SSH session, REQUIRE a positive success signal
                            // (password prompt or "Last login:" MOTD) before
                            // injecting the inner host's marker. Without this,
                            // a cancelled ssh (Ctrl+C before auth, which
                            // produces no error output) leaves the state
                            // machine pointing at the dead inner host, and the
                            // next outer-shell prompt triggers a stale
                            // injection — `[ssh:dead] [ssh:outer] [prompt]`.
                            //
                            // MOTD spans chunks, so we accumulate
                            // `nested_confirm_buf` across iterations but only
                            // scan it when this chunk looks like a prompt (the
                            // sole consumer) — avoids re-stripping ANSI over
                            // the whole 8KB buffer per chunk.
                            let nested_needs_success_signal = !nested_host_stack.is_empty();
                            if nested_needs_success_signal && !data.is_empty() {
                                nested_confirm_buf.push_str(&String::from_utf8_lossy(data));
                                // Cap memory: keep only the last 8KB.
                                if nested_confirm_buf.len() > 8 * 1024 {
                                    let keep = 8 * 1024;
                                    let start = nested_confirm_buf.len() - keep;
                                    let mut truncated = nested_confirm_buf.split_off(start);
                                    std::mem::swap(&mut truncated, &mut nested_confirm_buf);
                                }
                            }
                            if is_session
                                && !at_password_prompt
                                && !in_search_mode
                                && !interceptor.is_ai_processing()
                                && ps1_marker_done_for != remote_host_for_probe
                                && last_line_is_remote_prompt(data)
                                && (!nested_needs_success_signal
                                    || scan_output_for_ssh_success(&nested_confirm_buf))
                            {
                                // Both the structured info and the legacy host
                                // string are sourced from the same parse; gate
                                // the injection on having both (defensive —
                                // they should always be set together).
                                if let (Some(info), Some(host)) = (
                                    remote_info_for_probe.as_ref(),
                                    remote_host_for_probe.as_deref(),
                                ) {
                                    // Determine shell kind from prompt shape.
                                    // Bash default covers `$ `, `# `, and
                                    // bracketed prompts; zsh `% `; fish `> `.
                                    // Unknown (no last line) falls back to
                                    // Bash — safest default, also most common.
                                    let shell_kind = match stripped_last_line(data) {
                                        Some(s) if s.ends_with(b"% ") => ShellKind::Zsh,
                                        Some(s) if s.ends_with(b"> ") => ShellKind::Fish,
                                        Some(_) => ShellKind::Bash,
                                        None => ShellKind::Bash,
                                    };
                                    let is_bash = matches!(shell_kind, ShellKind::Bash);

                                    // Git hook only makes sense for bash; honour
                                    // the user's enable_remote_git_prompt toggle.
                                    let enable_git = enable_remote_git_prompt && is_bash;

                                    // Probe only when rich mode is on AND shell
                                    // is bash — non-bash shells can't host the
                                    // PROMPT_COMMAND hook so the segments would
                                    // never update anyway. The probe blocks the
                                    // PTY thread for up to 5s, but runs exactly
                                    // once per host on first prompt.
                                    let mut snapshot = if remote_rich_prompt && is_bash {
                                        let (probe_result, residual) =
                                            probe_remote_command(self.master_fd);
                                        // Re-inject only the TRAILING bytes that
                                        // arrived after the probe body (next
                                        // prompt, resize echoes, async output).
                                        // Bytes before the start marker — i.e.
                                        // the echoed probe command itself — are
                                        // intentionally discarded by
                                        // `compute_probe_residual` to avoid
                                        // leaking the probe literal to the UI.
                                        if !residual.is_empty() {
                                            debug!(
                                                residual_len = residual.len(),
                                                "re-injecting probe trailing bytes to UI"
                                            );
                                            write_stdout_all(&residual);
                                        }
                                        let s = probe_result.unwrap_or_else(|| {
                                            RemoteContextSnapshot::minimal(shell_kind.clone())
                                        });
                                        let mut s = s;
                                        s.shell_type = shell_kind.clone();
                                        s
                                    } else {
                                        let mut s =
                                            RemoteContextSnapshot::minimal(shell_kind.clone());
                                        s.shell_type = shell_kind.clone();
                                        s
                                    };

                                    // Apply config-driven segment visibility by
                                    // mutating the snapshot so both rich and
                                    // legacy paths share one build call.
                                    if !remote_show_container {
                                        snapshot.container = None;
                                    }
                                    if !remote_show_kube {
                                        snapshot.kube_context = None;
                                        snapshot.is_kube_prod = false;
                                    }
                                    let show_venv = remote_show_venv;

                                    let danger_static =
                                        info.danger_static(&compiled_danger_patterns);
                                    let danger = danger_static.max(snapshot.kube_danger());

                                    let cmd = build_ps1_marker_command(
                                        info,
                                        &snapshot,
                                        danger,
                                        enable_git,
                                        show_venv,
                                        remote_show_container,
                                        remote_show_kube,
                                    );
                                    debug!(
                                        "PS1 inject: host={}, enable_git={}, rich={}, shell={:?}, danger={:?}, cmd_len={}, nesting={}, done_for={:?}, last_line={:?}",
                                        host,
                                        enable_git,
                                        remote_rich_prompt,
                                        snapshot.shell_type,
                                        danger,
                                        cmd.len(),
                                        nested_host_stack.len(),
                                        ps1_marker_done_for,
                                        stripped_last_line(data).as_deref().unwrap_or(&[][..]),
                                    );
                                    let master_fd = self.master_fd;
                                    let written_all = write_all_with_retry(&cmd, |remaining| {
                                        let rc = unsafe {
                                            libc::write(
                                                master_fd,
                                                remaining.as_ptr() as *const libc::c_void,
                                                remaining.len(),
                                            )
                                        };
                                        if rc < 0 {
                                            Err(std::io::Error::last_os_error()
                                                .raw_os_error()
                                                .unwrap_or(libc::EIO))
                                        } else {
                                            Ok(rc as usize)
                                        }
                                    });
                                    // Only arm the suppressor + mark done when
                                    // the full command made it into the PTY.
                                    // On partial write / EINTR-exhaustion / EPIPE
                                    // we leave ps1_marker_done_for untouched so
                                    // the next prompt-looking chunk retries the
                                    // injection instead of silently dropping it.
                                    if written_all {
                                        debug!(
                                            "PS1 inject OK: host={}, armed suppressor, remaining=2",
                                            host
                                        );
                                        ps1_echo_suppressor = Some(build_ps1_echo_suppressor(
                                            info,
                                            &snapshot,
                                            danger,
                                            enable_git,
                                            show_venv,
                                            remote_show_container,
                                            remote_show_kube,
                                        ));
                                        ps1_marker_done_for = Some(host.to_string());
                                        // Successful injection: clear the
                                        // success-signal accumulator so the
                                        // next nested ssh starts fresh.
                                        nested_confirm_buf.clear();
                                    } else {
                                        debug!(
                                            "PS1 marker write failed for host {}, will retry on next prompt",
                                            host
                                        );
                                    }
                                }
                            }
                            // Display unless AI is processing or capturing probe output.
                            // Use was_probe_active to prevent leaking the chunk where
                            // probe completes (probe_active changes to false mid-parse).
                            // PS1 echo stripping already happened above (before output_buf
                            // was extended), so `data` here is the cleaned slice.
                            if !interceptor.is_ai_processing()
                                && !probe_active
                                && !was_probe_active
                                && !data.is_empty()
                            {
                                write_stdout_all(data);
                                if let Some(ref cb) = on_output {
                                    let text = String::from_utf8_lossy(data);
                                    cb(&text);
                                }
                            }
                        }
                    }
                    0 => {
                        // EOF on master_fd means the bash slave closed --
                        // the child process exited.
                        debug!("master_fd EOF, marking not running and done");
                        self.running.store(false, Ordering::SeqCst);
                        done = true;
                    }
                    _ => {}
                }
            }

            // Process pending AI response (from TriggerAi or followup chain).
            if let Some(response) = pending_response.take() {
                // Guard: if a previous Ctrl+C hard-aborted, skip all further
                // pending responses and return to the shell prompt.
                if ai_cancelled {
                    if let Some(followup) = response.followup {
                        std::thread::spawn(move || {
                            let _ = followup("Command cancelled by user", None);
                        });
                    }
                    skip_leading_newline = true;
                    unsafe {
                        libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                    }
                    continue;
                }
                // Handle ask_user first
                if let Some((request, channel)) = response.ask_user {
                    let aborted = handle_ask_user_interaction(
                        request,
                        channel,
                        stdin_fd,
                        self.master_fd,
                        &mut pending_response,
                        &interceptor,
                    );
                    if aborted {
                        ai_cancelled = true;
                        pending_response = None;
                        skip_leading_newline = true;
                        unsafe {
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                    }
                    // pending_response may now contain the final AI response
                    // which will be processed on the next loop iteration.
                } else if let Some(ref cmd) = response.command {
                    // Show tool indicator matching local aish style
                    let tool_text = aish_i18n::t_with_args("shell.session.tool_bash", &{
                        let mut m = std::collections::HashMap::new();
                        m.insert("command".to_string(), cmd.clone());
                        m
                    });
                    let tool_line = format!("\x1b[36m{}\x1b[0m\r\n", tool_text);
                    // Display directly (terminal cursor is already on a new line
                    // after user input).  Record with leading \r\n so the cast
                    // replay starts the indicator on a fresh line.
                    let _ = unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            tool_line.as_ptr() as *const libc::c_void,
                            tool_line.len(),
                        )
                    };
                    if let Some(ref cb) = on_output {
                        cb(&format!("\r\n{}", tool_line));
                    }

                    // Confirmation prompt before execution
                    let confirm = format!(
                        "\x1b[33m{}\x1b[0m ",
                        aish_i18n::t("shell.session.confirm_execute")
                    );
                    show(&confirm);

                    // Read one byte for confirmation (raw mode)
                    let mut ans = [0u8; 1];
                    let approved = match unsafe {
                        libc::read(stdin_fd, ans.as_mut_ptr() as *mut libc::c_void, 1)
                    } {
                        1 => {
                            // Ctrl+C: hard abort — skip followup entirely
                            if ans[0] == 0x03 {
                                show("^C\r\n");
                                drain_stdin_trailing(stdin_fd, master_fd);
                                ai_cancelled = true;
                                false
                            } else {
                                let echo = if ans[0] == b'y'
                                    || ans[0] == b'Y'
                                    || ans[0] == b'\r'
                                    || ans[0] == b'\n'
                                {
                                    "y\r\n"
                                } else {
                                    "n\r\n"
                                };
                                show(echo);
                                drain_stdin_trailing(stdin_fd, master_fd);
                                ans[0] == b'y'
                                    || ans[0] == b'Y'
                                    || ans[0] == b'\r'
                                    || ans[0] == b'\n'
                            }
                        }
                        _ => false,
                    };

                    if approved {
                        // InputGuard: AI-generated commands must clear the
                        // same safety gate as user-typed ones, even after
                        // the generic Y/n approval. Screen the placeholder
                        // form so any display message keeps secret tokens.
                        if !screen_injected_command(&interceptor, stdin_fd, self.master_fd, cmd) {
                            if let Some(followup) = response.followup {
                                std::thread::spawn(move || {
                                    let _ = followup("Command cancelled by user", None);
                                });
                            }
                            ai_cancelled = true;
                            skip_leading_newline = true;
                            unsafe {
                                libc::write(
                                    self.master_fd,
                                    b"\r".as_ptr() as *const libc::c_void,
                                    1,
                                );
                            }
                            continue;
                        }
                        // Show "Running..." feedback
                        let running_msg = format!(
                            "\x1b[90m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.session.running")
                        );
                        show(&running_msg);
                        // Restore secret placeholders before execution.
                        let mut cmd_restored = cmd.clone();
                        if let Some(ref vault) = secret_vault {
                            let (restored, count) = vault.lock().unwrap().restore(cmd);
                            if count > 0 {
                                let mut rargs = std::collections::HashMap::new();
                                rargs.insert("count".to_string(), count.to_string());
                                let msg = aish_i18n::t_with_args(
                                    "shell.security.secret.restored",
                                    &rargs,
                                );
                                let info = format!("\x1b[2m{}\x1b[0m\r\n", msg);
                                show(&info);
                                cmd_restored = restored;
                            }
                        }
                        let safe_cmd = close_unclosed_heredoc(&cmd_restored);
                        skip_echo_cmd = Some(safe_cmd.clone());
                        let mut inject = safe_cmd.as_bytes().to_vec();
                        inject.push(b'\r');
                        unsafe {
                            libc::write(
                                self.master_fd,
                                inject.as_ptr() as *const libc::c_void,
                                inject.len(),
                            );
                        }
                        if response.followup.is_some() {
                            followup_captured.clear();
                            followup_capturing = true;
                            followup_prompt_seen = false;
                            pending_followup = response.followup;
                            // Create offloader for followup output
                            let session_uuid = uuid::Uuid::new_v4().to_string();
                            let base_dir =
                                std::env::temp_dir().to_str().unwrap_or("/tmp").to_string();
                            followup_offloader = Some(crate::PtyOutputOffload::new(
                                &safe_cmd,
                                &session_uuid,
                                "",
                                1024,
                                &base_dir,
                            ));
                            // Reset idle counter for the new command
                            // (see the other injection site for rationale).
                            idle_poll_count = 0;
                        }
                    } else if ai_cancelled {
                        // Hard abort: print cancel and return to shell.
                        // Fire-and-forget the followup so the LLM thread
                        // receives output instead of "Channel closed".
                        let cancel_msg = format!(
                            "\x1b[33m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.command_cancelled")
                        );
                        show(&cancel_msg);
                        unsafe {
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                        if let Some(followup) = response.followup {
                            std::thread::spawn(move || {
                                let _ = followup("Command cancelled by user", None);
                            });
                        }
                        skip_leading_newline = true;
                    } else {
                        let cancel_msg = format!(
                            "\x1b[33m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.command_cancelled")
                        );
                        show(&cancel_msg);
                        unsafe {
                            libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                        }
                        // User rejected the command — terminate the
                        // entire tool chain.  Fire-and-forget so the
                        // LLM thread receives output instead of
                        // "Channel closed".
                        if let Some(followup) = response.followup {
                            std::thread::spawn(move || {
                                let _ = followup("Command rejected by user. Stop calling bash tools and adjust your approach.", None);
                            });
                        }
                        skip_leading_newline = true;
                    }
                } else {
                    // AI returned explanation only (no command)
                    unsafe {
                        libc::write(self.master_fd, b"\r".as_ptr() as *const libc::c_void, 1);
                    }
                }
            }

            // Read control pipe for events (only during normal phase).
            if !draining && unsafe { libc::FD_ISSET(self.control_fd, &read_fds) } {
                let mut tmp = [0u8; 4096];
                match unsafe {
                    libc::read(
                        self.control_fd,
                        tmp.as_mut_ptr() as *mut libc::c_void,
                        tmp.len(),
                    )
                } {
                    n if n > 0 => {
                        debug!("control_fd read {} bytes", n);
                        let events =
                            decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]);
                        debug!("control_fd decoded {} events", events.len());
                        for event in &events {
                            debug!("control_fd event: {:?}", event);
                            if let BackendControlEvent::ShellExiting { .. } = event {
                                // Bash is shutting down -- mark as not running so
                                // the caller can restart the PTY before the next
                                // command.
                                self.running.store(false, Ordering::SeqCst);
                            }
                            if let BackendControlEvent::PromptReady { .. } = event {
                                let output_so_far =
                                    strip_ansi_escapes(&String::from_utf8_lossy(&output_buf));
                                let over_budget = deferred_since
                                    .is_some_and(|since| since.elapsed() >= DEFERRED_MAX_DELAY);
                                if !over_budget
                                    && crate::exit_code::polkit_auth_in_progress(&output_so_far)
                                {
                                    deferred_since.get_or_insert_with(std::time::Instant::now);
                                    deferred_control_events.push(event.clone());
                                    continue;
                                }
                            }
                            if let Some(r) = self.command_state.handle_event(event) {
                                debug!("command_state.handle_event returned Some({:?}), entering drain phase", r);
                                result_exit_code = crate::exit_code::infer_exit_code_from_output(
                                    r.exit_code,
                                    &strip_ansi_escapes(&String::from_utf8_lossy(&output_buf)),
                                );
                                // Discard any stdin bytes captured in the same
                                // poll cycle.  Without this, a buffered newline
                                // could execute a stale line before the next
                                // command's Ctrl-U gets a chance to clear it.
                                write_buf.clear();
                                // Enter drain phase instead of exiting immediately.
                                // The control pipe may deliver PromptReady before
                                // all PTY output has been flushed to master_fd.
                                draining = true;
                            }
                            if let BackendControlEvent::PromptReady { cwd, .. } = event {
                                result_cwd = cwd.clone();
                            }
                        }
                    }
                    0 => {
                        // Control pipe closed -- bash exited.
                        self.running.store(false, Ordering::SeqCst);
                        done = true;
                    }
                    _ => {}
                }
            }

            if !deferred_control_events.is_empty() {
                let budget_exhausted =
                    deferred_since.is_some_and(|since| since.elapsed() >= DEFERRED_MAX_DELAY);
                if budget_exhausted {
                    debug!(
                        "deferred events exceeded {:?}; flushing regardless of polkit heuristic",
                        DEFERRED_MAX_DELAY
                    );
                }
                let output_so_far = strip_ansi_escapes(&String::from_utf8_lossy(&output_buf));
                let mut still_deferred = Vec::new();
                for event in deferred_control_events.drain(..) {
                    if !budget_exhausted {
                        if let BackendControlEvent::PromptReady { .. } = &event {
                            if crate::exit_code::polkit_auth_in_progress(&output_so_far) {
                                still_deferred.push(event);
                                continue;
                            }
                        }
                    }
                    if let Some(r) = self.command_state.handle_event(&event) {
                        result_exit_code = crate::exit_code::infer_exit_code_from_output(
                            r.exit_code,
                            &output_so_far,
                        );
                        write_buf.clear();
                        draining = true;
                    }
                    if let BackendControlEvent::PromptReady { cwd, .. } = &event {
                        result_cwd = cwd.clone();
                    }
                }
                deferred_control_events = still_deferred;
                if deferred_control_events.is_empty() {
                    deferred_since = None;
                }
            }
        }

        // Restore terminal.
        if let Some(ref saved) = saved_termios {
            let _ = tcsetattr(stdin_borrowed, SetArg::TCSANOW, saved);
        }

        // Decode captured output, stripping ANSI escape sequences for a clean
        // text representation suitable for LLM context.
        let raw_output = String::from_utf8_lossy(&output_buf).to_string();
        let output = strip_ansi_escapes(&raw_output);
        result_exit_code = crate::exit_code::infer_exit_code_from_output(result_exit_code, &output);

        Ok((result_exit_code, result_cwd, output))
    }

    /// Resize the PTY.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        ws.ws_row = rows;
        ws.ws_col = cols;
        unsafe {
            libc::ioctl(self.master_fd, libc::TIOCSWINSZ, &ws);
        }
    }

    /// Stop the bash session.
    pub fn stop(&mut self) {
        let was_running = self.running.swap(false, Ordering::SeqCst);

        if was_running || !wait_for_child_exit(self.child_pid, Duration::from_millis(200)) {
            let _ = kill_pg(self.child_pid, Signal::SIGTERM);
            if !wait_for_child_exit(self.child_pid, Duration::from_millis(200)) {
                let _ = kill_pg(self.child_pid, Signal::SIGKILL);
            }
        }

        reap_child(self.child_pid);

        // Close fds (use raw close to avoid IO Safety issues with from_raw_fd).
        if self.master_fd >= 0 {
            let _ = unsafe { libc::close(self.master_fd) };
            self.master_fd = -1;
        }
        if self.control_fd >= 0 {
            let _ = unsafe { libc::close(self.control_fd) };
            self.control_fd = -1;
        }
        self.command_state.reset();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// PTY master file descriptor (for use in external select loops, e.g. daemon).
    pub fn master_fd(&self) -> RawFd {
        self.master_fd
    }

    /// Control pipe read file descriptor (for use in external select loops).
    pub fn control_fd(&self) -> RawFd {
        self.control_fd
    }

    /// Bash child process PID.
    pub fn child_pid(&self) -> Pid {
        self.child_pid
    }

    /// Write raw bytes directly to the PTY master fd.
    pub fn write_master_pub(&self, data: &[u8]) -> aish_core::Result<()> {
        self.write_master(data)
    }

    /// Current terminal rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Current terminal columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn last_exit_code(&self) -> i32 {
        self.command_state.last_exit_code()
    }

    pub fn last_command(&self) -> &str {
        self.command_state.last_command()
    }

    pub fn can_correct_error(&self) -> bool {
        self.command_state.can_correct_error()
    }

    pub fn consume_error(&mut self) -> Option<(String, i32)> {
        self.command_state.consume_error()
    }

    pub fn clear_error_correction(&mut self) {
        self.command_state.clear_error_correction();
    }

    // ---- Internal helpers ----

    fn allocate_backend_seq(&mut self) -> i32 {
        let seq = self.next_backend_seq;
        self.next_backend_seq -= 1;
        seq
    }

    /// Drain any remaining data from master_fd and discard it.
    /// Used to clear stale prompt rendering output before sending a
    /// new command, so it does not leak into the forwarding loop.
    fn drain_master_silent(&self) {
        let mut tmp = [0u8; 8192];
        loop {
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => { /* discard */ }
                _ => break,
            }
        }
    }

    /// Drain stale data from the control pipe and discard it without
    /// processing events through the command state machine.  Called
    /// before registering a new command to prevent a stale PromptReady
    /// (e.g. from bash's initial prompt or a late-arriving event) from
    /// being matched with the wrong command submission.
    fn drain_control_pipe_raw(&mut self) {
        self.control_buffer.clear();
        let mut tmp = [0u8; 4096];
        loop {
            match unsafe {
                libc::read(
                    self.control_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => { /* discard */ }
                _ => break,
            }
        }
    }

    /// Drain any remaining data from master_fd to stdout.
    /// Called after the forwarding loop exits to prevent stale output
    /// from appearing at the start of the next command.
    fn drain_master_to_stdout(&self) {
        let mut tmp = [0u8; 8192];
        loop {
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    let _ = unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            tmp[..n as usize].as_ptr() as *const libc::c_void,
                            n as usize,
                        )
                    };
                }
                _ => break, // EAGAIN / EWOULDBLOCK / error -- nothing more to read
            }
        }
    }

    /// Drain remaining master_fd output into the exec buffer.
    /// Used by execute_command() to capture all output before returning.
    /// Retries with a small sleep to catch data still in flight from
    /// pipeline commands (e.g. `cat | sort | grep`).
    fn drain_master_to_exec_buffer(&self) {
        let mut tmp = [0u8; 8192];
        let mut retries = 0;
        loop {
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    retries = 0;
                    if self.exec_mode.load(Ordering::SeqCst) {
                        self.exec_buffer
                            .lock()
                            .unwrap()
                            .extend_from_slice(&tmp[..n as usize]);
                    }
                }
                _ => {
                    // No data available right now — retry a few times
                    // to catch output still in flight from pipeline
                    // commands.  The control event (prompt_ready) can
                    // arrive before the last chunk of master_fd output.
                    retries += 1;
                    if retries >= 5 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    /// Wait for the first PromptReady event from bash (the initial prompt
    /// displayed after startup).  Best-effort — a timeout is not fatal
    /// because `send_command_interactive` also drains stale events.
    fn wait_for_initial_prompt_ready(&mut self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let mut tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.control_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    let events = decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]);
                    for event in &events {
                        if matches!(event, BackendControlEvent::PromptReady { .. }) {
                            debug!("consumed initial prompt_ready from bash");
                            return;
                        }
                    }
                }
                0 => {
                    // Control pipe closed.
                    return;
                }
                _ => {}
            }
            // Drain master_fd to the output callback so initial bash output
            // (MOTD, etc.) is not lost.
            let mut mtmp = [0u8; 8192];
            match unsafe {
                libc::read(
                    self.master_fd,
                    mtmp.as_mut_ptr() as *mut libc::c_void,
                    mtmp.len(),
                )
            } {
                n if n > 0 => {
                    if let Some(ref cb) = self.output_callback {
                        cb(&mtmp[..n as usize]);
                    }
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        debug!("timed out waiting for initial prompt_ready (non-fatal)");
    }

    fn write_master(&self, data: &[u8]) -> aish_core::Result<()> {
        let mut written = 0;
        while written < data.len() {
            match unsafe {
                libc::write(
                    self.master_fd,
                    data[written..].as_ptr() as *const libc::c_void,
                    data.len() - written,
                )
            } {
                n if n > 0 => written += n as usize,
                _ => {
                    return Err(AishError::Pty("failed to write to master fd".into()));
                }
            }
        }
        Ok(())
    }
}

// ---- Remote command execution helpers ----

/// Execute a command on the remote host via PTY and capture its output.
fn execute_remote_command(master_fd: i32, command: &str) -> String {
    let mut cmd_bytes = command.as_bytes().to_vec();
    cmd_bytes.push(b'\r');
    unsafe {
        libc::write(
            master_fd,
            cmd_bytes.as_ptr() as *const libc::c_void,
            cmd_bytes.len(),
        );
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut output = Vec::new();
    let mut drain_buf = [0u8; 4096];
    let mut idle_count: u32 = 0;
    let mut timed_out = false;
    loop {
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rfds);
            libc::FD_SET(master_fd, &mut rfds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 100_000,
        };
        let sel = unsafe {
            libc::select(
                master_fd + 1,
                &mut rfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &rfds) } {
            let n = unsafe {
                libc::read(
                    master_fd,
                    drain_buf.as_mut_ptr() as *mut libc::c_void,
                    drain_buf.len(),
                )
            };
            if n > 0 {
                output.extend_from_slice(&drain_buf[..n as usize]);
                idle_count = 0;
            } else {
                break;
            }
        } else {
            idle_count += 1;
            // Only start counting idle after receiving first byte
            if !output.is_empty() && idle_count >= 3 {
                break;
            }
        }
    }

    // On timeout, cancel the remote command and drain remaining output
    // to restore the shell to a clean prompt state.
    if timed_out {
        unsafe {
            libc::write(master_fd, b"\x03".as_ptr() as *const libc::c_void, 1);
        }
        drain_pty(master_fd, Duration::from_millis(500));
    }

    let raw = String::from_utf8_lossy(&output).to_string();
    strip_remote_output(&raw, command)
}

/// Drain any remaining bytes from the PTY master fd for up to `timeout`.
fn drain_pty(master_fd: i32, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    loop {
        if std::time::Instant::now() >= deadline {
            break;
        }
        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rfds);
            libc::FD_SET(master_fd, &mut rfds);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let mut tv = libc::timeval {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_usec: remaining.subsec_micros() as libc::suseconds_t,
        };
        let sel = unsafe {
            libc::select(
                master_fd + 1,
                &mut rfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &rfds) } {
            let n =
                unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
        } else {
            break;
        }
    }
}

/// Strip command echo and ANSI codes from remote command output.
fn strip_remote_output(raw: &str, command: &str) -> String {
    let mut clean = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !((0x40..=0x7E).contains(&bytes[i])) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else if bytes[i] == b'\r' {
            i += 1;
        } else {
            clean.push(bytes[i] as char);
            i += 1;
        }
    }

    let mut lines: Vec<&str> = clean.lines().collect();
    let cmd_trimmed = command.trim();
    if !lines.is_empty() && lines[0].contains(cmd_trimmed) {
        lines.remove(0);
    }
    while let Some(last) = lines.last() {
        if last.is_empty() || last.contains('$') || last.contains('#') || last.contains('~') {
            lines.pop();
        } else {
            break;
        }
    }
    lines.join("\n").trim().to_string()
}

impl PersistentPty {
    /// Returns Ok(true) if PromptReady was also seen in the same batch.
    fn wait_for_session_ready(&mut self, timeout: Duration) -> aish_core::Result<bool> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            // Drain any initial bash output from master_fd.
            let mut tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.master_fd,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            } {
                n if n > 0 => {
                    if let Some(ref cb) = self.output_callback {
                        cb(&tmp[..n as usize]);
                    }
                }
                0 => {
                    // EOF on master_fd -- bash exited during init.
                    self.running.store(false, Ordering::SeqCst);
                    return Err(AishError::Pty("bash exited before session_ready".into()));
                }
                _ => {}
            }

            // Read control pipe for session_ready.
            let mut ctrl_tmp = [0u8; 4096];
            match unsafe {
                libc::read(
                    self.control_fd,
                    ctrl_tmp.as_mut_ptr() as *mut libc::c_void,
                    ctrl_tmp.len(),
                )
            } {
                n if n > 0 => {
                    let events =
                        decode_control_chunk(&mut self.control_buffer, &ctrl_tmp[..n as usize]);
                    let mut found_session = false;
                    let mut found_prompt = false;
                    for event in &events {
                        match event {
                            BackendControlEvent::SessionReady { .. } => found_session = true,
                            BackendControlEvent::PromptReady { .. } => found_prompt = true,
                            _ => {}
                        }
                    }
                    if found_session {
                        debug!("received session_ready from bash");
                        return Ok(found_prompt);
                    }
                }
                0 => {
                    // Control pipe closed -- bash exited during init.
                    self.running.store(false, Ordering::SeqCst);
                    return Err(AishError::Pty(
                        "control pipe closed before session_ready".into(),
                    ));
                }
                _ => {}
            }

            std::thread::sleep(Duration::from_millis(10));
        }
        Err(AishError::Pty(
            "timeout waiting for session_ready event".into(),
        ))
    }
}

// ---- secret detection confirmation for SSH sessions ----

/// Render the warning + options + help into a byte buffer.
/// Returns the buffer and the exact number of `\r\n` sequences it contains,
/// which equals the number of cursor-up moves needed to return to the first row.
fn render_secret_confirmation(warning: &str, options: &[&str], cursor: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();

    // Warning message — replace \n with \r\n for terminal correctness
    let warning_display = warning.replace('\n', "\r\n");
    out.extend_from_slice(warning_display.as_bytes());
    out.extend_from_slice(b"\r\n");

    // Options with cursor highlight (inquire-style)
    for (i, opt) in options.iter().enumerate() {
        if i == cursor {
            out.extend_from_slice(b"\x1b[36m> \x1b[1m");
        } else {
            out.extend_from_slice(b"  ");
        }
        out.extend_from_slice(opt.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    }

    // Help line (no trailing \r\n — cursor stays on this row)
    let esc_hint = aish_i18n::t("shell.security.secret.option_esc");
    out.extend_from_slice(b"\x1b[2m");
    out.extend_from_slice(esc_hint.as_bytes());
    out.extend_from_slice(b"\x1b[0m");

    // Count \r\n occurrences = exact up-movement needed from last row to first row
    let up_moves = out.windows(2).filter(|w| w == b"\r\n").count();

    (out, up_moves)
}

/// Choice for SSH secret dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SshSecretChoice {
    Redact,
    Allow,
    Abort,
}

/// Display a three-option secret dialog with up/down selection (SSH path).
fn show_secret_dialog(warning: &str, stdin_fd: libc::c_int) -> SshSecretChoice {
    let option_redact = aish_i18n::t("shell.security.secret.redact");
    let option_allow = aish_i18n::t("shell.security.secret.allow");
    let option_abort = aish_i18n::t("shell.security.secret.abort");
    let options = [
        option_redact.as_str(),
        option_allow.as_str(),
        option_abort.as_str(),
    ];
    let choice_values = [
        SshSecretChoice::Redact,
        SshSecretChoice::Allow,
        SshSecretChoice::Abort,
    ];
    // Default cursor on "Redact" (index 0, safest)
    let mut cursor: usize = 0;

    let (buf, _) = render_secret_confirmation(warning, &options, cursor);
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            buf.as_ptr() as *const libc::c_void,
            buf.len(),
        );
    }

    loop {
        match read_byte(stdin_fd) {
            Some(byte) => match byte {
                0x03 => {
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            b"^C\r\n".as_ptr() as *const _,
                            b"^C\r\n".len(),
                        );
                    }
                    return SshSecretChoice::Abort;
                }
                0x1B => {
                    if stdin_poll(stdin_fd, 50_000) {
                        let next = read_byte(stdin_fd);
                        if next == Some(b'[') {
                            if let Some(final_byte) = consume_csi(stdin_fd) {
                                match final_byte {
                                    b'A' if cursor > 0 => {
                                        cursor = cursor.saturating_sub(1);
                                    }
                                    b'B' if cursor < options.len() - 1 => {
                                        cursor += 1;
                                    }
                                    _ => {}
                                }
                                let (buf, up) =
                                    render_secret_confirmation(warning, &options, cursor);
                                let mut redraw = Vec::new();
                                redraw
                                    .extend_from_slice(format!("\x1b[{}A\r\x1b[J", up).as_bytes());
                                redraw.extend_from_slice(&buf);
                                unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        redraw.as_ptr() as *const libc::c_void,
                                        redraw.len(),
                                    );
                                }
                            }
                            continue;
                        }
                    }
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, b"\r\n".as_ptr() as *const _, 2);
                    }
                    return SshSecretChoice::Abort;
                }
                b'\r' | b'\n' => {
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, b"\r\n".as_ptr() as *const _, 2);
                    }
                    return choice_values[cursor];
                }
                _ => {}
            },
            None => return SshSecretChoice::Abort,
        }
    }
}

// ---- ask_user helpers for SSH sessions ----

/// Drain trailing bytes (e.g. `\n` or `\r`) from stdin after a single-byte
/// confirmation read so they don't leak into the next input cycle.
fn drain_stdin_trailing(stdin_fd: libc::c_int, master_fd: libc::c_int) {
    // Drain up to 256 bytes with a 100ms timeout to handle cases where
    // the user typed ahead (e.g. started typing the next command while
    // the confirmation was being processed).
    //
    // Control characters (Ctrl+C, Ctrl+Z, Ctrl+\, Ctrl+D) are NOT drained:
    // they are written straight to `master_fd` so the remote PTY sees them
    // immediately. Otherwise a user who confirms a destructive command and
    // immediately hits Ctrl+C to abort would have the cancel silently
    // swallowed by this drain — the command runs anyway and the user has
    // to press Ctrl+C repeatedly while the remote ssh keeps retrying.
    let mut discard = [0u8; 256];
    loop {
        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rfds);
            libc::FD_SET(stdin_fd, &mut rfds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 100_000, // 100ms
        };
        let sel = unsafe {
            libc::select(
                stdin_fd + 1,
                &mut rfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel <= 0 {
            break;
        }
        let n = unsafe {
            libc::read(
                stdin_fd,
                discard.as_mut_ptr() as *mut libc::c_void,
                discard.len(),
            )
        };
        if n <= 0 {
            break;
        }
        let n = n as usize;
        for &b in &discard[..n] {
            if matches!(b, 0x03 | 0x1a | 0x1c | 0x04) {
                // Forward control characters straight to the PTY so the
                // remote has a chance to react before the queued command.
                unsafe {
                    libc::write(master_fd, &b as *const u8 as *const libc::c_void, 1);
                }
            }
        }
        if n < discard.len() {
            break;
        }
    }
}

/// Truncate a string to `max` bytes, respecting UTF-8 boundaries.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Debug helper: describe the answer kind without exposing the value.
fn answer_kind(answer: &crate::AskUserAnswer) -> &'static str {
    match answer {
        crate::AskUserAnswer::Response(_) => "Response",
        crate::AskUserAnswer::Cancelled => "Cancelled",
    }
}

/// How many lines the ask_user display occupies (so we can erase/redraw).
fn count_display_lines(request: &crate::AskUserRequest) -> usize {
    let mut lines = 1; // Header
    if request.kind == "choice_or_text" {
        if let Some(ref options) = request.options {
            lines += options.len() + 1; // options + custom input
        }
    }
    if request.default.is_some() {
        lines += 1;
    }
    lines += 1; // Help line
                // Prompt line "> " only for text_input mode
    if request.kind != "choice_or_text" {
        lines += 1;
    }
    lines
}

/// Erase the current ask_user display and redraw with the cursor at
/// `cursor` (only meaningful for choice_or_text).
fn redraw_ask_user(request: &crate::AskUserRequest, prev_lines: usize, cursor: usize) {
    let mut out = Vec::new();

    // Move up and clear
    if prev_lines > 0 {
        out.extend_from_slice(format!("\x1b[{}A", prev_lines).as_bytes());
    }
    out.extend_from_slice(b"\x1b[J"); // Clear from cursor to end of screen

    // Header — match local aish's inquire style
    out.extend_from_slice(b"\x1b[36m? \x1b[1m");
    if let Some(ref title) = request.title {
        out.extend_from_slice(title.as_bytes());
        out.extend_from_slice(b": ");
    }
    out.extend_from_slice(request.prompt.as_bytes());
    out.extend_from_slice(b"\x1b[0m\r\n");

    // Options with cursor highlight for choice_or_text
    if request.kind == "choice_or_text" {
        if let Some(ref options) = request.options {
            for (i, opt) in options.iter().enumerate() {
                // Use inquire-style cursor: ">" for selected, " " for others
                if i == cursor {
                    out.extend_from_slice(b"\x1b[36m> \x1b[1m");
                } else {
                    out.extend_from_slice(b"  ");
                }
                out.extend_from_slice(opt.label.as_bytes());
                if let Some(ref desc) = opt.description {
                    out.extend_from_slice(format!(" - {}", desc).as_bytes());
                }
                out.extend_from_slice(b"\x1b[0m\r\n");
            }
            // Custom input entry at the bottom — same label as local aish
            let custom_label = aish_i18n::t("shell.session.ask_user.custom_input_label");
            if cursor == options.len() {
                out.extend_from_slice(b"\x1b[36m> \x1b[1m");
            } else {
                out.extend_from_slice(b"  ");
            }
            out.extend_from_slice(format!("({})", custom_label).as_bytes());
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
    }

    // Default hint — match local aish's [default: xxx] format
    if let Some(ref default) = request.default {
        let default_hint = aish_i18n::t_with_args("shell.session.ask_user.default_hint", &{
            let mut m = std::collections::HashMap::new();
            m.insert("default".to_string(), default.clone());
            m
        });
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(default_hint.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    }

    // Help message — match local aish's style
    if request.allow_cancel {
        let help = aish_i18n::t("shell.session.ask_user.help_with_cancel");
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(help.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    } else {
        let help = aish_i18n::t("shell.session.ask_user.help_no_cancel");
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(help.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    }

    // Prompt (only for text_input mode)
    if request.kind != "choice_or_text" {
        out.extend_from_slice(b"\x1b[33m> \x1b[0m");
    }

    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            out.as_ptr() as *const libc::c_void,
            out.len(),
        );
    }
}

/// Initial display — ensure we start on a fresh line.
fn display_ask_user(request: &crate::AskUserRequest) {
    // Move to a new line to avoid garbling with previous AI output
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            b"\r\n".as_ptr() as *const libc::c_void,
            2,
        );
    }
    redraw_ask_user(request, 0, 0);
}
/// Display a yellow warning and prompt `[y/N]`, then read one raw key.
/// Returns `true` only when the user presses `y` or `Y`.
fn read_confirm_raw(stdin_fd: libc::c_int, master_fd: libc::c_int, reason: &str) -> bool {
    let msg = format!("\r\n\x1b[33m{}\x1b[0m\r\nExecute anyway? [y/N] ", reason);
    unsafe {
        libc::write(libc::STDOUT_FILENO, msg.as_ptr() as *const _, msg.len());
    }
    let result = match read_byte(stdin_fd) {
        Some(b'y' | b'Y') => {
            unsafe {
                libc::write(libc::STDOUT_FILENO, b"y\r\n".as_ptr() as *const _, 3);
            }
            true
        }
        Some(0x03) => {
            // Ctrl+C
            unsafe {
                libc::write(libc::STDOUT_FILENO, b"^C\r\n".as_ptr() as *const _, 4);
            }
            false
        }
        Some(0x1b) => {
            // ESC could be a standalone keypress (cancel) or the start
            // of a CSI sequence (arrow keys, etc.).  Poll briefly: if no
            // more bytes arrive within 10ms, it was a standalone ESC.
            if stdin_poll(stdin_fd, 10_000) {
                if let Some(next) = read_byte(stdin_fd) {
                    if next == b'[' {
                        let _ = consume_csi(stdin_fd);
                    }
                }
            }
            false
        }
        Some(_) => {
            // Any other key (n, Enter, etc.) → reject
            unsafe {
                libc::write(libc::STDOUT_FILENO, b"n\r\n".as_ptr() as *const _, 3);
            }
            false
        }
        None => false,
    };
    // Drain any trailing typeahead (e.g. user typed `y<Enter>` — the
    // Enter would otherwise be forwarded to the remote PTY as the
    // start of a new command). Control characters are forwarded to
    // the PTY by the helper instead of being drained.
    drain_stdin_trailing(stdin_fd, master_fd);
    result
}

/// Screen an AI-injected or BashExec command via InputGuard before it
/// reaches the remote PTY. Returns true when the command may proceed
/// (Allow, or Confirm/Unknown that the user explicitly acknowledged);
/// false when the command is blocked or the user declined.
///
/// `placeholder_cmd` is the command WITH secret placeholders still in
/// place — screening this form keeps real secret values out of any
/// InputGuard display message.
fn screen_injected_command(
    interceptor: &crate::SessionInterceptor,
    stdin_fd: libc::c_int,
    master_fd: libc::c_int,
    placeholder_cmd: &str,
) -> bool {
    use aish_security::input_guard::InputVerdict;
    let verdict = interceptor.screen_command(placeholder_cmd);
    match &verdict {
        InputVerdict::Allow => true,
        InputVerdict::Block { .. } => {
            let msg = format!("\r\n\x1b[31m{}\x1b[0m\r\n", verdict.format_display());
            unsafe {
                libc::write(
                    libc::STDOUT_FILENO,
                    msg.as_ptr() as *const libc::c_void,
                    msg.len(),
                );
            }
            false
        }
        InputVerdict::Confirm { .. } | InputVerdict::Unknown { .. } => {
            read_confirm_raw(stdin_fd, master_fd, &verdict.format_display())
        }
    }
}

/// Read one raw byte from stdin with EINTR retry.
/// Returns the byte on success, or None on EOF/error.
fn read_byte(stdin_fd: libc::c_int) -> Option<u8> {
    loop {
        let mut byte = [0u8; 1];
        let n = unsafe { libc::read(stdin_fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
        match n {
            1 => return Some(byte[0]),
            -1 => {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    continue;
                }
                debug!("read_byte: error, errno={}", errno);
                return None;
            }
            0 => {
                debug!("read_byte: EOF");
                return None;
            }
            _ => {
                debug!("read_byte: unexpected return {}", n);
                continue;
            }
        }
    }
}

/// Check whether stdin has data available within `timeout_us` microseconds.
fn stdin_poll(stdin_fd: libc::c_int, timeout_us: i64) -> bool {
    let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::FD_ZERO(&mut rfds);
        libc::FD_SET(stdin_fd, &mut rfds);
    }
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: timeout_us as _,
    };
    let sel = unsafe {
        libc::select(
            stdin_fd + 1,
            &mut rfds,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut tv,
        )
    };
    sel > 0
}

/// Consume a CSI escape sequence (already read `\x1b[`).
/// CSI format: parameters (0x30-0x3F)* intermediate (0x20-0x2F)* final (0x40-0x7E)
/// Returns the final byte (e.g. 'A' for up arrow) or None on error.
fn consume_csi(stdin_fd: libc::c_int) -> Option<u8> {
    loop {
        match read_byte(stdin_fd) {
            Some(b) if (0x40..=0x7E).contains(&b) => return Some(b),
            Some(_) => continue, // parameter or intermediate byte
            None => return None,
        }
    }
}

/// Read a line of user input in raw mode with escape-sequence handling.
/// For choice_or_text: up/down arrows navigate options (including custom
/// input slot at the bottom), Enter selects. Typing text switches to
/// custom input mode.
/// For text_input: normal text editing, Enter submits.
/// Ctrl+C always cancels. Esc cancels only if allow_cancel is true.
fn read_line_from_stdin_raw(
    stdin_fd: libc::c_int,
    request: &crate::AskUserRequest,
) -> crate::AskUserAnswer {
    let is_choice = request.kind == "choice_or_text";
    let num_options = request.options.as_ref().map_or(0, |o| o.len());
    let has_options = is_choice && num_options > 0;
    // Total selectable slots: options + 1 custom-input slot
    let total_slots = if has_options { num_options + 1 } else { 0 };

    // For choice mode, track cursor position
    let mut cursor: usize = 0;
    let mut text_buf: Vec<u8> = Vec::new();

    loop {
        match read_byte(stdin_fd) {
            Some(byte) => match byte {
                // Ctrl+C → always cancel
                0x03 => {
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            b"^C\r\n".as_ptr() as *const _,
                            b"^C\r\n".len(),
                        );
                    }
                    return crate::AskUserAnswer::Cancelled;
                }
                // Enter → submit
                b'\r' | b'\n' => {
                    // After printing \r\n the cursor is one line below the
                    // display.  prev_lines must account for the full display
                    // height so redraw can move back to the header line.
                    let prev = count_display_lines(request) + 1; // +1 for the \r\n
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, b"\r\n".as_ptr() as *const _, 2);
                    }
                    // If user typed text, treat as custom input
                    if !text_buf.is_empty() {
                        let text = String::from_utf8_lossy(&text_buf).to_string();
                        let trimmed = text.trim().to_string();
                        if trimmed.is_empty() {
                            // Empty after trim — treat like empty
                            if let Some(ref default) = request.default {
                                return crate::AskUserAnswer::Response(default.clone());
                            }
                            if request.allow_cancel {
                                return crate::AskUserAnswer::Cancelled;
                            }
                            // Required — redisplay and loop
                            redraw_ask_user(request, prev, cursor);
                            text_buf.clear();
                            continue;
                        }
                        if trimmed.len() < request.min_length {
                            let min_len_msg = aish_i18n::t_with_args(
                                "shell.session.ask_user.min_length_error",
                                &{
                                    let mut m = std::collections::HashMap::new();
                                    m.insert("min".to_string(), request.min_length.to_string());
                                    m
                                },
                            );
                            let msg = format!("\x1b[31m{}\x1b[0m\r\n", min_len_msg);
                            unsafe {
                                libc::write(
                                    libc::STDOUT_FILENO,
                                    msg.as_ptr() as *const libc::c_void,
                                    msg.len(),
                                );
                            }
                            redraw_ask_user(request, prev, cursor);
                            text_buf.clear();
                            continue;
                        }
                        return crate::AskUserAnswer::Response(trimmed);
                    }
                    // No text typed — select by cursor position
                    if has_options {
                        if cursor < num_options {
                            // Regular option selected
                            let value = request.options.as_ref().unwrap()[cursor].value.clone();
                            return crate::AskUserAnswer::Response(value);
                        } else {
                            // Custom-input slot selected with no text —
                            // stay in input mode (same as local AskUserTool
                            // which goes back to select on empty input)
                            redraw_ask_user(request, prev, cursor);
                            continue;
                        }
                    }
                    // text_input mode with empty input
                    if let Some(ref default) = request.default {
                        return crate::AskUserAnswer::Response(default.clone());
                    }
                    if request.allow_cancel {
                        return crate::AskUserAnswer::Cancelled;
                    }
                    // Required — redisplay and loop
                    redraw_ask_user(request, prev, cursor);
                    continue;
                }
                // Backspace / Delete
                0x7F | 0x08 => {
                    if !text_buf.is_empty() {
                        // Pop trailing UTF-8 continuation bytes, then leader
                        while text_buf.last().is_some_and(|b| b & 0xC0 == 0x80) {
                            text_buf.pop();
                        }
                        let leader = text_buf.pop().unwrap();
                        // Display width: ASCII=1, 2-byte=1, 3-byte(CJK)=2, 4-byte=2
                        let width = if leader < 0x80 || leader & 0xE0 == 0xC0 {
                            1
                        } else {
                            2
                        };
                        let erase = format!(
                            "{}{}{}",
                            "\x08".repeat(width),
                            " ".repeat(width),
                            "\x08".repeat(width),
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                erase.as_ptr() as *const _,
                                erase.len(),
                            );
                        }
                    }
                }
                // Escape — could be standalone Esc or start of escape sequence
                0x1B => {
                    // Use 100ms timeout: long enough to cover SSH network
                    // latency (direction keys arrive as ESC [ A in separate
                    // packets) while still allowing standalone ESC to cancel.
                    if stdin_poll(stdin_fd, 100_000) {
                        // Escape sequence — read next byte
                        match read_byte(stdin_fd) {
                            Some(b'[') => {
                                // CSI sequence
                                match consume_csi(stdin_fd) {
                                    Some(b'A') | Some(b'k') if total_slots > 0 => {
                                        // Up arrow — navigate in choice mode
                                        if cursor > 0 {
                                            cursor -= 1;
                                        } else {
                                            cursor = total_slots - 1;
                                        }
                                        // Clear text buffer when navigating
                                        if !text_buf.is_empty() {
                                            let erase = "\x08".repeat(text_buf.len())
                                                + &" ".repeat(text_buf.len())
                                                + &"\x08".repeat(text_buf.len());
                                            unsafe {
                                                libc::write(
                                                    libc::STDOUT_FILENO,
                                                    erase.as_ptr() as *const _,
                                                    erase.len(),
                                                );
                                            }
                                            text_buf.clear();
                                        }
                                        let prev = if request.kind == "choice_or_text" {
                                            count_display_lines(request)
                                        } else {
                                            count_display_lines(request).saturating_sub(1)
                                        };
                                        redraw_ask_user(request, prev, cursor);
                                    }
                                    Some(b'B') | Some(b'j') if total_slots > 0 => {
                                        // Down arrow — navigate in choice mode
                                        if cursor + 1 < total_slots {
                                            cursor += 1;
                                        } else {
                                            cursor = 0;
                                        }
                                        if !text_buf.is_empty() {
                                            let erase = "\x08".repeat(text_buf.len())
                                                + &" ".repeat(text_buf.len())
                                                + &"\x08".repeat(text_buf.len());
                                            unsafe {
                                                libc::write(
                                                    libc::STDOUT_FILENO,
                                                    erase.as_ptr() as *const _,
                                                    erase.len(),
                                                );
                                            }
                                            text_buf.clear();
                                        }
                                        let prev = if request.kind == "choice_or_text" {
                                            count_display_lines(request)
                                        } else {
                                            count_display_lines(request).saturating_sub(1)
                                        };
                                        redraw_ask_user(request, prev, cursor);
                                    }
                                    _ => {
                                        // Other CSI sequences (Home, End, PgUp, etc.) — ignore
                                    }
                                }
                            }
                            Some(b'O') => {
                                // SS3 sequence (F-keys, etc.) — consume final byte and ignore
                                let _ = read_byte(stdin_fd);
                            }
                            Some(_) => {
                                // Other escape sequences — ignore
                            }
                            None => {
                                // Incomplete sequence — treat as Esc
                                if request.allow_cancel {
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            b"\r\n".as_ptr() as *const _,
                                            2,
                                        );
                                    }
                                    return crate::AskUserAnswer::Cancelled;
                                }
                                // Not allowed to cancel — ignore
                            }
                        }
                    } else {
                        // Standalone Escape
                        if request.allow_cancel {
                            unsafe {
                                libc::write(libc::STDOUT_FILENO, b"\r\n".as_ptr() as *const _, 2);
                            }
                            return crate::AskUserAnswer::Cancelled;
                        }
                        // Not allowed to cancel — ignore
                    }
                }
                // Normal byte — typing text automatically switches to custom input
                _ => {
                    text_buf.push(byte);
                    // Echo
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            &byte as *const u8 as *const libc::c_void,
                            1,
                        );
                    }
                }
            },
            None => {
                // EOF or error
                return crate::AskUserAnswer::Cancelled;
            }
        }
    }
}

/// Handle an ask_user interaction: display question, read answer, wait for
/// next LLM event.  Sets `pending_response` with the final AI response (or
/// None if the LLM finished without further action).
/// Returns `true` if the user pressed Ctrl+C (hard abort requested).
fn handle_ask_user_interaction(
    request: crate::AskUserRequest,
    channel: crate::AskUserChannel,
    stdin_fd: libc::c_int,
    master_fd: libc::c_int,
    pending_response: &mut Option<crate::AiResponse>,
    interceptor: &crate::SessionInterceptor,
) -> bool {
    debug!(
        "handle_ask_user: kind={}, prompt={}",
        request.kind, request.prompt
    );

    // Show tool indicator matching local aish style
    let args_preview = match request.kind.as_str() {
        "choice_or_text" => {
            let n = request.options.as_ref().map_or(0, |o| o.len());
            let mut m = std::collections::HashMap::new();
            m.insert(
                "prompt".to_string(),
                truncate_str(&request.prompt, 60).to_string(),
            );
            m.insert("count".to_string(), n.to_string());
            aish_i18n::t_with_args("shell.session.ask_user.choice_preview", &m)
        }
        _ => {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "prompt".to_string(),
                truncate_str(&request.prompt, 80).to_string(),
            );
            aish_i18n::t_with_args("shell.session.ask_user.text_preview", &m)
        }
    };
    let mut tool_args = std::collections::HashMap::new();
    tool_args.insert("preview".to_string(), args_preview);
    let tool_line = format!(
        "\x1b[36m{}\x1b[0m\r\n",
        aish_i18n::t_with_args("shell.session.ask_user.tool_banner", &tool_args)
    );
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            tool_line.as_ptr() as *const libc::c_void,
            tool_line.len(),
        );
    }

    display_ask_user(&request);
    let answer = read_line_from_stdin_raw(stdin_fd, &request);
    debug!("handle_ask_user: got answer {:?}", answer_kind(&answer));

    if channel.answer_sender.send(answer).is_err() {
        debug!("handle_ask_user: answer channel closed");
        return false;
    }

    // Wait for next event from LLM, forwarding PTY output meanwhile.
    loop {
        match channel.event_receiver.try_recv() {
            Ok(crate::AiEvent::Done(resp)) => {
                debug!("handle_ask_user: LLM done, has_command={}", resp.is_some());
                *pending_response = resp;
                break;
            }
            Ok(crate::AiEvent::AskUser(next_req)) => {
                debug!(
                    "handle_ask_user: follow-up ask_user, prompt={}",
                    next_req.prompt
                );
                display_ask_user(&next_req);
                let answer = read_line_from_stdin_raw(stdin_fd, &next_req);
                debug!(
                    "handle_ask_user: follow-up answer {:?}",
                    answer_kind(&answer)
                );
                if channel.answer_sender.send(answer).is_err() {
                    break;
                }
                continue;
            }
            Ok(crate::AiEvent::BashExec {
                command,
                output_sender,
            }) => {
                debug!("handle_ask_user: follow-up bash_exec, cmd={}", command);
                // Show tool indicator and confirmation, then execute inline.
                let tool_text = aish_i18n::t_with_args("shell.session.tool_bash", &{
                    let mut m = std::collections::HashMap::new();
                    m.insert("command".to_string(), command.clone());
                    m
                });
                let tool_line = format!("\x1b[36m{}\x1b[0m\r\n", tool_text);
                unsafe {
                    libc::write(
                        libc::STDOUT_FILENO,
                        tool_line.as_ptr() as *const libc::c_void,
                        tool_line.len(),
                    );
                }
                let confirm = format!(
                    "\x1b[33m{}\x1b[0m ",
                    aish_i18n::t("shell.session.confirm_execute")
                );
                unsafe {
                    libc::write(
                        libc::STDOUT_FILENO,
                        confirm.as_ptr() as *const libc::c_void,
                        confirm.len(),
                    );
                }
                let mut ans = [0u8; 1];
                let mut hard_abort = false;
                let approved =
                    match unsafe { libc::read(stdin_fd, ans.as_mut_ptr() as *mut libc::c_void, 1) }
                    {
                        1 => {
                            // Ctrl+C: hard abort
                            if ans[0] == 0x03 {
                                unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        b"^C\r\n".as_ptr() as *const libc::c_void,
                                        4,
                                    );
                                }
                                drain_stdin_trailing(stdin_fd, master_fd);
                                hard_abort = true;
                                false
                            } else {
                                let echo = if ans[0] == b'y'
                                    || ans[0] == b'Y'
                                    || ans[0] == b'\r'
                                    || ans[0] == b'\n'
                                {
                                    b"y\r\n"
                                } else {
                                    b"n\r\n"
                                };
                                unsafe {
                                    libc::write(
                                        libc::STDOUT_FILENO,
                                        echo.as_ptr() as *const libc::c_void,
                                        echo.len(),
                                    );
                                }
                                drain_stdin_trailing(stdin_fd, master_fd);
                                ans[0] == b'y'
                                    || ans[0] == b'Y'
                                    || ans[0] == b'\r'
                                    || ans[0] == b'\n'
                            }
                        }
                        _ => false,
                    };
                if approved {
                    // InputGuard: BashExec commands must clear the same
                    // safety gate as user-typed ones. If blocked or the
                    // user declines the InputGuard confirmation, return
                    // an aborted result so the AI sees a clean response
                    // and the caller stops further tool chaining.
                    if !screen_injected_command(interceptor, stdin_fd, master_fd, &command) {
                        let cancel_msg = format!(
                            "\x1b[33m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.command_cancelled")
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                cancel_msg.as_ptr() as *const libc::c_void,
                                cancel_msg.len(),
                            );
                        }
                        let _ = output_sender.send(crate::session_interceptor::BashExecResult {
                            output: format!("(cancelled: {})", command),
                            offload_path: None,
                        });
                        return true;
                    }
                    // Show "Running..." feedback
                    let running_msg = format!(
                        "\x1b[90m{}\x1b[0m\r\n",
                        aish_i18n::t("shell.session.running")
                    );
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            running_msg.as_ptr() as *const libc::c_void,
                            running_msg.len(),
                        );
                    }
                    let safe_cmd = close_unclosed_heredoc(&command);
                    let mut inject = safe_cmd.as_bytes().to_vec();
                    inject.push(b'\r');
                    unsafe {
                        libc::write(
                            master_fd,
                            inject.as_ptr() as *const libc::c_void,
                            inject.len(),
                        );
                    }
                    // Create local offloader for streaming output capture
                    let session_uuid = uuid::Uuid::new_v4().to_string();
                    let base_dir = std::env::temp_dir().to_str().unwrap_or("/tmp").to_string();
                    let mut offloader = crate::PtyOutputOffload::new(
                        &command,
                        &session_uuid,
                        "",
                        1024, // keep_bytes - data below this stays in memory
                        &base_dir,
                    );
                    // Wait for command output. Long-running commands are
                    // supported — prompt detection fires quickly when the
                    // shell returns to an idle prompt, otherwise a 5 s grace
                    // period is used. The user can interrupt with Ctrl+C.
                    let mut captured = Vec::new();
                    let mut idle_count: u32 = 0;
                    let mut prompt_seen = false;
                    const BASH_EXEC_PROMPT_IDLE: u32 = 10; // 500 ms after prompt
                    const BASH_EXEC_GRACE_IDLE: u32 = 100; // 5 s without prompt
                    let max_fd = master_fd.max(stdin_fd);
                    loop {
                        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
                        unsafe {
                            libc::FD_ZERO(&mut rfds);
                            libc::FD_SET(master_fd, &mut rfds);
                            libc::FD_SET(stdin_fd, &mut rfds);
                        }
                        let mut tv = libc::timeval {
                            tv_sec: 0,
                            tv_usec: 50_000,
                        };
                        let sel = unsafe {
                            libc::select(
                                max_fd + 1,
                                &mut rfds,
                                std::ptr::null_mut(),
                                std::ptr::null_mut(),
                                &mut tv,
                            )
                        };
                        // Check stdin for Ctrl+C
                        if sel > 0 && unsafe { libc::FD_ISSET(stdin_fd, &rfds) } {
                            let mut tmp = [0u8; 1];
                            if unsafe {
                                libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, 1)
                            } == 1
                                && tmp[0] == 0x03
                            {
                                unsafe {
                                    libc::write(
                                        master_fd,
                                        b"\x03".as_ptr() as *const libc::c_void,
                                        1,
                                    );
                                }
                                hard_abort = true;
                                break;
                            }
                        }
                        if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &rfds) } {
                            let mut tmp = [0u8; 4096];
                            match unsafe {
                                libc::read(
                                    master_fd,
                                    tmp.as_mut_ptr() as *mut libc::c_void,
                                    tmp.len(),
                                )
                            } {
                                n if n > 0 => {
                                    let data = &tmp[..n as usize];
                                    captured.extend_from_slice(data);
                                    // Stream to local offloader
                                    offloader
                                        .append_overflow(crate::types::StreamName::Stdout, data);
                                    unsafe {
                                        libc::write(
                                            libc::STDOUT_FILENO,
                                            data.as_ptr() as *const libc::c_void,
                                            data.len(),
                                        );
                                    }
                                    // Detect shell prompt to end capture quickly
                                    if !prompt_seen && data.len() >= 2 {
                                        let tail = &data[data.len().saturating_sub(4)..];
                                        if tail.ends_with(b"# ")
                                            || tail.ends_with(b"$ ")
                                            || tail.ends_with(b"#\r")
                                            || tail.ends_with(b"$\r")
                                        {
                                            prompt_seen = true;
                                        }
                                    }
                                    idle_count = 0;
                                }
                                _ => break,
                            }
                        } else {
                            idle_count += 1;
                            let threshold = if prompt_seen {
                                BASH_EXEC_PROMPT_IDLE
                            } else {
                                BASH_EXEC_GRACE_IDLE
                            };
                            if idle_count >= threshold {
                                break;
                            }
                        }
                    }
                    if hard_abort {
                        // Ctrl+C during execution — cancel all tool chaining
                        // Clean up any temp files created by offloader
                        offloader.cancel();
                        let cancel_msg = format!(
                            "\x1b[33m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.command_cancelled")
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                cancel_msg.as_ptr() as *const libc::c_void,
                                cancel_msg.len(),
                            );
                        }
                        let _ = output_sender.send(crate::session_interceptor::BashExecResult {
                            output: format!("(cancelled: {})", command),
                            offload_path: None,
                        });
                        return true;
                    }
                    let output = String::from_utf8_lossy(&captured).to_string();
                    let clean = strip_ansi_and_prompt(&output);

                    // Finalize local offload
                    let offload_result = offloader.finalize(&[], &[], 0);
                    // Prefer clean_path (ANSI-stripped, valid UTF-8) over raw
                    // path so read_file can decode it correctly.
                    let offload_path = if offload_result.stdout.status == "offloaded" {
                        offload_result
                            .stdout
                            .clean_path
                            .clone()
                            .or(offload_result.stdout.path.clone())
                    } else {
                        None
                    };

                    let _ = output_sender.send(crate::session_interceptor::BashExecResult {
                        output: clean,
                        offload_path,
                    });
                } else if hard_abort {
                    // Ctrl+C hard abort: signal caller to stop all tool chaining
                    let cancel_msg = format!(
                        "\x1b[33m{}\x1b[0m\r\n",
                        aish_i18n::t("shell.command_cancelled")
                    );
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            cancel_msg.as_ptr() as *const libc::c_void,
                            cancel_msg.len(),
                        );
                    }
                    let _ = output_sender.send(crate::session_interceptor::BashExecResult {
                        output: format!("(cancelled: {})", command),
                        offload_path: None,
                    });
                    return true;
                } else {
                    let cancel_msg = format!(
                        "\x1b[33m{}\x1b[0m\r\n",
                        aish_i18n::t("shell.command_cancelled")
                    );
                    unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            cancel_msg.as_ptr() as *const libc::c_void,
                            cancel_msg.len(),
                        );
                    }
                    let _ = output_sender.send(crate::session_interceptor::BashExecResult {
                        output: format!("(cancelled: {})", command),
                        offload_path: None,
                    });
                }
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Forward PTY output while waiting for LLM.
                // Also monitor stdin for Ctrl+C cancellation.
                let max_fd = master_fd.max(stdin_fd);
                let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
                unsafe {
                    libc::FD_ZERO(&mut rfds);
                    libc::FD_SET(master_fd, &mut rfds);
                    libc::FD_SET(stdin_fd, &mut rfds);
                }
                let mut tv = libc::timeval {
                    tv_sec: 0,
                    tv_usec: 50_000, // 50ms
                };
                let sel = unsafe {
                    libc::select(
                        max_fd + 1,
                        &mut rfds,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut tv,
                    )
                };
                // Check stdin for Ctrl+C
                if sel > 0 && unsafe { libc::FD_ISSET(stdin_fd, &rfds) } {
                    let mut tmp = [0u8; 1];
                    if unsafe { libc::read(stdin_fd, tmp.as_mut_ptr() as *mut libc::c_void, 1) }
                        == 1
                        && tmp[0] == 0x03
                    {
                        unsafe {
                            libc::write(master_fd, b"\x03".as_ptr() as *const libc::c_void, 1);
                        }
                        // Signal the LLM thread to stop
                        let cancel_msg = format!(
                            "\x1b[33m{}\x1b[0m\r\n",
                            aish_i18n::t("shell.command_cancelled")
                        );
                        unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                cancel_msg.as_ptr() as *const libc::c_void,
                                cancel_msg.len(),
                            );
                        }
                        return true;
                    }
                }
                if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &rfds) } {
                    let mut tmp = [0u8; 4096];
                    match unsafe {
                        libc::read(master_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                    } {
                        n if n > 0 => unsafe {
                            libc::write(
                                libc::STDOUT_FILENO,
                                tmp.as_ptr() as *const libc::c_void,
                                n as usize,
                            );
                        },
                        _ => {}
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
    }
    false
}

impl Drop for PersistentPty {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- Child process ----

fn child_main(slave_fd: RawFd, control_write_fd: RawFd, rcfile_path: &str, cwd: &str) -> ! {
    unsafe { libc::setsid() };
    unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) };

    let _ = dup2(slave_fd, libc::STDIN_FILENO);
    let _ = dup2(slave_fd, libc::STDOUT_FILENO);
    let _ = dup2(slave_fd, libc::STDERR_FILENO);

    if slave_fd > 2 {
        let _ = close(slave_fd);
    }

    // Set CWD.
    let _ = std::env::set_current_dir(cwd);

    // Set env.
    std::env::set_var("TERM", "xterm-256color");

    // dup2 control_write_fd to fd 3 if it's not already.
    if control_write_fd != 3 {
        let rc = dup2(control_write_fd, 3);
        if rc.is_err() {
            let msg = b"aish: dup2 control_write_fd to fd 3 failed\n";
            unsafe {
                libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            }
            unsafe {
                libc::_exit(126);
            }
        }
        let _ = close(control_write_fd);
    }
    std::env::set_var("AISH_CONTROL_FD", "3");

    let c_shell = CString::new("/bin/bash").unwrap();
    let c_rcfile = CString::new(rcfile_path).unwrap();
    let c_interactive = CString::new("-i").unwrap();
    let c_rcfile_flag = CString::new("--rcfile").unwrap();

    let args = vec![c_shell.clone(), c_rcfile_flag, c_rcfile, c_interactive];

    let _ = execvp(&c_shell, &args);

    // execvp failed.
    unsafe {
        libc::_exit(127);
    }
}

// ---- Helpers ----

fn set_nonblocking(fd: &OwnedFd) -> aish_core::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = fcntl(raw, FcntlArg::F_GETFL)
        .map_err(|e| AishError::Pty(format!("fcntl F_GETFL failed: {e}")))?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(raw, FcntlArg::F_SETFL(flags))
        .map_err(|e| AishError::Pty(format!("fcntl F_SETFL O_NONBLOCK failed: {e}")))?;
    Ok(())
}

fn sync_window_size(src_fd: RawFd, dst_fd: RawFd) -> nix::Result<()> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(src_fd, libc::TIOCGWINSZ, &mut ws) };
    if rc >= 0 {
        unsafe {
            libc::ioctl(dst_fd, libc::TIOCSWINSZ, &ws);
        }
    }
    Ok(())
}

fn kill_pg(pid: Pid, sig: Signal) -> nix::Result<()> {
    kill(Pid::from_raw(-pid.as_raw()), sig)
}

/// Get the foreground process group currently attached to the PTY.
/// On the master fd this returns the foreground group of the PTY session
/// (e.g. a pager like `less` invoked by `nmcli -p`). Returns None if the
/// ioctl is unsupported or the PTY has no foreground group yet.
fn pty_foreground_pgrp(master_fd: RawFd) -> Option<Pid> {
    let mut pgrp: libc::pid_t = 0;
    let rc = unsafe { libc::ioctl(master_fd, libc::TIOCGPGRP, &mut pgrp) };
    if rc == 0 && pgrp > 0 {
        Some(Pid::from_raw(pgrp))
    } else {
        None
    }
}

/// Forcefully cancel the foreground job of the PTY. Interactive pagers
/// (`less`/`more`/`man`) swallow SIGINT, so after the Ctrl+C byte is sent we
/// escalate to SIGTERM/SIGKILL against the foreground process group. The
/// bash process group (`child_pid`) is never killed directly: bash is the
/// long-lived session leader and must survive so the next command can run.
fn force_cancel_pty_foreground(master_fd: RawFd, child_pid: Pid) {
    if let Some(fg) = pty_foreground_pgrp(master_fd) {
        if fg != child_pid {
            let _ = kill_pg(fg, Signal::SIGTERM);
            std::thread::sleep(Duration::from_millis(100));
            // Re-check the foreground group before escalating: SIGTERM usually
            // terminates the pager, after which bash reclaims the foreground
            // (group == child_pid) or starts a new job. Only send SIGKILL when
            // the same foreground group is still active, so we never signal a
            // recycled or unrelated PGID.
            if pty_foreground_pgrp(master_fd) == Some(fg) {
                let _ = kill_pg(fg, Signal::SIGKILL);
            }
        }
    }
}

fn child_has_exited(pid: Pid) -> bool {
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => return true,
            Ok(WaitStatus::StillAlive) => return false,
            Ok(_) => return false,
            Err(nix::errno::Errno::ECHILD) => return true,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return false,
        }
    }
}

fn wait_for_child_exit(pid: Pid, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if child_has_exited(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child_has_exited(pid)
}

fn reap_child(pid: Pid) {
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => return,
            Ok(WaitStatus::StillAlive) | Ok(_) => match waitpid(pid, None) {
                Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => return,
                Err(nix::errno::Errno::ECHILD) => return,
                Err(nix::errno::Errno::EINTR) => continue,
                _ => return,
            },
            Err(nix::errno::Errno::ECHILD) => return,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return,
        }
    }
}

/// Write the rc wrapper as a unique file under `base` (normally the system
/// temp dir). No subdirectory is created: a shared `aish-rc` dir breaks when the
/// first creator (often root via `sudo aish`) owns it and later users cannot write.
fn write_rcfile_temp_in(base: &std::path::Path) -> aish_core::Result<std::path::PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = base.join(format!("aish-rc-{}-{}.sh", getuid(), uuid::Uuid::new_v4()));
    // Create with 0600 atomically so the file is never briefly world-readable.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| AishError::Pty(format!("failed to write rcfile temp: {e}")))?;
    f.write_all(BASH_RC_WRAPPER.as_bytes())
        .map_err(|e| AishError::Pty(format!("failed to write rcfile temp: {e}")))?;
    Ok(path)
}

/// Write the rc wrapper script to a temp file and return the path.
fn write_rcfile_temp() -> aish_core::Result<std::path::PathBuf> {
    write_rcfile_temp_in(&std::env::temp_dir())
}

/// Simple shell quoting for embedding a command in a bash assignment.
pub fn shell_quote_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Check if a command needs a full interactive terminal.
pub fn is_interactive_command(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or("");
    let basename = first.rsplit('/').next().unwrap_or(first);
    if INTERACTIVE_COMMANDS.contains(&basename) {
        return true;
    }
    // sudo/su with interactive flags.
    if basename == "sudo" || basename == "su" {
        let lower = command.to_lowercase();
        if lower.contains("-i") || lower.contains("-s") || lower.contains("bash") {
            return true;
        }
    }
    false
}

/// Check if a command is an interactive session command (ssh/telnet etc.)
fn is_session_command(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or("");
    let basename = first.rsplit('/').next().unwrap_or(first);
    SESSION_COMMANDS.contains(&basename)
}

/// Parsed SSH/session command. Structured form preserving the user and
/// ProxyJump chain needed for environment-aware PS1 injection.
#[derive(Debug)]
pub(crate) struct SshCommandInfo {
    pub user: Option<String>,
    pub host: String,
    pub jump_chain: Vec<String>,
    /// Original first non-option token verbatim (`user@host` or `host`).
    pub dest_raw: String,
}

impl SshCommandInfo {
    /// Render the jump chain as ` ⤴ j1,j2` or empty string.
    pub fn display_jumps(&self) -> String {
        if self.jump_chain.is_empty() {
            String::new()
        } else {
            format!(" ⤴ {}", self.jump_chain.join(","))
        }
    }

    /// Danger from static signals: `root` user or hostname matching one of
    /// the pre-compiled patterns.
    pub fn danger_static(&self, patterns: &[regex::Regex]) -> DangerLevel {
        if self.user.as_deref() == Some("root") {
            return DangerLevel::Danger;
        }
        for re in patterns {
            if re.is_match(&self.host) {
                return DangerLevel::Danger;
            }
        }
        DangerLevel::None
    }
}

/// Remote shell family inferred from prompt shape.
#[derive(Clone, Debug)]
pub(crate) enum ShellKind {
    Bash,
    Zsh,
    Fish,
}

/// One-shot probe result collected at first prompt. Static segments in
/// the PS1 literal (container / kube context) come from here.
pub(crate) struct RemoteContextSnapshot {
    pub container: Option<String>,
    pub shell_type: ShellKind,
    /// True when `kube_context` matches the prod regex set. Set by the
    /// caller of `probe_remote_command`, not by the parser itself.
    pub is_kube_prod: bool,
    pub kube_context: Option<String>,
}

impl RemoteContextSnapshot {
    /// Fallback snapshot used when the probe fails or times out. All
    /// optional fields None; only `shell_type` is populated.
    pub fn minimal(shell: ShellKind) -> Self {
        Self {
            container: None,
            shell_type: shell,
            kube_context: None,
            is_kube_prod: false,
        }
    }

    pub fn kube_danger(&self) -> DangerLevel {
        if self.is_kube_prod {
            DangerLevel::Danger
        } else {
            DangerLevel::None
        }
    }
}

/// Default prod-context prefixes. Matched against the kube context name
/// via `starts_with` only (NOT regex, NOT `contains`). Prefix-match is
/// intentionally conservative: it catches `prod-cluster`, `prd-blue`,
/// `production-west` while excluding false positives like `preprod-green`,
/// `staging-prd-blue`, `eu-prod-mirror-west` (which often denote
/// non-prod environments despite containing a `prod`/`prd` token). This
/// mirrors the anchored `^prod-`/`^prd-` semantics used for hostname
/// danger patterns in `aish-config`.
//
// NOTE: not yet surfaced as a user-configurable knob (the parallel
// `remote_danger_patterns` config covers hostnames only). If callers need
// to override, they must edit this constant and rebuild.
const DEFAULT_KUBE_PROD_PATTERNS: &[&str] = &["prod", "prd", "production"];

/// Parse the marker-delimited body emitted by the probe command. Returns
/// None if either start or end marker is missing. Field lines are
/// permissive: trailing `\r` stripped, empty strings allowed.
pub(crate) fn parse_probe_output(raw: &str) -> Option<RemoteContextSnapshot> {
    // Use the LAST occurrence of the start marker rather than the first.
    // On a real PTY with ECHO enabled (the default for ssh sessions), bash
    // echoes the typed probe command BEFORE executing it, so the byte stream
    // looks like:
    //   <echo @@aish_ctx_start@@\r\n>   <- command echo (typed input reflected)
    //   <@@aish_ctx_start@@\r\n>        <- actual marker output
    //   <id -u ... output>\r\n
    //   ...
    // The first `find` hits the echoed command line and `body` ends up off by
    // one row (uid/os/container/kube all shifted). The real marker output is
    // always emitted AFTER the echo, so `rfind` correctly skips the echo.
    let start = raw.rfind("@@aish_ctx_start@@")?;
    let after_start = &raw[start + "@@aish_ctx_start@@".len()..];
    let end = after_start.find("@@aish_ctx_end@@")?;
    // Skip the single line terminator immediately following the start marker
    // (the marker is emitted on its own line: `... echo @@aish_ctx_start@@\n`).
    // Without this, `lines()` would yield a spurious leading empty field and
    // shift uid/container/kube by one row.
    let body_raw = &after_start[..end];
    let body = body_raw
        .strip_prefix("\r\n")
        .or_else(|| body_raw.strip_prefix('\n'))
        .unwrap_or(body_raw);

    let mut lines = body.lines();
    let _uid = lines.next()?; // consumed but unused (root via $EUID in hook)
    let container = lines.next().and_then(|l| {
        let t = trim_probe_line(l);
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    });
    let kube_raw = lines.next().map(trim_probe_line);
    let kube_context = kube_raw
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let is_kube_prod = kube_context.as_deref().is_some_and(|ctx| {
        DEFAULT_KUBE_PROD_PATTERNS
            .iter()
            .any(|p| ctx.starts_with(p))
    });

    Some(RemoteContextSnapshot {
        container,
        shell_type: ShellKind::Bash,
        kube_context,
        is_kube_prod,
    })
}

/// Send the marker probe to the remote shell and parse the result.
/// Returns None on 5s timeout or when markers are absent from output.
/// Caller is responsible for ensuring this runs only at first prompt
/// detection (gated by `ps1_marker_done_for`).
///
/// In addition to the parsed snapshot, returns the **residual bytes** that
/// were read during the 5s probe window but lie OUTSIDE the recognized
/// marker-delimited region (e.g. reconnected tmux output, delayed MOTD,
/// async notifications, resize echoes). The caller MUST re-inject these
/// into the normal UI data stream so they are not silently swallowed.
/// Bytes inside the residual are the unrecognized content; on success the
/// probe body itself is consumed and not duplicated into the residual.
#[allow(unused_assignments)]
pub(crate) fn probe_remote_command(master_fd: i32) -> (Option<RemoteContextSnapshot>, Vec<u8>) {
    // Guard against fd >= FD_SETSIZE: FD_SET would write past the bitmap
    // end and corrupt the stack. In practice openpty returns low fds, but
    // a long-running session that has cycled through many socket/timer
    // fds can produce master_fd >= 1024. Bail out gracefully rather than
    // corrupt memory.
    if master_fd < 0 || master_fd as usize >= libc::FD_SETSIZE {
        debug!(
            master_fd,
            "probe_remote_command: fd outside FD_SETSIZE range, skipping probe"
        );
        return (None, Vec::new());
    }
    // Multi-command one-shot: id/uname/container/kubectl. Each command
    // silent on failure (`2>/dev/null`, `|| true`). Markers wrap the
    // output so we can parse reliably regardless of which sub-commands
    // returned empty.
    let probe_cmd = concat!(
        " echo @@aish_ctx_start@@;",
        " id -u 2>/dev/null;",
        " [ -f /.dockerenv ] && echo docker;",
        " [ -f /run/.containerenv ] && echo podman;",
        " command -v kubectl >/dev/null 2>&1 && kubectl config current-context 2>/dev/null;",
        " echo @@aish_ctx_end@@\r",
    );
    let cmd_bytes = probe_cmd.as_bytes();
    let write_rc = unsafe {
        libc::write(
            master_fd,
            cmd_bytes.as_ptr() as *const libc::c_void,
            cmd_bytes.len(),
        )
    };
    if write_rc < 0 {
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        debug!(
            master_fd,
            err, "probe_remote_command: write failed, aborting probe"
        );
        return (None, Vec::new());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    let mut idle_polls: u32 = 0;
    let mut saw_end_marker = false;

    while std::time::Instant::now() < deadline {
        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rfds);
            libc::FD_SET(master_fd, &mut rfds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 100_000,
        };
        let sel = unsafe {
            libc::select(
                master_fd + 1,
                &mut rfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            // EINTR is benign (signal during select); keep polling until
            // deadline. Anything else (EBADF, EINVAL) means the fd is no
            // longer usable — abort so we don't spin for 5s.
            if err != libc::EINTR {
                debug!(
                    master_fd,
                    err, "probe_remote_command: select failed, aborting probe"
                );
                break;
            }
            continue;
        }
        if sel > 0 && unsafe { libc::FD_ISSET(master_fd, &rfds) } {
            let n =
                unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                output.extend_from_slice(&buf[..n as usize]);
                idle_polls = 0;
                if let Ok(s) = std::str::from_utf8(&output) {
                    if s.contains("@@aish_ctx_end@@") {
                        saw_end_marker = true;
                        // One more short drain to catch trailing bytes.
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        while let Some(n) = try_read_fd(master_fd, &mut buf) {
                            output.extend_from_slice(&buf[..n]);
                            if n < buf.len() {
                                break;
                            }
                        }
                        break;
                    }
                }
            } else if n == 0 {
                break;
            } else {
                // n < 0: read error. EAGAIN/EWOULDBLOCK should not reach
                // here (select said readable), so treat any error as
                // fatal for this probe — abort instead of spinning.
                let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if err != libc::EAGAIN && err != libc::EWOULDBLOCK {
                    debug!(
                        master_fd,
                        err, "probe_remote_command: read failed, aborting probe"
                    );
                    break;
                }
            }
        } else {
            idle_polls += 1;
            if !output.is_empty() && idle_polls > 5 && saw_end_marker {
                break;
            }
        }
    }

    let raw = String::from_utf8_lossy(&output);
    let residual = compute_probe_residual(&raw);
    (parse_probe_output(&raw), residual)
}

/// Compute the bytes that fall AFTER the recognized marker-delimited probe
/// region. These trailing bytes are real user-visible output that arrived
/// during the 5s probe window after the marker body (e.g. the next shell
/// prompt, resize echoes, async notifications). The caller must re-inject
/// them into the UI stream so they aren't silently dropped.
///
/// Bytes BEFORE the last `@@aish_ctx_start@@` are deliberately NOT returned.
/// On a real PTY with ECHO enabled, that prefix contains the probe command
/// echo (` echo @@aish_ctx_start@@; id -u; ...; echo @@aish_ctx_end@@\r\n`)
/// reflected by bash before execution. Returning those bytes to the UI
/// leaks the command literal to the user's terminal. The rare cost: if a
/// real async output byte arrives interleaved with the command echo, it is
/// dropped. This is an acceptable tradeoff — such interleaving is
/// vanishingly rare (probe fires once per host on first prompt, when the
/// remote is otherwise idle), while the command-echo leak is deterministic.
///
/// If markers are absent or only the start marker is present, returns empty
/// — in those failure modes the entire `raw` is dominated by the echoed
/// command, so re-injecting it would only ever cause the leak described
/// above.
pub(crate) fn compute_probe_residual(raw: &str) -> Vec<u8> {
    let start_tok = "@@aish_ctx_start@@";
    let end_tok = "@@aish_ctx_end@@";
    let bytes = raw.as_bytes();

    // No start marker: probe never produced output. The raw buffer holds the
    // echoed command at most; drop it to avoid leaking the probe literal.
    let Some(start) = raw.rfind(start_tok) else {
        return Vec::new();
    };
    let after_start = &raw[start + start_tok.len()..];
    // No end marker after start: probe incomplete/timed out. Whatever is in
    // `raw` is the echoed command plus a partial body; nothing safe to
    // re-inject.
    let Some(relative_end) = after_start.find(end_tok) else {
        return Vec::new();
    };
    let end = start + start_tok.len() + relative_end + end_tok.len();
    // Trailing bytes only: anything after the end marker is real output that
    // arrived post-probe (next prompt, resize echoes, etc.).
    bytes[end..].to_vec()
}

/// Non-blocking read helper. Returns Some(n) on success (including n==0 for
/// EOF), None on EAGAIN/EWOULDBLOCK.
fn try_read_fd(fd: i32, buf: &mut [u8]) -> Option<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n >= 0 {
        Some(n as usize)
    } else {
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
            None
        } else {
            Some(0)
        }
    }
}

/// Strip trailing `\r` (added by PTY OPOST) and trim surrounding whitespace.
fn trim_probe_line(s: &str) -> String {
    s.trim_end_matches('\r').trim().to_string()
}

/// Two-state danger flag for PS1 coloring. Container marks don't escalate.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DangerLevel {
    None,
    Danger,
}

impl DangerLevel {
    /// Max of two levels — Danger dominates None.
    pub fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Danger, _) | (_, Self::Danger) => Self::Danger,
            _ => Self::None,
        }
    }
}

/// Parse an SSH/telnet/mosh/sftp/nc/netcat command into structured form.
/// Returns None for any other command (vim, ls, scp, etc.).
pub(crate) fn parse_ssh_command(command: &str) -> Option<SshCommandInfo> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let cmd = parts.first()?;
    if !matches!(*cmd, "ssh" | "telnet" | "mosh" | "sftp" | "nc" | "netcat") {
        return None;
    }
    // Options that take an argument. `-J` is preserved into jump_chain.
    let opts_with_arg: &[&str] = &[
        "-p", "-l", "-i", "-o", "-L", "-R", "-S", "-W", "-J", "-b", "-c", "-F", "-I", "-K", "-m",
        "-Q", "-q",
    ];
    let mut jump_chain: Vec<String> = Vec::new();
    let mut iter = parts.iter().skip(1).peekable();
    while let Some(part) = iter.next() {
        if part.starts_with('-') {
            let opt_name = if let Some(eq) = part.find('=') {
                &part[..eq]
            } else {
                *part
            };
            let combined_with_eq = part.contains('=');
            if opts_with_arg.contains(&opt_name) && !combined_with_eq {
                if let Some(arg) = iter.next() {
                    if opt_name == "-J" {
                        jump_chain.extend(arg.split(',').map(|s| s.to_string()));
                    }
                }
            }
            continue;
        }
        // First non-option token is the destination.
        let dest_raw = part.to_string();
        let (user, host) = if let Some(at) = dest_raw.rfind('@') {
            (
                Some(dest_raw[..at].to_string()),
                dest_raw[at + 1..].to_string(),
            )
        } else {
            (None, dest_raw.clone())
        };
        return Some(SshCommandInfo {
            user,
            host,
            jump_chain,
            dest_raw,
        });
    }
    None
}

/// Default connection-timeout (seconds) injected into ssh commands when the
/// user has not specified one. Chosen to be short enough that an unreachable
/// host fails fast (the user sees the error in ~5s rather than waiting for
/// the Linux kernel's ~127s SYN retry) but long enough to accommodate slow
/// networks and high-latency links. The ssh client's `ConnectTimeout`
/// option aborts the connection attempt client-side via an alarm signal,
/// interrupting the kernel `connect()` syscall even though the syscall
/// itself is normally uninterruptible (D-state) — this is what makes the
/// timeout effective where Ctrl+C alone is not.
const DEFAULT_SSH_CONNECT_TIMEOUT: u32 = 5;

/// Rewrite an ssh command line to inject `-o ConnectTimeout=N` when no
/// explicit ConnectTimeout is already present. Returns the original line
/// unchanged when:
///   - the command is not an ssh invocation (basename != `ssh`)
///   - the line already mentions `ConnectTimeout` (case-sensitive — ssh's
///     option name is conventionally camel-case)
///   - the line is empty or whitespace-only
///
/// Leading whitespace and trailing characters (including any `\n`) are
/// preserved so the caller can write the result back to the PTY without
/// further adjustment.
///
/// Rationale: without this rewrite, ssh falls back to the kernel TCP SYN
/// retry schedule (~127s on default Linux) when the target is unreachable.
/// During this window the ssh process is in D-state (uninterruptible
/// sleep) inside `connect()` and SIGINT cannot abort it — the user sees
/// aish hang for tens of seconds to a couple of minutes regardless of how
/// many times they press Ctrl+C.
fn inject_ssh_connect_timeout(line: &str, timeout_secs: u32) -> String {
    if line.trim().is_empty() {
        return line.to_string();
    }
    if line.contains("ConnectTimeout") {
        return line.to_string();
    }
    let leading_ws_len = line.len() - line.trim_start().len();
    let after_leading = &line[leading_ws_len..];
    let first_token_end = after_leading
        .find(|c: char| c.is_whitespace())
        .unwrap_or(after_leading.len());
    let first_token = &after_leading[..first_token_end];
    let basename = first_token.rsplit('/').next().unwrap_or(first_token);
    if basename != "ssh" {
        return line.to_string();
    }
    let mut result = String::with_capacity(line.len() + 24);
    result.push_str(&line[..leading_ws_len]);
    result.push_str(first_token);
    result.push_str(" -o ConnectTimeout=");
    result.push_str(&timeout_secs.to_string());
    result.push_str(&after_leading[first_token_end..]);
    result
}

/// Handle dossier commands (;remember, ;notes, ;forget, ;refresh).
/// Returns Some(display_text) if handled, None to fall through to AI.
fn handle_dossier_command(question: &str, remote_host: Option<&str>) -> Option<String> {
    let host_key = remote_host?;
    let lower = question.to_lowercase();

    // Strip prefix helper: returns content after prefix, handling both
    // "verb " (with space) and "verb" (without space, for Chinese).
    fn strip_prefix<'a>(s: &'a str, prefix_with: &str, prefix_without: &str) -> Option<&'a str> {
        if s.starts_with(prefix_with) {
            Some(&s[prefix_with.len()..])
        } else if s.starts_with(prefix_without) && s.len() > prefix_without.len() {
            // For Chinese verbs, users often don't add a space (e.g., "记住这个")
            let rest = &s[prefix_without.len()..];
            if rest.starts_with(char::is_whitespace) {
                Some(rest.trim_start())
            } else {
                Some(rest)
            }
        } else {
            None
        }
    }

    if let Some(content) = strip_prefix(&lower, "remember ", "remember")
        .or_else(|| strip_prefix(question, "记住 ", "记住"))
    {
        let content = content.trim().to_string();
        if content.is_empty() {
            return Some("Empty note, nothing saved.".to_string());
        }
        let mut profile = aish_hosts::get_or_create_profile(host_key);
        let id = profile.add_note(content);
        match aish_hosts::save_profile(&profile) {
            Ok(()) => Some(format!("Note #{} saved to {}", id, host_key)),
            Err(e) => Some(format!("Failed to save: {}", e)),
        }
    } else if lower == "notes" || lower == "档案" || lower == "dossier" {
        match aish_hosts::load_profile(host_key) {
            Some(profile) => Some(profile.format_display()),
            None => Some(format!("No profile found for {}", host_key)),
        }
    } else if let Some(keyword) = strip_prefix(&lower, "forget ", "forget")
        .or_else(|| strip_prefix(question, "忘记 ", "忘记"))
    {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Some("Usage: ;forget <keyword>".to_string());
        }
        match aish_hosts::load_profile(host_key) {
            Some(mut profile) => {
                let removed = profile.remove_notes(keyword);
                if removed > 0 {
                    let _ = aish_hosts::save_profile(&profile);
                    Some(format!(
                        "Removed {} note(s) matching '{}'",
                        removed, keyword
                    ))
                } else {
                    Some(format!("No notes matching '{}'", keyword))
                }
            }
            None => Some(format!("No profile found for {}", host_key)),
        }
    } else if lower == "refresh" {
        match aish_hosts::load_profile(host_key) {
            Some(mut profile) => {
                profile.system = aish_hosts::SystemInfo::default();
                let _ = aish_hosts::save_profile(&profile);
                Some(format!(
                    "Profile for {} cleared. Re-probe on next connection.",
                    host_key
                ))
            }
            None => Some(format!("No profile found for {}", host_key)),
        }
    } else {
        None
    }
}

/// Strip ANSI escape sequences (CSI, OSC, two-byte) from a string.
/// Uses byte-level scanning for escape detection but preserves UTF-8
/// multi-byte characters by extracting them as complete code points.
fn strip_ansi_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    // CSI sequence: skip parameter/intermediate bytes + final byte
                    i += 2;
                    while i < bytes.len() && !((0x40..=0x7e).contains(&bytes[i])) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
                b']' => {
                    // OSC sequence: skip until BEL or ST
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 2; // two-byte escape
                }
            }
        } else if bytes[i] == 0x0d {
            // Treat bare CR (without LF) as whitespace — shells use
            // CR to redraw the current line during interactive editing.
            i += 1;
        } else if bytes[i] == 0x08 {
            // BS (backspace) — remove last char from result.
            // Terminals use BS to correct typos in echoed input.
            if !result.is_empty() {
                pop_last_utf8_from_string(&mut result);
            }
            i += 1;
        } else {
            // Preserve complete UTF-8 characters instead of pushing
            // individual bytes (which would break CJK/multi-byte text).
            let start = i;
            if bytes[i] & 0x80 == 0 {
                // ASCII — single byte
                i += 1;
            } else if bytes[i] & 0xE0 == 0xC0 {
                // 2-byte UTF-8
                i += 2;
            } else if bytes[i] & 0xF0 == 0xE0 {
                // 3-byte UTF-8
                i += 3;
            } else if bytes[i] & 0xF8 == 0xF0 {
                // 4-byte UTF-8
                i += 4;
            } else {
                // Stray continuation byte — skip
                i += 1;
                continue;
            }
            // Clamp to slice bounds in case of truncated UTF-8
            let end = i.min(bytes.len());
            if end > start {
                // from_utf8 is safe here because the input `s` is valid UTF-8
                // and we are slicing at character boundaries (or skipping).
                result.push_str(&s[start..end]);
            }
        }
    }
    result
}

/// Pop the last complete UTF-8 character from a String.
fn pop_last_utf8_from_string(s: &mut String) {
    let last_char_len = s.chars().last().map(|c| c.len_utf8()).unwrap_or(0);
    s.truncate(s.len() - last_char_len);
}

/// Strip ANSI escapes and trim trailing shell prompt from captured output.
/// Removes the last non-empty line (typically a prompt like `user@host:~$ `).
fn strip_ansi_and_prompt(raw: &str) -> String {
    let clean = strip_ansi_escapes(raw);
    let mut lines: Vec<&str> = clean.lines().collect();
    // Remove trailing empty lines
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    // Remove last non-empty line (shell prompt)
    if !lines.is_empty() {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

/// Clean PTY output: strip ANSI, command echo, trailing prompt.
fn clean_pty_output(raw: &str, command: &str) -> String {
    // Strip ANSI escape sequences.
    let re = regex_simple();
    let text = re.replace_all(raw, "").to_string();

    // CRLF -> LF.
    let text = text.replace("\r\n", "\n").replace('\r', "");

    // Remove command echo.
    let cmd_trimmed = command.trim();
    if let Some(pos) = text.find(cmd_trimmed) {
        let after = &text[pos + cmd_trimmed.len()..];
        // Skip to next newline after the echo.
        if let Some(nl) = after.find('\n') {
            let cleaned = after[nl + 1..].to_string();
            return strip_auth_interaction_noise(&cleaned, command);
        }
    }

    strip_auth_interaction_noise(&text, command)
}

fn strip_auth_interaction_noise(text: &str, command: &str) -> String {
    if !command_may_prompt_for_auth(command) {
        return text.trim().to_string();
    }

    text.lines()
        .filter(|line| !is_auth_interaction_line(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn command_may_prompt_for_auth(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.contains("sudo")
        || lower.starts_with("su ")
        || lower == "su"
        || lower.contains(" su ")
        || lower.starts_with("ssh ")
        || lower.contains(" ssh ")
}

fn is_auth_interaction_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lower = trimmed.to_lowercase();
    lower.contains("password:")
        || lower.ends_with("password")
        || lower.contains("password for ")
        || trimmed.contains("请输入密码")
        || trimmed.contains("密码：")
        || trimmed.contains("验证成功")
        || trimmed.contains("认证成功")
        || lower.contains("authentication successful")
        || lower.contains("authentication succeeded")
}

fn regex_simple() -> regex::Regex {
    regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap()
}

/// Detect unclosed heredoc in a shell command and close it.
/// Returns the command with missing heredoc closing delimiters appended.
/// e.g. "cat > f << 'EOF'" → "cat > f << 'EOF'\nEOF"
fn close_unclosed_heredoc(cmd: &str) -> String {
    let bytes = cmd.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    let mut result = cmd.to_string();
    let mut appended = false;

    while i + 1 < len {
        if bytes[i] == b'<' && bytes[i + 1] == b'<' {
            // Skip << and optional -
            let mut j = i + 2;
            if j < len && bytes[j] == b'-' {
                j += 1;
            }
            // Skip whitespace
            while j < len && bytes[j] == b' ' {
                j += 1;
            }
            // Skip optional quote
            if j < len && (bytes[j] == b'\'' || bytes[j] == b'"') {
                j += 1;
            }
            // Extract delimiter word
            let delim_start = j;
            while j < len
                && ![b' ', b'\n', b'\r', b';', b'&', b'|', b'<', b'>', b'#'].contains(&bytes[j])
                && bytes[j] != b'\''
                && bytes[j] != b'"'
            {
                j += 1;
            }
            let delimiter = &cmd[delim_start..j];

            if !delimiter.is_empty() {
                // Check if delimiter appears as a standalone line after the <<
                let search_start = if appended { 0 } else { j.min(len) };
                let rest = &result[search_start..];
                let closed = rest.lines().any(|line| line.trim() == delimiter);
                if !closed {
                    result.push('\n');
                    result.push_str(delimiter);
                    appended = true;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    result
}

/// Detect if PTY output looks like a continuation prompt (PS2: `> `).
/// Used to detect stuck heredoc/quote states after command injection.
fn looks_like_continuation_prompt(output: &[u8]) -> bool {
    if output.is_empty() {
        return false;
    }
    let stripped = strip_ansi_escapes(&String::from_utf8_lossy(output));
    let lines: Vec<&str> = stripped.lines().collect();
    if let Some(last_line) = lines.last() {
        let trimmed_line = last_line.trim();
        return trimmed_line == ">" || trimmed_line.ends_with("> ");
    }
    false
}

/// Scan PTY output for an SSH command and extract the target host.
/// Only checks complete lines (terminated by \r\n) so that partial
/// typing echoes don't trigger false positives.
///
/// Accepts one shape:
/// * Line has ` ssh ` in the middle, with the **prefix ending in a shell
///   prompt terminator** (`#`, `$`, `%`, `>`). This is the shape produced
///   when the user runs ssh from inside an already-connected remote bash
///   — bash redraws the line as `<prompt> ssh ...`, then echoes it on
///   Enter. The terminator check rejects command output that happens to
///   mention ssh (e.g. `w | grep ssh` prints `root pts/1 ... 0.05s ssh
///   -l root 10.10.17.243` — the prefix ends with `s`, not a prompt
///   terminator).
///
/// Bare `ssh ` at the start of a line is intentionally NOT accepted: the
/// outer session's initial ssh command is echoed verbatim at the top of
/// the PTY (no prompt prefix), and treating it as a nested session would
/// push a fake stack frame and re-enable PS1 injection against the local
/// prompt after disconnect. Callers that need to detect a freshly submitted
/// outer ssh command should use stdin shadowing, not this scan.
fn scan_output_for_ssh_host(output: &str) -> Option<String> {
    // Only scan up to the last newline — anything after it is an
    // incomplete line (user still typing, reverse-i-search, etc.).
    let scanable = match output.rfind('\n') {
        Some(pos) => &output[..pos + 1],
        None => return None,
    };
    let clean = strip_ansi_escapes(scanable);
    for line in clean.lines() {
        let line = line.trim();
        // Skip reverse-i-search lines — they show history matches, not
        // executed commands.  When the user confirms a search with Enter,
        // the command appears as a normal line in subsequent PTY output.
        let is_isearch = line.contains("(reverse-i-search)")
            || line.contains("(i-search)")
            || line.contains("(failed reverse-i-search)")
            || line.contains("(failed i-search)");
        if is_isearch {
            continue;
        }
        // Locate where the `ssh ...` part starts in the line. Only accept
        // the `<prompt> ssh ...` shape (ssh appears after a space, preceded
        // by a prompt terminator). A bare `ssh ...` at the start of a line
        // is the outer session's own command echo and must NOT be treated
        // as a nested session — see the function doc comment.
        //
        // `before` is everything in the line up to (but excluding) the
        // leading space of ` ssh ` — so for `[root@host ~]# ssh ...` it is
        // `[root@host ~]#`, ending in `#`.
        //
        // We match against `line` directly (not `line.to_lowercase()`)
        // because the ssh command is itself lowercase — `SSH user@host` is
        // not a valid command and bash would reject it. Skipping the
        // lowercase conversion avoids a UTF-8 panic: `str::to_lowercase`
        // can change byte length for some Unicode chars (e.g. `İ` → `i̇`),
        // so an offset found in the lowercased string might not land on a
        // char boundary when used to slice the original `line`.
        let idx = match line.find(" ssh ") {
            Some(idx) => idx,
            None => continue,
        };
        let before = &line[..idx];
        let after = &line[idx + 1..];
        // Prefix must end with a shell prompt terminator — otherwise this
        // is command output that merely mentions ssh.
        let prompt_terminated = before.ends_with('#')
            || before.ends_with('$')
            || before.ends_with('%')
            || before.ends_with('>');
        if !prompt_terminated {
            continue;
        }
        if let Some(info) = parse_ssh_command(after.trim()) {
            if is_plausible_ssh_host(&info.dest_raw) {
                return Some(info.dest_raw);
            }
        }
    }
    None
}

/// Decide whether `host` looks like a real SSH destination (IPv4, hostname,
/// or `user@...`). Used to filter out garbage produced when
/// `output_ssh_scan` concatenates partial typing across chunks (e.g. the
/// user typed `ssh 10.10.17.243` and then `ssh` while poking around in
/// reverse-i-search — the accumulated buffer becomes `ssh 10.10.17.243ssh`,
/// which the extractor would otherwise happily return as a host).
fn is_plausible_ssh_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let host_part = host.rsplit('@').next().unwrap_or(host);
    if host_part.is_empty() {
        return false;
    }
    let labels: Vec<&str> = host_part.split('.').collect();
    // Strict IPv4: 4 octets, each 0-255.
    let is_strict_ipv4 = labels.len() == 4
        && labels.iter().all(|o| {
            !o.is_empty()
                && o.len() <= 3
                && o.chars().all(|c| c.is_ascii_digit())
                && o.parse::<u32>().map(|n| n <= 255).unwrap_or(false)
        });
    if is_strict_ipv4 {
        return true;
    }
    // 4-label all-numeric that is NOT strict IPv4 (e.g. `999.999.999.999`)
    // is never a valid hostname either — fall-through to the general
    // hostname check would accept it because each label is alphanumeric.
    // Reject explicitly so we don't poison remote_host_for_probe with an
    // impossible address.
    let is_ipv4_like = labels.len() == 4
        && labels
            .iter()
            .all(|o| !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()));
    if is_ipv4_like {
        return false;
    }
    // Corruption pattern: three leading octets look like IPv4 but the last
    // label glues digits together with trailing letters (e.g. "243ssh").
    // Real hostnames don't normally look like that.
    if labels.len() == 4
        && labels[..3]
            .iter()
            .all(|o| !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()))
        && labels[3].chars().any(|c| c.is_ascii_digit())
        && labels[3].chars().any(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    // General hostname: each label alphanumeric + hyphens, not at edges.
    labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_alphanumeric() || c == '-')
    })
}

/// Scan PTY output for SSH disconnection messages like
/// "Connection to X closed." and return the host that disconnected.
fn scan_output_for_disconnect(output: &str) -> Option<String> {
    let clean = strip_ansi_escapes(output);
    let marker = "connection to ";
    let lower = clean.to_lowercase();
    if let Some(start) = lower.find(marker) {
        let after = &clean[start + marker.len()..];
        // Extract host (everything up to " closed" or space)
        let host = after
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_string();
        if !host.is_empty() {
            return Some(host);
        }
    }
    None
}

/// Scan PTY output for signals that a nested ssh attempt has *successfully*
/// reached the auth phase or established a session. Used to gate PS1 marker
/// injection in nested-ssh scenarios so we don't inject a dead host's marker
/// when the user cancelled with Ctrl+C — Ctrl+C aborts ssh client-side
/// without emitting any error on stderr, bash simply returns to the outer
/// prompt, and without a positive success signal there is no way to tell
/// that scenario apart from a successful ssh that hasn't emitted MOTD yet.
///
/// Recognized success signals (case-insensitive, after ANSI stripping):
/// - `password:` / `password for` — ssh daemon accepted the TCP connection
///   and is requesting credentials. The connection itself succeeded.
/// - `last login:` — OpenSSH's default MOTD line after successful auth.
///
/// Matching is **line-anchored** for `last login:` (must appear at line
/// start) to avoid false positives like `# last login: 2024-...` in shell
/// comments. The password markers are matched anywhere because ssh's
/// `user@host's password:` prompt is itself a complete line.
fn scan_output_for_ssh_success(output: &str) -> bool {
    let clean = strip_ansi_escapes(output);
    for raw_line in clean.lines() {
        let line = raw_line.trim().to_lowercase();
        // OpenSSH password prompt: `user@host's password:` or bare
        // `password:`. Always at end of line (cursor waits for input
        // immediately after the colon). Reject mid-line occurrences like
        // `# I will password: protect this` (shell comments).
        if line.ends_with("password:") || line.ends_with("password: ") {
            return true;
        }
        // `password for X` is emitted by some ssh wrappers (Dropbear, sudo
        // prompts forwarded over ssh) — accept only at line start.
        if line.starts_with("password for ") {
            return true;
        }
        // OpenSSH MOTD line, always at start of line.
        if line.starts_with("last login:") {
            return true;
        }
    }
    false
}

/// Scan PTY output for SSH connection failure errors and return Some(())
/// when one is recognized. Used to roll back nested-SSH state when the
/// inner ssh command fails (host unreachable, refused, timed out, auth
/// failure, etc.) — without this the state machine keeps the dead host
/// as `remote_host_for_probe`, which triggers a 5-second probe block on
/// the next outer-shell prompt and effectively hangs the UI.
///
/// Matching is **line-anchored**: each marker must appear at the start
/// of a line (after stripping leading whitespace and ANSI escapes). This
/// excludes false positives from remote commands that print these
/// phrases mid-line — e.g. `ls: cannot open 'foo': Permission denied`
/// from an `ls` inside a working nested SSH session will NOT trigger a
/// rollback, while genuine `ssh: connect to host ...: Connection timed
/// out` will.
///
/// Recognized line-start markers (case-insensitive, after ANSI strip):
/// - `ssh: connect to host ...` (Network unreachable / refused / timed out /
///   no route; covers the bare-phrase variants `connection timed out`,
///   `no route to host`, `connection refused` via the `ssh:` prefix)
/// - `ssh: could not resolve hostname ...` (DNS failure)
/// - `kex_exchange_identification: ...` (Pre-auth protocol failure)
/// - `received disconnect from ...` (Server-initiated disconnect)
/// - `<user>@<host>: Permission denied (...)` or `<host>: Permission denied
///   (...)` — SSH auth failure. Requires the `(...)` auth-method suffix to
///   exclude generic file-access errors like `ls: ...: Permission denied`
///   or `bash: ./x: Permission denied`.
///
/// Returns `Some(())` (not the host) because the failure message format
/// varies and the host is not always extractable; callers only need to
/// know that the nested session will never produce a usable prompt.
fn scan_output_for_ssh_failure(output: &str) -> Option<()> {
    let clean = strip_ansi_escapes(output);
    for raw_line in clean.lines() {
        let line = raw_line.trim_start().to_lowercase();
        // ssh-prefixed errors. OpenSSH always emits these at line start.
        if line.starts_with("ssh: connect to host ")
            || line.starts_with("ssh: could not resolve hostname ")
            || line.starts_with("kex_exchange_identification")
            || line.starts_with("received disconnect from ")
        {
            return Some(());
        }
        // SSH auth failure: "user@host: Permission denied (publickey,password)."
        // The `(...)` suffix is mandatory — generic `bash: ./x: Permission
        // denied` and `ls: ...: Permission denied` lines lack it, so they
        // don't false-positive.
        if line.contains(": permission denied (") {
            return Some(());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_danger_level_max() {
        assert!(matches!(
            DangerLevel::None.max(DangerLevel::None),
            DangerLevel::None
        ));
        assert!(matches!(
            DangerLevel::Danger.max(DangerLevel::None),
            DangerLevel::Danger
        ));
        assert!(matches!(
            DangerLevel::None.max(DangerLevel::Danger),
            DangerLevel::Danger
        ));
        assert!(matches!(
            DangerLevel::Danger.max(DangerLevel::Danger),
            DangerLevel::Danger
        ));
    }

    #[test]
    fn test_write_rcfile_temp_in_creates_unique_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("aish-rc-base-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).expect("create base");

        let path = write_rcfile_temp_in(&base).expect("write should succeed");
        assert_eq!(path.parent(), Some(base.as_path()));
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            name.starts_with(&format!("aish-rc-{}-", getuid())) && name.ends_with(".sh"),
            "unexpected rc path: {}",
            path.display()
        );
        assert!(path.exists());
        let mode = std::fs::metadata(&path)
            .expect("file meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            !base.join(format!("aish-rc-{}", getuid())).exists(),
            "should not create a per-user directory"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_write_rcfile_temp_in_ignores_legacy_shared_dir() {
        let base =
            std::env::temp_dir().join(format!("aish-rc-legacy-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).expect("create base");
        let legacy = base.join("aish-rc");
        std::fs::create_dir_all(&legacy).expect("create legacy dir");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o555))
            .expect("chmod legacy dir");

        let path = write_rcfile_temp_in(&base).expect("should not use legacy aish-rc dir");
        assert_eq!(path.parent(), Some(base.as_path()));
        assert!(!path.starts_with(&legacy));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_is_interactive_command() {
        assert!(is_interactive_command("vim file.txt"));
        assert!(is_interactive_command("ssh user@host"));
        assert!(is_interactive_command("htop"));
        assert!(!is_interactive_command("ls -la"));
        assert!(!is_interactive_command("echo hello"));
    }

    #[test]
    fn test_looks_like_remote_prompt_bash_bracket_root() {
        assert!(looks_like_remote_prompt(b"[root@host ~]# "));
    }

    #[test]
    fn test_looks_like_remote_prompt_bash_bracket_user() {
        assert!(looks_like_remote_prompt(b"[user@host dir]$ "));
    }

    #[test]
    fn test_looks_like_remote_prompt_with_ansi_color() {
        assert!(looks_like_remote_prompt(
            b"\x1b[01;31mroot@host\x1b[00m:\x1b[01;34m~\x1b[00m# "
        ));
    }

    #[test]
    fn test_looks_like_remote_prompt_with_osc_title_prefix() {
        // Bash's default PS1 on RHEL/Fedora emits an OSC title-setting
        // sequence before the visible prompt. Detection must see through it.
        assert!(looks_like_remote_prompt(
            b"\x1b]0;root@xzxserver:~\x07\x1b[?1034h[root@xzxserver ~]# "
        ));
    }

    #[test]
    fn test_strip_ansi_handles_csi_osc_and_single_char() {
        assert_eq!(strip_ansi(b"abc"), "abc");
        assert_eq!(strip_ansi(b"\x1b[31mred\x1b[0m"), "red");
        // OSC with BEL terminator
        assert_eq!(strip_ansi(b"\x1b]0;title\x07rest"), "rest");
        // OSC with ST (\x1b\\) terminator
        assert_eq!(strip_ansi(b"\x1b]0;title\x1b\\rest"), "rest");
        // Single-char escape (save cursor)
        assert_eq!(strip_ansi(b"\x1b7X"), "X");
    }

    #[test]
    fn test_looks_like_remote_prompt_rejects_long_output() {
        // A 200-char line ending in `# ` should be rejected as it is almost
        // certainly command output, not a prompt.
        let long_line = format!("{}# ", "x".repeat(200));
        assert!(!looks_like_remote_prompt(long_line.as_bytes()));
    }

    #[test]
    fn test_looks_like_remote_prompt_rejects_plain_text() {
        // No prompt structure punctuation -> reject.
        assert!(!looks_like_remote_prompt(b"hello world# "));
        assert!(!looks_like_remote_prompt(b"done$ "));
    }

    #[test]
    fn test_looks_like_remote_prompt_rejects_no_terminator() {
        assert!(!looks_like_remote_prompt(b"[root@host ~]"));
        assert!(!looks_like_remote_prompt(b"[root@host ~]#"));
        assert!(!looks_like_remote_prompt(b"[root@host ~] #"));
    }

    #[test]
    fn test_looks_like_remote_prompt_accepts_zsh_bare_percent() {
        // zsh's default prompt is often just `host% ` with no `@`/`[`/`]`.
        // Without explicit support these would be rejected and PS1
        // injection never fired on plain zsh prompts.
        assert!(looks_like_remote_prompt(b"host% "));
        assert!(looks_like_remote_prompt(b"build-01% "));
    }

    #[test]
    fn test_looks_like_remote_prompt_rejects_percent_with_garbage_stem() {
        // Trailing `% ` only counts as a zsh prompt when the stem looks
        // like a bare hostname — free-form text ending in `% ` is still
        // command output, not a prompt.
        assert!(!looks_like_remote_prompt(b"some output text% "));
        assert!(!looks_like_remote_prompt(b"50% "));
        assert!(!looks_like_remote_prompt(b"foo bar% "));
    }

    #[test]
    fn test_last_line_is_remote_prompt_plain_prompt() {
        assert!(last_line_is_remote_prompt(b"[root@host ~]# "));
    }

    #[test]
    fn test_last_line_is_remote_prompt_after_output() {
        assert!(last_line_is_remote_prompt(b"file1\nfile2\n[root@host ~]# "));
    }

    #[test]
    fn test_last_line_is_remote_prompt_rejects_command_echo() {
        // Prompt followed by partial user input must not trigger — we'd be
        // interrupting the user mid-type.
        assert!(!last_line_is_remote_prompt(b"[root@host ~]# pwd"));
        assert!(!last_line_is_remote_prompt(b"[root@host ~]# ls"));
    }

    #[test]
    fn test_last_line_is_remote_prompt_rejects_plain_output() {
        assert!(!last_line_is_remote_prompt(b"file1\nfile2\nfile3\n"));
        assert!(!last_line_is_remote_prompt(b"hello world"));
    }

    fn make_simple_info(host: &str) -> SshCommandInfo {
        SshCommandInfo {
            user: None,
            host: host.to_string(),
            jump_chain: vec![],
            dest_raw: host.to_string(),
        }
    }

    fn make_minimal_snapshot() -> RemoteContextSnapshot {
        RemoteContextSnapshot::minimal(ShellKind::Bash)
    }

    #[test]
    fn test_build_ps1_marker_command_contains_zero_width_markers() {
        let info = make_simple_info("10.10.17.130");
        let snap = make_minimal_snapshot();
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::None, false, true, true, true);
        let s = String::from_utf8(cmd).expect("valid UTF-8");
        assert!(s.starts_with(' '), "must start with space for HISTCONTROL");
        assert!(
            s.contains("\\[\\e[33m\\]"),
            "missing \\[ before yellow color"
        );
        assert!(s.contains("\\[\\e[0m\\]"), "missing \\[ before reset");
        assert!(s.contains("[ssh:10.10.17.130"), "missing marker text");
        assert!(s.ends_with('\r'), "must end with CR");
        assert!(s.contains("\"$PS1\""));
        assert!(
            s.contains("printf '\\33[A\\33[J'"),
            "missing echo-clearing printf"
        );
    }

    #[test]
    fn test_build_ps1_marker_command_danger_uses_red_bold() {
        let info = make_simple_info("prod-web-03");
        let snap = make_minimal_snapshot();
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::Danger, false, true, true, true);
        let s = String::from_utf8_lossy(&cmd);
        assert!(s.contains("\\[\\e[31;1m\\]"), "Danger must use red+bold");
        assert!(!s.contains("\\[\\e[33m\\]"), "Danger must not use yellow");
    }

    #[test]
    fn test_build_ps1_marker_command_wraps_live_ansi_in_zero_width_markers() {
        // Live segments (venv, git, ROOT) MUST be plain text — no ANSI
        // colour escapes. Bash does not parse `\[ \]` markers from
        // variable expansion (only from literal PS1), so wrapping the
        // escapes is futile and would either:
        //   (a) leave bare ESC bytes that readline counts as visible
        //       columns — causing residual characters after Up/Down
        //       history navigation; or
        //   (b) leak literal `\[\]` glyphs into the prompt (what
        //       happens if you embed `\[` in the variable value).
        // Stripping colour from the variable avoids both. The segments
        // stay visible as plain text (`|main`, `[ROOT]`, `|venv-name`).
        let info = make_simple_info("v25");
        let snap = make_minimal_snapshot();
        let cmd = build_ps1_marker_command(&info, &snap, DangerLevel::None, true, true, true, true);
        let s = String::from_utf8(cmd).expect("valid UTF-8");
        // venv segment: plain concatenation, no $'\x1b...' ANSI-C quote.
        assert!(
            !s.contains(r"$'\x1b[36m"),
            "venv segment must not use ANSI escape; cmd={}",
            s
        );
        assert!(
            !s.contains(r"$'\x1b[35m"),
            "git segment must not use ANSI escape; cmd={}",
            s
        );
        assert!(
            !s.contains(r"$'\x1b[31m"),
            "ROOT segment must not use ANSI escape; cmd={}",
            s
        );
        // The text labels must still appear (so the feature works).
        assert!(
            s.contains("[ROOT]"),
            "ROOT badge text must be present; cmd={}",
            s
        );
        // No literal \[\] leaking.
        assert!(
            !s.contains(r"\[\]"),
            "no literal \\[\\] glyphs should appear in cmd; cmd={}",
            s
        );
    }

    #[test]
    fn test_build_ps1_marker_command_disabled_path_is_minimal() {
        // enable_git=false produces a single-line legacy-shaped injection
        // (no hook, no PROMPT_COMMAND touch). Used for non-bash shells too.
        let info = make_simple_info("v25");
        let snap = make_minimal_snapshot();
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::None, false, true, true, true);
        let s = String::from_utf8_lossy(&cmd);
        assert!(
            !s.contains("__aish_ctx_hook"),
            "disabled path must not install hook"
        );
        assert!(
            !s.contains("PROMPT_COMMAND"),
            "disabled path must not touch PROMPT_COMMAND"
        );
    }

    #[test]
    fn test_build_ps1_marker_command_includes_segments() {
        let info = SshCommandInfo {
            user: Some("root".into()),
            host: "prod-web-03".into(),
            jump_chain: vec!["bastion".into()],
            dest_raw: "root@prod-web-03".into(),
        };
        let snap = RemoteContextSnapshot {
            container: Some("docker".into()),
            shell_type: ShellKind::Bash,
            kube_context: Some("prod-cluster".into()),
            is_kube_prod: true,
        };
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::Danger, true, true, true, true);
        let s = String::from_utf8_lossy(&cmd);
        assert!(s.contains("[ssh:root@prod-web-03"), "user@host");
        assert!(s.contains("⤴ bastion"), "jump chain");
        assert!(s.contains("bash"), "shell name");
        assert!(s.contains("docker"), "container");
        assert!(s.contains("kube:prod-cluster"), "kube context");
        assert!(s.contains("__aish_ctx_hook"), "hook definition");
        assert!(
            s.contains("PROMPT_COMMAND=__aish_ctx_hook"),
            "hook installation"
        );
        assert!(s.contains("[ \"$EUID\" = 0 ]"), "live root escalation");
        assert!(s.contains("${__aish_ctx_live}"), "live var reference");
    }

    #[test]
    fn test_build_ps1_marker_command_non_bash_omits_hook() {
        let info = make_simple_info("host");
        let mut snap = make_minimal_snapshot();
        snap.shell_type = ShellKind::Zsh;
        let cmd = build_ps1_marker_command(&info, &snap, DangerLevel::None, true, true, true, true);
        let s = String::from_utf8_lossy(&cmd);
        assert!(
            !s.contains("__aish_ctx_hook"),
            "non-bash must not install hook"
        );
        assert!(s.contains("[ssh:host"), "still has host marker");
    }

    #[test]
    fn test_build_ps1_marker_command_hide_container_when_disabled() {
        let info = make_simple_info("host");
        let snap = RemoteContextSnapshot {
            container: Some("docker".into()),
            shell_type: ShellKind::Bash,
            kube_context: None,
            is_kube_prod: false,
        };
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::None, true, true, false, true);
        let s = String::from_utf8_lossy(&cmd);
        assert!(
            !s.contains("| docker"),
            "container segment must hide when show_container=false"
        );
    }

    #[test]
    fn test_build_ps1_marker_command_hide_kube_when_disabled() {
        let info = make_simple_info("host");
        let snap = RemoteContextSnapshot {
            container: None,
            shell_type: ShellKind::Bash,
            kube_context: Some("prod-cluster".into()),
            is_kube_prod: false,
        };
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::None, true, true, true, false);
        let s = String::from_utf8_lossy(&cmd);
        assert!(
            !s.contains("kube:prod-cluster"),
            "kube segment must hide when show_kube=false"
        );
    }

    // Integration-shape tests covering the Task 6 injection-site decisions.
    // These reproduce the gating logic of the inline block in
    // `send_command_interactive` so a regression in either site is caught.

    /// Mirrors the ShellKind detection performed at the PS1 injection site.
    fn detect_shell_kind_from_prompt(last_line: &[u8]) -> ShellKind {
        if last_line.ends_with(b"% ") {
            ShellKind::Zsh
        } else if last_line.ends_with(b"> ") {
            ShellKind::Fish
        } else {
            // Includes `$ `, `# `, bracketed prompts, and the empty case —
            // bash is the safest default and the most common shell.
            ShellKind::Bash
        }
    }

    #[test]
    fn test_shell_kind_detection_from_prompt_shape() {
        assert!(matches!(
            detect_shell_kind_from_prompt(b"user@host:~$ "),
            ShellKind::Bash
        ));
        assert!(matches!(
            detect_shell_kind_from_prompt(b"[root@host ~]# "),
            ShellKind::Bash
        ));
        assert!(matches!(
            detect_shell_kind_from_prompt(b"% "),
            ShellKind::Zsh
        ));
        assert!(matches!(
            detect_shell_kind_from_prompt(b"host ~> "),
            ShellKind::Fish
        ));
        // Empty / unknown → bash default.
        assert!(matches!(
            detect_shell_kind_from_prompt(b""),
            ShellKind::Bash
        ));
    }

    /// When remote_rich_prompt is OFF (or enable_git is OFF), the legacy path
    /// must produce a `[ssh:host]` literal with NO `| bash` segment so the
    /// DejaGnu opt-out test (which forbids `\|[\w]+` after the marker) keeps
    /// passing.
    #[test]
    fn test_legacy_path_produces_minimal_marker_for_opt_out() {
        let info = make_simple_info("localhost");
        let snap = RemoteContextSnapshot::minimal(ShellKind::Bash);
        let cmd =
            build_ps1_marker_command(&info, &snap, DangerLevel::None, false, false, false, false);
        let s = String::from_utf8_lossy(&cmd);
        assert!(
            s.contains("[ssh:localhost]"),
            "legacy literal must be [ssh:localhost], got: {}",
            s
        );
        assert!(
            !s.contains("| bash"),
            "legacy path must NOT include shell_name segment (would trip DejaGnu opt-out)"
        );
        assert!(!s.contains("docker"), "no container in legacy");
        assert!(!s.contains("kube:"), "no kube in legacy");
    }

    /// Danger escalation: root user + prod kube context → Danger.
    /// Validates the `max(static, kube)` expression used at injection time.
    #[test]
    fn test_danger_escalation_root_plus_prod_kube() {
        let info = SshCommandInfo {
            user: Some("root".into()),
            host: "host".into(),
            jump_chain: vec![],
            dest_raw: "root@host".into(),
        };
        // Static danger from root user.
        assert!(matches!(info.danger_static(&[]), DangerLevel::Danger));
        // Static danger from hostname pattern.
        let info2 = SshCommandInfo {
            user: None,
            host: "prod-web-01".into(),
            jump_chain: vec![],
            dest_raw: "prod-web-01".into(),
        };
        let patterns = vec!["^prod-".to_string()];
        let compiled = aish_config::compile_remote_danger_patterns(&patterns);
        assert!(matches!(
            info2.danger_static(&compiled),
            DangerLevel::Danger
        ));
        // kube danger.
        let snap = RemoteContextSnapshot {
            container: None,
            shell_type: ShellKind::Bash,
            kube_context: Some("prod-cluster".into()),
            is_kube_prod: true,
        };
        assert!(matches!(snap.kube_danger(), DangerLevel::Danger));
        // Combined: max(static=Danger, kube=None) → Danger.
        let combined = info.danger_static(&[]).max(snap.kube_danger());
        assert!(matches!(combined, DangerLevel::Danger));
    }

    /// Rich path on bash with prod kube context bakes kube segment into PS1
    /// literal and selects the danger color (red+bold).
    #[test]
    fn test_rich_path_bakes_kube_segment_with_danger_color() {
        let info = make_simple_info("localhost");
        let snap = RemoteContextSnapshot {
            container: None,
            shell_type: ShellKind::Bash,
            kube_context: Some("prod-cluster".into()),
            is_kube_prod: true,
        };
        let cmd = build_ps1_marker_command(
            &info,
            &snap,
            DangerLevel::Danger,
            true, // enable_git
            true, // show_venv
            true, // show_container
            true, // show_kube
        );
        let s = String::from_utf8_lossy(&cmd);
        assert!(s.contains("[ssh:localhost | bash | kube:prod-cluster]"));
        assert!(
            s.contains("\\[\\e[31;1m\\]"),
            "danger color must be red+bold"
        );
    }

    #[test]
    fn test_is_session_command() {
        assert!(is_session_command("ssh user@host"));
        assert!(is_session_command("telnet example.com"));
        assert!(!is_session_command("vim file.txt"));
        assert!(!is_session_command("ls"));
    }

    #[test]
    fn test_parse_ssh_command_simple_host() {
        let info = parse_ssh_command("ssh host").expect("ssh host should parse");
        assert_eq!(info.host, "host");
        assert_eq!(info.user, None);
        assert!(info.jump_chain.is_empty());
        assert_eq!(info.dest_raw, "host");
    }

    #[test]
    fn test_parse_ssh_command_user_at_host() {
        let info = parse_ssh_command("ssh root@10.10.17.130").unwrap();
        assert_eq!(info.user.as_deref(), Some("root"));
        assert_eq!(info.host, "10.10.17.130");
        assert_eq!(info.dest_raw, "root@10.10.17.130");
    }

    #[test]
    fn test_parse_ssh_command_preserves_jump_chain() {
        let info = parse_ssh_command("ssh -J bastion root@host").unwrap();
        assert_eq!(info.user.as_deref(), Some("root"));
        assert_eq!(info.host, "host");
        assert_eq!(info.jump_chain, vec!["bastion".to_string()]);
    }

    #[test]
    fn test_parse_ssh_command_multiple_jumps() {
        let info = parse_ssh_command("ssh -J j1,j2 user@host").unwrap();
        assert_eq!(info.jump_chain, vec!["j1".to_string(), "j2".to_string()]);
    }

    #[test]
    fn test_parse_ssh_command_combined_options() {
        let info = parse_ssh_command("ssh -J b -p 2222 root@host").unwrap();
        assert_eq!(info.user.as_deref(), Some("root"));
        assert_eq!(info.jump_chain, vec!["b".to_string()]);
    }

    #[test]
    fn test_parse_ssh_command_equals_form_option() {
        let info = parse_ssh_command("ssh -oFoo=bar user@host").unwrap();
        assert_eq!(info.user.as_deref(), Some("user"));
    }

    #[test]
    fn test_parse_ssh_command_rejects_non_session() {
        assert!(parse_ssh_command("vim file").is_none());
        assert!(parse_ssh_command("ls -la").is_none());
    }

    #[test]
    fn test_parse_ssh_command_rejects_scp() {
        // scp is NOT in the session command allowlist.
        assert!(parse_ssh_command("scp user@host:src dst").is_none());
    }

    #[test]
    fn test_inject_ssh_connect_timeout_simple_host() {
        let out = inject_ssh_connect_timeout("ssh host", 5);
        assert_eq!(out, "ssh -o ConnectTimeout=5 host");
    }

    #[test]
    fn test_inject_ssh_connect_timeout_with_options() {
        let out = inject_ssh_connect_timeout("ssh -l root 10.10.17.242", 5);
        assert_eq!(out, "ssh -o ConnectTimeout=5 -l root 10.10.17.242");
    }

    #[test]
    fn test_inject_ssh_connect_timeout_preserves_existing() {
        // User already specified ConnectTimeout — must not double-inject.
        let inp = "ssh -o ConnectTimeout=10 host";
        let out = inject_ssh_connect_timeout(inp, 5);
        assert_eq!(out, inp);
    }

    #[test]
    fn test_inject_ssh_connect_timeout_preserves_user_at_host() {
        let out = inject_ssh_connect_timeout("ssh root@host", 8);
        assert_eq!(out, "ssh -o ConnectTimeout=8 root@host");
    }

    #[test]
    fn test_inject_ssh_connect_timeout_non_ssh_unchanged() {
        // Non-ssh commands must not be touched.
        for cmd in ["ls -la", "scp file host:/tmp", "echo ssh host", "vim file"] {
            assert_eq!(inject_ssh_connect_timeout(cmd, 5), cmd);
        }
    }

    #[test]
    fn test_inject_ssh_connect_timeout_preserves_leading_ws() {
        // Leading whitespace (common when commands are re-injected into the
        // PTY after readline rendering) must be preserved.
        let out = inject_ssh_connect_timeout("  ssh host", 5);
        assert_eq!(out, "  ssh -o ConnectTimeout=5 host");
    }

    #[test]
    fn test_inject_ssh_connect_timeout_with_full_path() {
        // /usr/bin/ssh should also be detected.
        let out = inject_ssh_connect_timeout("/usr/bin/ssh host", 5);
        assert_eq!(out, "/usr/bin/ssh -o ConnectTimeout=5 host");
    }

    #[test]
    fn test_inject_ssh_connect_timeout_empty_line() {
        assert_eq!(inject_ssh_connect_timeout("", 5), "");
        assert_eq!(inject_ssh_connect_timeout("   \n", 5), "   \n");
    }

    #[test]
    fn test_ssh_command_info_display_jumps() {
        let no_jumps = SshCommandInfo {
            user: None,
            host: "h".into(),
            jump_chain: vec![],
            dest_raw: "h".into(),
        };
        assert_eq!(no_jumps.display_jumps(), "");

        let with_jumps = SshCommandInfo {
            user: None,
            host: "h".into(),
            jump_chain: vec!["j1".into(), "j2".into()],
            dest_raw: "h".into(),
        };
        assert_eq!(with_jumps.display_jumps(), " ⤴ j1,j2");
    }

    #[test]
    fn test_ssh_command_info_danger_static_root_user() {
        let info = SshCommandInfo {
            user: Some("root".into()),
            host: "anyhost".into(),
            jump_chain: vec![],
            dest_raw: "root@anyhost".into(),
        };
        assert!(matches!(info.danger_static(&[]), DangerLevel::Danger));
    }

    #[test]
    fn test_ssh_command_info_danger_static_prod_hostname() {
        let info = SshCommandInfo {
            user: None,
            host: "prod-web-03".into(),
            jump_chain: vec![],
            dest_raw: "prod-web-03".into(),
        };
        let patterns = vec!["^prod-".to_string()];
        let compiled = aish_config::compile_remote_danger_patterns(&patterns);
        assert!(matches!(info.danger_static(&compiled), DangerLevel::Danger));
    }

    #[test]
    fn test_ssh_command_info_danger_static_dev_box() {
        let info = SshCommandInfo {
            user: None,
            host: "dev-box".into(),
            jump_chain: vec![],
            dest_raw: "dev-box".into(),
        };
        let patterns = vec!["^prod-".to_string()];
        let compiled = aish_config::compile_remote_danger_patterns(&patterns);
        assert!(matches!(info.danger_static(&compiled), DangerLevel::None));
    }

    #[test]
    fn test_parse_probe_output_complete() {
        let raw = "@@aish_ctx_start@@\n1000\ndocker\nprod-cluster\n@@aish_ctx_end@@\n";
        let snap = parse_probe_output(raw).expect("complete probe must parse");
        assert_eq!(snap.container.as_deref(), Some("docker"));
        assert!(matches!(snap.shell_type, ShellKind::Bash));
        assert_eq!(snap.kube_context.as_deref(), Some("prod-cluster"));
        assert!(
            snap.is_kube_prod,
            "prod-cluster must be flagged is_kube_prod via prod/prd/production substring"
        );
    }

    #[test]
    fn test_parse_probe_output_kube_missing() {
        let raw = "@@aish_ctx_start@@\n1000\ndocker\n\n@@aish_ctx_end@@\n";
        let snap = parse_probe_output(raw).unwrap();
        assert_eq!(snap.kube_context, None);
    }

    #[test]
    fn test_parse_probe_output_flags_prod_kube_context() {
        let raw = "@@aish_ctx_start@@\n1000\ndocker\nprod-cluster\n@@aish_ctx_end@@\n";
        let snap = parse_probe_output(raw).expect("must parse");
        assert!(
            snap.is_kube_prod,
            "prod-cluster must be flagged is_kube_prod"
        );
        assert!(matches!(snap.kube_danger(), DangerLevel::Danger));
    }

    #[test]
    fn test_parse_probe_output_does_not_flag_benign_kube_context() {
        let raw = "@@aish_ctx_start@@\n1000\ndocker\nminikube\n@@aish_ctx_end@@\n";
        let snap = parse_probe_output(raw).unwrap();
        assert!(!snap.is_kube_prod, "minikube must not be flagged");
    }

    #[test]
    fn test_parse_probe_output_kube_prefix_match_excludes_false_positives() {
        // Regression: `starts_with`-only matching must NOT flag contexts
        // that merely contain a `prod`/`prd`/`production` token somewhere
        // in the middle — those usually denote preprod/staging environments.
        let benign_contexts = [
            "preprod-green",
            "staging-prd-blue",
            "qa-production-mirror",
            "eu-west-1-preprod",
            "my-prod-cluster-test",
        ];
        for ctx in &benign_contexts {
            let raw = format!(
                "@@aish_ctx_start@@\n1000\ndocker\n{}\n@@aish_ctx_end@@\n",
                ctx
            );
            let snap = parse_probe_output(&raw).expect("must parse");
            assert!(
                !snap.is_kube_prod,
                "context {:?} must NOT be flagged danger (would false-positive on preprod/staging)",
                ctx
            );
        }
    }

    #[test]
    fn test_parse_probe_output_kube_prefix_match_catches_true_prod() {
        // True prod contexts must still be flagged.
        let prod_contexts = ["prod-cluster", "prd-blue", "production-west", "prod", "prd"];
        for ctx in &prod_contexts {
            let raw = format!(
                "@@aish_ctx_start@@\n1000\ndocker\n{}\n@@aish_ctx_end@@\n",
                ctx
            );
            let snap = parse_probe_output(&raw).expect("must parse");
            assert!(
                snap.is_kube_prod,
                "context {:?} must be flagged danger",
                ctx
            );
        }
    }

    #[test]
    fn test_parse_probe_output_container_missing() {
        let raw = "@@aish_ctx_start@@\n1000\n\nprod-cluster\n@@aish_ctx_end@@\n";
        let snap = parse_probe_output(raw).unwrap();
        assert_eq!(snap.container, None);
        assert_eq!(snap.kube_context.as_deref(), Some("prod-cluster"));
    }

    #[test]
    fn test_parse_probe_output_echo_leak_with_cr() {
        // PTY OPOST turns \n into \r\n. Parser must tolerate trailing \r.
        let raw = "@@aish_ctx_start@@\r\n1000\r\ndocker\r\nprod\r\n@@aish_ctx_end@@\r\n";
        let snap = parse_probe_output(raw).unwrap();
        assert_eq!(snap.container.as_deref(), Some("docker"));
    }

    #[test]
    fn test_parse_probe_output_skips_echo_to_pick_last_start_marker() {
        // On a real PTY with ECHO on, bash echoes the typed probe command
        // (which contains the markers as literal text) BEFORE executing it.
        // The parser MUST skip the echo and parse the real marker body —
        // i.e. use `rfind` for the start marker (last occurrence wins).
        // Pure-function replacement for the old PTY-based
        // `test_probe_remote_command_parses_with_echo_enabled` which was
        // racy: line-discipline echo ordering vs the test's slave-side
        // writes was nondeterministic, flipping rfind's choice under load.
        let raw = " echo @@aish_ctx_start@@; id -u 2>/dev/null; echo @@aish_ctx_end@@\r\
                   @@aish_ctx_start@@\r\n\
                   1000\r\n\
                   docker\r\n\
                   prod-cluster\r\n\
                   @@aish_ctx_end@@\r\n";
        let snap = parse_probe_output(raw).expect("must skip echo and parse real body");
        assert_eq!(snap.container.as_deref(), Some("docker"));
        assert_eq!(snap.kube_context.as_deref(), Some("prod-cluster"));
        assert!(snap.is_kube_prod);
    }

    #[test]
    fn test_parse_probe_output_no_markers() {
        assert!(parse_probe_output("just some output\n").is_none());
    }

    #[test]
    fn test_parse_probe_output_partial_missing_end() {
        let raw = "@@aish_ctx_start@@\n1000\n";
        assert!(parse_probe_output(raw).is_none());
    }

    #[test]
    fn test_remote_context_snapshot_minimal() {
        let snap = RemoteContextSnapshot::minimal(ShellKind::Bash);
        assert!(matches!(snap.shell_type, ShellKind::Bash));
        assert_eq!(snap.container, None);
        assert_eq!(snap.kube_context, None);
        assert!(!snap.is_kube_prod);
    }

    #[test]
    fn test_remote_context_snapshot_kube_danger() {
        let dangerous = RemoteContextSnapshot {
            container: None,
            shell_type: ShellKind::Bash,
            kube_context: Some("prod-cluster".into()),
            is_kube_prod: true,
        };
        assert!(matches!(dangerous.kube_danger(), DangerLevel::Danger));

        let benign = RemoteContextSnapshot {
            container: None,
            shell_type: ShellKind::Bash,
            kube_context: Some("minikube".into()),
            is_kube_prod: false,
        };
        assert!(matches!(benign.kube_danger(), DangerLevel::None));
    }

    #[test]
    fn test_scan_output_ignores_ssh_in_reverse_isearch() {
        // Ctrl+R search display should NOT trigger detection
        let output = "(reverse-i-search)`ssh': ssh -l root 10.10.17.243\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_is_plausible_ssh_host_accepts_ipv4() {
        assert!(is_plausible_ssh_host("10.10.17.243"));
        assert!(is_plausible_ssh_host("192.168.1.1"));
        assert!(is_plausible_ssh_host("root@10.10.17.243"));
    }

    #[test]
    fn test_is_plausible_ssh_host_accepts_hostname() {
        assert!(is_plausible_ssh_host("example.com"));
        assert!(is_plausible_ssh_host("build-ssh-01.prod.example.com"));
        assert!(is_plausible_ssh_host("localhost"));
        assert!(is_plausible_ssh_host("user@build-ssh-01"));
    }

    #[test]
    fn test_is_plausible_ssh_host_rejects_glued_digit_letter() {
        // The corruption pattern: output_ssh_scan concatenates partial
        // typing across chunks (e.g. `ssh 10.10.17.243` followed by `ssh`
        // while reverse-i-search is in flight), producing a line like
        // `ssh 10.10.17.243ssh`. parse_ssh_command returns the trailing
        // token verbatim; the validator must reject it.
        assert!(!is_plausible_ssh_host("10.10.17.243ssh"));
        assert!(!is_plausible_ssh_host("10.10.17.243abc"));
        assert!(!is_plausible_ssh_host("192.168.1.1xyz"));
    }

    #[test]
    fn test_is_plausible_ssh_host_rejects_empty_and_garbage() {
        assert!(!is_plausible_ssh_host(""));
        assert!(!is_plausible_ssh_host("@"));
        assert!(!is_plausible_ssh_host("user@"));
        assert!(!is_plausible_ssh_host("-bad.example.com"));
        assert!(!is_plausible_ssh_host("bad-.example.com"));
    }

    #[test]
    fn test_is_plausible_ssh_host_rejects_invalid_ipv4() {
        // 4 all-numeric labels that fail strict IPv4 validation must NOT
        // fall through to the general hostname check (each label is
        // alphanumeric). Without this rejection they would be accepted
        // and poison remote_host_for_probe with an impossible address.
        assert!(!is_plausible_ssh_host("999.999.999.999"));
        assert!(!is_plausible_ssh_host("256.256.256.256"));
        assert!(!is_plausible_ssh_host("1.2.3.256"));
    }

    #[test]
    fn test_scan_output_rejects_glued_corruption() {
        // Simulates the buffer state when output_ssh_scan has accumulated
        // partial typing from the user. The line `ssh 10.10.17.243ssh`
        // would be extracted by parse_ssh_command but must be rejected by
        // the plausibility check so the host in remote_host_for_probe
        // does not get poisoned.
        let output = "ssh 10.10.17.243ssh\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_scan_output_rejects_ssh_in_command_output() {
        // `w | grep ssh` prints lines describing active SSH sessions —
        // the `ssh` token appears in the middle of the line as data, not
        // as a command being executed. Picking it up would update
        // remote_host_for_probe and re-trigger PS1 injection, stacking
        // multiple `[ssh:...]` markers in the prompt.
        let output =
            "root     pts/1    10.10.73.60      086月26 14days  0.06s  0.05s ssh -l root 10.10.17.243\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_scan_output_rejects_log_line_with_ssh() {
        // System log lines that mention ssh are not user-typed commands.
        let output =
            "Jun 23 02:00:00 host sshd[1234]: Accepted publickey for root from 10.0.0.5\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_scan_output_ignores_ssh_in_forward_isearch() {
        let output = "(i-search)`ssh': ssh user@192.168.1.1\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_scan_output_rejects_bare_ssh_at_line_start() {
        // A bare `ssh ...` at the start of a line is the outer session's
        // own command echo (no prompt prefix). Treating it as a nested
        // session would push a fake stack frame and re-enable PS1
        // injection after the outer session disconnects. Only the
        // `<prompt> ssh ...` shape is accepted.
        let output = "ssh -l root 10.10.17.243\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_scan_output_detects_ssh_after_remote_prompt() {
        // User pressed Up-arrow to recall an ssh command from inside an
        // already-connected remote bash. Bash redraws the line with the
        // prompt prefix, then echoes it on Enter. The scan must see the
        // prompt-terminated shape and detect the nested SSH target.
        let output = "[ssh:10.10.17.130] [root@xzxserver ~]# ssh -l root 10.10.17.243\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, Some("10.10.17.243".to_string()));
    }

    #[test]
    fn test_scan_output_detects_ssh_after_bare_prompt() {
        // Same shape without our own marker prefix (e.g. right after SSH,
        // before PS1 injection completed).
        let output = "[root@host ~]# ssh user@10.0.0.5\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, Some("user@10.0.0.5".to_string()));
    }

    #[test]
    fn test_scan_output_ignores_non_ssh_isearch() {
        let output = "(reverse-i-search)`ls': ls -la\r\n";
        let host = scan_output_for_ssh_host(output);
        assert_eq!(host, None);
    }

    #[test]
    fn test_scan_output_for_ssh_success_detects_password_prompt() {
        // OpenSSH password prompt — connection succeeded, auth starting.
        let cases = [
            "root@10.10.17.242's password:",
            "user@host's password: ",
            "Password:",
            "Password: ",
            "password for root@10.10.17.242:",
        ];
        for c in &cases {
            assert!(
                scan_output_for_ssh_success(c),
                "expected success detection for: {:?}",
                c
            );
        }
    }

    #[test]
    fn test_scan_output_for_ssh_success_detects_last_login_motd() {
        // OpenSSH default MOTD line — session fully established.
        let cases = [
            "Last login: Fri Jun 26 14:23:01 2026 from 10.0.0.5\r\n",
            "last login: Wed Apr  1 09:00:00 2026\r\n",
        ];
        for c in &cases {
            assert!(
                scan_output_for_ssh_success(c),
                "expected success detection for: {:?}",
                c
            );
        }
    }

    #[test]
    fn test_scan_output_for_ssh_success_ignores_normal_output() {
        // Normal prompts, errors, MOTD-without-login must NOT trigger.
        let benign = [
            "[root@v25 ~]# ",
            "user@host:~$ ",
            "Welcome to Ubuntu 22.04 LTS\r\n",
            "ssh: connect to host 10.10.17.242 port 22: Connection timed out\r\n",
            "# I will password: protect this\r\n",
            "echo 'last login: never'\r\n",
        ];
        for c in &benign {
            assert!(
                !scan_output_for_ssh_success(c),
                "false positive on benign output: {:?}",
                c
            );
        }
    }

    #[test]
    fn test_scan_output_for_ssh_failure_detects_common_errors() {
        // Each realistic OpenSSH error line must trigger failure detection.
        let cases = [
            "ssh: connect to host 10.10.17.242 port 22: Connection timed out\r\n",
            "ssh: connect to host 10.10.17.242 port 22: No route to host\r\n",
            "ssh: connect to host 10.10.17.242 port 22: Connection refused\r\n",
            "ssh: Could not resolve hostname badhost.xyz: Name or service not known\r\n",
            "kex_exchange_identification: read: Connection reset by peer\r\n",
            "received disconnect from 10.10.17.242 port 22:2: Too many authentication failures\r\n",
            "root@10.10.17.242: Permission denied (publickey,password).\r\n",
        ];
        for c in &cases {
            assert!(
                scan_output_for_ssh_failure(c).is_some(),
                "expected failure detection for: {:?}",
                c
            );
        }
    }

    #[test]
    fn test_scan_output_for_ssh_failure_ignores_normal_output() {
        // Normal prompts and MOTD must NOT trigger failure detection.
        let benign = [
            "Last login: Fri Jun 26 04:59:52 2026 from 10.10.73.60\r\n",
            "[root@v25 ~]# ",
            "user@host:~$ ",
            "Welcome to Ubuntu 22.04 LTS\r\n",
            "ssh -l root 10.10.17.242\r\n", // echoed command line itself
        ];
        for c in &benign {
            assert!(
                scan_output_for_ssh_failure(c).is_none(),
                "false positive on benign output: {:?}",
                c
            );
        }
    }

    #[test]
    fn test_scan_output_for_ssh_failure_ignores_remote_command_errors() {
        // Regression: phrases like "Permission denied" appearing mid-line
        // inside a legitimate nested SSH session must NOT trigger failure
        // detection. Only SSH-client-emitted line-start markers count.
        let benign = [
            // File-access error from `ls` inside the nested session.
            "ls: cannot open 'foo': Permission denied\r\n",
            // bash script invocation failure.
            "bash: ./script.sh: Permission denied\r\n",
            // sudo prompt mid-line.
            "sudo: user1 is not in the sudoers file. This incident will be reported.\r\n",
            // Generic error containing the bare phrases — must NOT match.
            "echo 'connection timed out' was the error\r\n",
            "grep -i 'no route to host' /var/log/messages\r\n",
        ];
        for c in &benign {
            assert!(
                scan_output_for_ssh_failure(c).is_none(),
                "false positive on remote command output: {:?}",
                c
            );
        }
    }

    #[test]
    fn test_scan_output_for_ssh_failure_strips_ansi_escapes() {
        // Errors may arrive with colour escapes (e.g. from a remote shell
        // wrapper). Detection must still fire after stripping.
        let raw = "\x1b[31mssh: connect to host 10.10.17.242 port 22: \
                   Connection timed out\x1b[0m\r\n";
        assert!(scan_output_for_ssh_failure(raw).is_some());
    }

    #[test]
    fn test_clean_pty_output() {
        let raw = "\x1b[0m\x1b[32mecho hello\x1b[0m\r\nhello world\r\n\x1b[?2004l";
        let cleaned = clean_pty_output(raw, "echo hello");
        assert_eq!(cleaned, "hello world");
    }

    #[test]
    fn test_clean_pty_output_strips_sudo_auth_noise() {
        let raw = "sudo ls /root\r\n请输入密码：\r\n验证成功\r\nconfig.yaml\r\n";
        let cleaned = clean_pty_output(raw, "sudo ls /root");
        assert_eq!(cleaned, "config.yaml");
    }

    #[test]
    fn test_write_all_with_retry_retries_eintr_and_partial_writes() {
        let mut calls = Vec::new();
        let mut results = vec![Err(libc::EINTR), Ok(2usize), Ok(1usize), Ok(2usize)].into_iter();

        let fully_written = write_all_with_retry(b"hello", |chunk| {
            calls.push(String::from_utf8(chunk.to_vec()).unwrap());
            results.next().expect("expected another write result")
        });

        assert!(fully_written, "should report full write on success");
        assert_eq!(calls, vec!["hello", "hello", "llo", "lo"]);
    }

    #[test]
    fn test_write_all_with_retry_returns_false_on_zero_write() {
        // Ok(0) signals EOF / closed fd — helper must give up and report false
        // so callers (e.g. PS1 injection) can retry on the next opportunity.
        let fully_written = write_all_with_retry(b"hello", |_| Ok(0));
        assert!(!fully_written);
    }

    #[test]
    fn test_write_all_with_retry_returns_false_on_non_eintr_error() {
        // Non-EINTR errors abort immediately; caller sees false.
        let fully_written = write_all_with_retry(b"hello", |_| Err(libc::EIO));
        assert!(!fully_written);
    }

    #[test]
    fn test_shell_quote_escape() {
        assert_eq!(shell_quote_escape("ls -la"), "'ls -la'");
        assert_eq!(shell_quote_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_persistent_pty_start_stop() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");
        let child_pid = pty.child_pid;
        assert!(pty.is_running());
        pty.stop();
        assert!(!pty.is_running());
        assert!(matches!(
            waitpid(child_pid, Some(WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
        ));
    }

    #[test]
    fn test_stop_cleans_up_even_when_running_already_false() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");
        let child_pid = pty.child_pid;

        pty.running.store(false, Ordering::SeqCst);
        pty.stop();

        assert_eq!(pty.master_fd, -1);
        assert_eq!(pty.control_fd, -1);
        assert!(matches!(
            waitpid(child_pid, Some(WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
        ));
    }

    #[test]
    fn test_stop_allows_graceful_exit_when_running_already_false() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");

        let tempdir = std::env::temp_dir().join(format!(
            "aish-pty-stop-graceful-exit-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tempdir).expect("create tempdir");
        let marker = tempdir.join("exit-marker");

        pty.send_command(
            &format!(
                "trap \"touch {}\" EXIT; exit",
                shell_quote_escape(&marker.display().to_string())
            ),
            None,
        )
        .expect("send exit command");

        pty.running.store(false, Ordering::SeqCst);
        pty.stop();

        assert!(marker.exists(), "expected graceful exit trap to run");

        let _ = std::fs::remove_dir_all(&tempdir);
    }

    #[test]
    fn test_execute_simple_command() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");

        let (output, exit_code, result_cwd) = pty
            .execute_command("echo hello_world_123", Duration::from_secs(5), None, false)
            .expect("execute should succeed");
        assert_eq!(exit_code, 0);
        assert_eq!(result_cwd, cwd);
        assert!(output.contains("hello_world_123"), "output was: {}", output);

        pty.stop();
    }

    #[test]
    fn test_execute_multiple_commands() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");

        let (out1, code1, cwd1) = pty
            .execute_command("echo first", Duration::from_secs(5), None, false)
            .expect("cmd1");
        assert_eq!(code1, 0);
        assert_eq!(cwd1, cwd);
        assert!(out1.contains("first"));

        let (out2, code2, cwd2) = pty
            .execute_command("echo second", Duration::from_secs(5), None, false)
            .expect("cmd2");
        assert_eq!(code2, 0);
        assert_eq!(cwd2, cwd);
        assert!(out2.contains("second"));

        pty.stop();
    }

    #[test]
    fn test_execute_command_persists_cwd_for_following_commands() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start should succeed");

        let tempdir = std::env::temp_dir().join(format!(
            "aish-pty-cwd-persist-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tempdir).expect("create tempdir");
        let target = tempdir.display().to_string();

        let (_output, exit_code, result_cwd) = pty
            .execute_command(
                &format!("cd {}", shell_quote_escape(&target)),
                Duration::from_secs(5),
                None,
                false,
            )
            .expect("cd should succeed");
        assert_eq!(exit_code, 0);
        assert_eq!(result_cwd, target);

        let (pwd_output, pwd_exit_code, pwd_cwd) = pty
            .execute_command("pwd", Duration::from_secs(5), None, false)
            .expect("pwd should succeed");
        assert_eq!(pwd_exit_code, 0);
        assert_eq!(pwd_cwd, target);
        assert_eq!(pwd_output.trim(), target);

        pty.stop();
        let _ = std::fs::remove_dir_all(&tempdir);
    }

    #[test]
    fn test_strip_ps1_echo_no_suppressor_returns_input_untouched() {
        // When remaining == 0, the suppressor is inert and returns data as-is.
        let mut sup = Ps1EchoSuppressor {
            pattern: build_ps1_marker_command(
                &make_simple_info("10.10.17.243"),
                &make_minimal_snapshot(),
                DangerLevel::None,
                false,
                true,
                true,
                true,
            ),
            remaining: 0,
            started: std::time::Instant::now(),
            pending: Vec::new(),
            pending_printf_erase: None,
        };
        let data = b"hello world\n";
        assert_eq!(strip_ps1_echo(data, &mut sup), data.to_vec());
    }

    #[test]
    fn test_strip_ps1_echo_strips_local_echo_with_cr() {
        // Local PTY ECHO returns the exact bytes written (including trailing \r).
        let pattern = build_ps1_marker_command(
            &make_simple_info("10.10.17.243"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            false,
            true,
            true,
            true,
        );
        let mut sup = Ps1EchoSuppressor {
            pattern: pattern.clone(),
            remaining: 2,
            started: std::time::Instant::now(),
            pending: Vec::new(),
            pending_printf_erase: None,
        };
        // Construct an echoed chunk: leading banner + the echoed command (pattern
        // already ends with \r) + trailing prompt fragment.
        let mut data = b"banner line\r\n".to_vec();
        data.extend_from_slice(&pattern);
        data.extend_from_slice(b"[root@host ~]# ");
        let stripped = strip_ps1_echo(&data, &mut sup);
        assert_eq!(sup.remaining, 1, "should have consumed one echo");
        let s = String::from_utf8_lossy(&stripped);
        assert!(!s.contains("PS1="), "PS1= still in output: {}", s);
        assert!(s.contains("banner line"), "banner dropped: {}", s);
        assert!(s.contains("[root@host ~]#"), "prompt dropped: {}", s);
    }

    #[test]
    fn test_strip_ps1_echo_strips_remote_echo_with_escape_sequence() {
        // Remote bash ECHO + printf output: the literal command (with the trailing
        // \r turning into \r\n through OPOST) followed by the actual escape bytes
        // \x1b[A\x1b[J emitted by printf.
        let pattern = build_ps1_marker_command(
            &make_simple_info("10.10.17.243"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            false,
            true,
            true,
            true,
        );
        let escape = vec![0x1b, b'[', b'A', 0x1b, b'[', b'J'];
        let mut sup = Ps1EchoSuppressor {
            pattern: pattern.clone(),
            remaining: 1, // simulating local echo already consumed
            started: std::time::Instant::now(),
            pending: Vec::new(),
            pending_printf_erase: None,
        };
        // Strip the trailing \r from pattern, simulate \r\n line ending, then escape.
        let mut data = pattern.clone();
        let cr_idx = data.len() - 1; // last byte is \r
        data[cr_idx] = b'\n';
        data.insert(cr_idx, b'\r'); // now ends with \r\n
        data.extend_from_slice(&escape);
        data.extend_from_slice(b"[ssh:10.10.17.243] [root@v25 ~]# ");
        let stripped = strip_ps1_echo(&data, &mut sup);
        assert_eq!(sup.remaining, 0, "should have consumed the remote echo");
        let s = String::from_utf8_lossy(&stripped);
        assert!(!s.contains("PS1="), "PS1= still in output: {}", s);
        // The escape bytes should now be preserved (not stripped).
        assert!(
            s.contains('\x1b'),
            "escape bytes should be present: {:?}",
            s
        );
        assert!(s.contains("[root@v25 ~]#"), "prompt dropped: {}", s);
    }

    #[test]
    fn test_strip_ps1_echo_strips_both_echoes_in_one_chunk() {
        // Pathological case: both echoes arrive in a single read() (e.g. very
        // fast local SSH). The function must loop and strip both.
        let pattern = build_ps1_marker_command(
            &make_simple_info("10.10.17.243"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            false,
            true,
            true,
            true,
        );
        let escape = vec![0x1b, b'[', b'A', 0x1b, b'[', b'J'];
        let mut sup = Ps1EchoSuppressor {
            pattern: pattern.clone(),
            remaining: 2,
            started: std::time::Instant::now(),
            pending: Vec::new(),
            pending_printf_erase: None,
        };
        let mut data = pattern.clone(); // local echo (ends with \r)
                                        // Remote echo: pattern without trailing \r, then \r\n, then escape bytes.
        let mut remote = pattern[..pattern.len() - 1].to_vec();
        remote.push(b'\r');
        remote.push(b'\n');
        remote.extend_from_slice(&escape);
        data.extend_from_slice(&remote);
        data.extend_from_slice(b"prompt# ");
        let stripped = strip_ps1_echo(&data, &mut sup);
        assert_eq!(sup.remaining, 0, "should have consumed both echoes");
        let s = String::from_utf8_lossy(&stripped);
        assert!(!s.contains("PS1="), "PS1= leaked: {}", s);
        // The escape bytes should now be preserved (not stripped).
        assert!(s.contains('\x1b'), "escape should be present: {:?}", s);
        assert!(s.contains("prompt#"), "prompt dropped: {}", s);
    }

    #[test]
    fn test_strip_ps1_echo_no_match_returns_input_unchanged() {
        // No pattern match → data unchanged.
        let mut sup = Ps1EchoSuppressor {
            pattern: build_ps1_marker_command(
                &make_simple_info("10.10.17.243"),
                &make_minimal_snapshot(),
                DangerLevel::None,
                false,
                true,
                true,
                true,
            ),
            remaining: 2,
            started: std::time::Instant::now(),
            pending: Vec::new(),
            pending_printf_erase: None,
        };
        let data = b"totally unrelated output\n";
        assert_eq!(strip_ps1_echo(data, &mut sup), data.to_vec());
        assert_eq!(sup.remaining, 2, "should not have consumed anything");
    }

    #[test]
    fn test_strip_ps1_echo_buffers_split_match_across_chunks() {
        // PTY reads are not message-framed: a slow SSH link can split the
        // echoed PS1 command across two reads. The suppressor must retain
        // the trailing partial match from chunk 1 and strip the full
        // pattern once chunk 2 arrives, instead of leaking the first
        // fragment.
        let pattern = build_ps1_marker_command(
            &make_simple_info("10.10.17.243"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            false,
            true,
            true,
            true,
        );
        let mut sup = Ps1EchoSuppressor {
            pattern: pattern.clone(),
            remaining: 1,
            started: std::time::Instant::now(),
            pending: Vec::new(),
            pending_printf_erase: None,
        };
        let split_at = pattern.len() / 2;
        let chunk1 = &pattern[..split_at];
        let chunk2 = &pattern[split_at..];

        let stripped1 = strip_ps1_echo(chunk1, &mut sup);
        assert_eq!(
            sup.remaining, 1,
            "should not consume on partial match — waiting for more bytes"
        );
        // chunk1 is entirely a prefix of the pattern, so nothing is emitted.
        assert!(
            stripped1.is_empty(),
            "partial trailing suffix should be buffered, not emitted: {:?}",
            stripped1
        );
        assert_eq!(sup.pending, chunk1, "pending should hold chunk1 bytes");

        let mut chunk2_data = chunk2.to_vec();
        chunk2_data.extend_from_slice(b"\x1b[A\x1b[J[ssh:10.10.17.243] [root@v25 ~]# ");
        let stripped2 = strip_ps1_echo(&chunk2_data, &mut sup);
        assert_eq!(sup.remaining, 0, "should consume the full match on chunk2");
        let s = String::from_utf8_lossy(&stripped2);
        assert!(!s.contains("PS1="), "PS1= leaked: {}", s);
        assert!(
            s.contains('\x1b'),
            "escape bytes should be present: {:?}",
            s
        );
        assert!(s.contains("[root@v25 ~]#"), "prompt dropped: {}", s);
    }

    #[test]
    fn test_build_ps1_echo_suppressor_constructor() {
        let info = make_simple_info("10.10.17.243");
        let snap = make_minimal_snapshot();
        let sup =
            build_ps1_echo_suppressor(&info, &snap, DangerLevel::None, false, true, true, true);
        assert_eq!(sup.remaining, 2);
        assert!(
            sup.pattern.starts_with(b" PS1='"),
            "pattern: {:?}",
            sup.pattern
        );
        assert!(sup.pattern.ends_with(b"\r"), "pattern must end with CR");
    }

    #[test]
    fn test_build_ps1_marker_command_enabled_contains_git_function() {
        let info = make_simple_info("v25");
        let snap = make_minimal_snapshot();
        let cmd = build_ps1_marker_command(&info, &snap, DangerLevel::None, true, true, true, true);
        let s = String::from_utf8_lossy(&cmd);

        // Must define the context hook function.
        assert!(
            s.contains("__aish_ctx_hook()"),
            "missing function definition"
        );
        assert!(
            s.contains("git symbolic-ref --short HEAD"),
            "missing git invocation"
        );

        // Must wrap PROMPT_COMMAND (preserving any user-existing one).
        assert!(
            s.contains("__aish_orig_pc"),
            "missing PROMPT_COMMAND preservation"
        );
        assert!(
            s.contains("PROMPT_COMMAND=__aish_ctx_hook"),
            "missing PROMPT_COMMAND assignment"
        );

        // Must reference the variable in PS1 prefix.
        assert!(
            s.contains("${__aish_ctx_live}"),
            "missing variable reference in PS1"
        );

        // Must still contain the host marker.
        assert!(s.contains("[ssh:v25"), "missing host marker");

        // Must still end with the echo-suppression printf + CR.
        assert!(cmd.ends_with(b"\r"), "must end with CR");
        assert!(
            s.contains("printf '\\33[A\\33[J'"),
            "missing echo-erase printf"
        );

        // Hostile host name with single quote must NOT break out of quoting.
        let evil_info = make_simple_info("a'b");
        let evil =
            build_ps1_marker_command(&evil_info, &snap, DangerLevel::None, true, true, true, true);
        let es = String::from_utf8_lossy(&evil);
        // The single quote must be escaped via the standard '\'\\'' pattern.
        assert!(es.contains("'\\''"), "single quote in host must be escaped");
    }

    #[test]
    fn test_build_ps1_marker_command_enabled_path_is_longer_than_disabled() {
        let info = make_simple_info("v25");
        let snap = make_minimal_snapshot();
        let off =
            build_ps1_marker_command(&info, &snap, DangerLevel::None, false, true, true, true);
        let on = build_ps1_marker_command(&info, &snap, DangerLevel::None, true, true, true, true);
        assert!(
            on.len() > off.len() + 100,
            "git-aware command must be substantially longer"
        );
    }

    #[test]
    fn test_build_ps1_echo_suppressor_git_aware_pattern_matches() {
        let sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        assert_eq!(
            sup.pattern, cmd,
            "suppressor pattern must match the injected command byte-for-byte"
        );
        assert_eq!(
            sup.remaining, 2,
            "must expect 2 echoes (local PTY + remote bash)"
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_split_across_chunks() {
        // The git-aware injection is ~410 bytes; under nested SSH the PTY
        // frequently splits the bash echo of that injection across reads.
        // The anchor strategy must buffer the partial echo and strip the
        // full pattern once the second half arrives, instead of leaking the
        // first fragment (regression observed on aish → ssh → ssh chains).
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Land the split inside the start anchor: chunk1 ends with the
        // partial prefix ` __aish_ctx_hoo`, chunk2 starts with `k()...`.
        // Pending must bridge the gap so the full anchor is seen on chunk2.
        let anchor = b" __aish_ctx_hook()";
        let anchor_pos = cmd.windows(anchor.len()).position(|w| w == anchor).unwrap();
        let kept_prefix = b" __aish_ctx_hoo"; // 15 bytes — a true prefix of the anchor
        let split = anchor_pos + kept_prefix.len();
        let chunk1 = cmd[..split].to_vec();
        let chunk2 = cmd[split..].to_vec();

        let out1 = strip_ps1_echo(&chunk1, &mut sup);
        assert_eq!(sup.remaining, 2, "suppressor must stay armed while waiting");
        assert_eq!(
            sup.pending, *kept_prefix,
            "pending must hold the partial anchor"
        );
        // Bytes before the held-back prefix pass through unchanged.
        assert_eq!(
            out1,
            cmd[..anchor_pos],
            "prefix before partial anchor must pass through"
        );

        // chunk2 finishes the echo and adds the bash trailing bytes.
        let mut chunk2_data = chunk2;
        chunk2_data.extend_from_slice(b"\r\n\x1b[A\x1b[J");
        let out2 = strip_ps1_echo(&chunk2_data, &mut sup);
        // `remaining` decrements per stripped echo; the suppressor is built
        // expecting 2 echoes (local PTY + remote bash), so one strip leaves
        // it armed for the second.
        assert_eq!(
            sup.remaining, 1,
            "suppressor decrements per echo, not consumed"
        );
        let s = String::from_utf8_lossy(&out2);
        assert!(!s.contains("__aish_ctx_hook"), "echo body leaked: {}", s);
        assert!(
            !s.contains("PROMPT_COMMAND"),
            "PROMPT_COMMAND leaked: {}",
            s
        );
        assert!(
            s.contains('\x1b'),
            "erase sequence should be present: {:?}",
            s
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_strips_both_echoes() {
        // build_ps1_echo_suppressor arms remaining = 2 because the injected
        // command can be echoed twice: once by the local PTY (ECHO termios
        // on the aish-owned pty), once by the remote bash readline. Both
        // echoes carry the same bytes and must be stripped, otherwise the
        // user sees a literal `__aish_ctx_hook()...` line on screen.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Simulate local PTY echo: command bytes + bash trailing sequence.
        let mut echo1 = cmd.clone();
        echo1.extend_from_slice(b"\r\n\x1b[A\x1b[J");
        // Simulate remote bash echo: same shape, possibly separated in time.
        let mut echo2 = cmd.clone();
        echo2.extend_from_slice(b"\r\n\x1b[A\x1b[J");

        let out1 = strip_ps1_echo(&echo1, &mut sup);
        assert_eq!(sup.remaining, 1, "first echo decrements remaining to 1");
        let s1 = String::from_utf8_lossy(&out1);
        assert!(!s1.contains("__aish_ctx_hook"), "echo1 body leaked: {}", s1);

        let out2 = strip_ps1_echo(&echo2, &mut sup);
        assert_eq!(sup.remaining, 0, "second echo decrements remaining to 0");
        let s2 = String::from_utf8_lossy(&out2);
        assert!(!s2.contains("__aish_ctx_hook"), "echo2 body leaked: {}", s2);
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_strips_both_echoes_one_chunk() {
        // Both echoes can land in the same PTY read. The strip loop must
        // consume them in a single call rather than stopping after the first.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        let mut both = Vec::with_capacity(cmd.len() * 2 + 16);
        both.extend_from_slice(&cmd);
        both.extend_from_slice(b"\r\n\x1b[A\x1b[J");
        both.extend_from_slice(&cmd);
        both.extend_from_slice(b"\r\n\x1b[A\x1b[J");

        let out = strip_ps1_echo(&both, &mut sup);
        assert_eq!(sup.remaining, 0, "both echoes must be consumed");
        let s = String::from_utf8_lossy(&out);
        assert!(
            !s.contains("__aish_ctx_hook"),
            "echo body leaked in combined chunk: {}",
            s
        );
        assert!(
            !s.contains("PROMPT_COMMAND"),
            "PROMPT_COMMAND leaked in combined chunk: {}",
            s
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_printf_erase_split_across_chunks() {
        // Regression: when the bash echo's terminating \r\n arrives in one
        // PTY read but the printf escape sequence (\x1b[A\x1b[J) arrives in
        // the next, the suppressor must NOT commit the strip on the first
        // chunk. Committing decrements `remaining` and lets the unanchored
        // PRINTF_ERASE leak through in the next chunk — those bytes move the
        // user's cursor up one line and erase to end of screen, producing
        // wrong cursor position and "swallowed" keystrokes after SSH login.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Chunk 1: full echo + trailing CRLF, but NO printf erase yet.
        let mut chunk1 = cmd.clone();
        chunk1.extend_from_slice(b"\r\n");
        // Chunk 2: PRINTF_ERASE arrives, followed by the new prompt.
        let mut chunk2 = b"\x1b[A\x1b[J".to_vec();
        chunk2.extend_from_slice(b"[ssh:v25] [root@host ~]# ");

        let out1 = strip_ps1_echo(&chunk1, &mut sup);
        assert!(
            sup.remaining >= 1,
            "suppressor must stay armed (still expecting PRINTF_ERASE): out1={:?}, remaining={}",
            String::from_utf8_lossy(&out1),
            sup.remaining,
        );

        let out2 = strip_ps1_echo(&chunk2, &mut sup);
        let s = String::from_utf8_lossy(&out2);
        assert!(
            !s.contains("\x1b[A\x1b[J"),
            "PRINTF_ERASE must be stripped from chunk 2, got {:?}",
            s,
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_printf_erase_partial_prefix_split() {
        // Edge case for the PRINTF_ERASE split fix: chunk 1 ends with a
        // partial prefix of PRINTF_ERASE (e.g. `\x1b[A` — the first 3 of 6
        // bytes). Pending must bridge the partial prefix so chunk 2 can
        // either complete PRINTF_ERASE (strip it) or refute it (emit it).
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Chunk 1: echo + CRLF + first 3 bytes of PRINTF_ERASE.
        let mut chunk1 = cmd.clone();
        chunk1.extend_from_slice(b"\r\n\x1b[A");
        // Chunk 2: last 3 bytes of PRINTF_ERASE + new prompt.
        let mut chunk2 = b"\x1b[J".to_vec();
        chunk2.extend_from_slice(b"[ssh:v25] [root@host ~]# ");

        let out1 = strip_ps1_echo(&chunk1, &mut sup);
        let s1 = String::from_utf8_lossy(&out1);
        assert!(
            !s1.contains("\x1b[A"),
            "partial PRINTF_ERASE prefix must not leak via chunk 1: {:?}",
            s1,
        );

        let out2 = strip_ps1_echo(&chunk2, &mut sup);
        let s2 = String::from_utf8_lossy(&out2);
        assert!(
            !s2.contains("\x1b[A\x1b[J"),
            "PRINTF_ERASE must be stripped once completed in chunk 2: {:?}",
            s2,
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_printf_erase_three_way_split() {
        // Stress the pending_printf_erase re-buffer logic: chunk 1 ends with
        // a strict prefix of PRINTF_ERASE, chunk 2 *also* ends with a strict
        // prefix (extending what chunk 1 started), and only chunk 3 completes
        // the sequence. The suppressor must keep re-buffering until it can
        // confirm or refute — without this, an intermediate chunk would
        // either leak bytes or decrement `remaining` at the wrong time.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Chunk 1: echo + CRLF + byte 1 of PRINTF_ERASE (\x1b).
        let mut chunk1 = cmd.clone();
        chunk1.extend_from_slice(b"\r\n\x1b");
        // Chunk 2: bytes 2-3 of PRINTF_ERASE ([A) — still a strict prefix.
        let chunk2 = b"[A".to_vec();
        // Chunk 3: bytes 4-6 of PRINTF_ERASE (\x1b[J) + new prompt.
        let mut chunk3 = b"\x1b[J".to_vec();
        chunk3.extend_from_slice(b"[ssh:v25] [root@host ~]# ");

        let out1 = strip_ps1_echo(&chunk1, &mut sup);
        assert!(
            sup.remaining >= 1,
            "after chunk 1 suppressor must stay armed: remaining={}, out={:?}",
            sup.remaining,
            String::from_utf8_lossy(&out1),
        );

        let out2 = strip_ps1_echo(&chunk2, &mut sup);
        assert!(
            sup.remaining >= 1,
            "after chunk 2 (still partial prefix) suppressor must stay armed: remaining={}, out={:?}",
            sup.remaining,
            String::from_utf8_lossy(&out2),
        );

        let out3 = strip_ps1_echo(&chunk3, &mut sup);
        let s3 = String::from_utf8_lossy(&out3);
        assert!(
            !s3.contains("\x1b[A\x1b[J"),
            "PRINTF_ERASE must be stripped once completed in chunk 3: {:?}",
            s3,
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_printf_erase_prefix_then_refute() {
        // When chunk 1 ends with a strict prefix of PRINTF_ERASE but chunk 2
        // arrives with bytes that DON'T extend PRINTF_ERASE, the suppressor
        // must emit the buffered prefix verbatim (it's real output, not the
        // escape sequence) and decrement `remaining` exactly once for the
        // echo that was already stripped in chunk 1.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Chunk 1: echo + CRLF + first 3 bytes of PRINTF_ERASE (\x1b[A).
        let mut chunk1 = cmd.clone();
        chunk1.extend_from_slice(b"\r\n\x1b[A");
        // Chunk 2: NOT the rest of PRINTF_ERASE — starts with 'X' which
        // refutes the match. Followed by real prompt output.
        let mut chunk2 = b"Xreal output\n".to_vec();
        chunk2.extend_from_slice(b"[ssh:v25] [root@host ~]# ");

        let out1 = strip_ps1_echo(&chunk1, &mut sup);
        let s1 = String::from_utf8_lossy(&out1);
        assert!(
            !s1.contains("\x1b[A"),
            "partial PRINTF_ERASE prefix must not leak via chunk 1: {:?}",
            s1,
        );

        let remaining_after_1 = sup.remaining;

        let out2 = strip_ps1_echo(&chunk2, &mut sup);
        let s2 = String::from_utf8_lossy(&out2);

        // The buffered `\x1b[A` (3 bytes) must be emitted as real output
        // because the refute proves it wasn't a PRINTF_ERASE sequence.
        assert!(
            s2.contains("\x1b[A"),
            "refuted prefix bytes must be emitted as real output: {:?}",
            s2,
        );
        assert!(
            s2.contains("Xreal output"),
            "bytes after the refuted prefix must pass through: {:?}",
            s2,
        );
        // Exactly one decrement for the echo stripped in chunk 1.
        assert_eq!(
            sup.remaining,
            remaining_after_1.saturating_sub(1),
            "remaining must decrement exactly once across the refute path",
        );
    }

    #[test]
    fn test_ps1_echo_suppressor_git_aware_pending_limit() {
        // If pending grows past PENDING_LIMIT without resolving, the
        // suppressor must give up and flush — otherwise a stray anchor
        // fragment could swallow an unbounded amount of real output.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        sup.pending.clear();

        // Feed a 2 KiB chunk that contains the anchor but no \n: pending
        // grows until it exceeds PENDING_LIMIT, then flushes.
        let mut chunk = b" __aish_ctx_hook() xyz".to_vec();
        chunk.resize(2048, b'x');

        let out = strip_ps1_echo(&chunk, &mut sup);
        assert_eq!(sup.remaining, 0, "suppressor must give up past limit");
        assert_eq!(out.len(), 2048, "all bytes flushed when giving up");
    }

    #[test]
    fn test_strip_ps1_echo_handles_terminal_wrap_cr() {
        // Regression: bash readline inserts bare \r (no \n) at terminal
        // width boundaries when echoing a long command. The strip must
        // tolerate these wrap artifacts.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Simulate 80-col wrap: insert a standalone \r every 80 bytes.
        let mut wrapped = Vec::with_capacity(cmd.len() + cmd.len() / 80);
        for (i, &b) in cmd.iter().enumerate() {
            wrapped.push(b);
            if (i + 1) % 80 == 0 && i + 1 < cmd.len() {
                wrapped.push(b'\r');
            }
        }
        // Append the bytes bash emits after the command: \n then printf output.
        wrapped.extend_from_slice(b"\n\x1b[A\x1b[J");

        let stripped = strip_ps1_echo(&wrapped, &mut sup);
        let stripped_str = String::from_utf8_lossy(&stripped);

        assert!(
            !stripped_str.contains("__aish_ctx_hook"),
            "command bytes leaked through strip: {:?}",
            stripped_str
        );
        assert!(
            !stripped_str.contains("PROMPT_COMMAND"),
            "PROMPT_COMMAND text leaked through strip: {:?}",
            stripped_str
        );
        assert!(
            sup.remaining < 2,
            "suppressor did not match the wrapped echo"
        );
    }

    #[test]
    fn test_strip_ps1_echo_handles_terminal_wrap_with_spaces() {
        // Real-world regression observed in production: bash readline on
        // 119-col terminals inserts not just \r but also SPACE bytes at
        // wrap boundaries when echoing the 410-byte git-aware injection.
        // Byte-exact pattern matching breaks because the wrap spaces are
        // indistinguishable from legitimate spaces in the command. The
        // anchor-based strip path must tolerate them.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("10.10.17.130"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Real echo captured from a live ssh session: the full injected
        // command but with extra spaces inserted at wrap boundaries.
        // Constructed to mirror what bash readline actually produces.
        let cmd = build_ps1_marker_command(
            &make_simple_info("10.10.17.130"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let mut echo = Vec::with_capacity(cmd.len() + 16);
        // Insert a space every ~80 bytes (simulating wrap).
        for (i, &b) in cmd.iter().enumerate() {
            echo.push(b);
            if (i + 1) % 80 == 0 && i + 1 < cmd.len() {
                echo.push(b' ');
            }
        }
        // Trailing bytes: \r\n + printf output.
        echo.extend_from_slice(b"\r\n\x1b[A\x1b[J");

        let stripped = strip_ps1_echo(&echo, &mut sup);
        let stripped_str = String::from_utf8_lossy(&stripped);

        assert!(
            !stripped_str.contains("__aish_ctx_hook"),
            "command bytes leaked through anchor strip: {:?}",
            stripped_str
        );
        assert!(
            !stripped_str.contains("PROMPT_COMMAND"),
            "PROMPT_COMMAND leaked through anchor strip: {:?}",
            stripped_str
        );
        assert!(sup.remaining < 2, "anchor strip did not consume the echo");
    }

    #[test]
    fn test_strip_ps1_echo_preserves_crlf_pairs() {
        // Sanity: \r\n pairs (normal line terminators) must NOT be stripped.
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            false,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            false,
            true,
            true,
            true,
        );

        // Trailing bytes after echo: \r\n + escape sequence (real-world shape).
        let mut data = cmd.clone();
        data.extend_from_slice(b"\r\n\x1b[A\x1b[J");

        let stripped = strip_ps1_echo(&data, &mut sup);
        // The command itself stripped, but \r\n + escapes preserved.
        let s = String::from_utf8_lossy(&stripped);
        assert!(
            s.contains("\r\n"),
            "CRLF pair must be preserved, got: {:?}",
            s
        );
        assert!(
            s.contains("\x1b[J"),
            "escape sequence must be preserved, got: {:?}",
            s
        );
    }

    #[test]
    fn test_stripped_last_line_basic_bash_prompt() {
        let data = b"some output\r\n[root@host ~]# ";
        let last = stripped_last_line(data).expect("should find last line");
        assert_eq!(last, b"[root@host ~]# ");
    }

    #[test]
    fn test_stripped_last_line_strips_ansi() {
        let data = b"\x1b[32muser@host\x1b[0m:~$ ";
        let last = stripped_last_line(data).expect("should find last line");
        assert_eq!(last, b"user@host:~$ ");
    }

    #[test]
    fn test_stripped_last_line_zsh_percent_prompt() {
        let data = b"output\nhost% ";
        let last = stripped_last_line(data).expect("should find last line");
        assert_eq!(last, b"host% ");
    }

    #[test]
    fn test_stripped_last_line_empty_returns_none() {
        assert!(stripped_last_line(b"").is_none());
        assert!(
            stripped_last_line(b"abc\n").is_none(),
            "trailing newline → empty last line"
        );
    }

    /// Open a non-blocking pty pair for I/O tests. Slave ECHO disabled so
    /// probe's own write doesn't get echoed back into the read buffer.
    fn open_test_pty_pair_no_echo() -> (std::fs::File, std::fs::File) {
        use std::os::unix::io::FromRawFd;
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;
        let rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());

        // Disable ECHO on slave so master writes don't echo back.
        // Also disable OPOST/ONLCR so slave-written "\r\n" reaches master
        // unchanged (otherwise ONLCR would translate "\n" to "\r\n" and
        // double the "\r", desynchronizing the marker line parser).
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(slave_fd, &mut tio) == 0 {
                tio.c_lflag &= !libc::ECHO;
                tio.c_oflag &= !libc::OPOST;
                libc::tcsetattr(slave_fd, libc::TCSANOW, &tio);
            }
            // Master non-blocking so probe drain loop polls rather than hangs.
            let flags = libc::fcntl(master_fd, libc::F_GETFL);
            libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
        (master, slave)
    }

    #[test]
    fn test_probe_remote_command_parses_slave_output() {
        use std::os::unix::io::AsRawFd;
        let (master, slave) = open_test_pty_pair_no_echo();
        let slave_fd = slave.as_raw_fd();

        // Simulate remote bash emitting the marker body after probe sends its command.
        // (Probe writes its command first; slave picks it up via line discipline
        // but since no process reads slave, no actual exec happens. We just need
        // the marker bytes to appear at master read.)
        let output_bytes = concat!(
            "@@aish_ctx_start@@\r\n",
            "1000\r\n",
            "docker\r\n",
            "prod-cluster\r\n",
            "@@aish_ctx_end@@\r\n",
        )
        .as_bytes()
        .to_vec();
        let _writer = std::thread::spawn(move || {
            // Give probe time to write its command first.
            std::thread::sleep(std::time::Duration::from_millis(100));
            unsafe {
                libc::write(
                    slave_fd,
                    output_bytes.as_ptr() as *const libc::c_void,
                    output_bytes.len(),
                );
            }
            // Hold slave open until probe drains.
            std::thread::sleep(std::time::Duration::from_secs(2));
        });

        let (snap_opt, _residual) = probe_remote_command(master.as_raw_fd());
        let snap = snap_opt.expect("probe must parse marker body");
        assert_eq!(snap.container.as_deref(), Some("docker"));
        assert_eq!(snap.kube_context.as_deref(), Some("prod-cluster"));
    }

    #[test]
    fn test_probe_remote_command_returns_none_on_timeout() {
        use std::os::unix::io::AsRawFd;
        let (master, slave) = open_test_pty_pair_no_echo();
        // Slave silent — probe should hit 5s deadline and return None.
        // Hold slave open in a thread so master doesn't see EOF immediately.
        let _holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(6));
            drop(slave);
        });
        let start = std::time::Instant::now();
        let (result, _residual) = probe_remote_command(master.as_raw_fd());
        assert!(result.is_none(), "must return None when no markers arrive");
        // Allow slack for select() granularity.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(7),
            "must respect 5s deadline (got {:?})",
            start.elapsed()
        );
    }

    /// Regression: bytes that arrive on the PTY AFTER the probe end marker
    /// during the 5s window must be re-injected into the UI stream (next
    /// prompt, async notifications). Bytes BEFORE the start marker — i.e.
    /// the echoed probe command on a real PTY — must NOT be re-injected, or
    /// the probe literal leaks to the user's terminal. This test seeds both
    /// regions on the slave PTY and asserts only trailing bytes survive in
    /// the residual.
    #[test]
    fn test_probe_remote_command_residual_preserves_noise_bytes() {
        use std::os::unix::io::AsRawFd;
        let (master, slave) = open_test_pty_pair_no_echo();
        let slave_fd = slave.as_raw_fd();

        // `pre_noise` simulates the echoed probe command reflected by bash
        // before execution. The literal must NOT appear in the residual.
        let pre_noise = b" echo @@aish_ctx_start@@; id -u; echo @@aish_ctx_end@@\r\n";
        let marker_body = concat!(
            "@@aish_ctx_start@@\r\n",
            "1000\r\n",
            "docker\r\n",
            "prod-cluster\r\n",
            "@@aish_ctx_end@@\r\n",
        )
        .as_bytes()
        .to_vec();
        let post_noise = b"async notification after markers\r\n";
        let pre = pre_noise.to_vec();
        let post = post_noise.to_vec();
        let _writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            unsafe {
                libc::write(slave_fd, pre.as_ptr() as *const libc::c_void, pre.len());
                libc::write(
                    slave_fd,
                    marker_body.as_ptr() as *const libc::c_void,
                    marker_body.len(),
                );
                libc::write(slave_fd, post.as_ptr() as *const libc::c_void, post.len());
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        });

        let (snap_opt, residual) = probe_remote_command(master.as_raw_fd());
        assert!(snap_opt.is_some(), "probe must still parse marker body");

        let residual_str = String::from_utf8_lossy(&residual);
        // Trailing noise must survive.
        assert!(
            residual_str.contains("async notification after markers"),
            "post-marker noise must survive in residual; got {:?}",
            residual_str
        );
        // CRITICAL: the echoed probe command must NOT leak into residual.
        // If it does, the user sees `echo @@aish_ctx_start@@; id -u; ...`
        // printed to their terminal after the prompt.
        assert!(
            !residual_str.contains("id -u"),
            "echoed probe command must NOT leak into residual; got {:?}",
            residual_str
        );
        // Probe body itself must NOT leak into residual (it's consumed by the
        // parser). Spot-check the kube line which only appears inside markers.
        assert!(
            !residual_str.contains("prod-cluster"),
            "probe body must not be duplicated into residual; got {:?}",
            residual_str
        );
    }

    /// Pure unit test for the residual computation: no PTY, just string in.
    /// Verifies the contract documented on `compute_probe_residual`:
    ///   - No markers -> empty (raw is dominated by echoed command, unsafe to return)
    ///   - Start only -> empty (probe failed, same reason)
    ///   - Full markers -> only trailing bytes survive (pre-bytes are the
    ///     echoed command and would leak the probe literal if returned).
    #[test]
    fn test_compute_probe_residual_pure() {
        // No markers at all -> empty: the raw buffer holds the echoed command
        // at most, and re-injecting it would leak the probe literal.
        let r = compute_probe_residual("echo @@aish_ctx_start@@; id -u\r\n");
        assert!(
            r.is_empty(),
            "no-marker input must return empty; got {:?}",
            r
        );

        // Start only -> empty: probe timed out, body never completed.
        let r = compute_probe_residual("noise\r\n@@aish_ctx_start@@\r\n1000");
        assert!(
            r.is_empty(),
            "start-only input must return empty; got {:?}",
            r
        );

        // Full markers -> only trailing bytes (POST), pre-bytes (PRE) dropped.
        let raw =
            "PRE\r\n@@aish_ctx_start@@\r\n1000\r\ndocker\r\nprod\r\n@@aish_ctx_end@@\r\nPOST\r\n";
        let r = compute_probe_residual(raw);
        let s = String::from_utf8_lossy(&r);
        assert!(
            !s.contains("PRE"),
            "pre-marker bytes (echoed command) must be dropped; got {:?}",
            s
        );
        assert!(
            s.contains("POST\r\n"),
            "trailing bytes must survive; got {:?}",
            s
        );
        assert!(
            !s.contains("Linux"),
            "body must not be duplicated; got {:?}",
            s
        );
    }

    /// Regression: after the first echo is stripped, the suppressor stays
    /// armed waiting for a second echo that may never come (readline-mode
    /// bash produces only one echo). While armed, `strip_ps1_echo_anchor`
    /// must NOT swallow the user's subsequent input — but the original
    /// pending-buffer logic held back any byte that happened to be a prefix
    /// of the start anchor (` __aish_ctx_hook()` starts with a space),
    /// making the spacebar appear dead until the next keypress arrived.
    #[test]
    fn test_ps1_echo_suppressor_does_not_buffer_user_space_after_first_strip() {
        let mut sup = build_ps1_echo_suppressor(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );
        let cmd = build_ps1_marker_command(
            &make_simple_info("v25"),
            &make_minimal_snapshot(),
            DangerLevel::None,
            true,
            true,
            true,
            true,
        );

        // Simulate the single real echo (readline display) followed by the
        // trailing cursor-up erase — this is what a remote bash in readline
        // mode actually emits after the aish injection.
        let mut echo = cmd.clone();
        echo.extend_from_slice(b"\r\n\x1b[A\x1b[J");
        let out = strip_ps1_echo(&echo, &mut sup);
        assert_eq!(
            sup.remaining, 1,
            "first echo strips, second is still pending"
        );
        let s = String::from_utf8_lossy(&out);
        assert!(!s.contains("__aish_ctx_hook"), "echo body leaked: {}", s);

        // User presses SPACE. Before the fix this byte was held in
        // `sup.pending` because `' '` is a prefix of ` __aish_ctx_hook()`,
        // so the spacebar looked dead. It must pass through immediately.
        let out_space = strip_ps1_echo(b" ", &mut sup);
        assert_eq!(
            out_space, b" ",
            "user space keystroke must pass through, not be buffered"
        );
        assert!(
            sup.pending.is_empty(),
            "pending must stay empty after a non-anchor byte"
        );

        // User presses 'p' next; must also pass through unchanged.
        let out_p = strip_ps1_echo(b"p", &mut sup);
        assert_eq!(out_p, b"p", "user 'p' keystroke must pass through");
    }
}
