use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::theme;
use aish_i18n::{t, t_with_args};

/// Whether the current locale is a CJK (Chinese/Japanese/Korean) locale.
/// Cached after the first call. When true, ambiguous-width Unicode
/// characters (●, ➜, etc.) should be treated as 2 columns wide — matching
/// how CJK terminals render them.
fn is_cjk_locale() -> bool {
    static CJK: OnceLock<bool> = OnceLock::new();
    *CJK.get_or_init(|| {
        let lang = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_CTYPE"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let lang = lang.to_lowercase();
        lang.contains("zh") || lang.contains("ja") || lang.contains("ko")
    })
}

/// Compute the visible terminal width of a string, respecting the current
/// locale's handling of ambiguous-width characters. On CJK locales,
/// ambiguous chars count as 2 columns (matching the terminal); on other
/// locales, they count as 1.
pub fn term_width(s: &str) -> usize {
    if is_cjk_locale() {
        unicode_width::UnicodeWidthStr::width_cjk(s)
    } else {
        unicode_width::UnicodeWidthStr::width(s)
    }
}

/// Compute the visible terminal width of a single character, respecting
/// the current locale's handling of ambiguous-width characters.
pub fn term_char_width(ch: char) -> usize {
    if is_cjk_locale() {
        unicode_width::UnicodeWidthChar::width_cjk(ch).unwrap_or(0)
    } else {
        unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
    }
}

/// Walk up from `cwd` to find a `.git/HEAD` file and extract the branch name.
///
/// Returns `Some(branch)` if inside a git repo:
/// - For a normal branch: the branch name (e.g. "main")
/// - For a detached HEAD: first 8 chars of the commit hash
///
/// Returns `None` if no `.git` directory is found walking up to root.
pub fn read_git_branch(cwd: &str) -> Option<String> {
    let mut dir = Path::new(cwd);
    loop {
        let git_head = dir.join(".git").join("HEAD");
        if git_head.is_file() {
            let content = std::fs::read_to_string(&git_head).ok()?;
            let content = content.trim();
            if let Some(rest) = content.strip_prefix("ref: refs/heads/") {
                return Some(rest.to_string());
            }
            // Detached HEAD: return first 8 chars of the hash
            let short = &content[..content.len().min(8)];
            return Some(short.to_string());
        }
        dir = dir.parent()?;
    }
}

/// Cache for git dirty status: (checked_at, cwd, is_dirty).
/// Prevents spawning `git status` on every prompt render — without this,
/// each prompt (every Enter keypress) blocks ~50ms+ while git scans the
/// worktree (worse on NFS-mounted repos).
static GIT_DIRTY_CACHE: Mutex<Option<(Instant, String, bool)>> = Mutex::new(None);

/// How long a cached dirty result remains valid.
const DIRTY_CACHE_TTL: Duration = Duration::from_secs(2);

/// Check if the git working tree has uncommitted changes.
///
/// Cached per-cwd with a 2-second TTL so repeated prompts within the same
/// repo don't re-spawn `git status`.
fn is_git_dirty(cwd: &str) -> bool {
    let now = Instant::now();

    // Fast path: return cached result if still fresh for this cwd.
    if let Ok(cache) = GIT_DIRTY_CACHE.lock() {
        if let Some((checked_at, cached_cwd, dirty)) = cache.as_ref() {
            if *cached_cwd == cwd && now.duration_since(*checked_at) < DIRTY_CACHE_TTL {
                return *dirty;
            }
        }
    }

    // Cache miss: spawn git status for the authoritative answer.
    let dirty = std::process::Command::new("git")
        .args(["--no-optional-locks", "status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    if let Ok(mut cache) = GIT_DIRTY_CACHE.lock() {
        *cache = Some((now, cwd.to_string(), dirty));
    }

    dirty
}

/// Abbreviate a path by keeping `~` and the last component intact,
/// while shortening middle components to their first character.
///
/// Example: `~/nfs/xzx/github/aish` -> `~/n/x/g/aish`
fn abbreviate_path(path: &str, home: &str) -> String {
    let display = if !home.is_empty() && path.starts_with(home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };

    let parts: Vec<&str> = display.split('/').collect();
    if parts.len() <= 2 {
        return display;
    }

    let mut result = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            result.push_str(part);
        } else if i == parts.len() - 1 {
            result.push('/');
            result.push_str(part);
        } else if !part.is_empty() {
            result.push('/');
            if let Some(ch) = part.chars().next() {
                result.push(ch);
            }
        }
    }
    result
}

