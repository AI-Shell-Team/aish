//! Markdown renderer with width-aware paragraph wrapping and nested lists.
//!
//! Replaces `richrs::markdown::Markdown` for the `ContentSegment::Markdown`
//! branch. Two improvements over richrs:
//!
//! 1. **Paragraph wrapping** — text is wrapped at `width` using
//!    [`aish_md_table::wrap_spans`], so long paragraphs no longer overflow the
//!    terminal. Inline styles (bold/italic/code) are preserved across breaks.
//!
//! 2. **Nested lists** — a `list_stack` tracks depth. Each level indents 2
//!    cells, and continuation lines use a hanging indent aligned with the
//!    bullet text, so wrapped item content stays readable at any depth.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use richrs::segment::{Segment, Segments};
use richrs::style::Style;
use unicode_width::UnicodeWidthStr;

use aish_md_table::{spans_width, wrap_spans, Span};

/// Render markdown `text` to terminal segments, wrapping at `width`.
pub fn render(text: &str, width: usize) -> Segments {
    let mut r = Renderer {
        width,
        segs: Segments::new(),
        list_stack: Vec::new(),
        bold: false,
        italic: false,
        para_buf: Vec::new(),
        heading_level: None,
        heading_buf: Vec::new(),
        pending_bullet: None,
        item_indent: String::new(),
        in_code_block: false,
        quote_depth: 0,
        link_url: None,
        in_link: false,
    };
    for event in Parser::new(text) {
        r.handle(event);
    }
    // Flush any trailing paragraph text (tight list items without End events).
    r.flush_paragraph();
    r.segs
}

struct ListState {
    ordered: bool,
    next_number: u64,
}

struct Renderer {
    width: usize,
    segs: Segments,
    /// One entry per nesting level; depth = len - 1.
    list_stack: Vec<ListState>,
    /// Inline style state accumulated during paragraph/item rendering.
    bold: bool,
    italic: bool,
    /// Buffer for paragraph inline spans; flushed on paragraph/item end.
    para_buf: Vec<Span>,
    /// Active heading level (None outside a heading).
    heading_level: Option<u32>,
    /// Buffer for heading spans.
    heading_buf: Vec<Span>,
    /// Bullet + indent prefix for the current list item's first line.
    /// `Some(prefix)` means the next `flush_paragraph` call should emit the
    /// prefix on its first line (the bullet hasn't been printed yet).
    pending_bullet: Option<String>,
    /// Hanging indent for the current list item, carried across multiple
    /// paragraphs (loose lists). Set on `Start(Item)`, cleared on `End(Item)`.
    /// When `pending_bullet` is `None` but this is non-empty, continuation
    item_indent: String,
    /// Whether we're inside a `CodeBlock` (indented or fenced that slipped
    /// through `split_content`). Text is emitted verbatim with indentation.
    in_code_block: bool,
    /// Blockquote nesting depth. Each level reserves 2 cells (│ + space) and
    /// applies dim+italic to the quoted content.
    quote_depth: usize,
    /// Current link URL (set on `Start(Link)`, consumed on `End(Link)`).
    link_url: Option<String>,
    /// Whether we're inside a `Link` element (text gets underline + URL suffix).
    in_link: bool,
}

