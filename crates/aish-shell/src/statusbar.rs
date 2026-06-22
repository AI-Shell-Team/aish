//! Bottom status bar for token usage, context budget, and cost display.
//!
//! Uses ANSI scroll region (DECSTBM) to pin the bar to the terminal bottom.
//! Command output and AI responses scroll above the bar without disturbing it.
//!
//! Two styles (both single-line content):
//! - `"single"` (default): detailed — model, progress bar, %, usage, cost, state
//! - `"minimal"`: compact — short model, %, usage, cost, state icon
//!
//! Visual layout (always 2 terminal rows):
//! Row H-1: dim separator line (full width)
//! Row H  : dark background + content text

use std::collections::HashMap;
use std::io::Write;

use aish_config::{ConfigModel, PricingEntry};
use aish_llm::TokenStats;

// ---------------------------------------------------------------------------
// ANSI color helpers
// ---------------------------------------------------------------------------

/// ANSI 256 color codes used by the status bar.
mod ansi {
    pub const BG_DARK: &str = "\x1b[48;5;236m"; // status bar background
    pub const FG_WHITE: &str = "\x1b[38;5;255m";
    pub const FG_GRAY: &str = "\x1b[38;5;245m";
    pub const FG_DIM: &str = "\x1b[38;5;240m";
    pub const ACCENT: &str = "\x1b[38;5;39m"; // cyan-blue
    pub const GREEN: &str = "\x1b[38;5;42m";
    pub const YELLOW: &str = "\x1b[38;5;214m";
    pub const RED: &str = "\x1b[38;5;203m";
    pub const PURPLE: &str = "\x1b[38;5;141m";
    pub const ORANGE: &str = "\x1b[38;5;208m";
    pub const RESET: &str = "\x1b[0m";
    /// Reset foreground only (preserves background).
    pub const RESET_FG: &str = "\x1b[39m";
}

// ---------------------------------------------------------------------------
// Status bar state
// ---------------------------------------------------------------------------

/// Runtime state of the status bar — what to display.
#[derive(Debug, Clone)]
pub struct StatusBarState {
    /// Model identifier (e.g. "deepseek/deepseek-chat").
    pub model: String,
    /// Token usage for the last 7 days (from TokenUsageStore).
    pub token_stats: TokenStats,
    /// Current context window estimated token count.
    pub context_tokens: usize,
    /// Context window size (max tokens).
    pub context_window: usize,
    /// Budget policy name (e.g. "sliding-window").
    pub budget_policy: String,
    /// Whether an AI operation is currently in progress.
    pub ai_active: bool,
    /// Current tool call description, if any (e.g. "bash: ls -la").
    pub tool_call: Option<String>,
    /// Whether context compaction is in progress.
    pub compacting: bool,
    /// Current working directory (shortened, e.g. "~/aish").
    pub cwd: String,
    /// Last AI API response latency in milliseconds.
    pub last_api_latency_ms: Option<u64>,
}

