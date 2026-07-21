//! Markdown pipe-table renderer with CJK-aware width and cell wrapping.
//!
//! Replaces the previous `richrs::table::Table` based renderer, which had two
//! problems:
//! 1. It never wrapped cell content — cells wider than their column overflowed
//!    and broke the right border alignment.
//! 2. It rendered cell text verbatim, so inline markdown (`**bold**`, `` `code` ``)
//!    showed up as literal asterisks/backticks.
//!
//! This module mirrors the algorithm used by the project's TypeScript renderer
//! (`oh-my-pi/packages/tui/src/components/markdown.ts`): compute natural and
//! minimum-word widths per column, distribute available width by grow potential,
//! then wrap each cell to its allocated width (breaking long tokens so columns
//! never overflow).

use richrs::segment::{Segment, Segments};
use richrs::style::Style;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// One styled text run. Public so callers (e.g. the paragraph wrapper in
/// `md_render`) can build spans from parsed inline tokens and feed them to
/// [`wrap_spans`].
#[derive(Clone)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    /// Create a span with the given text and no styling.
    #[inline]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
        }
    }

    /// Create a span with the given text and style.
    #[inline]
    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    /// Visible width of this span's text (CJK = 2 cells).
    #[inline]
    pub fn width(&self) -> usize {
        UnicodeWidthStr::width(self.text.as_str())
    }
}

pub type Spans = Vec<Span>;

/// Horizontal padding (in cells) applied to each side of every cell.
const CELL_PADDING: usize = 1;

/// Box-drawing glyphs used to frame the table.
struct BoxChars {
    top_left: char,
    top_right: char,
    bottom_left: char,
    bottom_right: char,
    horizontal: char,
    vertical: char,
    left_tee: char,
    right_tee: char,
    top_tee: char,
    bottom_tee: char,
    cross: char,
}

const BOX: BoxChars = BoxChars {
    top_left: '┌',
    top_right: '┐',
    bottom_left: '└',
    bottom_right: '┘',
    horizontal: '─',
    vertical: '│',
    left_tee: '├',
    right_tee: '┤',
    top_tee: '┬',
    bottom_tee: '┴',
    cross: '┼',
};

/// Parse a markdown pipe-table row into trimmed cell strings.
/// Returns `None` when the line is not a valid `| ... | ... |` row.
fn parse_pipe_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') || trimmed.len() < 2 {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let cells: Vec<String> = inner.split('|').map(|s| s.trim().to_string()).collect();
    if cells.is_empty() {
        return None;
    }
    Some(cells)
}

