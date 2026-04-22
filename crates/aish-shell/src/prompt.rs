use std::path::Path;

use aish_i18n::t;

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

/// Check if the git working tree has uncommitted changes.
fn is_git_dirty(cwd: &str) -> bool {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output();
    match output {
        Ok(o) => !o.stdout.is_empty(),
        Err(_) => false,
    }
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
            result.push_str(&part[..1]);
        }
    }
    result
}

/// Calculate the visible display width of a string, ignoring ANSI escape sequences.
/// Accounts for CJK double-width characters.
fn strip_ansi_len(s: &str) -> usize {
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
            len += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    len
}

/// Render the shell prompt in compact.aish theme style.
///
/// Format: `<mode> ~/n/x/g/aish|branch●➜ `
///
/// - Mode badge: magenta `<aish>` or yellow `<plan>`
/// - Path is abbreviated and colored blue
/// - Git branch in magenta with clean (green ●) or dirty (red ●) indicator
/// - Prompt symbol: green ➜ on success, red ➜➜ on error
pub fn render_prompt(cwd: &str, _model: &str, last_exit_code: i32, mode: &str) -> String {
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    // Mode badge (magenta for aish, yellow for plan)
    let mode_color = if mode == "plan" { "33" } else { "35" };
    let mut prompt = format!("\x1b[{}m<{}>\x1b[0m ", mode_color, mode);

    // Abbreviated path in blue
    let abbreviated = abbreviate_path(cwd, &home);
    prompt.push_str(&format!("\x1b[34m{}\x1b[0m", abbreviated));

    // Git branch and status
    if let Some(branch) = read_git_branch(cwd) {
        prompt.push_str(&format!("|\x1b[35m{}\x1b[0m", branch));
        if is_git_dirty(cwd) {
            prompt.push_str("\x1b[31m●\x1b[0m");
        } else {
            prompt.push_str("\x1b[32m●\x1b[0m");
        }
    }

    // Prompt symbol based on last exit code
    if last_exit_code == 0 {
        prompt.push_str("\x1b[32m➜\x1b[0m ");
    } else {
        prompt.push_str("\x1b[31m➜➜\x1b[0m ");
    }

    prompt
}

