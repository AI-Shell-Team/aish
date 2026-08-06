//! Centralized color palette and symbol system for aish terminal display.
//!
//! Inspired by oh-my-pi's theme system: truecolor hex colors with graceful
//! fallback to 256-color ANSI when the terminal doesn't support 24-bit color.
//! All display code should use these tokens instead of hardcoded ANSI codes.

use std::sync::LazyLock;
use unicode_width::UnicodeWidthChar;

// ───────────────────────────────────────────────────────────────────────────
// Terminal capability detection
// ───────────────────────────────────────────────────────────────────────────

/// Whether the current terminal supports 24-bit truecolor.
/// Detected once from `COLORTERM` and cached for the process lifetime.
/// Honors the `NO_COLOR` convention: any value disables all color.
static TRUECOLOR: LazyLock<bool> = LazyLock::new(|| {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false)
});

/// Whether all color output is disabled via the `NO_COLOR` environment
/// variable (convention: presence of any value, including empty, disables).
static NO_COLOR: LazyLock<bool> = LazyLock::new(|| std::env::var_os("NO_COLOR").is_some());

fn supports_truecolor() -> bool {
    *TRUECOLOR
}

// ───────────────────────────────────────────────────────────────────────────
// Color helpers
// ───────────────────────────────────────────────────────────────────────────

