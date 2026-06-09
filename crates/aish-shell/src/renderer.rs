use std::io::{self, Write};

use crate::recorder::SharedRecorder;
use richrs::color::{Color, StandardColor};
use richrs::console::Console;
use richrs::markdown::Markdown;
use richrs::style::Style;
use richrs::syntax::Syntax;
use richrs::table::{Column, Row, Table};

// ---------------------------------------------------------------------------
// Content splitting: code blocks → tables → regular markdown
// ---------------------------------------------------------------------------

/// A segment of markdown content classified by rendering strategy.
enum ContentSegment {
    /// Fenced code block with optional language tag.
    CodeBlock { lang: String, code: String },
    /// Markdown pipe table.
    Table(String),
    /// Everything else — rendered via richrs Markdown.
    Markdown(String),
}

/// Split markdown text into typed segments for specialized rendering.
///
/// Processing order:
/// 1. Fenced code blocks (` ```lang ... ``` `)
/// 2. Pipe tables (`| ... | ... |`)
/// 3. Remaining text → richrs Markdown
fn split_content(text: &str) -> Vec<ContentSegment> {
    // Phase 1: extract fenced code blocks
    let after_code = split_code_blocks(text);
    // Phase 2: within non-code segments, extract tables
    let mut result = Vec::new();
    for seg in after_code {
        match seg {
            ContentOrText::Content(cb) => result.push(cb),
            ContentOrText::Text(rest) => {
                let table_segs = split_tables(&rest);
                for (is_table, content) in table_segs {
                    if is_table {
                        result.push(ContentSegment::Table(content));
                    } else if !content.trim().is_empty() {
                        result.push(ContentSegment::Markdown(content));
                    }
                }
            }
        }
    }
    result
}

/// Intermediate type used during two-pass splitting.
enum ContentOrText {
    Content(ContentSegment),
    Text(String),
}

/// Extract fenced code blocks from text.
fn split_code_blocks(text: &str) -> Vec<ContentOrText> {
    let mut segments: Vec<ContentOrText> = Vec::new();
    let mut remaining = text;

    loop {
        // Find opening fence
        let open_idx = find_fence_open(remaining);
        match open_idx {
            None => {
                if !remaining.is_empty() {
                    segments.push(ContentOrText::Text(remaining.to_string()));
                }
                break;
            }
            Some(open_end) => {
                // Text before the fence
                let before = &remaining[..open_end.start_of_fence];
                if !before.is_empty() {
                    segments.push(ContentOrText::Text(before.to_string()));
                }

                // Extract lang tag (text after ``` on the opening line, trimmed)
                let lang = remaining[open_end.lang_range()].to_string();

                // Find closing fence
                let code_and_after = &remaining[open_end.code_start..];
                if let Some(close_offset) = find_fence_close(code_and_after) {
                    let code = code_and_after[..close_offset].to_string();
                    segments.push(ContentOrText::Content(ContentSegment::CodeBlock {
                        lang,
                        code,
                    }));
                    // Skip past closing fence line
                    let after_close = &code_and_after[close_offset..];
                    let skip = after_close
                        .find('\n')
                        .map(|p| p + 1)
                        .unwrap_or(after_close.len());
                    remaining = &after_close[skip..];
                } else {
                    // No closing fence — treat rest as code
                    segments.push(ContentOrText::Content(ContentSegment::CodeBlock {
                        lang,
                        code: code_and_after.to_string(),
                    }));
                    break;
                }
            }
        }
    }

    segments
}

struct FenceOpen {
    start_of_fence: usize, // byte offset where ``` begins
    lang_start: usize,     // byte offset of first char after ```
    lang_end: usize,       // byte offset of end of lang text (before \n)
    code_start: usize,     // byte offset of first code line (after \n)
}

impl FenceOpen {
    fn lang_range(&self) -> std::ops::Range<usize> {
        self.lang_start..self.lang_end
    }
}

fn find_fence_open(text: &str) -> Option<FenceOpen> {
    let mut search_from = 0;
    loop {
        let idx = text[search_from..].find("```")?;
        let abs = search_from + idx;
        // Make sure it's at the start of a line (or preceded only by spaces)
        let line_start = text[..abs].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = &text[line_start..abs];
        if !prefix.trim().is_empty() {
            search_from = abs + 3;
            continue;
        }
        // Find end of this line
        let after_ticks = abs + 3;
        let line_end = text[after_ticks..]
            .find('\n')
            .map(|p| after_ticks + p)
            .unwrap_or(text.len());
        let _lang = text[after_ticks..line_end].trim();
        return Some(FenceOpen {
            start_of_fence: line_start,
            lang_start: after_ticks,
            lang_end: line_end,
            code_start: if line_end < text.len() {
                line_end + 1
            } else {
                text.len()
            },
        });
    }
}