/// Calculate the visible display width of a string, ignoring ANSI escape sequences.
/// Accounts for CJK double-width characters.
pub fn strip_ansi_len(s: &str) -> usize {
    strip_ansi_len_with(s, term_char_width)
}

/// Visible width for fixed box layouts (welcome panel).
///
/// Always treats East-Asian *Ambiguous* characters (box-drawing, `·`, `—`, …)
/// as 1 column. The panel's `╭─` / `│` / `╰─` math assumes that, and modern
/// terminals — including WeTTY and most GUI emulators under `zh_CN` — render
/// Ambiguous as narrow. Using `term_char_width` (CJK locale → Ambiguous=2)
/// here makes the right border drift by the number of Ambiguous glyphs.
fn panel_strip_ansi_len(s: &str) -> usize {
    strip_ansi_len_with(s, |ch| {
        unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
    })
}

fn strip_ansi_len_with(s: &str, char_width: impl Fn(char) -> usize) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            len += char_width(ch);
        }
    }
    len
}

/// Path to the last-seen changelog version marker (`~/.config/aish/last-changelog-version`).
fn last_changelog_version_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aish")
        .join("last-changelog-version")
}

fn read_last_changelog_version() -> Option<String> {
    let raw = std::fs::read_to_string(last_changelog_version_path()).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn write_last_changelog_version(version: &str) {
    let path = last_changelog_version_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, version.trim());
}

/// Consume the startup upgrade changelog for `current_version`.
///
/// Mimics omp: the first interactive launch after an upgrade shows notes from
/// the embedded CHANGELOG for every version in `last_seen → current`. The
/// marker then advances so steady-state launches keep the existing
/// current-version welcome panel.
///
/// Returns `(ansi_summary, plain_expand_text, previous_version)` when a range
/// should be shown.
pub fn take_startup_changelog_summary(current_version: &str) -> Option<(String, String, String)> {
    let current = current_version.strip_prefix('v').unwrap_or(current_version);
    let last = match read_last_changelog_version() {
        Some(v) => v,
        None => {
            write_last_changelog_version(current);
            return None;
        }
    };
    let last_norm = last.strip_prefix('v').unwrap_or(&last);
    if last_norm == current {
        return None;
    }
    if aish_i18n::changelog::compare_changelog_versions(last_norm, current)
        != std::cmp::Ordering::Less
    {
        // Marker is ahead or incomparable — reseat to current and skip.
        write_last_changelog_version(current);
        return None;
    }

    let summary = aish_i18n::changelog::format_changelog_range_summary(last_norm, current);
    let plain = aish_i18n::changelog::format_changelog_range_plain(last_norm, current);
    write_last_changelog_version(current);
    match (summary, plain) {
        (Some(ansi), Some(text)) => Some((ansi, text, last_norm.to_string())),
        _ => None,
    }
}

/// Localized changelog title for the current version (e.g. "v0.3.4 更新内容").
/// Shared between the welcome panel header and the Ctrl+O record title so
/// they never drift apart.
pub fn changelog_title(version: &str) -> String {
    let mut args = HashMap::new();
    args.insert("version".to_string(), format!("v{}", version));
    t_with_args("shell.welcome2.changelog_title", &args)
}

/// Truncate a string with ANSI codes to a maximum visible length.
///
/// Only handles SGR sequences (`\x1b[...m`) — changelog lines use color codes
/// only, so cursor-movement escapes are not expected here. If broader ANSI
/// coverage is needed later, switch to a proper parser.
fn truncate_ansi(s: &str, max_visible: usize) -> String {
    let mut result = String::new();
    let mut visible = 0;
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Find the end of the ANSI escape sequence
            let end = bytes[i + 2..]
                .iter()
                .position(|&b| b == b'm')
                .map(|p| i + 2 + p + 1);
            if let Some(end_pos) = end {
                result.push_str(&s[i..end_pos]);
                i = end_pos;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        let ch_len = ch.len_utf8();
        // Ambiguous=1, same as panel_strip_ansi_len — welcome-panel only.
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if visible + ch_width > max_visible {
            break;
        }
        result.push(ch);
        visible += ch_width;
        i += ch_len;
    }
    result
}