/// Apply foreground truecolor (or 256-color fallback) to `text`.
/// Returns plain text when `NO_COLOR` is set.
fn fg_rgb(r: u8, g: u8, b: u8, fallback256: u8, text: &str) -> String {
    if *NO_COLOR {
        return text.to_string();
    }
    if supports_truecolor() {
        format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
    } else {
        format!("\x1b[38;5;{fallback256}m{text}\x1b[0m")
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Color tokens (r, g, b, 256-color fallback)
// Palette inspired by oh-my-pi "titanium" theme.
// ───────────────────────────────────────────────────────────────────────────

/// Accent color RGB channels (electric blue), shared with markdown renderer.
pub const ACCENT_RGB: (u8, u8, u8) = (0, 180, 255);

/// Electric blue — emphasis, headings, spinner, mode badge.
pub fn accent(text: &str) -> String {
    fg_rgb(ACCENT_RGB.0, ACCENT_RGB.1, ACCENT_RGB.2, 39, text)
}

/// Readout green — success, completion, git clean.
pub fn success(text: &str) -> String {
    fg_rgb(0, 255, 136, 48, text)
}

/// Alert red — errors, git dirty, failure indicators.
pub fn error(text: &str) -> String {
    fg_rgb(255, 71, 87, 203, text)
}

/// Warning amber — warnings, dirty indicator, pending states.
pub fn warning(text: &str) -> String {
    fg_rgb(255, 179, 71, 215, text)
}

/// Dim aluminum — secondary text, tool output, timestamps.
pub fn muted(text: &str) -> String {
    fg_rgb(156, 163, 176, 247, text)
}

/// Gray — tertiary text, hints, descriptions.
pub fn dim(text: &str) -> String {
    fg_rgb(107, 114, 128, 240, text)
}

/// Titanium gold — highlights, cost, special labels.
pub fn gold(text: &str) -> String {
    fg_rgb(212, 192, 144, 222, text)
}

// ───────────────────────────────────────────────────────────────────────────
// Text style helpers
// ───────────────────────────────────────────────────────────────────────────

/// Bold text.
pub fn bold(text: &str) -> String {
    format!("\x1b[1m{text}\x1b[0m")
}

/// Dim text (ANSI dim attribute, independent of color).
pub fn faint(text: &str) -> String {
    format!("\x1b[2m{text}\x1b[0m")
}

/// Bold + dim text (combined SGR attributes).
pub fn bold_faint(text: &str) -> String {
    format!("\x1b[1;2m{text}\x1b[0m")
}

// ───────────────────────────────────────────────────────────────────────────
// Symbol constants
// ───────────────────────────────────────────────────────────────────────────

// Status icons
pub const ICON_SUCCESS: &str = "✔";
pub const ICON_ERROR: &str = "✘";
pub const ICON_WARNING: &str = "⚠";
pub const ICON_RUNNING: &str = "⟳";
pub const ICON_DONE: &str = "•";
pub const ICON_PENDING: &str = "⏳";

// Git / branch
pub const ICON_GIT: &str = "⎇";
pub const ICON_BRANCH: &str = "⑂";

// Prompt symbols
pub const PROMPT_OK: &str = "➜";
pub const PROMPT_ERR: &str = "➜➜";

// Tree connectors
pub const TREE_BRANCH: &str = "├─";
pub const TREE_LAST: &str = "└─";
pub const TREE_VERTICAL: &str = "│";
pub const TREE_CORNER: &str = "┌─";

// Markdown / formatting
pub const QUOTE_RAIL: &str = "▏";
pub const BULLET: &str = "•";
pub const HR_CHAR: &str = "─";

// Tool execution
pub const TOOL_PREFIX: &str = "❯";

/// Tool execution box drawing (layered visual grouping).
pub const TOOL_BOX_TOP: &str = "╭─";
pub const TOOL_BOX_MID: &str = "│";
pub const TOOL_BOX_BOT: &str = "╰─";

/// Mode badge icon prefix (replaces angle brackets `<aish>`).
pub const MODE_ICON: &str = "◆";

// ───────────────────────────────────────────────────────────────────────────
// Spinner frames
// ───────────────────────────────────────────────────────────────────────────

/// Status spinner: wide Braille frames for loaders and tool indicators.
/// 8 frames, designed for ~80ms interval (12.5fps).
/// Visually wider and more premium than the narrow dots pattern.
pub const SPINNER_STATUS: &[&str] = &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// Activity spinner: standard Braille dots for high-frequency UI updates.
/// 10 frames, designed for ~33ms interval (30fps).
pub const SPINNER_ACTIVITY: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Get a spinner frame by index (wraps around).
pub fn spinner_frame<'a>(frames: &'a [&'a str], index: usize) -> &'a str {
    frames[index % frames.len()]
}

// ───────────────────────────────────────────────────────────────────────────
// Shimmer animation — rainbow gradient sweep
// ───────────────────────────────────────────────────────────────────────────

/// Shimmer tunables — a cosine-bump band sweeps across text at fixed velocity.
const SHIMMER_SPEED: f64 = 30.0; // cells per second
const SHIMMER_PADDING: f64 = 10.0; // virtual padding so band enters/exits smoothly
const SHIMMER_BAND_HALF: f64 = 6.0; // half-width of the cosine bump
const SHIMMER_TIER_HIGH: f64 = 0.65; // for 256-color fallback
const SHIMMER_TIER_MID: f64 = 0.22;

/// HSL → RGB conversion.  Returns 8-bit channels.
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = (h.rem_euclid(360.0)) / 360.0;
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |t: f64| -> f64 {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (
        (f(h + 1.0 / 3.0) * 255.0).round() as u8,
        (f(h) * 255.0).round() as u8,
        (f(h - 1.0 / 3.0) * 255.0).round() as u8,
    )
}

/// Pre-resolved ANSI codes for the 256-color shimmer fallback.
struct ShimmerAnsi {
    high: &'static str,
    mid: &'static str,
    low: &'static str,
    reset: &'static str,
}

static SHIMMER_ANSI: LazyLock<ShimmerAnsi> = LazyLock::new(|| ShimmerAnsi {
    high: "\x1b[38;5;39m",
    mid: "\x1b[38;5;247m",
    low: "\x1b[38;5;240m",
    reset: "\x1b[0m",
});

/// Apply a rainbow cosine-bump shimmer sweep across `text`.
///
/// A bright band travels left → right at fixed velocity (30 cells/s). Each
/// character's hue rotates with its position, creating a flowing rainbow.
/// Brightness is modulated by the cosine bump: dim at rest, vivid at the crest.
/// On 256-color terminals, falls back to a three-tier monochrome palette.
pub fn shimmer_text(text: &str, time_ms: u64) -> String {
    // NO_COLOR convention: return plain text without ANSI.
    if *NO_COLOR {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len == 0 {
        return String::new();
    }

    // Compute display-width position for each character so the shimmer band
    // sweeps correctly across CJK / fullwidth glyphs (which occupy 2 columns).
    let mut total_width = 0.0_f64;
    let char_pos: Vec<f64> = chars
        .iter()
        .map(|&ch| {
            let start = total_width;
            total_width += UnicodeWidthChar::width(ch).unwrap_or(0) as f64;
            start
        })
        .collect();
    let period = total_width + SHIMMER_PADDING * 2.0;
    let pos = ((time_ms as f64 / 1000.0) * SHIMMER_SPEED) % period;

    if supports_truecolor() {
        // Rainbow mode: per-character HSL with position-based hue drift.
        let mut result = String::with_capacity(text.len() * 6);
        for (i, &ch) in chars.iter().enumerate() {
            let idx = char_pos[i];
            let dist = (idx + SHIMMER_PADDING - pos).abs();
            let intensity = if dist >= SHIMMER_BAND_HALF {
                0.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * dist / SHIMMER_BAND_HALF).cos())
            };
            // Hue rotates per character and drifts over time for flow effect.
            let hue = (i as f64) * 28.0 + time_ms as f64 * 0.05;
            let sat = 0.45 + intensity * 0.45;
            let light = 0.30 + intensity * 0.42;
            let (r, g, b) = hsl_to_rgb(hue, sat, light);
            result.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}"));
        }
        result.push_str("\x1b[0m");
        result
    } else {
        // 256-color fallback: three-tier monochrome.
        let ansi = &*SHIMMER_ANSI;
        let mut result = String::with_capacity(text.len() * 4);
        let mut prev_tier: i8 = -1;
        let mut run_start: usize = 0;
        for (i, _ch) in chars.iter().enumerate() {
            let idx = char_pos[i];
            let dist = (idx + SHIMMER_PADDING - pos).abs();
            let intensity = if dist >= SHIMMER_BAND_HALF {
                0.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * dist / SHIMMER_BAND_HALF).cos())
            };
            let tier = if intensity >= SHIMMER_TIER_HIGH {
                2
            } else if intensity >= SHIMMER_TIER_MID {
                1
            } else {
                0
            };
            if tier != prev_tier {
                if prev_tier >= 0 && i > run_start {
                    let run: String = chars[run_start..i].iter().collect();
                    let open = match prev_tier {
                        2 => ansi.high,
                        1 => ansi.mid,
                        _ => ansi.low,
                    };
                    result.push_str(open);
                    result.push_str(&run);
                    result.push_str(ansi.reset);
                }
                prev_tier = tier;
                run_start = i;
            }
        }
        if prev_tier >= 0 {
            let run: String = chars[run_start..].iter().collect();
            let open = match prev_tier {
                2 => ansi.high,
                1 => ansi.mid,
                _ => ansi.low,
            };
            result.push_str(open);
            result.push_str(&run);
            result.push_str(ansi.reset);
        }
        result
    }
}

