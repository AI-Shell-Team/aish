//! Shared layout / truncation helpers for inline panels.
//!
//! Previously choice / select / settings_ui each redefined these identical
//! helpers. Centralizing them removes the duplication and — importantly —
//! unifies `padded_area` on overflow-safe `saturating_*` arithmetic:
//! settings_ui used plain `+`, which risks a u16 panic on a pathological
//! terminal geometry, while choice / select already used the safe form.
//! skill_ui is new and adopts the shared saturating version directly.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Horizontal inset applied to panel content so it does not hug the edges.
pub const PANEL_PADDING_X: u16 = 2;

/// Inset `area` by `PANEL_PADDING_X` columns on each side (clamped to half the
/// width). Uses saturating arithmetic so a zero/tiny-width area cannot panic.
pub fn padded_area(area: Rect) -> Rect {
    let padding = PANEL_PADDING_X.min(area.width / 2);
    Rect::new(
        area.x.saturating_add(padding),
        area.y,
        area.width.saturating_sub(padding.saturating_mul(2)),
        area.height,
    )
}

/// Truncate `s` to at most `max_width` display columns. When truncation is
/// needed, a single ellipsis (`…`, width 1) is appended **within** the budget
/// — the earlier implementation appended it past the limit, overflowing.
pub fn truncate_str(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    let mut truncated = false;
    for ch in s.chars() {
        let cw = unicode_width_ch(ch);
        if w + cw > max_width {
            truncated = true;
            break;
        }
        w += cw;
        out.push(ch);
    }
    if truncated {
        // The ellipsis (width 1) must stay within `max_width`, so evict the
        // last character(s) until a single column is free.
        while w + 1 > max_width && !out.is_empty() {
            let last = out.pop().unwrap();
            w -= unicode_width_ch(last);
        }
        out.push('…');
    }
    out
}

/// Truncate a styled span line to `max_width` columns, appending an ellipsis
/// inside the last fitting span when the line overflows.
pub fn truncate_line(spans: Vec<Span<'_>>, max_width: usize) -> Line<'_> {
    if max_width == 0 {
        return Line::from(Vec::new());
    }
    let mut out: Vec<Span<'_>> = Vec::with_capacity(spans.len());
    let mut w = 0usize;
    for sp in spans {
        let sw = sp.content.width();
        if w + sw > max_width {
            let remaining = max_width.saturating_sub(w);
            if remaining > 1 {
                let mut piece = String::new();
                let mut pw = 0usize;
                for ch in sp.content.chars() {
                    let cw = unicode_width_ch(ch);
                    if pw + cw + 1 > remaining {
                        break;
                    }
                    pw += cw;
                    piece.push(ch);
                }
                piece.push('…');
                out.push(Span::styled(piece, sp.style));
            } else if remaining == 1 {
                // Exactly one column remains: a lone ellipsis (width 1) fits
                // and still signals the truncation that occurred.
                out.push(Span::styled("…", sp.style));
            }
            break;
        }
        w += sw;
        out.push(sp);
    }
    Line::from(out)
}

/// Display width of a single character.
pub fn unicode_width_ch(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_area_insets_and_clamps() {
        let area = Rect::new(0, 0, 20, 5);
        let inset = padded_area(area);
        assert_eq!(inset.x, 2);
        assert_eq!(inset.width, 16);

        // Tiny width clamps padding to half so x never exceeds width/2.
        let tiny = padded_area(Rect::new(0, 0, 2, 5));
        assert!(tiny.x <= 1);
    }

    #[test]
    fn padded_area_saturating_on_overflow() {
        // A pathological geometry where x + padding would overflow u16. The
        // saturating version must not panic (the legacy `+` arithmetic in
        // settings_ui would have). The exact x is not asserted because
        // ratatui's Rect clamps x+width internally.
        let area = Rect::new(u16::MAX - 1, 0, 10, 5);
        let _ = padded_area(area);
    }

    #[test]
    fn truncate_str_appends_ellipsis() {
        // "hello world" at width 5 -> "hell…" (4 chars + ellipsis = 5), not
        // "hello…" (width 6, the old overflow bug).
        assert_eq!(truncate_str("hello world", 5), "hell…");
        assert_eq!(truncate_str("hi", 5), "hi");
        assert_eq!(truncate_str("abc", 0), "");
    }

    #[test]
    fn truncate_str_never_exceeds_max_width() {
        // Regression for the overflow bug: result width must stay within the
        // budget even when truncation is needed.
        for (input, max) in [("hello world", 5usize), ("abcdef", 3), ("ab", 1)] {
            let out = truncate_str(input, max);
            let width = out.width();
            assert!(
                width <= max,
                "truncate_str({input:?}, {max}) = {out:?} (width {width}) exceeds {max}"
            );
        }
    }

    #[test]
    fn unicode_width_ch_handles_cjk() {
        assert_eq!(unicode_width_ch('a'), 1);
        assert_eq!(unicode_width_ch('中'), 2);
    }

    #[test]
    fn truncate_line_fits_prefix_then_ellipsis() {
        // "hello " (6 cols) fits; the next span overflows with 3 columns left,
        // so a 2-char prefix + ellipsis is packed into the last fitting span.
        let spans = vec![Span::raw("hello "), Span::raw("world")];
        let line = truncate_line(spans, 9);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(rendered, "hello wo…");
    }

    #[test]
    fn truncate_line_lone_ellipsis_when_one_column_remains() {
        // "hello " (6) + max_width 7 leaves exactly 1 column: the fix emits a
        // standalone ellipsis instead of an empty, marker-less truncation.
        let spans = vec![Span::raw("hello "), Span::raw("world")];
        let line = truncate_line(spans, 7);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(rendered, "hello …");
    }
}