fn find_fence_close(text: &str) -> Option<usize> {
    let mut search_from = 0;
    loop {
        let idx = text[search_from..].find("```")?;
        let abs = search_from + idx;
        // Must be at start of line (optionally preceded by spaces)
        let line_start = text[..abs].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = text[line_start..abs].trim();
        if prefix.is_empty() {
            // It's a closing fence — return offset up to line start
            return Some(line_start);
        }
        search_from = abs + 3;
    }
}

/// Split a non-code-block segment into table and non-table pieces.
fn split_tables(text: &str) -> Vec<(bool, String)> {
    let mut segments: Vec<(bool, String)> = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();
    let mut in_table = false;

    let flush_non_table = |lines: &mut Vec<&str>, segs: &mut Vec<(bool, String)>| {
        if !lines.is_empty() {
            segs.push((false, lines.join("\n")));
            lines.clear();
        }
    };
    let flush_table = |lines: &mut Vec<&str>, segs: &mut Vec<(bool, String)>| {
        if !lines.is_empty() {
            segs.push((true, lines.join("\n")));
            lines.clear();
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        let is_pipe_row = trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 2;
        let is_sep_row = is_pipe_row
            && trimmed
                .chars()
                .all(|c| c == '|' || c == '-' || c == ':' || c == ' ');

        if is_pipe_row && !is_sep_row {
            if !in_table {
                flush_non_table(&mut current_lines, &mut segments);
                in_table = true;
            }
            current_lines.push(line);
        } else if is_sep_row && in_table {
            current_lines.push(line);
        } else {
            if in_table {
                flush_table(&mut current_lines, &mut segments);
                in_table = false;
            }
            current_lines.push(line);
        }
    }

    if in_table {
        flush_table(&mut current_lines, &mut segments);
    } else {
        flush_non_table(&mut current_lines, &mut segments);
    }

    segments
}

// ---------------------------------------------------------------------------
// Table rendering via richrs Table
// ---------------------------------------------------------------------------

/// Render a markdown pipe table using richrs Table.
fn render_table_to_segments(lines: &[&str], width: usize) -> Option<richrs::segment::Segments> {
    let headers = parse_pipe_row(lines.first()?)?;
    let sep = lines.get(1)?.trim();
    if !sep
        .chars()
        .all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
    {
        return None;
    }

    let mut table = Table::new()
        .border_style(Style::new().dim())
        .header_style(Style::new().bold());

    for h in &headers {
        table.add_column(Column::new(h.as_str()));
    }

    for line in lines.iter().skip(2) {
        if let Some(cells) = parse_pipe_row(line) {
            if cells.len() == headers.len() {
                table.add_row(Row::new(cells));
            }
        }
    }

    Some(table.render(width))
}

fn parse_pipe_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len().saturating_sub(1)];
    let cells: Vec<String> = inner.split('|').map(|s| s.trim().to_string()).collect();
    if cells.is_empty() {
        return None;
    }
    Some(cells)
}

// ---------------------------------------------------------------------------
// Code block rendering
// ---------------------------------------------------------------------------

/// Render a fenced code block with syntax highlighting.
/// Minimal style: dim bold language tag, then syntax-highlighted code, no borders.
fn render_code_block(
    console: &mut Console,
    lang: &str,
    code: &str,
    width: usize,
    shared_recorder: &Option<SharedRecorder>,
) {
    let code_trimmed = code.trim_end();
    if code_trimmed.is_empty() {
        return;
    }

    // Dim bold language label
    if !lang.is_empty() {
        let lang_ansi = format!("\x1b[1;2m{}\x1b[0m\n", lang);
        if let Some(ref sr) = shared_recorder {
            crate::recorder::shared_record_output(sr, &lang_ansi);
        }
        println!("\x1b[1;2m{}\x1b[0m", lang);
    }

    // Syntax-highlighted code
    let syntax = Syntax::new(code_trimmed, lang).theme("base16-ocean.dark");
    let segments = syntax.render(width);
    if let Some(ref sr) = shared_recorder {
        crate::recorder::shared_record_output(sr, &segments.to_ansi());
    }
    let _ = console.write_segments(&segments);
    let _ = console.flush();
}

// ---------------------------------------------------------------------------
// ShellRenderer
// ---------------------------------------------------------------------------

/// Terminal renderer using richrs for markdown, syntax highlighting, tables, and separators.
pub struct ShellRenderer {
    console: Console,
    /// Tracks whether streaming content has been received.
    streaming_active: bool,
    terminal_width: usize,
    /// Shared recorder — multiple renderer instances can write to the same file.
    shared_recorder: Option<SharedRecorder>,
}