/// Render the welcome banner shown when the shell starts.
///
/// Matches the Python version with:
/// - ASCII art logo with grayscale gradient
/// - Rounded box info panel
/// - Quick start tips
/// - Risk warning
pub fn render_welcome(_version: &str, model: &str, skill_count: usize) -> String {
    let mut out = String::new();

    // ASCII art logo with grayscale gradient
    let logo_lines = [
        " █████╗ ██╗███████╗██╗  ██╗",
        "██╔══██╗██║██╔════╝██║  ██║",
        "███████║██║███████╗███████║",
        "██╔══██║██║╚════██║██╔══██║",
        "██║  ██║██║███████║██║  ██║",
        "╚═╝  ╚═╝╚═╝╚══════╝╚═╝  ╚═╝",
    ];
    let gray_colors: [u8; 6] = [250, 248, 245, 243, 240, 238];
    for (i, line) in logo_lines.iter().enumerate() {
        out.push_str(&format!("\x1b[38;5;{}m{}\x1b[0m\n", gray_colors[i], line));
    }

    out.push('\n');

    // Rounded box panel (fixed width 60 chars)
    let panel_width: usize = 60;
    let inner_width = panel_width - 2; // minus the two │ chars

    // Panel content lines
    let model_label = t("shell.welcome2.label.model");
    let config_label = t("shell.welcome2.label.config");
    let skills_label = t("cli.startup.label.skills");
    let config_path = "~/.config/aish/config.yaml";
    let model_hint = t("shell.welcome2.model_hint");
    let skills_suffix = t("shell.welcome2.skills_loaded_suffix");

    let content_lines = vec![
        String::new(),
        format!(
            "  \x1b[1m{}:\x1b[0m {} \x1b[2m{}\x1b[0m",
            model_label, model, model_hint
        ),
        format!("  \x1b[1m{}:\x1b[0m {}", config_label, config_path),
        format!(
            "  \x1b[1m{}:\x1b[0m \x1b[92m#{}\x1b[0m {}",
            skills_label, skill_count, skills_suffix
        ),
        String::new(),
    ];

    // Render rounded box top
    out.push_str(&format!("\x1b[37m╭{}╮\x1b[0m\n", "─".repeat(inner_width)));

    // Render content lines
    for line in &content_lines {
        let visible_len = strip_ansi_len(line);
        let padding = inner_width.saturating_sub(visible_len);
        out.push_str(&format!(
            "\x1b[37m│\x1b[0m{}{}\x1b[37m│\x1b[0m\n",
            line,
            " ".repeat(padding)
        ));
    }

    // Render rounded box bottom
    out.push_str(&format!("\x1b[37m╰{}╯\x1b[0m\n", "─".repeat(inner_width)));

    out.push('\n');

    // Quick start section
    let qs_title = t("shell.welcome2.quick_start.title");
    out.push_str(&format!("\x1b[1m{}\x1b[0m\n", qs_title));

    let item1_prefix = t("shell.welcome2.quick_start.item1_prefix");
    let item1_suffix = t("shell.welcome2.quick_start.item1_suffix");
    out.push_str(&format!(
        " \x1b[1;36m•\x1b[0m {} \x1b[1;36m{}\x1b[0m {}\n",
        item1_prefix, "ls, top, vim, ssh", item1_suffix
    ));

    let item2_prefix = t("shell.welcome2.quick_start.item2_prefix");
    let item2_example = t("shell.welcome2.quick_start.item2_example");
    out.push_str(&format!(
        " \x1b[1;36m•\x1b[0m {} \x1b[1;36m{}\x1b[0m\n",
        item2_prefix, item2_example
    ));

    let item3_prefix = t("shell.welcome2.quick_start.item3_prefix");
    let item3_suffix_1 = t("shell.welcome2.quick_start.item3_suffix_1");
    let item3_keyword = t("shell.welcome2.quick_start.item3_keyword");
    let item3_suffix_2 = t("shell.welcome2.quick_start.item3_suffix_2");
    out.push_str(&format!(
        " \x1b[1;36m•\x1b[0m {} \x1b[1;36m{} {}\x1b[0m {}\n",
        item3_prefix, item3_suffix_1, item3_keyword, item3_suffix_2
    ));

    out.push('\n');

    // Risk warning
    let risk = t("shell.welcome2.risk");
    out.push_str(&format!("\x1b[2m{}\x1b[0m\n", risk));

    out
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
    fn test_strip_ansi_len_plain() {
        assert_eq!(strip_ansi_len("hello"), 5);
        assert_eq!(strip_ansi_len(""), 0);
    }

    #[test]
    fn test_strip_ansi_len_with_escape() {
        assert_eq!(strip_ansi_len("\x1b[32mhello\x1b[0m"), 5);
        assert_eq!(strip_ansi_len("\x1b[1;36m•\x1b[0m"), 1); // • is 1 display column
    }

    #[test]
    fn test_strip_ansi_len_complex() {
        let line = format!("  \x1b[1m{}:\x1b[0m {}", "model", "gpt-4");
        // "  model: gpt-4" visible = 14
        assert_eq!(strip_ansi_len(&line), 14);
    }

    #[test]
    fn test_render_prompt_with_home_substitution() {
        let home = dirs::home_dir().expect("home dir should exist");
        let cwd = home.join("projects").to_string_lossy().to_string();
        let result = render_prompt(&cwd, "test-model", 0, "aish");
        assert!(
            result.contains("~"),
            "should substitute home with ~: {}",
            result
        );
        assert!(result.contains("➜"), "should contain prompt symbol");
        assert!(result.contains("<aish>"), "should contain mode badge");
    }

    #[test]
    fn test_render_prompt_without_git() {
        // /tmp is very unlikely to be inside a git repo
        let result = render_prompt("/tmp", "test-model", 0, "aish");
        // Should NOT contain git branch separator '|'
        assert!(
            !result.contains("|"),
            "should not contain git branch separator when no .git: {}",
            result
        );
    }

    #[test]
    fn test_render_prompt_success_symbol() {
        let result = render_prompt("/tmp", "test-model", 0, "aish");
        assert!(
            result.contains("\x1b[32m➜\x1b[0m"),
            "should have green single arrow on success"
        );
        assert!(
            !result.contains("➜➜"),
            "should not have double arrow on success"
        );
    }

    #[test]
    fn test_render_prompt_error_symbol() {
        let result = render_prompt("/tmp", "test-model", 1, "aish");
        assert!(
            result.contains("\x1b[31m➜➜\x1b[0m"),
            "should have red double arrow on error"
        );
    }

    #[test]
    fn test_render_prompt_aish_mode_badge() {
        let result = render_prompt("/tmp", "test-model", 0, "aish");
        assert!(result.contains("<aish>"), "should show aish mode badge");
        assert!(
            result.contains("\x1b[35m<aish>"),
            "aish badge should be magenta"
        );
    }

    #[test]
    fn test_render_prompt_plan_mode_badge() {
        let result = render_prompt("/tmp", "test-model", 0, "plan");
        assert!(result.contains("<plan>"), "should show plan mode badge");
        assert!(
            result.contains("\x1b[33m<plan>"),
            "plan badge should be yellow"
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
    fn test_render_welcome_contains_logo() {
        let result = render_welcome("0.1.0", "gpt-4", 3);
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
        assert!(result.contains("#3"), "should contain skill count");
    }

    #[test]
    fn test_render_welcome_contains_quick_start() {
        let result = render_welcome("0.1.0", "gpt-4", 0);
        assert!(result.contains("•"), "should contain bullet points");
        assert!(
            result.contains("ls, top, vim, ssh"),
            "should contain example commands"
        );
    }
}