/// Short descriptive label for a tool, shown in the shimmer animation.
/// Falls back to the raw tool name for unknown tools.
pub fn tool_status_label(name: &str) -> String {
    match name {
        // Shell / execution
        "bash" => "Executing command",
        "python_exec" => "Executing Python",
        "Agent" => "Spawning agent",
        // File operations
        "read_file" => "Reading file",
        "write_file" => "Writing file",
        "edit_file" => "Editing file",
        // Search
        "glob" => "Finding files",
        "grep" => "Searching content",
        // Web
        "WebFetch" => "Fetching web content",
        "web_search" => "Searching web",
        // Memory / context
        "memory" => "Accessing memory",
        "skill" => "Using skill",
        "host_note" => "Managing host note",
        // Plan mode
        "enter_plan_mode" => "Entering plan mode",
        "exit_plan_mode" => "Exiting plan mode",
        "list_plan_templates" => "Listing templates",
        // Interactive
        "ask_user" => "Awaiting input",
        "resolve" => "Awaiting approval",
        // External / MCP tools (may not always be present)
        "debug" => "Debugging",
        "lsp" => "Code intelligence",
        "generate_image" => "Generating image",
        _ => return name.to_string(),
    }
    .to_string()
}

// ───────────────────────────────────────────────────────────────────────────
// Response metadata footer helpers
// ───────────────────────────────────────────────────────────────────────────