impl Default for StatusBarState {
    fn default() -> Self {
        Self {
            model: String::new(),
            token_stats: TokenStats::default(),
            context_tokens: 0,
            context_window: 128_000,
            budget_policy: "sliding".to_string(),
            ai_active: false,
            tool_call: None,
            compacting: false,
            cwd: String::new(),
            last_api_latency_ms: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Visibility control
// ---------------------------------------------------------------------------

/// Tracks whether the status bar is currently visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusBarVisible(pub bool);

impl StatusBarVisible {
    pub fn toggle(&mut self) {
        self.0 = !self.0;
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Format a token count with K/M suffixes.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a cost in USD.
fn format_cost(cost: f64) -> String {
    if cost < 0.01 {
        format!("${:.4}", cost)
    } else {
        format!("${:.2}", cost)
    }
}

/// Compute cost using either config overrides or the built-in pricing table.
fn compute_cost(config: &ConfigModel, model: &str, stats: &TokenStats) -> Option<f64> {
    // Check config overrides first
    let lower = model.to_lowercase();
    let name_after_slash = model.rsplit('/').next().unwrap_or(model);
    let name_lower = name_after_slash.to_lowercase();

    for (key, entry) in &config.statusbar.pricing {
        let key_lower = key.to_lowercase();
        if lower == key_lower || name_lower == key_lower {
            let p = PricingEntry {
                input_per_1k: entry.input_per_1k,
                output_per_1k: entry.output_per_1k,
            };
            return Some(
                (stats.total_input as f64 / 1000.0) * p.input_per_1k
                    + (stats.total_output as f64 / 1000.0) * p.output_per_1k,
            );
        }
    }

    // Fall back to built-in pricing table
    aish_llm::estimate_cost(model, stats.total_input, stats.total_output)
}

/// Build the progress bar string with color coding.
///
/// Returns (filled_string, percentage, color_code).
fn progress_bar(used: usize, total: usize, width: usize) -> (String, usize, &'static str) {
    let pct = if total == 0 {
        0
    } else {
        ((used as f64 / total as f64) * 100.0) as usize
    };

    let filled = if total == 0 {
        0
    } else {
        ((used as f64 / total as f64) * width as f64).round() as usize
    };
    let filled = filled.min(width);

    let color = match pct {
        0..=49 => ansi::GREEN,
        50..=79 => ansi::ACCENT,
        80..=94 => ansi::YELLOW,
        _ => ansi::RED,
    };

    let bar = format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled));
    (bar, pct, color)
}

/// Shorten a model name for display (strip provider prefix if > 20 chars).
fn short_model(model: &str) -> String {
    if model.len() > 20 {
        model.rsplit('/').next().unwrap_or(model).to_string()
    } else {
        model.to_string()
    }
}

/// Shorten current working directory with ~ for home, truncate long paths.
pub fn short_cwd() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let display = if let Some(home) = dirs::home_dir() {
        if cwd.starts_with(&home) {
            let rest = cwd.strip_prefix(&home).unwrap();
            if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~{}", rest.display())
            }
        } else {
            cwd.display().to_string()
        }
    } else {
        cwd.display().to_string()
    };
    // Truncate if too long: keep last 2 path components
    if display.len() > 20 {
        let parts: Vec<&str> = display.split('/').collect();
        if parts.len() > 3 {
            return format!("\u{2026}/{}", parts[parts.len() - 2..].join("/"));
        }
    }
    display
}

/// Format API latency for display.
fn format_latency(ms: Option<u64>) -> String {
    match ms {
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) => format!("{:.1}s", ms as f64 / 1000.0),
        None => String::new(),
    }
}

/// Truncate a string to max_chars (on char boundary) with ellipsis.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}

// ---------------------------------------------------------------------------
// Content rendering (single line)
// ---------------------------------------------------------------------------

/// Build the state indicator segment (idle / generating / tool / compact).
fn render_state_seg(state: &StatusBarState) -> String {
    if let Some(ref tool) = state.tool_call {
        let t = truncate_str(tool, 25);
        format!("{}⚡ ▸ {}{}", ansi::YELLOW, t, ansi::RESET)
    } else if state.compacting {
        format!("{}⚠ compacting{}", ansi::YELLOW, ansi::RESET)
    } else if state.ai_active {
        format!("{}✦ generating{}", ansi::PURPLE, ansi::RESET)
    } else {
        format!("{}· idle{}", ansi::FG_GRAY, ansi::RESET)
    }
}