/// Render the shell prompt in compact.aish theme style.
///
/// Format: `◆ aish ~/n/x/g/aish ⎇ branch ●➜ `
///
/// - Mode badge: accent-blue `◆ aish` or warning-amber `◆ plan`
/// - Path is abbreviated and muted
/// - Git branch in gold with the `⎇` icon; clean (green `●`) or dirty (amber `●`) dot
/// - Prompt symbol: green `➜` on success, red `➜➜` on error
pub fn render_prompt(
    cwd: &str,
    _model: &str,
    last_exit_code: i32,
    mode: &str,
    recording: bool,
    prompt_style: Option<&str>,
) -> String {
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    // Recording indicator
    let mut prompt = String::new();
    if recording {
        prompt.push_str(&format!("{} ", theme::error("⏺")));
    }

    // Mode badge (accent for aish, warning for plan)
    let badge = if mode == "plan" {
        theme::warning(&format!("{} {}", theme::MODE_ICON, mode))
    } else {
        theme::accent(&format!("{} {}", theme::MODE_ICON, mode))
    };
    prompt.push_str(&format!("{} ", badge));

    // Abbreviated path (muted)
    let abbreviated = abbreviate_path(cwd, &home);
    prompt.push_str(&theme::muted(&abbreviated));

    // Git branch and status: "⎇ branch ●" — a space before the git icon and
    // between the branch name and the dirty/clean dot for readability.
    if let Some(branch) = read_git_branch(cwd) {
        prompt.push_str(&format!(
            " {}",
            theme::gold(&format!("{} {}", theme::ICON_GIT, branch))
        ));
        if is_git_dirty(cwd) {
            prompt.push_str(&format!(" {}", theme::warning("●")));
        } else {
            prompt.push_str(&format!(" {}", theme::success("●")));
        }
    }

    // Prompt symbol based on last exit code. A custom `prompt_style`
    // overrides the default arrow (doubled on error to match the default).
    let ok_sym = prompt_style.unwrap_or(theme::PROMPT_OK);
    let err_sym = match prompt_style {
        Some(s) => format!("{s}{s}"),
        None => theme::PROMPT_ERR.to_string(),
    };
    if last_exit_code == 0 {
        prompt.push_str(&format!("{} ", theme::success(ok_sym)));
    } else {
        prompt.push_str(&format!("{} ", theme::error(&err_sym)));
    }

    prompt
}