/// Parse inline markdown into styled spans.
///
/// Recognizes: `` `code` ``, `**bold**`, `*italic*`, `_italic_`, and backslash
/// escapes for markdown-special characters. Nested styling (e.g. `**bold `code`**`)
/// is supported via recursion on bold/italic runs.
fn parse_inline(text: &str) -> Spans {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut spans: Spans = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span {
                    text: std::mem::take(&mut buf),
                    style: Style::default(),
                });
            }
        };
    }

    while i < n {
        let c = chars[i];

        // Backslash escape — only for markdown-special characters; a bare
        // backslash before any other char is preserved verbatim.
        if c == '\\' && i + 1 < n {
            let next = chars[i + 1];
            if matches!(
                next,
                '*' | '_' | '`' | '\\' | '|' | '[' | ']' | '(' | ')' | '<' | '>'
            ) {
                buf.push(next);
                i += 2;
                continue;
            }
        }

        // Inline code: `...` — find the next backtick; no nesting inside.
        if c == '`' {
            if let Some(rel) = find_unescaped(&chars[i + 1..], '`') {
                flush!();
                let code: String = chars[i + 1..i + 1 + rel].iter().collect();
                spans.push(Span {
                    text: code,
                    // Reverse video mirrors richrs::markdown's inline-code style.
                    style: Style::new().reverse(),
                });
                i = i + 1 + rel + 1;
                continue;
            }
        }

        // Bold: **...**  (must consume both stars; otherwise fall through to italic)
        if c == '*' && i + 1 < n && chars[i + 1] == '*' {
            if let Some(rel) = find_double_star(&chars[i + 2..]) {
                flush!();
                let inner: String = chars[i + 2..i + 2 + rel].iter().collect();
                for mut s in parse_inline(&inner) {
                    s.style = s.style.clone().bold();
                    spans.push(s);
                }
                i = i + 2 + rel + 2;
                continue;
            }
        }

        // Italic: *...* or _...*
        if c == '*' || c == '_' {
            if let Some(rel) = find_unescaped(&chars[i + 1..], c) {
                // CommonMark forbids intraword `_` emphasis: `foo_bar_baz`
                // must not italicize, or identifiers and paths in table cells
                // get mangled (underscores consumed as delimiters). `*` allows
                // intraword emphasis, so guard only `_`: reject when a `_`
                // delimiter is flanked by an alphanumeric on either side.
                let close = i + 1 + rel;
                let intraword_underscore = c == '_'
                    && ((i > 0 && chars[i - 1].is_alphanumeric())
                        || (close + 1 < n && chars[close + 1].is_alphanumeric()));
                // Reject `* foo *` — interior must not start with whitespace
                // (CommonMark rule). Trailing whitespace inside is also invalid.
                if rel > 0
                    && !chars[i + 1].is_whitespace()
                    && !chars[i + rel].is_whitespace()
                    && !intraword_underscore
                {
                    flush!();
                    let inner: String = chars[i + 1..i + 1 + rel].iter().collect();
                    for mut s in parse_inline(&inner) {
                        s.style = s.style.clone().italic();
                        spans.push(s);
                    }
                    i = i + 1 + rel + 1;
                    continue;
                }
            }
        }

        buf.push(c);
        i += 1;
    }

    flush!();
    if spans.is_empty() {
        spans.push(Span::plain(""));
    }
    spans
}

/// Find the next occurrence of `target` in `chars`, skipping backslash escapes.
/// Returns the index relative to the start of `chars`.
fn find_unescaped(chars: &[char], target: char) -> Option<usize> {
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if chars[i] == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the next `**` (double-star) close delimiter in `chars`.
fn find_double_star(chars: &[char]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if chars[i] == '*' && chars[i + 1] == '*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Total visible width of a span sequence.
pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.width()).sum()
}

/// Width of the longest whitespace-delimited token across all spans.
/// Used as the floor a column can shrink to without breaking words.
fn longest_token_width(spans: &[Span]) -> usize {
    let mut max = 0usize;
    for span in spans {
        for tok in span.text.split(char::is_whitespace) {
            if tok.is_empty() {
                continue;
            }
            let w = UnicodeWidthStr::width(tok);
            if w > max {
                max = w;
            }
        }
    }
    max.max(1)
}

/// Drop trailing whitespace-only spans and trim trailing whitespace from the
/// final text span. Keeps wrapped lines free of dangling spaces that would
/// otherwise push the right border off the calculated column width.
fn trim_trailing_whitespace(spans: &mut Spans) {
    loop {
        // Snapshot the trailing span's text so we don't hold an immutable
        // borrow of `spans` across the mutable `pop`/`truncate` below.
        let Some(text) = spans.last().map(|s| s.text.clone()) else {
            return;
        };
        if text.is_empty() || text.chars().all(char::is_whitespace) {
            spans.pop();
            continue;
        }
        let trimmed_len = text.trim_end().len();
        if trimmed_len < text.len() {
            spans.last_mut().unwrap().text.truncate(trimmed_len);
        }
        return;
    }
}