/// Format a token count compactly (e.g., 3200 → "3.2k", 1.2M → "1.2M").
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Render a context usage progress bar (10 cells).
/// Color changes with usage: green < 50%, amber 50-70%, red >= 70%.
pub fn context_bar(percent: u8) -> String {
    let filled = ((percent as f64 * 10.0 / 100.0).round() as usize)
        .max(if percent > 0 { 1 } else { 0 })
        .min(10);
    let empty = 10 - filled;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    if percent < 50 {
        format!("{} {}%", success(&bar), percent)
    } else if percent < 70 {
        format!("{} {}%", warning(&bar), percent)
    } else {
        format!("{} {}%", error(&bar), percent)
    }
}

/// Render a one-line response metadata footer:
/// `◉ model │ 3.2k in 1.1k out │ 3.2s │ req [████████░░] 23%`
///
/// `req` is the current request's share of the context window — NOT the
/// cumulative conversation size. Tool calls from prior turns are not
/// persisted into the context manager, so a short follow-up after a
/// tool-heavy turn legitimately shows a much smaller `req`. When
/// `compaction` is `Some`, a trailing `⟳compacted` / `⟳micro` hint marks
/// that the bar shrank because history was compacted this turn.
pub fn response_footer(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    ctx_percent: u8,
    compaction: Option<&str>,
    elapsed_secs: Option<f64>,
) -> String {
    let sep = dim("│");
    let time_part = match elapsed_secs {
        Some(secs) => format!(" {} {} ", dim("·"), muted(&format!("{:.1}s", secs))),
        None => " ".to_string(),
    };
    let compaction_part = match compaction {
        Some("full_compact") => format!(" {}", dim("⟳compacted")),
        Some("microcompact") => format!(" {}", dim("⟳micro")),
        _ => String::new(),
    };
    format!(
        "{} {} {} {} {}{}{} req {}{}",
        gold("◉"),
        gold(model),
        sep,
        success(&format!("{} in", format_tokens(input_tokens))),
        warning(&format!("{} out", format_tokens(output_tokens))),
        time_part,
        sep,
        context_bar(ctx_percent),
        compaction_part,
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Unified diff rendering
// ───────────────────────────────────────────────────────────────────────────

/// A single line in a line-level diff.
enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// Compute a line-level diff between `old_lines` and `new_lines` using the
/// classic LCS dynamic-programming table. Returns ordered diff lines.
/// Runs in O(n*m) time and memory — callers must bound the input size.
fn line_diff(old_lines: &[&str], new_lines: &[&str]) -> Vec<DiffLine> {
    let n = old_lines.len();
    let m = new_lines.len();
    // dp[i][j] = length of LCS of old_lines[i..] and new_lines[j..]
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if old_lines[i] == new_lines[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            out.push(DiffLine::Context(old_lines[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(DiffLine::Removed(old_lines[i].to_string()));
            i += 1;
        } else {
            out.push(DiffLine::Added(new_lines[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        out.push(DiffLine::Removed(old_lines[i].to_string()));
        i += 1;
    }
    while j < m {
        out.push(DiffLine::Added(new_lines[j].to_string()));
        j += 1;
    }
    out
}

/// Default unchanged-context lines shown around each changed region.
const DIFF_CONTEXT: usize = 3;

/// Render a colored unified diff between `old` and `new` text.
///
/// Output is split into hunks centered on changed lines (added/removed), each
/// surrounded by up to [`DIFF_CONTEXT`] unchanged context lines. Edits at the
/// end of a large file stay fully visible — leading context no longer crowds
/// them out. `max_lines` caps total output; context is shed first, and only
/// when the bare changed lines themselves overflow the cap are lines dropped
/// (with a trailing `... N more lines` hint). Returns empty when identical.
pub fn render_diff(old: &str, new: &str, max_lines: usize) -> String {
    // Skip expensive LCS when inputs are byte-identical.
    if old == new {
        return String::new();
    }
    // Use split('\n') (not lines()) so trailing-newline differences are visible.
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    // Bound input to avoid O(n*m) LCS blowup on very large diffs.
    const MAX_LCS_LINES: usize = 1000;
    let diff = line_diff(
        &old_lines[..old_lines.len().min(MAX_LCS_LINES)],
        &new_lines[..new_lines.len().min(MAX_LCS_LINES)],
    );
    if max_lines == 0 {
        return String::new();
    }
    let has_change = diff.iter().any(|l| !matches!(l, DiffLine::Context(_)));
    if !has_change {
        // Bounded region shows no change but inputs differ: the change lies
        // beyond the LCS bound. Fall back to diffing the tails so a
        // large-file tail edit is rendered instead of an empty diff.
        if old_lines.len() > MAX_LCS_LINES || new_lines.len() > MAX_LCS_LINES {
            let start_o = old_lines.len().saturating_sub(MAX_LCS_LINES);
            let start_n = new_lines.len().saturating_sub(MAX_LCS_LINES);
            let tail_diff = line_diff(&old_lines[start_o..], &new_lines[start_n..]);
            if tail_diff.iter().any(|l| !matches!(l, DiffLine::Context(_))) {
                return render_diff_hunks(&tail_diff, max_lines);
            }
        }
        return String::new();
    }
    render_diff_hunks(&diff, max_lines)
}

/// Merge changed-line indices into contiguous `[start, end)` regions, each
/// padded by `ctx` unchanged lines on both sides and clamped to `0..n`.
/// Adjacent or overlapping regions collapse into one.
fn merged_regions(changes: &[usize], ctx: usize, n: usize) -> Vec<(usize, usize)> {
    let pad = ctx as isize;
    let clamp = |v: isize| v.clamp(0, n as isize) as usize;
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for &c in changes {
        let s = clamp(c as isize - pad);
        let e = clamp((c + 1) as isize + pad);
        if let Some(last) = regions.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        regions.push((s, e));
    }
    regions
}

/// Total rendered line count for a set of regions: content lines plus one
/// separator line per gap (before the first region when it does not start at
/// the file head, between regions, and after the last when it does not reach
/// the tail).
fn rendered_size(regions: &[(usize, usize)], n: usize) -> usize {
    let content: usize = regions.iter().map(|(s, e)| e - s).sum();
    let lead = usize::from(regions.first().is_some_and(|(s, _)| *s > 0));
    let between = regions.len().saturating_sub(1);
    let tail = usize::from(regions.last().is_some_and(|(_, e)| *e < n));
    content + lead + between + tail
}

/// Choose the largest context whose rendered output fits `max_lines`; shed
/// context before dropping any changed line. Falls back to zero context when
/// even the bare changes overflow the cap.
fn choose_regions(changes: &[usize], n: usize, max_lines: usize) -> Vec<(usize, usize)> {
    let mut best = merged_regions(changes, 0, n);
    for try_ctx in (1..=DIFF_CONTEXT).rev() {
        let r = merged_regions(changes, try_ctx, n);
        if rendered_size(&r, n) <= max_lines {
            best = r;
            break;
        }
    }
    best
}

/// Render the diff as hunks: changed regions padded with context, separated
/// by `... N lines` hints for the elided stretches.
fn render_diff_hunks(diff: &[DiffLine], max_lines: usize) -> String {
    let n = diff.len();
    let changes: Vec<usize> = diff
        .iter()
        .enumerate()
        .filter_map(|(i, l)| match l {
            DiffLine::Context(_) => None,
            _ => Some(i),
        })
        .collect();
    if changes.is_empty() || max_lines == 0 {
        return String::new();
    }
    let regions = choose_regions(&changes, n, max_lines);

    let mut lines: Vec<String> = Vec::with_capacity(max_lines);
    let mut prev_end = 0usize;
    // Reserve one line for a trailing elision marker when content is
    // dropped, so output never exceeds max_lines. Emit whole regions and
    // stop at a region boundary on overflow — earlier changed lines stay
    // visible instead of truncating mid-region and hiding a later hunk.
    let soft_cap = max_lines.saturating_sub(1);
    let mut dropped = false;

    for &(s, e) in &regions {
        let gap_marker = match (prev_end, s) {
            (0, sp) if sp > 0 => Some(format!("  ... {} lines above", sp)),
            (pe, sp) if pe > 0 && sp > pe => Some(format!("  ... {} lines hidden", sp - pe)),
            _ => None,
        };
        let need = gap_marker.as_ref().map(|_| 1).unwrap_or(0) + (e - s);
        if !lines.is_empty() && lines.len() + need > soft_cap {
            dropped = true;
            break;
        }
        if let Some(m) = gap_marker {
            lines.push(dim(&m));
        }
        // A single oversized region (nothing emitted yet) still shows its
        // leading lines up to the soft cap so changed lines surface.
        let room = soft_cap.saturating_sub(lines.len());
        for line in diff[s..e].iter().take(room) {
            lines.push(render_diff_line(line));
        }
        if e - s > room {
            dropped = true;
            prev_end = s + room;
            break;
        }
        prev_end = e;
    }

    if (dropped || prev_end < n) && lines.len() < max_lines {
        lines.push(dim(&format!("  ... {} more lines", n - prev_end)));
    }

    let mut out = String::new();
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn render_diff_line(line: &DiffLine) -> String {
    match line {
        DiffLine::Context(s) => muted(&format!("  {s}")),
        DiffLine::Added(s) => success(&format!("+ {s}")),
        DiffLine::Removed(s) => error(&format!("- {s}")),
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                // consume CSI sequence: ESC [ ... letter
                chars.next();
                for sc in chars.by_ref() {
                    if sc.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn identical_inputs_produce_empty_diff() {
        assert_eq!(render_diff("a\nb\n", "a\nb\n", 20), "");
    }

    #[test]
    fn new_file_shows_all_additions() {
        let d = strip_ansi(&render_diff("", "hello\nworld\n", 20));
        assert!(d.contains("+ hello"));
        assert!(d.contains("+ world"));
        assert!(!d.contains("- "));
    }

    #[test]
    fn edit_shows_addition_and_removal() {
        let d = strip_ansi(&render_diff("old line\n", "new line\n", 20));
        assert!(d.contains("- old line"));
        assert!(d.contains("+ new line"));
    }

    #[test]
    fn unchanged_lines_are_context() {
        let d = strip_ansi(&render_diff("keep\nold\n", "keep\nnew\n", 20));
        assert!(d.contains("  keep"));
        assert!(d.contains("- old"));
        assert!(d.contains("+ new"));
    }

    #[test]
    fn truncation_hint_when_exceeding_max() {
        let old = String::new();
        let new: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let d = strip_ansi(&render_diff(&old, &new, 5));
        assert!(d.contains("... 47 more lines"));
    }

    #[test]
    fn trailing_newline_difference_is_visible() {
        // With split('\n'), "a\n" → ["a", ""] vs "a" → ["a"].
        // The removed empty line proves trailing-newline changes are surfaced.
        let d = strip_ansi(&render_diff("a\n", "a", 20));
        assert!(!d.is_empty(), "trailing newline diff must not be empty");
    }

    #[test]
    fn edit_at_tail_is_not_truncated() {
        // Regression: an edit appending lines at the end of a long file must
        // show the change in full. Leading context is collapsed into a hint,
        // not printed verbatim, so it cannot crowd the edit out of the window.
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut new = old.clone();
        new.push_str("added1\n");
        new.push_str("added2\n");
        let d = strip_ansi(&render_diff(&old, &new, 20));
        assert!(d.contains("+ added1"));
        assert!(d.contains("+ added2"));
        // Lines far from the edit are elided, not printed as context.
        assert!(!d.contains("line 0"));
        assert!(d.contains("lines above"));
    }

    #[test]
    fn edit_at_head_is_not_truncated() {
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut new = old.clone();
        // Prepend two changed lines at the very top.
        new.insert_str(0, "added1\nadded2\n");
        let d = strip_ansi(&render_diff(&old, &new, 20));
        assert!(d.contains("+ added1"));
        assert!(d.contains("+ added2"));
        assert!(!d.contains("line 39"));
    }

    #[test]
    fn separate_edits_produce_multiple_hunks() {
        // Two edits far apart yield two hunks separated by a "... lines hidden"
        // marker; both edits remain visible within the cap.
        let mut old: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let mut new = old.clone();
        // Edit near the top and near the bottom.
        old = old.replace("line 2\n", "old top\n");
        old = old.replace("line 27\n", "old bottom\n");
        new = new.replace("line 2\n", "new top\n");
        new = new.replace("line 27\n", "new bottom\n");
        let d = strip_ansi(&render_diff(&old, &new, 20));
        assert!(d.contains("- old top"));
        assert!(d.contains("+ new top"));
        assert!(d.contains("- old bottom"));
        assert!(d.contains("+ new bottom"));
        assert!(d.contains("lines hidden"));
    }

    #[test]
    fn tail_edit_beyond_lcs_bound_is_shown() {
        // Regression (CodeRabbit B): an edit beyond the 1000-line LCS bound
        // (leading prefix unchanged) must still render, via the tail fallback.
        let prefix: String = (0..1005).map(|i| format!("line {i}\n")).collect();
        let old = format!("{prefix}tail old\n");
        let new = format!("{prefix}tail new\n");
        let d = strip_ansi(&render_diff(&old, &new, 20));
        assert!(
            !d.is_empty(),
            "tail edit beyond LCS bound must not be empty"
        );
        assert!(d.contains("- tail old"));
        assert!(d.contains("+ tail new"));
    }

    #[test]
    fn output_never_exceeds_max_lines() {
        // Regression (CodeRabbit C): output must never exceed max_lines, even
        // with many scattered changes. The trailing elision marker is budgeted.
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..60 {
            old.push_str(&format!("old {i}\n"));
            new.push_str(&format!("new {i}\n"));
        }
        let d = strip_ansi(&render_diff(&old, &new, 10));
        let line_count = d.lines().count();
        assert!(
            line_count <= 10,
            "output must not exceed max_lines: got {line_count}"
        );
        assert!(!d.is_empty());
    }

    #[test]
    fn distant_hunks_kept_when_changed_fit() {
        // Regression (CodeRabbit C): when changed lines fit within max_lines,
        // distant hunks stay visible (not truncated away) and output <= cap.
        let mut old: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut new = old.clone();
        old = old.replace("line 5\n", "old a\n");
        old = old.replace("line 45\n", "old b\n");
        new = new.replace("line 5\n", "new a\n");
        new = new.replace("line 45\n", "new b\n");
        let d = strip_ansi(&render_diff(&old, &new, 12));
        assert!(d.contains("- old a"));
        assert!(d.contains("+ new a"));
        assert!(d.contains("- old b"));
        assert!(d.contains("+ new b"));
        assert!(d.lines().count() <= 12, "output must not exceed max_lines");
    }

    #[test]
    fn format_tokens_boundaries() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(1500), "1.5k");
        assert_eq!(format_tokens(999999), "1000.0k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn context_bar_produces_output() {
        // Verify context_bar produces a non-empty colored string for each tier.
        assert!(!context_bar(10).is_empty()); // green tier (<50%)
        assert!(!context_bar(55).is_empty()); // amber tier (50-70%)
        assert!(!context_bar(80).is_empty()); // red tier (>=70%)
    }

    #[test]
    fn context_bar_shows_at_least_one_cell_for_nonzero_percent() {
        // Sub-10% percentages must show at least 1 filled cell, not all empty.
        let bar6 = context_bar(6);
        assert!(
            bar6.contains('\u{2588}'),
            "6% should have >=1 filled cell: {}",
            bar6
        );

        let bar1 = context_bar(1);
        assert!(
            bar1.contains('\u{2588}'),
            "1% should have >=1 filled cell: {}",
            bar1
        );

        let bar0 = context_bar(0);
        assert!(
            !bar0.contains('\u{2588}'),
            "0% should have 0 filled cells: {}",
            bar0
        );
    }

    #[test]
    fn response_footer_without_elapsed_has_space_before_sep() {
        let footer = strip_ansi(&response_footer("gpt-4", 1000, 500, 23, None, None));
        assert!(
            footer.contains("out │"),
            "separator must have space before it when elapsed is None: {footer:?}"
        );
    }

    #[test]
    fn response_footer_with_elapsed_has_time_segment() {
        let footer = strip_ansi(&response_footer("gpt-4", 1000, 500, 23, None, Some(1.5)));
        assert!(
            footer.contains("1.5s"),
            "elapsed time must appear in footer: {footer:?}"
        );
    }

    #[test]
    fn shimmer_text_non_empty_for_ascii() {
        let result = shimmer_text("hello world", 0);
        assert!(!result.is_empty());
        assert!(result.contains('h'));
    }

    #[test]
    fn shimmer_text_non_empty_for_cjk() {
        let result = shimmer_text("你好世界", 500);
        assert!(!result.is_empty());
        // Each CJK char should appear in the output.
        assert!(result.contains('你'));
    }
}