/// Render the welcome banner shown when the shell starts.
///
/// Matches the Python version with:
/// - ASCII art logo in the terminal default foreground color
/// - Rounded box info panel
/// - Quick start tips
/// - Risk warning
/// - Changelog entries (up to 2 shown; Ctrl+O expands to full list)
pub fn render_welcome(
    version: &str,
    model: &str,
    skill_count: usize,
    changelog: Vec<aish_i18n::changelog::ChangelogEntry>,
) -> String {
    let mut out = String::new();
    out.push('\n');

    // Keep the logo in the terminal default foreground color to match Python.
    let logo_lines = [
        " █████╗ ██╗███████╗██╗  ██╗",
        "██╔══██╗██║██╔════╝██║  ██║",
        "███████║██║███████╗███████║",
        "██╔══██║██║╚════██║██╔══██║",
        "██║  ██║██║███████║██║  ██║",
        "╚═╝  ╚═╝╚═╝╚══════╝╚═╝  ╚═╝",
    ];
    for line in logo_lines {
        out.push_str(line);
        out.push('\n');
    }

    out.push('\n');

    // Rounded box panel (fixed width 60 chars)
    let panel_width: usize = 60;
    let inner_width = panel_width - 2; // minus the two │ chars
    let mut header_args = HashMap::new();
    header_args.insert("version".to_string(), format!("v{}", version));
    let header = t_with_args("shell.welcome2.header", &header_args);

    // Panel content lines
    let model_label = t("shell.welcome2.label.model");
    let config_label = t("shell.welcome2.label.config");
    let skills_label = t("cli.startup.label.skills");
    let config_path = "~/.config/aish/config.yaml";
    let model_hint = t("shell.welcome2.model_hint");
    let skills_suffix = t("shell.welcome2.skills_loaded_suffix");

    let mut content_lines = vec![
        String::new(),
        format!(
            "  {}: {} {}",
            theme::bold(&model_label),
            model,
            theme::faint(&model_hint)
        ),
        format!("  {}: {}", theme::bold(&config_label), config_path),
        format!(
            "  {}: {} {}",
            theme::bold(&skills_label),
            theme::success(&format!("#{}", skill_count)),
            skills_suffix
        ),
        String::new(),
    ];

    // Changelog section (inside the panel)
    if !changelog.is_empty() {
        let title = changelog_title(version);

        let max_entries = 2usize;
        let entries_to_show = changelog.len().min(max_entries);
        let has_more = changelog.len() > max_entries;

        // Title line. When the panel already shows every entry, omit the
        // "Ctrl+O 查看全部" hint — there's nothing more to expand, and the
        // hint would mislead users into opening Ctrl+O expecting hidden
        // entries.
        let header = if has_more {
            let mut hint_args = HashMap::new();
            hint_args.insert("version".to_string(), format!("v{}", version));
            hint_args.insert("count".to_string(), changelog.len().to_string());
            let hint_prefix = t_with_args("shell.welcome2.changelog_hint_prefix", &hint_args);
            let hint_suffix = t("shell.welcome2.changelog_hint_suffix");
            format!(
                "  {} {} {} {}",
                theme::accent(&theme::bold(&title)),
                theme::faint("-"),
                theme::faint(&format!(
                    "{} {} {}",
                    hint_prefix,
                    theme::accent("Ctrl+O"),
                    hint_suffix
                )),
                ""
            )
        } else {
            format!("  {}", theme::accent(&theme::bold(&title)))
        };
        content_lines.push(header);

        for entry in &changelog[..entries_to_show] {
            let cat = entry.category.as_str();
            let badge_styled = match cat {
                "Added" => theme::success("[+]"),
                "Changed" => theme::warning("[*]"),
                "Fixed" => theme::error("[!]"),
                _ => theme::dim("[-]"),
            };
            let mut line = format!("  {} {}", badge_styled, entry.text);
            let visible = panel_strip_ansi_len(&line);
            if visible > inner_width {
                let truncated = truncate_ansi(&line, inner_width - 3);
                // Re-append reset: truncate_ansi may have stopped before the
                // trailing \x1b[0m, leaving the active color to leak into the
                // panel padding that follows on the same rendered line.
                line = format!("{}...\x1b[0m", truncated);
            }
            content_lines.push(line);
        }
    }

    // Render rounded box top with the same title as the Python panel.
    let title = format!(" {} ", header);
    let title_len = panel_strip_ansi_len(&title);
    let top_fill = inner_width.saturating_sub(title_len + 1);
    out.push_str(&theme::dim(&format!(
        "╭─{}{}╮",
        title,
        "─".repeat(top_fill)
    )));
    out.push('\n');

    // Render content lines
    for line in &content_lines {
        let visible_len = panel_strip_ansi_len(line);
        let padding = inner_width.saturating_sub(visible_len);
        out.push_str(&theme::dim("│"));
        out.push_str(line);
        out.push_str(&" ".repeat(padding));
        out.push_str(&theme::dim("│"));
        out.push('\n');
    }

    // Render rounded box bottom
    out.push_str(&theme::dim(&format!("╰{}╯", "─".repeat(inner_width))));
    out.push('\n');

    out.push('\n');

    // Quick start section
    let qs_title = t("shell.welcome2.quick_start.title");
    out.push_str(&theme::bold(&qs_title));
    out.push('\n');

    let item1_prefix = t("shell.welcome2.quick_start.item1_prefix");
    let cmd_ls = t("shell.welcome2.quick_start.cmd_ls");
    let cmd_top = t("shell.welcome2.quick_start.cmd_top");
    let cmd_vim = t("shell.welcome2.quick_start.cmd_vim");
    let cmd_ssh = t("shell.welcome2.quick_start.cmd_ssh");
    let item1_suffix = t("shell.welcome2.quick_start.item1_suffix");
    let item2_prefix = t("shell.welcome2.quick_start.item2_prefix");
    let item2_example = t("shell.welcome2.quick_start.item2_example");
    let item3_prefix = t("shell.welcome2.quick_start.item3_prefix");
    let item3_suffix_1 = t("shell.welcome2.quick_start.item3_suffix_1");
    let item3_keyword = t("shell.welcome2.quick_start.item3_keyword");
    let item3_suffix_2 = t("shell.welcome2.quick_start.item3_suffix_2");

    let quick_1_prefix = format!(" • {}", item1_prefix);
    let quick_2_prefix = format!(" • {}", item2_prefix);
    let quick_3_prefix = format!(" • {}", item3_prefix);
    let content_start = [
        strip_ansi_len(&quick_1_prefix),
        strip_ansi_len(&quick_2_prefix),
        strip_ansi_len(&quick_3_prefix),
    ]
    .into_iter()
    .max()
    .unwrap_or(0)
        + 1;

    let mut quick_1 = String::new();
    quick_1.push_str(&quick_1_prefix);
    quick_1.push_str(&" ".repeat(content_start.saturating_sub(strip_ansi_len(&quick_1_prefix))));
    let cmds = format!(
        "{}, {}, {}, {}",
        theme::accent(&cmd_ls),
        theme::accent(&cmd_top),
        theme::accent(&cmd_vim),
        theme::accent(&cmd_ssh)
    );
    quick_1.push_str(&cmds);
    quick_1.push_str(&item1_suffix);
    out.push_str(&quick_1);
    out.push('\n');

    let mut quick_2 = String::new();
    quick_2.push_str(&quick_2_prefix);
    quick_2.push_str(&" ".repeat(content_start.saturating_sub(strip_ansi_len(&quick_2_prefix))));
    let item2_parts: Vec<&str> = item2_example.split(';').collect();
    if let Some(first_part) = item2_parts.first() {
        quick_2.push_str(first_part);
        for part in item2_parts.iter().skip(1) {
            quick_2.push_str(&theme::accent(";"));
            quick_2.push_str(part);
        }
    }
    out.push_str(&quick_2);
    out.push('\n');

    let mut quick_3 = String::new();
    quick_3.push_str(&quick_3_prefix);
    quick_3.push_str(&" ".repeat(content_start.saturating_sub(strip_ansi_len(&quick_3_prefix))));
    quick_3.push_str(&item3_suffix_1);
    quick_3.push_str(&theme::accent(&item3_keyword));
    quick_3.push_str(&item3_suffix_2);
    out.push_str(&quick_3);
    out.push('\n');

    out.push('\n');

    // Risk warning
    let risk = t("shell.welcome2.risk");
    out.push_str(&theme::faint(&risk));
    out.push('\n');

    out
}