impl Renderer {
    fn current_style(&self) -> Style {
        let mut s = Style::default();
        if self.bold {
            s = s.bold();
        }
        if self.italic {
            s = s.italic();
        }
        s
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(t) => self.text(&t),
            Event::Code(code) => self.inline_code(&code),
            // Soft and hard breaks are both treated as inline whitespace —
            // wrapping handles line splitting. Flushing on HardBreak would
            // fragment a single paragraph into two.
            Event::SoftBreak | Event::HardBreak => self.para_buf.push(Span::plain(" ")),
            Event::Rule => {
                self.flush_paragraph();
                let w = self.width.clamp(1, 80);
                self.segs.push(Segment::new("─".repeat(w)));
                self.segs.push(Segment::newline());
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.para_buf.clear();
            }
            Tag::Heading { level, .. } => {
                self.heading_level = Some(level as u32);
                self.heading_buf.clear();
            }
            Tag::Strong => self.bold = true,
            Tag::Emphasis => self.italic = true,
            Tag::CodeBlock(_) => {
                // Fenced code blocks are split off by `split_content` before
                // this renderer runs; indented code blocks (4-space) reach
                // here and are emitted verbatim with indentation.
                self.flush_paragraph();
                self.in_code_block = true;
            }
            Tag::List(start) => {
                let ordered = start.is_some();
                let number = start.unwrap_or(1);
                self.list_stack.push(ListState {
                    ordered,
                    next_number: number,
                });
            }
            Tag::Item => {
                self.flush_paragraph();
                self.start_list_item();
            }
            Tag::BlockQuote => {
                self.flush_paragraph();
                self.quote_depth += 1;
            }
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.into_string());
                self.in_link = true;
            }
            _ => {}
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => {
                self.flush_paragraph();
            }
            TagEnd::Heading(_) => {
                self.flush_heading();
            }
            TagEnd::Strong => self.bold = false,
            TagEnd::Emphasis => self.italic = false,
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.segs.push(Segment::newline());
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    // Blank line after the outermost list.
                    self.segs.push(Segment::newline());
                }
            }
            TagEnd::Item => {
                // Tight-list items have raw Text (no Paragraph); flush whatever
                // accumulated so the item content is printed.
                self.flush_paragraph();
                self.item_indent.clear();
            }
            TagEnd::BlockQuote => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::Link => {
                // Append the URL as a dim suffix so the destination is visible.
                if let Some(url) = self.link_url.take() {
                    self.para_buf
                        .push(Span::styled(format!(" ({url})"), Style::new().dim()));
                }
                self.in_link = false;
            }
            _ => {}
        }
    }

    fn text(&mut self, t: &str) {
        if self.in_code_block {
            // Indented code block that `split_content` didn't catch: emit
            // verbatim with 4-space indent, no inline parsing.
            for line in t.lines() {
                self.segs.push(Segment::new(format!("    {line}")));
                self.segs.push(Segment::newline());
            }
        } else if self.heading_level.is_some() {
            // Heading style (bold) is applied uniformly in `flush_heading`;
            // store plain text here so we don't double-apply.
            self.heading_buf.push(Span::plain(t.to_string()));
        } else {
            let mut style = self.current_style();
            if self.in_link {
                style = style.underline();
            }
            self.para_buf.push(Span::styled(t.to_string(), style));
        }
    }

    fn inline_code(&mut self, code: &str) {
        self.para_buf.push(Span::styled(
            format!(" {} ", code.trim()),
            Style::new().reverse(),
        ));
    }
    fn start_list_item(&mut self) {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let bullet = match self.list_stack.last_mut() {
            Some(list) if list.ordered => {
                let n = list.next_number;
                list.next_number += 1;
                format!("{}. ", n)
            }
            _ => "• ".to_string(),
        };
        let full = format!("{indent}{bullet}");
        self.pending_bullet = Some(full.clone());
        // Hanging indent for continuation lines and subsequent paragraphs
        // within this item: spaces matching the bullet line's width.
        self.item_indent = " ".repeat(UnicodeWidthStr::width(full.as_str()));
    }

    /// Flush `para_buf` as one or more wrapped lines. If a bullet is pending
    /// (list item first line), emit it as the first-line prefix and use a
    /// space-padded hanging indent for continuation lines.
    fn flush_paragraph(&mut self) {
        if self.para_buf.is_empty() {
            // Even an empty paragraph in a list item should consume the bullet
            // so numbering stays correct.
            if let Some(bullet) = self.pending_bullet.take() {
                self.segs.push(Segment::new(bullet));
                self.segs.push(Segment::newline());
            }
            return;
        }

        // Blockquote path: wrap to width minus quote border, prefix each line
        // with │, apply dim+italic. Keeps quote visually distinct without
        // interfering with list-indent math.
        if self.quote_depth > 0 {
            self.flush_quote();
            return;
        }

        let first_prefix = self
            .pending_bullet
            .take()
            .unwrap_or_else(|| self.item_indent.clone());
        // Continuation indent: same visible width as first_prefix, all spaces.
        let cont_prefix = " ".repeat(UnicodeWidthStr::width(first_prefix.as_str()));
        // Available content width = terminal width minus the prefix.
        let prefix_w = UnicodeWidthStr::width(cont_prefix.as_str());
        let avail = self.width.saturating_sub(prefix_w).max(1);

        let lines = wrap_spans(&self.para_buf, avail);
        for (i, line) in lines.iter().enumerate() {
            let prefix = if i == 0 { &first_prefix } else { &cont_prefix };
            if !prefix.is_empty() {
                self.segs.push(Segment::new(prefix.clone()));
            }
            push_spans(&mut self.segs, line);
            self.segs.push(Segment::newline());
        }

        // Blank line after a top-level paragraph; inside lists, keep items
        // tight (no extra blank line between wrapped content and the next
        // item).
        if self.list_stack.is_empty() {
            self.segs.push(Segment::newline());
        }

        self.para_buf.clear();
    }

    /// Flush paragraph content as a blockquote: each line gets a `│ ` prefix
    /// per nesting level and dim+italic styling.
    fn flush_quote(&mut self) {
        let quote_prefix = "│ ".repeat(self.quote_depth);
        let prefix_w = UnicodeWidthStr::width(quote_prefix.as_str());
        let avail = self.width.saturating_sub(prefix_w).max(1);

        let dim_italic = Style::new().dim().italic();
        let lines = wrap_spans(&self.para_buf, avail);
        for line in lines.iter() {
            self.segs
                .push(Segment::styled(quote_prefix.clone(), Style::new().dim()));
            for span in line {
                let combined = span.style.clone().combine(&dim_italic);
                self.segs.push(Segment::styled(span.text.clone(), combined));
            }
            self.segs.push(Segment::newline());
        }
        self.segs.push(Segment::newline());
        self.para_buf.clear();
    }

    fn flush_heading(&mut self) {
        let level = self.heading_level.take().unwrap_or(1);
        let bold = Style::new().bold();

        for span in &self.heading_buf {
            let combined = span.style.clone().combine(&bold);
            if combined.is_empty() {
                self.segs.push(Segment::new(span.text.clone()));
            } else {
                self.segs.push(Segment::styled(span.text.clone(), combined));
            }
        }
        self.segs.push(Segment::newline());

        // H1/H2 get a character underline sized to the heading text (not a
        // fixed 40 chars), capped to terminal width.
        if level <= 2 {
            let hw = spans_width(&self.heading_buf);
            let underline_w = hw.min(self.width).max(1);
            let ch = if level == 1 { '═' } else { '─' };
            self.segs
                .push(Segment::new(ch.to_string().repeat(underline_w)));
            self.segs.push(Segment::newline());
        }

        self.segs.push(Segment::newline());
        self.heading_buf.clear();
    }
}