impl ShellRenderer {
    pub fn new() -> Self {
        let console = Console::new();
        let terminal_width = console.width().max(40);
        Self {
            console,
            streaming_active: false,
            terminal_width,
            shared_recorder: None,
        }
    }

    /// Return the detected terminal width (columns).
    pub fn width(&self) -> usize {
        self.terminal_width
    }

    /// Render complete markdown text.
    /// Code blocks → syntax highlighting, tables → box drawing, rest → richrs Markdown.
    /// Records output for cast file: raw text for markdown (preserving line breaks),
    /// ANSI output for code blocks and tables (preserving formatting).
    pub fn render_markdown(&mut self, text: &str) {
        let segments = split_content(text);
        let mut last_was_markdown = false;
        for seg in segments {
            match seg {
                ContentSegment::CodeBlock { lang, code } => {
                    render_code_block(
                        &mut self.console,
                        &lang,
                        &code,
                        self.terminal_width,
                        &self.shared_recorder,
                    );
                    last_was_markdown = false;
                }
                ContentSegment::Table(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    if let Some(segs) = render_table_to_segments(&lines, self.terminal_width) {
                        if let Some(ref sr) = self.shared_recorder {
                            crate::recorder::shared_record_output(sr, &segs.to_ansi());
                        }
                        let _ = self.console.write_segments(&segs);
                    }
                    last_was_markdown = false;
                }
                ContentSegment::Markdown(content) => {
                    let inline_style = Style::new()
                        .with_color(Color::Standard(StandardColor::Cyan))
                        .bold();
                    let md = Markdown::new(&content).inline_code_style(inline_style);
                    let segs = md.render(self.terminal_width);
                    // Record ANSI-rendered output so agg/GIF shows proper formatting.
                    // richrs adds two trailing newlines per paragraph; the terminal
                    // removes one with \x1b[1A\x1b[M. Remove one trailing newline
                    // from the recording to match the terminal display.
                    if let Some(ref sr) = self.shared_recorder {
                        let mut ansi = segs.to_ansi();
                        if ansi.ends_with("\n\n") {
                            ansi.truncate(ansi.len() - 1);
                        }
                        crate::recorder::shared_record_output(sr, &ansi);
                    }
                    let _ = self.console.write_segments(&segs);
                    last_was_markdown = true;
                }
            }
        }
        let _ = self.console.flush();
        let _ = io::stdout().flush();
        // richrs Markdown adds two trailing newlines at paragraph end,
        // creating a blank line before the bottom separator.
        // Delete it only when the last segment was a Markdown paragraph.
        if last_was_markdown {
            print!("\x1b[1A\x1b[M");
            let _ = io::stdout().flush();
        }
    }

    /// Append a streaming delta — prints gray text for real-time feedback.
    /// Records with ANSI color for accurate replay/GIF.
    pub fn append_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(ref sr) = self.shared_recorder {
            crate::recorder::shared_record_output(sr, &format!("\x1b[1;90m{}\x1b[0m", delta));
        }
        self.streaming_active = true;
        print!("\x1b[1;90m{}\x1b[0m", delta);
        let _ = io::stdout().flush();
    }

    /// Finalize streaming — print a newline and reset state.
    /// Matches Python's _finalize_content_preview approach.
    pub fn finalize_stream(&mut self) {
        if !self.streaming_active {
            return;
        }
        self.streaming_active = false;
        println!();
    }

    /// Reset state (call at GenerationStart).
    pub fn reset(&mut self) {
        self.streaming_active = false;
    }

    /// Attach a shared recorder for recording rendered output.
    pub fn set_shared_recorder(&mut self, recorder: SharedRecorder) {
        self.shared_recorder = Some(recorder);
    }

    /// Render a green horizontal separator line spanning the terminal.
    /// Records a fixed 5-char separator with trailing newline for clean cast/GIF output.
    pub fn render_separator(&mut self) {
        let width = self.terminal_width.max(20);
        let sep = "─".repeat(width);
        if let Some(ref sr) = self.shared_recorder {
            crate::recorder::shared_record_output(sr, "─────\r\n");
        }
        println!("\x1b[32m{}\x1b[0m", sep);
    }
}