/// Format all changelog entries as plain text for Ctrl+O ExpandPanel view.
/// No ANSI codes — ratatui renders raw escape sequences as visible text.
/// Lines are wrapped at 76 chars to fit the panel.
///
/// Returns an empty `String` when `entries` is empty so the caller can skip
/// without leaving a header-only stub in the ExpandPanel.
pub fn format_changelog_full(
    version: &str,
    entries: &[aish_i18n::changelog::ChangelogEntry],
) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut out = String::new();

    let title = changelog_title(version);
    out.push_str(&title);
    out.push_str("\n\n");

    for entry in entries {
        let badge = match entry.category.as_str() {
            "Added" => "[+]",
            "Changed" => "[*]",
            "Fixed" => "[!]",
            _ => "[-]",
        };
        // Wrap long descriptions to fit panel width (~76 visible chars)
        let prefix = format!("{} ", badge);
        let prefix_len = prefix.chars().count();
        let max_line = 76usize.saturating_sub(prefix_len);
        let text = &entry.text;
        let mut first = true;
        for chunk in wrap_text(text, max_line) {
            if first {
                out.push_str(&prefix);
                out.push_str(&chunk);
                out.push('\n');
                first = false;
            } else {
                // Continuation line indented to align with badge text
                let indent = " ".repeat(prefix_len);
                out.push_str(&indent);
                out.push_str(&chunk);
                out.push('\n');
            }
        }
    }

    out
}