/// Push a slice of spans as segments, emitting plain text unstyled and styled
/// text with its ANSI codes.
fn push_spans(segs: &mut Segments, spans: &[Span]) {
    for span in spans {
        if span.style.is_empty() {
            segs.push(Segment::new(span.text.clone()));
        } else {
            segs.push(Segment::styled(span.text.clone(), span.style.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI escapes for layout assertions.
    fn plain(segs: &Segments) -> String {
        segs.plain_text()
    }

    #[test]
    fn wraps_long_paragraph() {
        let md = "This is a very long paragraph that should wrap at the terminal width instead of overflowing off the right side of the screen.";
        let out = plain(&render(md, 40));
        for line in out.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 40,
                "line exceeds width 40: {:?}",
                line
            );
        }
        // Content survives across wrapped lines.
        let joined: String = out.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(joined.contains("paragraph"));
    }

    #[test]
    fn preserves_inline_bold_in_paragraph() {
        let md = "This has **bold** text in a paragraph that is long enough to wrap across multiple lines of output.";
        let ansi = render(md, 30).to_ansi();
        // Bold ANSI escape (ESC [ 1 m) must appear somewhere.
        assert!(ansi.contains("\x1b[1m"), "no bold SGR in output");
    }

    #[test]
    fn nested_unordered_list_indent() {
        let md = "- outer\n  - inner\n";
        let out = plain(&render(md, 80));
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        // outer at column 0, inner at column 2
        let outer = lines.iter().find(|l| l.contains("outer")).unwrap();
        let inner = lines.iter().find(|l| l.contains("inner")).unwrap();
        assert_eq!(
            outer.find('•'),
            Some(0),
            "outer bullet at col 0: {:?}",
            outer
        );
        assert_eq!(
            inner.find('•'),
            Some(2),
            "inner bullet at col 2: {:?}",
            inner
        );
    }

    #[test]
    fn ordered_list_numbers() {
        let md = "1. first\n2. second\n3. third\n";
        let out = plain(&render(md, 80));
        assert!(out.contains("1. first"));
        assert!(out.contains("2. second"));
        assert!(out.contains("3. third"));
    }

    #[test]
    fn hanging_indent_for_wrapped_list_item() {
        // A long item whose text wraps: continuation lines must align under
        // the item text, not under the bullet.
        let md = "- a really long list item that will definitely need to wrap across multiple terminal lines";
        let out = plain(&render(md, 30));
        let lines: Vec<&str> = out.lines().collect();
        // First line starts with bullet at col 0.
        assert!(lines[0].starts_with("•"));
        // Continuation lines start with spaces (hanging indent), not text.
        for line in &lines[1..] {
            if line.trim().is_empty() {
                continue;
            }
            assert!(
                line.starts_with("  "),
                "continuation not hanging-indented: {:?}",
                line
            );
        }
    }

    #[test]
    fn heading_underline_matches_text_width() {
        let md = "# Hi\n";
        let out = plain(&render(md, 80));
        // Underline line should be 2 chars (width of "Hi"), not 40.
        let underline = out.lines().find(|l| l.chars().all(|c| c == '═'));
        assert!(underline.is_some(), "no H1 underline found");
        assert_eq!(
            UnicodeWidthStr::width(underline.unwrap()),
            2,
            "underline should match heading width"
        );
    }

    #[test]
    fn inline_code_reversestyled() {
        let md = "Use `println!` to print.";
        let ansi = render(md, 80).to_ansi();
        // Reverse video SGR (ESC [ 7 m) for inline code.
        assert!(ansi.contains("\x1b[7m"), "no reverse SGR for inline code");
    }

    #[test]
    fn preserves_cjk_width_in_wrap() {
        let md = "这是一段很长的中文段落内容用于测试换行功能是否正确处理中文字符的显示宽度";
        let out = plain(&render(md, 20));
        for line in out.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 20,
                "CJK line exceeds width: {:?} (w={})",
                line,
                UnicodeWidthStr::width(line)
            );
        }
    }

    #[test]
    fn blockquote_has_pipe_prefix() {
        let md = "> quoted text\n> second line";
        let out = plain(&render(md, 80));
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                line.starts_with('│'),
                "blockquote line missing │ prefix: {:?}",
                line
            );
        }
        assert!(out.contains("quoted text"));
    }

    #[test]
    fn link_text_underlined_with_url_suffix() {
        let md = "See [docs](https://example.com) for info.";
        let ansi = render(md, 80).to_ansi();
        // Link text gets underline SGR.
        assert!(ansi.contains("\x1b[4m"), "no underline for link text");
        // URL appears as a dim suffix.
        assert!(ansi.contains("https://example.com"), "URL not in output");
        // Dim SGR (ESC [ 2 m) somewhere for the URL suffix.
        assert!(ansi.contains("\x1b[2m"), "no dim for URL suffix");
    }
}