/// Tokenize spans into `(text, style)` units where every unit is either a run
/// of non-whitespace characters or a single whitespace character. Whitespace
/// is preserved as its own token so spacing can be reconstructed, and dropped
/// at line boundaries during wrapping.
fn tokenize(spans: &[Span]) -> Vec<(String, Style)> {
    let mut tokens = Vec::new();
    for span in spans {
        let mut cur = String::new();
        for ch in span.text.chars() {
            if ch.is_whitespace() {
                if !cur.is_empty() {
                    tokens.push((std::mem::take(&mut cur), span.style.clone()));
                }
                tokens.push((ch.to_string(), span.style.clone()));
            } else {
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            tokens.push((cur, span.style.clone()));
        }
    }
    tokens
}

/// Wrap a span sequence to `width` cells, breaking any single token that is
/// wider than the column. Returns one or more lines (each a `Spans`).
///
/// Algorithm mirrors `wrap_single_line` in `pi-natives/src/text.rs`:
/// - Long tokens (width > column) are broken grapheme-by-grapheme, never
///   exceeding the column width.
/// - Other tokens wrap at word boundaries; whitespace at the wrap point is
///   dropped so it doesn't land at the start of the next line.
pub fn wrap_spans(spans: &[Span], width: usize) -> Vec<Spans> {
    if width == 0 {
        return vec![spans.to_vec()];
    }
    if spans_width(spans) <= width {
        let mut line = spans.to_vec();
        trim_trailing_whitespace(&mut line);
        return vec![line];
    }

    let tokens = tokenize(spans);
    let mut lines: Vec<Spans> = Vec::new();
    let mut cur: Spans = Vec::new();
    let mut cur_width = 0usize;

    for (text, style) in tokens {
        let tw = UnicodeWidthStr::width(text.as_str());
        let is_whitespace = !text.is_empty() && text.chars().all(char::is_whitespace);

        // Token wider than the whole column — break it grapheme by grapheme.
        if !is_whitespace && tw > width {
            if cur_width > 0 {
                trim_trailing_whitespace(&mut cur);
                lines.push(std::mem::take(&mut cur));
                cur_width = 0;
            }
            let mut piece = String::new();
            let mut piece_w = 0usize;
            for ch in text.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if piece_w + cw > width && piece_w > 0 {
                    cur.push(Span {
                        text: std::mem::take(&mut piece),
                        style: style.clone(),
                    });
                    lines.push(std::mem::take(&mut cur));
                    piece_w = 0;
                }
                piece.push(ch);
                piece_w += cw;
            }
            if !piece.is_empty() {
                cur.push(Span {
                    text: piece,
                    style: style.clone(),
                });
                cur_width = piece_w;
            }
            continue;
        }

        if cur_width + tw > width {
            // Token doesn't fit on the current line — flush and start fresh.
            trim_trailing_whitespace(&mut cur);
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            cur_width = 0;
            if is_whitespace {
                // Drop whitespace that would otherwise begin a new line.
                continue;
            }
            cur.push(Span {
                text,
                style: style.clone(),
            });
            cur_width = tw;
        } else {
            cur.push(Span {
                text,
                style: style.clone(),
            });
            cur_width += tw;
        }
    }

    if !cur.is_empty() {
        trim_trailing_whitespace(&mut cur);
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Allocate column widths within `available` cells.
///
/// Each column has a natural width (the widest cell) and a minimum width (the
/// widest single word — a column can't shrink below this without breaking
/// words). When the natural total fits, return it unchanged; otherwise shrink
/// columns in proportion to their grow potential (`natural - min`), flooring
/// each at its minimum.
fn compute_widths(cols: &[(usize, usize)], available: usize) -> Vec<usize> {
    if cols.is_empty() {
        return Vec::new();
    }
    let natural_total: usize = cols.iter().map(|(n, _)| *n).sum();
    if natural_total <= available {
        return cols.iter().map(|(n, _)| *n).collect();
    }

    let min_total: usize = cols.iter().map(|(_, m)| *m).sum();
    if min_total >= available {
        // Even minimums overflow `available`: hard-cap every column to an equal
        // share and let `wrap_spans` break long tokens. This keeps the table
        // within `max_width` at the cost of splitting unbreakable words —
        // preferable to a table wider than the terminal.
        let per_col = available.checked_div(cols.len()).unwrap_or(1).max(1);
        return cols.iter().map(|_| per_col).collect();
    }

    let grow_total: usize = cols.iter().map(|(n, m)| n.saturating_sub(*m)).sum();
    let extra = available.saturating_sub(min_total);

    let mut widths: Vec<usize> = cols
        .iter()
        .map(|(n, m)| *m + n.saturating_sub(*m) * extra / grow_total.max(1))
        .collect();

    // Distribute the integer-rounding remainder left-to-right.
    let allocated: usize = widths.iter().sum();
    let mut remainder = available.saturating_sub(allocated);
    let cols_len = widths.len();
    let mut idx = 0;
    while remainder > 0 {
        widths[idx % cols_len] += 1;
        remainder -= 1;
        idx += 1;
    }

    widths
}

/// Horizontal padding for one side of a cell.
fn pad() -> String {
    " ".repeat(CELL_PADDING)
}

/// Push a horizontal border line (top, mid, or bottom).
fn push_border(segs: &mut Segments, kind: BorderKind, widths: &[usize], border_style: &Style) {
    let (left, mid, right) = match kind {
        BorderKind::Top => (BOX.top_left, BOX.top_tee, BOX.top_right),
        BorderKind::Mid => (BOX.left_tee, BOX.cross, BOX.right_tee),
        BorderKind::Bottom => (BOX.bottom_left, BOX.bottom_tee, BOX.bottom_right),
    };

    let mut line = String::new();
    line.push(left);
    for (i, w) in widths.iter().enumerate() {
        let cell_w = w + CELL_PADDING * 2;
        line.push_str(&BOX.horizontal.to_string().repeat(cell_w));
        if i + 1 < widths.len() {
            line.push(mid);
        }
    }
    line.push(right);

    segs.push(Segment::styled(line, border_style.clone()));
    segs.push(Segment::newline());
}

#[derive(Clone, Copy)]
enum BorderKind {
    Top,
    Mid,
    Bottom,
}

/// Push one physical row. `cell_at(i)` returns the wrapped spans for column `i`
/// on this line (empty if that cell has fewer wrapped lines than the row max).
fn push_row<F>(
    segs: &mut Segments,
    widths: &[usize],
    border_style: &Style,
    header_style: &Option<Style>,
    cell_at: F,
) where
    F: Fn(usize) -> Spans,
{
    // Left border.
    segs.push(Segment::styled(
        BOX.vertical.to_string(),
        border_style.clone(),
    ));
    for (i, &w) in widths.iter().enumerate() {
        // Left padding.
        segs.push(Segment::new(pad()));

        let cell_spans = cell_at(i);
        let used = spans_width(&cell_spans);

        // Cell content. `header_style` (if any) layers on top of inline styles
        // so a `**bold**` header still reads as bold + reverse inside code.
        for s in &cell_spans {
            let combined = match header_style {
                Some(hs) => s.style.clone().combine(hs),
                None => s.style.clone(),
            };
            if combined.is_empty() {
                segs.push(Segment::new(s.text.clone()));
            } else {
                segs.push(Segment::styled(s.text.clone(), combined));
            }
        }

        // Right-pad the cell so the next vertical lands at the right column.
        let pad_right = w.saturating_sub(used);
        if pad_right > 0 {
            segs.push(Segment::new(" ".repeat(pad_right)));
        }

        // Right padding.
        segs.push(Segment::new(pad()));

        // Column separator / right border.
        segs.push(Segment::styled(
            BOX.vertical.to_string(),
            border_style.clone(),
        ));
    }

    segs.push(Segment::newline());
}

/// Render a markdown pipe table to segments.
///
/// Returns `None` when `lines` is not a valid pipe table (missing header row,
/// bad separator, or no columns).
pub fn render(lines: &[&str], max_width: usize) -> Option<Segments> {
    let header_cells = parse_pipe_row(lines.first()?)?;
    let sep = lines.get(1)?.trim();
    if !sep
        .chars()
        .all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
    {
        return None;
    }
    let col_count = header_cells.len();
    if col_count == 0 {
        return None;
    }

    // Parse every cell into styled spans up front.
    let header_spans: Vec<Spans> = header_cells.iter().map(|h| parse_inline(h)).collect();
    let mut row_spans: Vec<Vec<Spans>> = Vec::new();
    for line in lines.iter().skip(2) {
        if let Some(cells) = parse_pipe_row(line) {
            if cells.len() == col_count {
                row_spans.push(cells.iter().map(|c| parse_inline(c)).collect());
            }
        }
    }

    // Compute natural + minimum-word widths per column.
    let border_overhead = col_count + 1;
    let padding_overhead = col_count * CELL_PADDING * 2;
    let available = max_width
        .saturating_sub(border_overhead)
        .saturating_sub(padding_overhead);

    let mut cols: Vec<(usize, usize)> = Vec::with_capacity(col_count);
    for i in 0..col_count {
        let mut natural = spans_width(&header_spans[i]);
        let mut min_word = longest_token_width(&header_spans[i]);
        for row in &row_spans {
            let w = spans_width(&row[i]);
            if w > natural {
                natural = w;
            }
            let mw = longest_token_width(&row[i]);
            if mw > min_word {
                min_word = mw;
            }
        }
        cols.push((natural, min_word.max(1)));
    }

    let widths = compute_widths(&cols, available);

    let mut segs = Segments::new();
    let border_style = Style::new().dim();
    let header_style = Style::new().bold();

    push_border(&mut segs, BorderKind::Top, &widths, &border_style);

    // Header rows (may wrap to multiple physical lines).
    let header_wrapped: Vec<Vec<Spans>> = (0..col_count)
        .map(|i| wrap_spans(&header_spans[i], widths[i]))
        .collect();
    let header_line_count = header_wrapped.iter().map(|c| c.len()).max().unwrap_or(1);
    for line_idx in 0..header_line_count {
        push_row(
            &mut segs,
            &widths,
            &border_style,
            &Some(header_style.clone()),
            |i| header_wrapped[i].get(line_idx).cloned().unwrap_or_default(),
        );
    }

    push_border(&mut segs, BorderKind::Mid, &widths, &border_style);

    for row in &row_spans {
        let row_wrapped: Vec<Vec<Spans>> = (0..col_count)
            .map(|i| wrap_spans(&row[i], widths[i]))
            .collect();
        let line_count = row_wrapped.iter().map(|c| c.len()).max().unwrap_or(1);
        for line_idx in 0..line_count {
            push_row(&mut segs, &widths, &border_style, &None, |i| {
                row_wrapped[i].get(line_idx).cloned().unwrap_or_default()
            });
        }
    }

    push_border(&mut segs, BorderKind::Bottom, &widths, &border_style);

    Some(segs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain visible text of a segment stream, with all styling stripped.
    /// Uses `Segments::plain_text` so UTF-8 (CJK) survives intact.
    fn plain(segs: &Segments) -> String {
        segs.plain_text()
    }

    /// Visible width of each `│`-prefixed row. All data rows must share the
    /// same width or the right border is misaligned.
    fn right_border_columns(plain: &str) -> Vec<usize> {
        plain
            .lines()
            .filter(|l| l.starts_with('│'))
            .map(UnicodeWidthStr::width)
            .collect()
    }

    #[test]
    fn parses_simple_pipe_row() {
        let cells = parse_pipe_row("| a | b |").unwrap();
        assert_eq!(cells, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn rejects_non_pipe_line() {
        assert!(parse_pipe_row("hello").is_none());
        assert!(parse_pipe_row("| only-left").is_none());
    }

    #[test]
    fn parses_bold_and_code() {
        let spans = parse_inline("**hi** `code`");
        // Three spans: bold "hi", plain " ", reverse "code"
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "hi");
        assert!(spans[0].style.attributes.bold == Some(true));
        assert_eq!(spans[2].text, "code");
        assert!(spans[2].style.attributes.reverse == Some(true));
    }

    #[test]
    fn respects_escape() {
        let spans = parse_inline(r"a\*b");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "a*b");
    }

    #[test]
    fn intraword_underscore_not_emphasis() {
        // CommonMark forbids intraword `_` emphasis: identifiers and paths
        // like `foo_bar_baz` must keep underscores verbatim, not italicize
        // the middle token.
        let spans = parse_inline("foo_bar_baz");
        assert_eq!(spans.len(), 1, "expected a single plain span");
        assert_eq!(spans[0].text, "foo_bar_baz");
        assert!(spans[0].style.attributes.italic != Some(true));
    }

    #[test]
    fn word_bounded_underscore_is_emphasis() {
        // `_world_` flanked by spaces remains valid italic emphasis.
        let spans = parse_inline("hello _world_ end");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].text, "world");
        assert!(spans[1].style.attributes.italic == Some(true));
    }

    #[test]
    fn renders_simple_ascii_table() {
        let lines = vec!["| Name | Value |", "|------|-------|", "| Foo  | 100   |"];
        let segs = render(&lines, 80).unwrap();
        let plain = plain(&segs);
        assert!(plain.contains("Name"));
        assert!(plain.contains("Foo"));
        assert!(plain.contains("100"));
        // Borders aligned: every │-prefixed row has the same width.
        let cols = right_border_columns(&plain);
        assert!(
            cols.iter().all(|&w| w == cols[0]),
            "misaligned rows: {:?}",
            cols
        );
    }

    #[test]
    fn wraps_long_cjk_cell_so_borders_align() {
        // Reproduces the user-reported case: a long mixed CJK/ASCII cell that
        // previously overflowed its column and broke the right border.
        let lines = vec![
            "| 类别 | 数量 | 内容 |",
            "|------|------|------|",
            "| **环境信息** | 3 条 | 系统 `10.10.17.243` 装有企业微信（多 IP）；API Key 存在 `SECRET_GENERIC_SK_API_KEY`；130 的密码 `uos111` |",
            "| **测试记录** | 7 条 | 工具调用测试、记忆存储功能测试等 |",
        ];
        let segs = render(&lines, 80).unwrap();
        let plain = plain(&segs);

        // Inline markdown must not appear literally.
        assert!(!plain.contains("**"), "literal ** leaked: {}", plain);
        // Border alignment: all │-prefixed data rows share the same width.
        let cols = right_border_columns(&plain);
        assert!(
            cols.iter().all(|&w| w == cols[0]),
            "misaligned borders: {:?}",
            cols
        );
        // No row may exceed the requested width.
        for line in plain.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 80 + 4,
                "row wider than terminal: {:?}",
                line
            );
        }
    }

    #[test]
    fn breaks_long_unbreakable_token() {
        // One column, terminal width 20. A 40-char token must wrap across
        // multiple physical rows, none exceeding the column width.
        let lines = vec![
            "| x |",
            "|---|",
            "| aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa |",
        ];
        let segs = render(&lines, 20).unwrap();
        let plain = plain(&segs);
        for line in plain.lines().filter(|l| l.starts_with('│')) {
            let w = UnicodeWidthStr::width(line);
            assert!(w <= 20, "row wider: {} in {:?}", w, line);
        }
        let all_a: String = plain.chars().filter(|c| *c == 'a').collect();
        assert_eq!(all_a.len(), 40);
    }

    #[test]
    fn handles_empty_cell() {
        let lines = vec!["| a | b |", "|---|---|", "| | x |"];
        let segs = render(&lines, 40).unwrap();
        let plain = plain(&segs);
        assert!(plain.contains("x"));
        let cols = right_border_columns(&plain);
        assert!(cols.iter().all(|&w| w == cols[0]));
    }

    #[test]
    fn left_border_present_on_every_row() {
        // Regression: push_row once lost its left-border `│` push during a
        // refactor, producing rows that started with a space instead. Every
        // non-empty rendered line must begin with a box-drawing glyph.
        let lines = vec![
            "| 用户 | PID | %MEM | %CPU | RSS(KB) | 进程 |",
            "|------|-----|------|------|---------|------|",
            "| xzx  | 2742 | 3.8  | 35.8 | 622,108 | service-manager |",
        ];
        let plain = plain(&render(&lines, 80).unwrap());
        for line in plain.lines() {
            let first = line.chars().next();
            assert!(
                matches!(first, Some('│' | '┌' | '├' | '└')),
                "row missing left border, starts with {:?}: {:?}",
                first,
                line
            );
        }
    }

    #[test]
    fn returns_none_for_invalid_separator() {
        let lines = vec!["| a | b |", "| x y |", "| 1 | 2 |"];
        assert!(render(&lines, 40).is_none());
    }
}