/// Render the status bar content as a single ANSI-colored line (no newline).
///
/// The separator line and background fill are handled by `render_fixed`.
pub fn render_content(state: &StatusBarState, config: &ConfigModel) -> String {
    let (width, _height) = crossterm::terminal::size().unwrap_or((80, 24));
    let width = width as usize;

    let model = short_model(&state.model);
    let bar_width = std::cmp::min(10, width / 8).max(5);
    let (bar, pct, bar_color) = progress_bar(state.context_tokens, state.context_window, bar_width);
    let total = state.token_stats.total_input + state.token_stats.total_output;
    let cost = compute_cost(config, &state.model, &state.token_stats);
    let state_seg = render_state_seg(state);

    let cost_str = cost
        .map(|c| format!("{}{}{}", ansi::ORANGE, format_cost(c), ansi::RESET))
        .unwrap_or_default();

    let sep = format!(" {}│{} ", ansi::FG_DIM, ansi::RESET);

    // Token usage split: ↑input ↓output
    let inout_seg = format!(
        "{}↑{}{} {}↓{}{}",
        ansi::GREEN,
        format_tokens(state.token_stats.total_input),
        ansi::RESET,
        ansi::ACCENT,
        format_tokens(state.token_stats.total_output),
        ansi::RESET,
    );

    // Context: bar pct used/total
    let ctx_used = format_tokens(state.context_tokens as u64);
    let ctx_total = format_tokens(state.context_window as u64);
    let ctx_seg = format!(
        "{}{}{} {}{}%{} {}{}/{}{}",
        bar_color,
        bar,
        ansi::RESET,
        ansi::FG_GRAY,
        pct,
        ansi::RESET,
        ansi::FG_WHITE,
        ctx_used,
        ctx_total,
        ansi::RESET,
    );

    // Request count
    let req_seg = format!(
        "{}{}req{}",
        ansi::FG_WHITE,
        state.token_stats.request_count,
        ansi::RESET,
    );

    // Latency
    let latency_str = format_latency(state.last_api_latency_ms);
    let latency_seg = if latency_str.is_empty() {
        String::new()
    } else {
        format!("{}{}{}", ansi::FG_GRAY, latency_str, ansi::RESET)
    };

    // CWD
    let cwd_seg = format!("{}{}{}", ansi::GREEN, state.cwd, ansi::RESET);

    match config.statusbar.style.as_str() {
        "minimal" => {
            // Compact: model pct total $cost state
            let short = model.rsplit('/').next().unwrap_or(&model);
            format!(
                "{}{}{} {}{}%{} {}{}{} {} {}",
                ansi::ACCENT,
                short.trim(),
                ansi::RESET,
                bar_color,
                pct,
                ansi::RESET,
                ansi::FG_WHITE,
                format_tokens(total),
                ansi::RESET,
                cost_str,
                state_seg,
            )
        }
        _ => {
            // "single" — detailed Plan C, adaptive width
            if width >= 100 {
                // model │ ~/path │ ▓▓░ pct used/total │ ↑in ↓out $cost │ Nreq latency · state
                let req_lat = if latency_seg.is_empty() {
                    req_seg.clone()
                } else {
                    format!("{} {}", req_seg, latency_seg)
                };
                format!(
                    "{}{}{}{}{}{}{}{}{}{} {} {}",
                    ansi::ACCENT,
                    model.trim(),
                    ansi::RESET,
                    sep,
                    cwd_seg,
                    sep,
                    ctx_seg,
                    sep,
                    inout_seg,
                    cost_str,
                    format!("{}{}{}", sep, req_lat, sep),
                    state_seg,
                )
            } else if width >= 70 {
                // Medium: drop latency + clock
                // model │ ~/path │ ▓▓░ pct used/total │ ↑in ↓out $cost │ · state
                format!(
                    "{}{}{}{}{}{}{}{}{}{} {} {}",
                    ansi::ACCENT,
                    model.trim(),
                    ansi::RESET,
                    sep,
                    cwd_seg,
                    sep,
                    ctx_seg,
                    sep,
                    inout_seg,
                    cost_str,
                    sep,
                    state_seg,
                )
            } else {
                // Narrow: same as minimal
                let short = model.rsplit('/').next().unwrap_or(&model);
                format!(
                    "{}{}{} {}{}%{} {}{}{} {} {}",
                    ansi::ACCENT,
                    short.trim(),
                    ansi::RESET,
                    bar_color,
                    pct,
                    ansi::RESET,
                    ansi::FG_WHITE,
                    format_tokens(total),
                    ansi::RESET,
                    cost_str,
                    state_seg,
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fixed-bottom mode (ANSI scroll region)
// ---------------------------------------------------------------------------

/// Number of terminal rows the status bar reserves (separator + content).
///
/// Always 2 regardless of style — both "single" and "minimal" use
/// 1 row for the thin separator line and 1 row for the content.
pub fn statusbar_height(_config: &ConfigModel) -> usize {
    2
}

/// Set the terminal scroll region to exclude the bottom N rows,
/// leaving them reserved for the status bar.
///
/// After calling this, all normal terminal output (prompts, command
/// results, AI responses) stays within the scroll region, while the
/// bottom N rows are free for `render_fixed`.
pub fn enter_fixed_mode(lines: usize) {
    let (_w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let bottom = h.saturating_sub(lines as u16);
    if bottom == 0 {
        return;
    }

    // DECSTBM sets scroll region [1, bottom] and homes cursor to (1,1).
    // Move cursor to the LAST row of the scroll region so the prompt
    // sits right above the status bar separator, not at the top of screen
    // and not inside the status bar area.
    let esc = format!("\x1b[1;{}r\x1b[{};1H", bottom, bottom);
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(esc.as_bytes());
    let _ = stdout.flush();
}

/// Re-assert the scroll region after a resize, clearing stale status bar
/// content left at the old position.
///
/// Saves the cursor (DECSC), clears old status bar rows if the terminal
/// grew, sets the new scroll region (DECSTBM — homes cursor to 1,1), then
/// restores the cursor (DECRC) so the prompt position is preserved.
///
/// Safe to call from the resize-watcher thread during `read_line` — uses
/// raw escape sequences only, no terminal state beyond DECSC/DECRC.
pub fn reassert_after_resize(old_h: u16, lines: usize) {
    let (_w, new_h) = crossterm::terminal::size().unwrap_or((80, 24));
    let new_bottom = new_h.saturating_sub(lines as u16);
    if new_bottom == 0 {
        return;
    }

    let mut esc = String::from("\x1b7"); // Save cursor (DECSC)

    // If the terminal grew taller, old status bar rows are now visible
    // in the middle of the screen. Clear them so no stale content lingers.
    if new_h > old_h && old_h >= lines as u16 {
        let old_sep = old_h.saturating_sub(lines as u16);
        if old_sep > 0 {
            for i in 0..lines as u16 {
                let row = old_sep + i;
                if row > 0 && row <= old_h {
                    esc.push_str(&format!("\x1b[{};1H\x1b[2K", row));
                }
            }
        }
    }

    // Set new scroll region (DECSTBM homes cursor to 1,1)
    esc.push_str(&format!("\x1b[1;{}r", new_bottom));
    // Restore cursor (DECRC) — puts cursor back at the prompt position
    esc.push('\x1b'); // DECRC = ESC 8
    esc.push('8');

    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(esc.as_bytes());
    let _ = stdout.flush();
}

/// Reset the scroll region to the full terminal and clear the bottom rows.
pub fn exit_fixed_mode() {
    let (_w, h) = crossterm::terminal::size().unwrap_or((80, 24));

    // Reset scroll region to full screen
    let mut esc = String::from("\x1b[r");

    // Clear the bottom 2 rows (separator + content)
    if h >= 2 {
        esc.push_str(&format!("\x1b[{};1H\x1b[2K", h - 1));
        esc.push_str(&format!("\x1b[{};1H\x1b[2K", h));
        esc.push_str(&format!("\x1b[{};1H", h.saturating_sub(2).max(1)));
    }

    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(esc.as_bytes());
    let _ = stdout.flush();
}

/// Render the status bar at the bottom of the terminal (fixed position).
///
/// Draws:
/// 1. A thin dim separator line at row H-1 (full terminal width)
/// 2. The status content at row H with dark background fill
///
/// Cursor is saved before and restored after, so the scrollable
/// area is not disturbed.
pub fn render_fixed(state: &StatusBarState, config: &ConfigModel) {
    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let width = w as usize;

    let content = render_content(state, config);

    let mut esc = String::new();

    // Save cursor position (DECSC)
    esc.push_str("\x1b7");

    // Row H-1: thin separator line (full width, dim)
    if h >= 2 {
        let sep_row = h - 1;
        esc.push_str(&format!("\x1b[{};1H\x1b[2K", sep_row));
        esc.push_str(&format!(
            "{}{}{}",
            ansi::FG_DIM,
            "─".repeat(width),
            ansi::RESET
        ));
    }

    // Row H: content with dark background
    if h >= 1 {
        // Fill the entire row with background first
        esc.push_str(&format!("\x1b[{};1H", h));
        esc.push_str(&format!(
            "{}{}{}",
            ansi::BG_DARK,
            " ".repeat(width),
            ansi::RESET
        ));

        // Overwrite the beginning with actual content.
        // Replace full resets with fg-only resets so the background
        // from the fill is preserved under the text.
        esc.push_str(&format!("\x1b[{};1H", h));
        let content_fg_only = content.replace(ansi::RESET, ansi::RESET_FG);
        esc.push_str(&format!(
            "{} {} {}",
            ansi::BG_DARK,
            content_fg_only,
            ansi::RESET
        ));
    }

    // Restore cursor position (DECRC)
    esc.push_str("\x1b8");

    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(esc.as_bytes());
    let _ = stdout.flush();
}

/// Clear the bottom N rows (used when hiding the bar).
pub fn clear_fixed(lines: usize) {
    let (_w, h) = crossterm::terminal::size().unwrap_or((80, 24));

    let mut esc = String::new();
    for i in 0..lines {
        let row = h.saturating_sub(i as u16);
        if row >= 1 {
            esc.push_str(&format!("\x1b[{};1H\x1b[2K", row));
        }
    }

    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(esc.as_bytes());
    let _ = stdout.flush();
}

/// Convenience: render the status bar content as a scrollable string.
/// Kept for backward compatibility with `/token` and tests.
pub fn render(state: &StatusBarState, config: &ConfigModel) -> String {
    format!("{}\n", render_content(state, config))
}

/// Render a brief toggle notification (shown after hiding).
pub fn render_hidden_notice() -> String {
    format!(
        "{}{} statusbar hidden · Ctrl+T show{}\n",
        ansi::FG_DIM,
        "┈",
        ansi::RESET
    )
}

/// Render the detailed token panel for `/token` command (enhanced version).
///
/// Shows the existing table plus context budget and cost estimation.
pub fn render_token_panel(state: &StatusBarState, config: &ConfigModel) -> String {
    let stats = &state.token_stats;
    let total = stats.total_input + stats.total_output;

    let bar_width = 40;
    let (bar, pct, bar_color) = progress_bar(state.context_tokens, state.context_window, bar_width);

    let cost_7d = compute_cost(config, &state.model, stats);
    let today_in = format_tokens(stats.total_input);
    let today_out = format_tokens(stats.total_output);
    let total_fmt = format_tokens(total);

    let mut output = String::new();

    // Title
    output.push_str(&format!(
        "\n\x1b[1;36m{}\x1b[0m\n",
        aish_i18n::t("shell.statusbar.usage_title")
    ));

    // Token stats table
    output.push_str(&format!(
        "  {} {:>12}\n",
        aish_i18n::t("shell.token.input_tokens"),
        today_in
    ));
    output.push_str(&format!(
        "  {} {:>11}\n",
        aish_i18n::t("shell.token.output_tokens"),
        today_out
    ));
    output.push_str(&format!(
        "  {} {:>16}\n",
        aish_i18n::t("shell.token.total"),
        total_fmt
    ));
    output.push_str(&format!(
        "  {} {:>8}\n",
        aish_i18n::t("shell.token.api_calls"),
        stats.request_count
    ));

    // Cost estimation
    if let Some(cost) = cost_7d {
        output.push_str(&format!(
            "\n  \x1b[38;5;208m{} ${:.4}\x1b[0m\n",
            aish_i18n::t("shell.statusbar.estimated_cost"),
            cost
        ));
    }

    // Context budget
    output.push_str(&format!(
        "\n\x1b[1;36m{}\x1b[0m\n",
        aish_i18n::t("shell.statusbar.context_title")
    ));
    output.push_str(&format!(
        "  {}{}{} {}/{} {}%{}\n",
        bar_color,
        bar,
        ansi::RESET,
        format_tokens(state.context_tokens as u64),
        format_tokens(state.context_window as u64),
        pct,
        ansi::RESET
    ));
    output.push_str(&format!(
        "  {}{}{}\n",
        ansi::FG_GRAY,
        aish_i18n::t_with_args("shell.statusbar.policy", &{
            let mut m = HashMap::new();
            m.insert("policy".to_string(), state.budget_policy.clone());
            m
        }),
        ansi::RESET
    ));

    // Visibility hint
    output.push_str(&format!(
        "\n  {}\n",
        aish_i18n::t("shell.statusbar.toggle_hint")
    ));

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(42_000), "42.0K");
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }

    #[test]
    fn test_progress_bar_colors() {
        let (_, pct, color) = progress_bar(10, 100, 10);
        assert_eq!(pct, 10);
        assert_eq!(color, ansi::GREEN);

        let (_, pct, color) = progress_bar(60, 100, 10);
        assert_eq!(pct, 60);
        assert_eq!(color, ansi::ACCENT);

        let (_, pct, color) = progress_bar(85, 100, 10);
        assert_eq!(pct, 85);
        assert_eq!(color, ansi::YELLOW);

        let (_, pct, color) = progress_bar(96, 100, 10);
        assert_eq!(pct, 96);
        assert_eq!(color, ansi::RED);
    }

    #[test]
    fn test_short_model() {
        assert_eq!(short_model("gpt-4o"), "gpt-4o");
        assert_eq!(short_model("deepseek/deepseek-chat"), "deepseek-chat"); // len > 20, strip prefix
        assert_eq!(
            short_model("some-very-long-provider/model-name-xyz"),
            "model-name-xyz"
        );
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello…");
        // Unicode safe
        assert_eq!(truncate_str("你好世界test", 4), "你好世界…");
    }

    #[test]
    fn test_render_single_style() {
        let state = StatusBarState {
            model: "deepseek/deepseek-chat".to_string(),
            token_stats: TokenStats {
                total_input: 152_000,
                total_output: 48_000,
                request_count: 32,
            },
            context_tokens: 8_400,
            context_window: 128_000,
            ..StatusBarState::default()
        };

        let config = ConfigModel::default();
        let output = render_content(&state, &config);
        assert!(!output.is_empty());
        assert!(output.contains("deepseek"));
        // Single-line: no newline in content
        assert!(!output.contains('\n'));
    }

    #[test]
    fn test_render_minimal_style() {
        let state = StatusBarState {
            model: "gpt-4o".to_string(),
            token_stats: TokenStats {
                total_input: 1000,
                total_output: 500,
                request_count: 2,
            },
            context_tokens: 1_000,
            context_window: 128_000,
            ..StatusBarState::default()
        };

        let mut config = ConfigModel::default();
        config.statusbar.style = "minimal".to_string();
        let output = render_content(&state, &config);
        assert!(!output.is_empty());
        assert!(output.contains("gpt-4o"));
        assert!(!output.contains('\n'));
    }

    #[test]
    fn test_render_state_seg_idle() {
        let state = StatusBarState::default();
        let seg = render_state_seg(&state);
        assert!(seg.contains("·"));
        assert!(seg.contains("idle"));
    }

    #[test]
    fn test_render_state_seg_generating() {
        let state = StatusBarState {
            ai_active: true,
            ..StatusBarState::default()
        };
        let seg = render_state_seg(&state);
        assert!(seg.contains("✦"));
        assert!(seg.contains("generating"));
    }

    #[test]
    fn test_render_state_seg_tool_call() {
        let state = StatusBarState {
            tool_call: Some("bash: ls -la".to_string()),
            ..StatusBarState::default()
        };
        let seg = render_state_seg(&state);
        assert!(seg.contains("⚡"));
        assert!(seg.contains("bash"));
    }

    #[test]
    fn test_render_state_seg_compacting() {
        let state = StatusBarState {
            compacting: true,
            ..StatusBarState::default()
        };
        let seg = render_state_seg(&state);
        assert!(seg.contains("⚠"));
        assert!(seg.contains("compacting"));
    }

    #[test]
    fn test_statusbar_height_always_2() {
        let mut config = ConfigModel::default();
        assert_eq!(statusbar_height(&config), 2);
        config.statusbar.style = "minimal".to_string();
        assert_eq!(statusbar_height(&config), 2);
        config.statusbar.style = "single".to_string();
        assert_eq!(statusbar_height(&config), 2);
    }

    #[test]
    fn test_token_panel() {
        let state = StatusBarState {
            model: "gpt-4o".to_string(),
            token_stats: TokenStats {
                total_input: 10000,
                total_output: 5000,
                request_count: 5,
            },
            context_tokens: 2_000,
            context_window: 128_000,
            ..StatusBarState::default()
        };

        let config = ConfigModel::default();
        let panel = render_token_panel(&state, &config);
        assert!(!panel.is_empty());
    }

    // --- Plan C enrichment tests ---

    /// Helper: build a state with all Plan C fields populated.
    fn plan_c_state() -> StatusBarState {
        StatusBarState {
            model: "deepseek/deepseek-chat".to_string(),
            token_stats: TokenStats {
                total_input: 20_000,
                total_output: 22_000,
                request_count: 32,
            },
            context_tokens: 8_400,
            context_window: 128_000,
            cwd: "~/aish".to_string(),
            last_api_latency_ms: Some(340),
            ..StatusBarState::default()
        }
    }

    #[test]
    fn test_no_clock_in_output() {
        // After removing HH:MM, no output should contain a time pattern.
        let state = plan_c_state();
        let config = ConfigModel::default();
        let output = render_content(&state, &config);
        let clean = strip_ansi(&output);
        // Should NOT contain HH:MM pattern (e.g. "14:32")
        assert!(
            !matches_hhmm(&clean),
            "Clock pattern found in statusbar output: {}",
            clean
        );
    }

    #[test]
    fn test_latency_formats_milliseconds() {
        // format_latency: <1000ms shows as "Nms"
        assert_eq!(format_latency(Some(340)), "340ms");
        assert_eq!(format_latency(Some(0)), "0ms");
        assert_eq!(format_latency(Some(999)), "999ms");
    }

    #[test]
    fn test_latency_formats_seconds() {
        // format_latency: >=1000ms shows as "N.Ns"
        assert_eq!(format_latency(Some(1000)), "1.0s");
        assert_eq!(format_latency(Some(1500)), "1.5s");
        assert_eq!(format_latency(Some(3400)), "3.4s");
    }

    #[test]
    fn test_latency_none_is_empty() {
        assert_eq!(format_latency(None), "");
    }

    #[test]
    fn test_short_cwd_home() {
        // short_cwd should produce a ~ path (tested in isolation;
        // actual cwd depends on test runner's working directory).
        let cwd = short_cwd();
        // Must be non-empty
        assert!(!cwd.is_empty(), "short_cwd returned empty string");
        // Should contain ~ if under home dir
        if let Some(home) = dirs::home_dir() {
            if std::env::current_dir()
                .map(|d| d.starts_with(&home))
                .unwrap_or(false)
            {
                assert!(cwd.starts_with('~'), "expected ~ in cwd, got: {}", cwd);
            }
        }
    }

    #[test]
    fn test_context_fields_in_state() {
        // Verify StatusBarState carries all Plan C fields correctly
        let state = plan_c_state();
        assert_eq!(state.cwd, "~/aish");
        assert_eq!(state.last_api_latency_ms, Some(340));
        assert_eq!(state.token_stats.request_count, 32);
        assert_eq!(state.context_tokens, 8_400);
        assert_eq!(state.context_window, 128_000);
    }

    #[test]
    fn test_state_idle_in_output() {
        let state = plan_c_state();
        let config = ConfigModel::default();
        let output = render_content(&state, &config);
        let clean = strip_ansi(&output);
        assert!(clean.contains("idle"), "missing idle state: {}", clean);
    }

    #[test]
    fn test_state_generating_in_output() {
        let mut state = plan_c_state();
        state.ai_active = true;
        let config = ConfigModel::default();
        let output = render_content(&state, &config);
        let clean = strip_ansi(&output);
        assert!(
            clean.contains("generating"),
            "missing generating state: {}",
            clean
        );
    }

    #[test]
    fn test_minimal_has_no_cwd() {
        // Minimal style should NOT show cwd regardless of terminal width
        let state = plan_c_state();
        let mut config = ConfigModel::default();
        config.statusbar.style = "minimal".to_string();
        let output = render_content(&state, &config);
        let clean = strip_ansi(&output);
        assert!(
            !clean.contains("~/aish"),
            "minimal should not have cwd: {}",
            clean
        );
    }

    #[test]
    fn test_single_line_output() {
        // All styles produce single-line output (no newlines)
        let state = plan_c_state();
        let config = ConfigModel::default();
        let output = render_content(&state, &config);
        assert!(!output.contains('\n'), "output contains newline");
    }

    #[test]
    fn test_default_has_empty_cwd_and_no_latency() {
        let state = StatusBarState::default();
        assert!(state.cwd.is_empty(), "default cwd should be empty");
        assert!(
            state.last_api_latency_ms.is_none(),
            "default latency should be None"
        );
    }

    // --- Helpers for tests ---

    /// Check if text contains an HH:MM time pattern.
    fn matches_hhmm(text: &str) -> bool {
        let bytes = text.as_bytes();
        if bytes.len() < 5 {
            return false;
        }
        for i in 0..bytes.len().saturating_sub(4) {
            if bytes[i].is_ascii_digit()
                && bytes[i + 1].is_ascii_digit()
                && bytes[i + 2] == b':'
                && bytes[i + 3].is_ascii_digit()
                && bytes[i + 4].is_ascii_digit()
            {
                return true;
            }
        }
        false
    }

    /// Strip ANSI escape codes from a string for readable assertions.
    fn strip_ansi(s: &str) -> String {
        let mut result = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip escape sequence
                if let Some(&next) = chars.peek() {
                    if next == '[' {
                        chars.next(); // consume '['
                                      // Skip until letter (end of CSI sequence)
                        for c2 in chars.by_ref() {
                            if c2.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    } else {
                        // Single-char escape like '7', '8'
                        chars.next();
                    }
                }
            } else {
                result.push(c);
            }
        }
        // Collapse multiple spaces for readability
        while result.contains("  ") {
            result = result.replace("  ", " ");
        }
        result.trim().to_string()
    }
}