/// Wrap text at `max` characters per line, breaking at word boundaries.
fn wrap_text(text: &str, max: usize) -> Vec<String> {
    if text.is_empty() || max == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if current_len == 0 {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= max {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            lines.push(current);
            current = word.to_string();
            current_len = word_len;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(text.to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_abbreviate_path_short() {
        // Short paths should not be abbreviated
        assert_eq!(abbreviate_path("/tmp", "/home/user"), "/tmp");
        assert_eq!(abbreviate_path("/home/user", "/home/user"), "~");
    }

    #[test]
    fn test_abbreviate_path_long() {
        let home = "/home/user";
        let path = "/home/user/nfs/xzx/github/aish";
        let result = abbreviate_path(path, home);
        assert_eq!(result, "~/n/x/g/aish");
    }

    #[test]
    fn test_abbreviate_path_two_parts() {
        let home = "/home/user";
        let path = "/home/user/projects";
        let result = abbreviate_path(path, home);
        assert_eq!(result, "~/projects");
    }

    #[test]
    fn test_abbreviate_path_outside_home() {
        // Paths outside home still get abbreviated when they have > 2 components
        let result = abbreviate_path("/usr/local/bin", "/home/user");
        assert_eq!(result, "/u/l/bin");
    }

    #[test]
    fn test_abbreviate_path_chinese() {
        // CJK path components should use their first character
        let home = "/home/user";
        let result = abbreviate_path("/home/user/桌面/测试/项目文件", home);
        assert_eq!(result, "~/桌/测/项目文件");
    }

    #[test]
    fn test_abbreviate_path_single_middle() {
        // Path with only one middle component (edge case for >= 3 parts)
        let result = abbreviate_path("/home/user/projects", "/home/user");
        assert_eq!(result, "~/projects");
    }

    #[test]
    fn test_strip_ansi_len_plain() {
        assert_eq!(strip_ansi_len("hello"), 5);
        assert_eq!(strip_ansi_len(""), 0);
    }

    #[test]
    fn test_strip_ansi_len_with_escape() {
        assert_eq!(strip_ansi_len("\x1b[32mhello\x1b[0m"), 5);
        // Use ASCII 'A' (always 1 col) instead of '•' (ambiguous: 1 or 2
        // cols depending on locale) so the test is locale-independent.
        assert_eq!(strip_ansi_len("\x1b[1;36mA\x1b[0m"), 1);
    }

    #[test]
    fn test_strip_ansi_len_complex() {
        let line = format!("  \x1b[1m{}:\x1b[0m {}", "model", "gpt-4");
        // "  model: gpt-4" visible = 14
        assert_eq!(strip_ansi_len(&line), 14);
    }

    /// Ambiguous-width characters (●, ➜, •) have different widths depending
    /// on locale: 1 col in non-CJK, 2 cols in CJK. `term_width` must match
    /// the locale so the inline-completion spinner's `lines_up` calculation
    /// agrees with the terminal's actual rendering. This test verifies the
    /// locale-dependent behavior is consistent — on a CJK locale, ambiguous
    /// chars are wider; ASCII is always 1 col regardless.
    #[test]
    fn test_term_width_locale_consistency() {
        // ASCII is always 1 col, regardless of locale.
        assert_eq!(term_width("hello"), 5);
        // CJK characters are always 2 cols (Wide, not Ambiguous).
        assert_eq!(term_width("中文"), 4);
        // The width of the ambiguous char ● depends on the locale — just
        // verify it's one of the two valid values (1 or 2).
        let bullet_width = term_char_width('●');
        assert!(bullet_width == 1 || bullet_width == 2);
    }

    #[test]
    fn test_render_prompt_with_home_substitution() {
        let home = dirs::home_dir().expect("home dir should exist");
        let cwd = home.join("projects").to_string_lossy().to_string();
        let result = render_prompt(&cwd, "test-model", 0, "aish", false, None);
        assert!(
            result.contains("~"),
            "should substitute home with ~: {}",
            result
        );
        assert!(result.contains("➜"), "should contain prompt symbol");
        assert!(result.contains("◆ aish"), "should contain mode badge");
    }

    #[test]
    fn test_render_prompt_without_git() {
        // /tmp is very unlikely to be inside a git repo
        let result = render_prompt("/tmp", "test-model", 0, "aish", false, None);
        // Should NOT contain git branch separator '|'
        assert!(
            !result.contains("|"),
            "should not contain git branch separator when no .git: {}",
            result
        );
    }

    #[test]
    fn test_render_prompt_success_symbol() {
        let result = render_prompt("/tmp", "test-model", 0, "aish", false, None);
        assert!(
            result.contains("➜"),
            "should have single arrow prompt on success: {}",
            result
        );
        assert!(
            !result.contains("➜➜"),
            "should not have double arrow on success"
        );
    }

    #[test]
    fn test_render_prompt_error_symbol() {
        let result = render_prompt("/tmp", "test-model", 1, "aish", false, None);
        assert!(
            result.contains("➜➜"),
            "should have double arrow prompt on error: {}",
            result
        );
    }

    #[test]
    fn test_render_prompt_aish_mode_badge() {
        let result = render_prompt("/tmp", "test-model", 0, "aish", false, None);
        assert!(result.contains("◆ aish"), "should show aish mode badge");
    }

    #[test]
    fn test_render_prompt_plan_mode_badge() {
        let result = render_prompt("/tmp", "test-model", 0, "plan", false, None);
        assert!(result.contains("◆ plan"), "should show plan mode badge");
    }

    #[test]
    fn test_truncate_ansi_counts_display_width_for_cjk() {
        // CJK glyphs occupy 2 terminal columns each. truncate_ansi must
        // measure display width (matching strip_ansi_len), not char count —
        // otherwise a "5-column" budget would let 5 CJK chars through (10
        // visible columns), blowing past the panel boundary.
        let s = "\x1b[32m中文字符串\x1b[0m"; // 5 CJK chars, display width 10
        assert_eq!(strip_ansi_len(s), 10);
        let truncated = truncate_ansi(s, 4);
        assert_eq!(
            strip_ansi_len(&truncated),
            4,
            "truncated display width must match budget, got {:?}",
            truncated,
        );
        // Two CJK chars (4 columns) kept; the third would push to 6.
        assert!(truncated.contains("中文"));
        assert!(!truncated.contains("字"));
    }

    #[test]
    fn test_truncate_ansi_preserves_inner_color_codes() {
        // Color codes inside the kept prefix must be preserved verbatim so
        // the truncated string keeps its styling up to the cut point.
        let s = "\x1b[32mgreen\x1b[0m text overflow";
        let truncated = truncate_ansi(s, 6);
        assert!(
            truncated.starts_with("\x1b[32m"),
            "leading SGR code must be preserved: {:?}",
            truncated,
        );
        assert!(
            truncated.contains("green"),
            "leading visible text must be preserved: {:?}",
            truncated,
        );
    }

    #[test]
    fn test_render_welcome_changelog_truncated_appends_reset() {
        // Long changelog lines get truncated to inner_width-3 chars plus
        // "...". Without an explicit reset, the panel padding after the
        // line would inherit the changelog entry's color. Verify the
        // truncated line ends with \x1b[0m so the padding stays neutral.
        let entry = aish_i18n::changelog::ChangelogEntry {
            category: "Added".to_string(),
            text: "X".repeat(120),
        };
        let result = render_welcome("0.3.5", "gpt-4", 0, vec![entry]);
        let lines: Vec<&str> = result.lines().collect();
        let truncated_line = lines
            .iter()
            .find(|line| line.contains("..."))
            .expect("expected a truncated changelog line in the output");
        // With NO_COLOR set, theme functions emit plain text (no ANSI), so
        // there is no color to reset. Otherwise the right border's own reset
        // makes the line end with \x1b[0m, keeping padding neutral.
        if std::env::var_os("NO_COLOR").is_none() {
            assert!(
                truncated_line.ends_with("\x1b[0m"),
                "truncated line must end with reset so padding stays neutral: {:?}",
                truncated_line,
            );
        }
    }

    #[test]
    fn test_render_prompt_recording_indicator() {
        let result = render_prompt("/tmp", "test-model", 0, "aish", true, None);
        assert!(
            result.contains("⏺"),
            "should contain recording indicator: {}",
            result
        );
    }

    #[test]
    fn test_render_prompt_no_recording_indicator() {
        let result = render_prompt("/tmp", "test-model", 0, "aish", false, None);
        assert!(
            !result.contains("⏺"),
            "should not contain recording indicator when not recording: {}",
            result
        );
    }

    #[test]
    fn test_read_git_branch_some() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature-branch\n").unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_git_branch(&cwd), Some("feature-branch".to_string()));
    }

    #[test]
    fn test_read_git_branch_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_git_branch(&cwd), None);
    }

    #[test]
    fn test_read_git_branch_detached() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(
            git_dir.join("HEAD"),
            "a1b2c3d4e5f67890abcdef1234567890abcd1234\n",
        )
        .unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_git_branch(&cwd), Some("a1b2c3d4".to_string()));
    }

    #[test]
    fn test_render_welcome_panel_right_border_aligns() {
        // Every panel row (top / content / bottom) must share the same
        // Ambiguous=1 display width so the right │ / ╮ / ╯ form a straight edge
        // under zh_CN and in WeTTY — where Ambiguous glyphs are still 1 col.
        let mk = |text: &str| aish_i18n::changelog::ChangelogEntry {
            category: "Added".to_string(),
            text: text.to_string(),
        };
        let result = render_welcome(
            "0.3.8",
            "deepseek-v4-flash",
            8,
            vec![mk("short change"), mk("another"), mk("third")],
        );
        let panel_lines: Vec<&str> = result
            .lines()
            .filter(|l| {
                // theme::dim may wrap the border; match on the box glyphs.
                panel_strip_ansi_len(l) > 0
                    && (l.contains('╭') || l.contains('╰') || l.contains('│'))
            })
            .collect();
        assert!(
            panel_lines.len() >= 4,
            "expected panel rows, got {}",
            panel_lines.len()
        );
        let widths: Vec<usize> = panel_lines
            .iter()
            .map(|l| panel_strip_ansi_len(l))
            .collect();
        let first = widths[0];
        assert!(
            widths.iter().all(|&w| w == first),
            "panel row widths must match: {:?}",
            widths
        );
    }

    #[test]
    fn test_render_welcome_contains_logo() {
        let result = render_welcome("0.1.0", "gpt-4", 3, vec![]);
        assert!(result.contains("█████"), "should contain ASCII art logo");
        assert!(result.contains("╭"), "should contain rounded box top-left");
        assert!(result.contains("╮"), "should contain rounded box top-right");
        assert!(
            result.contains("╰"),
            "should contain rounded box bottom-left"
        );
        assert!(
            result.contains("╯"),
            "should contain rounded box bottom-right"
        );
        assert!(result.contains("gpt-4"), "should contain model name");
        assert!(
            result.contains(">_ AI Shell v0.1.0"),
            "should contain titled panel with version"
        );
        assert!(result.contains("#3"), "should contain skill count");
    }

    #[test]
    fn test_render_welcome_contains_quick_start() {
        let result = render_welcome("0.1.0", "gpt-4", 0, vec![]);
        assert!(result.contains("•"), "should contain bullet points");
        assert!(result.contains("ls"), "should contain ls example command");
        assert!(result.contains("top"), "should contain top example command");
        assert!(result.contains("vim"), "should contain vim example command");
        assert!(result.contains("ssh"), "should contain ssh example command");
    }

    #[test]
    fn test_render_welcome_uses_default_logo_color() {
        let result = render_welcome("0.1.0", "gpt-4", 0, vec![]);
        assert!(
            !result.contains("\x1b[38;5;250m"),
            "logo should not use grayscale gradient colors"
        );
    }

    #[test]
    fn test_render_welcome_starts_with_blank_line() {
        let result = render_welcome("0.1.0", "gpt-4", 0, vec![]);
        assert!(
            result.starts_with('\n'),
            "welcome banner should leave a blank line above the logo"
        );
    }

    #[test]
    fn test_render_welcome_aligns_quick_start_content() {
        let result = render_welcome("0.1.0", "gpt-4", 0, vec![]);
        let item1_prefix = t("shell.welcome2.quick_start.item1_prefix");
        let item2_prefix = t("shell.welcome2.quick_start.item2_prefix");
        let item3_prefix = t("shell.welcome2.quick_start.item3_prefix");
        let item2_example = t("shell.welcome2.quick_start.item2_example");
        let item3_suffix_1 = t("shell.welcome2.quick_start.item3_suffix_1");

        let item1_line = result
            .lines()
            .find(|line| line.contains(&item1_prefix))
            .expect("item1 line should exist");
        let item2_line = result
            .lines()
            .find(|line| line.contains(&item2_prefix))
            .expect("item2 line should exist");
        let item3_line = result
            .lines()
            .find(|line| line.contains(&item3_prefix))
            .expect("item3 line should exist");

        let item1_anchor = item1_line.find("ls").expect("item1 content should exist");
        let item2_first_part = item2_example
            .split(';')
            .next()
            .expect("item2 example should have content");
        let item2_anchor = item2_line
            .find(item2_first_part)
            .expect("item2 content should exist");
        let item3_anchor = item3_line
            .find(&item3_suffix_1)
            .expect("item3 content should exist");

        let item1_column = strip_ansi_len(&item1_line[..item1_anchor]);
        let item2_column = strip_ansi_len(&item2_line[..item2_anchor]);
        let item3_column = strip_ansi_len(&item3_line[..item3_anchor]);

        assert_eq!(item1_column, item2_column);
        assert_eq!(item1_column, item3_column);
    }
}