impl Default for ShellRenderer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- pipe row parsing ---

    #[test]
    fn test_parse_pipe_row() {
        assert_eq!(
            parse_pipe_row("| a | b | c |").unwrap(),
            vec!["a", "b", "c"]
        );
    }
    #[test]
    fn test_parse_pipe_row_invalid() {
        assert!(parse_pipe_row("no pipes").is_none());
    }

    // --- table rendering (richrs Table) ---

    #[test]
    fn test_render_table_to_segments() {
        let lines = vec!["| Name | Value |", "|------|-------|", "| Foo  | 100   |"];
        let segments = render_table_to_segments(&lines, 80).unwrap();
        let text = segments.plain_text();
        assert!(text.contains("Name"));
        assert!(text.contains("Foo"));
        assert!(text.contains("100"));
    }
    #[test]
    fn test_render_table_to_segments_cjk() {
        let lines = vec!["| 项目 | 数量 |", "|------|------|", "| 总内存 | 15 GB |"];
        let segments = render_table_to_segments(&lines, 80).unwrap();
        let text = segments.plain_text();
        assert!(text.contains("项目"));
        assert!(text.contains("总内存"));
    }
    #[test]
    fn test_render_table_to_segments_invalid() {
        let lines = vec!["no table here"];
        assert!(render_table_to_segments(&lines, 80).is_none());
    }

    // --- split_tables ---

    #[test]
    fn test_split_tables_mixed() {
        let segs = split_tables("Hello\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\nMore");
        assert_eq!(segs.len(), 3);
        assert!(!segs[0].0);
        assert!(segs[1].0);
        assert!(!segs[2].0);
    }

    // --- split_content (code blocks) ---

    #[test]
    fn test_split_content_code_block() {
        let md = "Before\n```bash\necho hello\n```\nAfter";
        let segs = split_content(md);
        assert_eq!(segs.len(), 3, "expected 3 segments, got {}", segs.len());
        match &segs[0] {
            ContentSegment::Markdown(t) => assert!(t.contains("Before")),
            other => panic!("expected Markdown, got {:?}", seg_type(other)),
        }
        match &segs[1] {
            ContentSegment::CodeBlock { lang, code } => {
                assert_eq!(lang, "bash");
                assert!(code.contains("echo hello"));
            }
            other => panic!("expected CodeBlock, got {:?}", seg_type(other)),
        }
        match &segs[2] {
            ContentSegment::Markdown(t) => assert!(t.contains("After")),
            other => panic!("expected Markdown, got {:?}", seg_type(other)),
        }
    }

    #[test]
    fn test_split_content_no_code() {
        let segs = split_content("Just text\nMore text");
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            ContentSegment::Markdown(_) => {}
            other => panic!("expected Markdown, got {:?}", seg_type(other)),
        }
    }

    #[test]
    fn test_split_content_code_with_table() {
        let md = "```bash\nls -la\n```\n\n| A |\n|---|\n| 1 |";
        let segs = split_content(md);
        assert_eq!(segs.len(), 2);
        match &segs[0] {
            ContentSegment::CodeBlock { lang, .. } => assert_eq!(lang, "bash"),
            other => panic!("expected CodeBlock, got {:?}", seg_type(other)),
        }
        match &segs[1] {
            ContentSegment::Table(_) => {}
            other => panic!("expected Table, got {:?}", seg_type(other)),
        }
    }

    #[test]
    fn test_split_content_multiple_code_blocks() {
        let md = "```bash\necho 1\n```\nText\n```python\nprint(2)\n```";
        let segs = split_content(md);
        assert_eq!(segs.len(), 3);
    }

    #[test]
    fn test_bash_syntax_highlighting_has_ansi_codes() {
        let code = "echo hello\nip addr show\n";
        let syntax = Syntax::new(code, "bash").theme("base16-ocean.dark");
        let segments = syntax.render(80);
        let ansi_output = segments.to_ansi();
        // If syntax highlighting works, output should contain ANSI escape codes
        let plain = segments.plain_text();
        assert!(plain.contains("echo"));
        assert!(plain.contains("ip"));
        // Check that ANSI escape sequences are present (colors applied)
        assert!(
            ansi_output.contains("\x1b["),
            "bash code should have ANSI color codes, got: {:?}",
            ansi_output
        );
    }

    #[test]
    fn test_bash_vs_plain_text_differ() {
        let code = "echo hello\nls -la\n";
        let syntax_bash = Syntax::new(code, "bash").theme("base16-ocean.dark");
        let syntax_plain = Syntax::new(code, "text").theme("base16-ocean.dark");
        let ansi_bash = syntax_bash.render(80).to_ansi();
        let ansi_plain = syntax_plain.render(80).to_ansi();
        // bash-highlighted output should differ from plain text
        assert_ne!(
            ansi_bash, ansi_plain,
            "bash highlighting should produce different output than plain text"
        );
    }

    /// Helper to get segment type name for error messages.
    fn seg_type(seg: &ContentSegment) -> &'static str {
        match seg {
            ContentSegment::CodeBlock { .. } => "CodeBlock",
            ContentSegment::Table(_) => "Table",
            ContentSegment::Markdown(_) => "Markdown",
        }
    }
}
