use std::io::{self, Write};

use crate::file_mention::fuzzy_score;
use crate::text::strip_ansi_escapes;
use crate::util::truncate_str;
use unicode_width::UnicodeWidthStr;

use crossterm::{
    cursor, event,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, terminal,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal, TerminalOptions, Viewport,
};

/// Outcome of the slash input session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashInputOutcome {
    /// User selected a slash command to run as typed (e.g. "/help").
    Command(String),
    /// User picked a command that must not run bare (needs arguments, opens
    /// a picker, or is destructive): fill the readline with "cmd " and let
    /// the user confirm with a second Enter.
    Fill(String),
    /// Input no longer matches any command; return to normal readline with this text.
    Dismissed(String),
    /// User pressed Esc (or emptied the input): restore this text in the
    /// readline. Empty string means discard and return to a fresh prompt.
    Cancelled(String),
}

const MAX_VISIBLE_COMMANDS: usize = 8;
const MAX_PANEL_HEIGHT: u16 = 10;

/// One renderable row in the popup list: a group header or a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayRow {
    Header(usize),
    Command(usize),
}

/// Availability status shown next to a command row. Unavailable commands
/// stay visible (greyed) with the reason and an enable hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandStatus {
    /// Whether the command can run right now.
    pub enabled: bool,
    /// Short state word (e.g. "unavailable") rendered before the reason.
    pub status_text: String,
    /// Why it is unavailable, rendered as a suffix on the row.
    pub reason: String,
}

impl SlashCommandStatus {
    /// Default available status; the row renders the description only.
    pub fn available() -> Self {
        Self {
            enabled: true,
            status_text: String::new(),
            reason: String::new(),
        }
    }
}

/// One popup row: command name, localized description, and popup metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandEntry {
    /// Command with leading "/", e.g. "/help".
    pub name: String,
    /// Localized description (already translated by the caller).
    pub desc: String,
    /// Extra fuzzy-search terms (English aliases + scenario words).
    pub keywords: Vec<String>,
    /// Index into the caller's group-label list; rows sort by (group, name).
    pub group_index: usize,
    /// True when Enter must fill instead of execute (Fill policy).
    pub fill_only: bool,
    /// Availability status; commands without one are always available.
    pub status: Option<SlashCommandStatus>,
}

impl SlashCommandEntry {
    /// Best fuzzy score across name, description, and keywords. A positive
    /// score requires a word-initial or consecutive bonus to outweigh the
    /// position/length penalties, which filters scattered trivial matches.
    /// Single-char ASCII queries fall back to plain name-prefix matching so
    /// Tab prefix extension behaves exactly like the old prefix filter
    /// (fuzzy on one letter matches too many scattered name hits); CJK and
    /// other non-ASCII single chars use the full fuzzy path because they only
    /// ever match via descriptions/keywords. From two chars on, descriptions
    /// and keywords always join the fuzzy match.
    fn best_score(&self, query: &str) -> Option<i64> {
        let mut chars = query.chars();
        let is_single_char = chars.next().is_some() && chars.next().is_none();
        if is_single_char && query.chars().all(|c| c.is_ascii_alphanumeric()) {
            let ch = query.chars().next().expect("non-empty");
            let lower = ch.to_ascii_lowercase();
            return if self
                .name
                .trim_start_matches('/')
                .to_lowercase()
                .starts_with(lower)
            {
                Some(1)
            } else {
                None
            };
        }
        // Which field matched matters more than how well it matched. The
        // scores themselves are not comparable across fields: `fuzzy_score`
        // rewards short targets, so `/report` scored 191 on its own name while
        // `/feedback` scored 192 on its "report" keyword and took the first
        // row. A name hit is what the user meant; a keyword or description hit
        // is a hint about a different command.
        const NAME_TIER: i64 = 2_000;
        const KEYWORD_TIER: i64 = 1_000;
        let name = fuzzy_score(query, &self.name).map(|s| s + NAME_TIER);
        let kw = self
            .keywords
            .iter()
            .filter_map(|k| fuzzy_score(query, k))
            .max()
            .map(|s| s + KEYWORD_TIER);
        let desc = fuzzy_score(query, &self.desc);
        name.into_iter()
            .chain(desc)
            .chain(kw)
            .filter(|s| *s > 0)
            .max()
    }
}
pub struct SlashInputSession {
    entries: Vec<SlashCommandEntry>,
    /// Group header labels by group index; empty label = no header row.
    group_labels: Vec<String>,
    prompt: String,
    input: String,
    cursor: usize,
    selected: usize,
    filtered: Vec<usize>,
}

impl SlashInputSession {
    /// Build a session. Rows sort by (group_index, name) so the popup order
    /// is stable regardless of locale; empty group labels hide headers.
    pub fn new(
        mut entries: Vec<SlashCommandEntry>,
        group_labels: Vec<String>,
        prompt: String,
    ) -> Self {
        entries.sort_by(|a, b| {
            a.group_index
                .cmp(&b.group_index)
                .then_with(|| a.name.cmp(&b.name))
        });
        // Strip ANSI escape codes — ratatui renders via its own style system
        let prompt = strip_ansi_escapes(&prompt);
        let mut filtered = Vec::with_capacity(entries.len());
        filtered.extend(0..entries.len());
        Self {
            entries,
            group_labels,
            prompt,
            input: String::from("/"),
            cursor: 1,
            selected: 0,
            filtered,
        }
    }

    /// Run the session in raw mode. Returns the outcome.
    pub fn run(mut self) -> io::Result<SlashInputOutcome> {
        let _guard = RawModeGuard::enter()?;
        // Rustyline appends a newline after returning on Cmd::Interrupt.
        // Move back to the original prompt line before ratatui queries the
        // cursor position for the inline viewport.
        execute!(io::stdout(), cursor::MoveToColumn(0), cursor::MoveUp(1))?;
        // Drain any stale keyboard events before ratatui issues DSR queries.
        // Leftover bytes can corrupt the cursor position response parsing.
        drain_pending_events()?;
        let mut viewport_height = self.panel_height();
        let mut terminal = open_inline_terminal(viewport_height)?;

        let outcome = loop {
            let desired_height = self.panel_height();
            if desired_height != viewport_height {
                let _ = terminal.clear();
                drop(terminal);
                viewport_height = desired_height;
                terminal = open_inline_terminal(viewport_height)?;
            }
            terminal.draw(|frame| self.render(frame, frame.area()))?;

            let event = event::read()?;
            // A `Viewport::Fixed` is never auto-resized by ratatui, so a
            // window change would leave the popup drawn at stale absolute
            // rows. Re-fit it from the stored anchor instead.
            if matches!(event, Event::Resize(..)) {
                terminal.resize(viewport_height)?;
                continue;
            }
            if let Some(result) = self.handle_event(event) {
                break result;
            }
        };

        // All terminal cleanup MUST happen while still in raw mode.
        // If terminal is dropped after termios restore, its Drop impl may
        // write escape sequences in cooked mode, corrupting the terminal
        let _ = terminal.clear();
        drop(terminal);
        drop(_guard);
        let _ = io::stdout().flush();
        Ok(outcome)
    }

    /// Pre-fill the input after construction.
    ///
    /// Used when re-opening the popup from readline Tab completion on a
    /// `/` prefix — the popup starts with the text the user had typed.
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self.cursor = self.input.len();
        self.update_filtered();
        self
    }

    /// Dispatch a keyboard event without raw-mode TUI (for integration tests).
    #[doc(hidden)]
    pub fn dispatch_event(&mut self, event: Event) -> Option<SlashInputOutcome> {
        self.handle_event(event)
    }

    fn panel_height(&self) -> u16 {
        if self.filtered.is_empty() {
            return 1;
        }
        // Keep a stable height while the list is visible so typing does not resize
        // the inline viewport on every filter change (which causes flicker).
        (1 + MAX_VISIBLE_COMMANDS as u16).min(MAX_PANEL_HEIGHT)
    }

    /// Slash command token before the first space (e.g. `/help` from `/help foo`).
    fn command_query(&self) -> &str {
        match self.input.find(' ') {
            Some(i) => &self.input[..i],
            None => &self.input,
        }
    }
    fn update_filtered(&mut self) {
        if self.should_hide_command_list() {
            self.filtered.clear();
            return;
        }
        let query = self.command_query().trim_start_matches('/').to_lowercase();
        if query.is_empty() {
            self.filtered = (0..self.entries.len()).collect();
            self.selected = 0;
            return;
        }
        // Fuzzy match against name + localized description + keywords; keep
        // only subsequence hits and rank them by best score. Ties break by
        // registration order. Groups may interleave; display_rows renders
        // each group header once at its first occurrence.
        let mut scored: Vec<(i64, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.best_score(&query).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
        self.clamp_selected();
    }

    fn replace_command_token(&mut self, new_command: &str) {
        if let Some(space_idx) = self.input.find(' ') {
            self.input = format!("{new_command}{}", &self.input[space_idx..]);
        } else {
            self.input = new_command.to_string();
        }
        self.cursor = self.input.len();
    }

    fn clamp_selected(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    /// Whether the current input is still a fuzzy match for any command.
    fn has_command_prefix(&self) -> bool {
        let query = self.command_query().trim_start_matches('/').to_lowercase();
        if query.is_empty() {
            return true;
        }
        self.entries.iter().any(|e| e.best_score(&query).is_some())
    }

    /// True when input is an exact command followed by a space (Tab completion or typed).
    /// Hide the suggestion list so Enter does not pick a different row.
    fn should_hide_command_list(&self) -> bool {
        let Some(space_idx) = self.input.find(' ') else {
            return false;
        };
        let command_name = &self.input[..space_idx];
        self.entries.iter().any(|e| e.name == command_name)
    }

    /// True when the user has started typing arguments (not just a trailing space).
    fn has_real_command_args(input: &str) -> bool {
        if !input.contains(' ') {
            return false;
        }
        !input.ends_with(' ')
    }

    fn format_command_with_trailing_space(command_name: &str) -> String {
        format!("{command_name} ")
    }

    /// Entry of the currently highlighted command in the filtered list.
    fn selected_entry(&self) -> Option<&SlashCommandEntry> {
        self.filtered
            .get(self.selected)
            .map(|&idx| &self.entries[idx])
    }

    /// Enter: exact Execute command runs; exact Fill command fills; the
    /// highlighted row decides otherwise. Disabled commands do nothing
    /// (popup stays open so the user can read the enable hint).
    fn handle_submit(&mut self) -> Option<SlashInputOutcome> {
        let trimmed = self.input.trim();
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        let exact = self
            .entries
            .iter()
            .find(|e| first_word == e.name || trimmed == e.name);
        if let Some(entry) = exact {
            return self.outcome_for(entry, trimmed);
        }
        if !self.filtered.is_empty() {
            let Some(entry) = self.selected_entry() else {
                return Some(SlashInputOutcome::Dismissed(self.input.clone()));
            };
            return self.outcome_for(entry, &entry.name);
        }
        Some(SlashInputOutcome::Dismissed(self.input.clone()))
    }

    /// Command/Fill/no-op for one picked entry. Disabled entries never run.
    fn outcome_for(&self, entry: &SlashCommandEntry, text: &str) -> Option<SlashInputOutcome> {
        if entry.status.as_ref().is_some_and(|s| !s.enabled) {
            return None;
        }
        Some(if entry.fill_only {
            SlashInputOutcome::Fill(entry.name.clone())
        } else {
            SlashInputOutcome::Command(text.to_string())
        })
    }

    /// Tab: extend shared prefix, or complete the highlighted command and dismiss.
    fn handle_tab_complete(&mut self) -> Option<SlashInputOutcome> {
        if self.filtered.is_empty() {
            return None;
        }
        if self.filtered.len() == 1 {
            let command = self.selected_entry()?.name.clone();
            let completed = Self::format_command_with_trailing_space(&command);
            return Some(SlashInputOutcome::Dismissed(completed));
        }

        let names: Vec<&str> = self
            .filtered
            .iter()
            .map(|&idx| self.entries[idx].name.as_str())
            .collect();
        let lcp = longest_common_prefix(&names);
        let query = self.command_query();
        if lcp.len() > query.len() {
            self.replace_command_token(&lcp);
            self.update_filtered();
            return None;
        }

        let command = self.selected_entry()?.name.clone();
        let completed = Self::format_command_with_trailing_space(&command);
        Some(SlashInputOutcome::Dismissed(completed))
    }

    /// Handle an event. Returns `Some(outcome)` when the session should end.
    fn handle_event(&mut self, event: Event) -> Option<SlashInputOutcome> {
        let Event::Key(key) = event else { return None };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return None;
        }

        // Ctrl+C discards everything: restore to a fresh prompt.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(SlashInputOutcome::Cancelled(String::new()));
        }

        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
                self.update_filtered();
                if Self::has_real_command_args(&self.input) {
                    return Some(SlashInputOutcome::Dismissed(self.input.clone()));
                }
                // No longer a slash command prefix (e.g. /bin) → back to readline
                if !self.has_command_prefix() {
                    return Some(SlashInputOutcome::Dismissed(self.input.clone()));
                }
                None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.input.drain(prev..self.cursor);
                    self.cursor = prev;
                    self.update_filtered();
                    if self.input.is_empty() {
                        return Some(SlashInputOutcome::Cancelled(self.input.clone()));
                    }
                    if !self.has_command_prefix() {
                        return Some(SlashInputOutcome::Dismissed(self.input.clone()));
                    }
                }
                None
            }
            KeyCode::Up => {
                if !self.filtered.is_empty() && self.selected > 0 {
                    self.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1).min(self.filtered.len() - 1);
                }
                None
            }
            KeyCode::Enter => self.handle_submit(),
            KeyCode::Tab => self.handle_tab_complete(),
            KeyCode::Home => {
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.cursor = self.input.len();
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.input[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    self.cursor += self.input[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
                None
            }
            KeyCode::Esc => Some(SlashInputOutcome::Cancelled(self.input.clone())),
            _ => None,
        }
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let input_area = Rect::new(area.x, area.y, area.width, 1);
        let list_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );

        self.render_input_line(frame, input_area);
        self.render_command_list(frame, list_area);
    }

    fn render_input_line(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let prompt_style = Style::default().fg(Color::Green);
        let input_style = Style::default().fg(Color::White);
        let cursor_style = Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD);

        let before = &self.input[..self.cursor];
        let cursor_char = self.input[self.cursor..].chars().next();
        let after_start = self.cursor + cursor_char.map_or(0, |c| c.len_utf8());
        let after = &self.input[after_start..];

        let mut spans = vec![Span::styled(self.prompt.clone(), prompt_style)];
        spans.push(Span::styled(before.to_string(), input_style));
        if let Some(ch) = cursor_char {
            spans.push(Span::styled(ch.to_string(), cursor_style));
            spans.push(Span::styled(after.to_string(), input_style));
        } else {
            spans.push(Span::styled(" ", cursor_style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Expand filtered command indexes into rows, inserting a group header
    /// before the first command of each new group.
    fn display_rows(&self) -> Vec<DisplayRow> {
        let mut rows = Vec::with_capacity(self.filtered.len() + self.group_labels.len());
        // Score order may interleave groups; render each header exactly once,
        // at the group's first occurrence, so a group is never split by a
        // repeated header row.
        let mut rendered_groups: Vec<usize> = Vec::with_capacity(self.group_labels.len());
        for &idx in &self.filtered {
            let entry = &self.entries[idx];
            let Some(label) = self.group_labels.get(entry.group_index) else {
                rows.push(DisplayRow::Command(idx));
                continue;
            };
            if label.is_empty() {
                rows.push(DisplayRow::Command(idx));
                continue;
            }
            if !rendered_groups.contains(&entry.group_index) {
                rows.push(DisplayRow::Header(entry.group_index));
                rendered_groups.push(entry.group_index);
            }
            rows.push(DisplayRow::Command(idx));
        }
        rows
    }

    /// Build the spans for one command row: marker, name, description, and
    /// (when unavailable) the status suffix — all truncated to the width.
    /// Name and description use separate spans so the DarkGray description
    /// keeps visual contrast against the bright selected name.
    fn command_row(&self, idx: usize, is_selected: bool, width: usize) -> Line<'static> {
        let entry = &self.entries[idx];
        let disabled = entry.status.as_ref().is_some_and(|s| !s.enabled);
        let marker = if is_selected { "▸ " } else { "  " };
        let marker_style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let name_style = if disabled {
            Style::default().fg(Color::DarkGray)
        } else if is_selected {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let mut name = String::from(entry.name.as_str());
        name.push_str("  ");
        let mut desc = entry.desc.clone();
        let status = entry.status.as_ref();
        if let Some(status) = status.filter(|s| !s.status_text.is_empty()) {
            desc.push_str(&format!("  [{}]", status.status_text));
        }
        if disabled {
            let status = status.expect("disabled implies a status");
            if !status.reason.is_empty() {
                desc.push_str(&format!(" — {}", status.reason));
            }
        }
        let marker_width = marker.width();
        let name = truncate_str(&name, width.saturating_sub(marker_width));
        let desc_budget = width
            .saturating_sub(marker_width)
            .saturating_sub(name.width());
        let desc = truncate_str(&desc, desc_budget);

        let desc_style = if disabled {
            Style::default().fg(Color::DarkGray)
        } else if is_selected {
            Style::default().fg(Color::Gray)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        Line::from(vec![
            Span::styled(marker.to_string(), marker_style),
            Span::styled(name, name_style),
            Span::styled(desc, desc_style),
        ])
    }

    fn render_command_list(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if self.filtered.is_empty() || area.height == 0 {
            return;
        }

        let max_visible = area.height as usize;
        let width = area.width as usize;
        let rows = self.display_rows();
        let scroll = self.scroll_offset(&rows, max_visible);
        let end = (scroll + max_visible).min(rows.len());
        let header_style = Style::default().fg(Color::DarkGray);
        let lines: Vec<Line> = rows[scroll..end]
            .iter()
            .map(|row| match row {
                DisplayRow::Header(g) => {
                    let label = self.group_labels.get(*g).map(String::as_str).unwrap_or("");
                    Line::from(Span::styled(
                        truncate_str(&format!("── {label}"), width),
                        header_style,
                    ))
                }
                DisplayRow::Command(idx) => {
                    let is_selected = self.selected_command() == Some(*idx);
                    self.command_row(*idx, is_selected, width)
                }
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), area);
    }

    /// Index (into `entries`) of the currently highlighted command.
    fn selected_command(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    /// Row position (within `display_rows`) of the selected command.
    fn selected_command_row(&self) -> Option<usize> {
        let target = self.selected_command()?;
        self.display_rows()
            .iter()
            .position(|r| matches!(r, DisplayRow::Command(i) if *i == target))
    }

    /// First visible row index so the selected command stays on screen.
    /// A header at the window top would be an orphan row, so scroll past it.
    fn scroll_offset(&self, rows: &[DisplayRow], max_visible: usize) -> usize {
        let Some(sel_row) = self.selected_command_row() else {
            return 0;
        };
        if sel_row < max_visible {
            return 0;
        }
        let mut start = sel_row + 1 - max_visible;
        if matches!(rows.get(start), Some(DisplayRow::Header(_))) {
            start += 1;
        }
        start.min(rows.len().saturating_sub(max_visible))
    }
}

pub(crate) struct RawModeGuard;

impl RawModeGuard {
    pub(crate) fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show);
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().flush();
    }
}

pub(crate) fn drain_pending_events() -> io::Result<()> {
    while event::poll(std::time::Duration::from_millis(0))? {
        let _ = event::read()?;
    }
    Ok(())
}

/// Inline popup terminal docked at an absolute terminal row.
///
/// Wraps the ratatui terminal together with the rows it occupies so the
/// popup can erase exactly its own rows and park the cursor back on
/// `start_row` when it closes.
pub(crate) struct InlineTerminal {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    /// Absolute terminal row of the popup's first line.
    start_row: u16,
    /// Number of rows the popup occupies.
    height: u16,
    /// Row the popup should be anchored at when the terminal is resized.
    /// This is the prompt row after the initial scroll reservation; content
    /// above it lives in the scrollback buffer and is never destroyed.
    anchor_row: u16,
}

impl InlineTerminal {
    /// Redraw the popup contents.
    pub(crate) fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut ratatui::Frame<'_>),
    {
        self.terminal.draw(render)?;
        Ok(())
    }

    /// Rebuild the viewport after a terminal resize.
    ///
    /// A `Viewport::Fixed` is never auto-resized by ratatui, so drawing would
    /// keep targeting rows that no longer exist. Re-runs the reservation from
    /// the stored anchor and swaps in a fresh terminal.
    pub(crate) fn resize(&mut self, height: u16) -> io::Result<()> {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let (scroll, start_row, clamped_height) = fit_popup(self.anchor_row, rows, height);
        // Erase the old popup region, clipped to the rows that still exist: a
        // VT cursor move past the bottom edge stops at the last row, so an
        // unclipped erase after a shrink would clear whatever output now sits
        // there.
        let old_end = self.start_row.saturating_add(self.height).min(rows);
        if self.start_row < old_end {
            clear_rows(self.start_row, old_end - self.start_row)?;
        }
        // Propagate the reservation failure: on a partial scroll the rows the
        // popup is about to claim have not been freed yet, and clearing them
        // would erase command output.
        scroll_up(scroll, rows)?;
        clear_rows(start_row, clamped_height)?;
        self.terminal = Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, start_row, cols, clamped_height)),
            },
        )?;
        self.start_row = start_row;
        self.height = clamped_height;
        // The prompt moved with the scroll, so the next refit must anchor at
        // the post-scroll row: re-anchoring at the pre-scroll row would scroll
        // again on every resize event (including width-only changes).
        self.anchor_row = start_row;
        Ok(())
    }

    /// Erase every popup row and move the cursor back to `start_row`.
    ///
    /// `Terminal::clear()` is deliberately NOT used here: on ratatui-core
    /// 0.1.2 it snapshots the cursor via DSR and restores it afterwards to
    /// wherever the last `draw` left it (the bottom of the popup), so the
    /// caller's next `read_line` redraws the prompt several rows below the
    /// original position and leaves blank lines above it.
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        clear_rows(self.start_row, self.height)
    }
}

/// Erase `height` screen rows starting at `start_row` and park the cursor on
/// `start_row`.
///
/// Shared by [`InlineTerminal`] (open and close) and `PanelRuntime` so the
/// popup and panel cleanup policies cannot drift apart. The erase is bounded
/// to the rows the popup actually owns: content below it is left untouched.
/// Best-effort — a write failure aborts the remaining rows, but the cursor is
/// still parked and the caller decides whether that is fatal (`PanelRuntime`
/// ignores it and still returns its outcome).
pub(crate) fn clear_rows(start_row: u16, height: u16) -> io::Result<()> {
    let mut stdout = io::stdout();
    let end = start_row.saturating_add(height.max(1));
    let result = (start_row..end).try_for_each(|y| {
        execute!(
            stdout,
            cursor::MoveTo(0, y),
            terminal::Clear(terminal::ClearType::CurrentLine)
        )
    });
    // Park the cursor even when a row write failed: the caller's prompt is
    // drawn next and must not land inside the erased region.
    let park = execute!(stdout, cursor::MoveTo(0, start_row));
    stdout.flush()?;
    result.and(park)
}

/// Scroll the screen up by `lines` rows so the popup has room below the
/// prompt.
///
/// Moving off the last row pushes the top rows into the terminal's scrollback
/// buffer instead of destroying them, which is what `Viewport::Inline` used to
/// do via `append_lines`. Erasing the rows in place would permanently lose
/// command output, so scrolling is the only non-destructive way to make room.
///
/// Plain `\n` bytes are used deliberately: `MoveToNextLine` (`ESC[1E`) is a
/// cursor motion, so on the last row it does nothing. Only a newline at the
/// bottom margin triggers the terminal's scroll, moving the top rows into
/// scrollback.
pub(crate) fn scroll_up(lines: u16, rows: u16) -> io::Result<()> {
    if lines == 0 {
        return Ok(());
    }
    let mut stdout = io::stdout();
    execute!(stdout, cursor::MoveTo(0, rows.saturating_sub(1)))?;
    for _ in 0..lines {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}

/// Resolve the popup placement as `(scroll_lines, start_row, height)`.
///
/// The popup is docked at the anchor row when the remaining rows below can
/// hold it; otherwise the screen is scrolled up by exactly the shortfall so
/// the full height still fits and the rows above move into scrollback instead
/// of being erased.
///
/// A failed cursor query (spawned PTYs, non-answering multiplexers) docks the
/// popup at the bottom rows rather than anchoring at row 0, so the caller's
/// pre-clear cannot wipe the whole screen.
pub(crate) fn fit_popup(anchor_row: u16, rows: u16, height: u16) -> (u16, u16, u16) {
    let rows = rows.max(1);
    let height = height.clamp(1, rows);
    let anchor = anchor_row.min(rows - 1);
    let scroll = (anchor + height).saturating_sub(rows);
    (scroll, anchor - scroll, height)
}

/// Cursor row queried from the terminal, falling back to the bottom rows when
/// the DSR reply never arrives.
pub(crate) fn cursor_anchor_row(rows: u16, height: u16) -> u16 {
    let rows = rows.max(1);
    crossterm::cursor::position()
        .map(|(_, y)| y.min(rows - 1))
        .unwrap_or_else(|_| rows.saturating_sub(height.clamp(1, rows)))
}

pub(crate) fn open_inline_terminal(height: u16) -> io::Result<InlineTerminal> {
    // Reserve space by scrolling instead of switching to `Viewport::Inline`.
    // Inline's `append_lines` did scroll (freeing the popup from the
    // cursor-restore bug only after the fact), but `Terminal::clear()` then
    // restored the cursor to the bottom of the popup and the next `read_line`
    // redrew the prompt several rows lower, leaving blank lines above it.
    // Here the reservation and the cleanup are both explicit.
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let anchor_row = cursor_anchor_row(rows, height);
    let (scroll, start_row, clamped_height) = fit_popup(anchor_row, rows, height);
    scroll_up(scroll, rows)?;
    // Blank exactly the rows the popup owns so stale terminal content does not
    // bleed through the gaps between its cells. Bounded to the popup: content
    // below stays intact, content above was scrolled into scrollback.
    clear_rows(start_row, clamped_height)?;
    let area = Rect::new(0, start_row, cols, clamped_height);
    let terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(area),
        },
    )?;
    Ok(InlineTerminal {
        terminal,
        start_row,
        height: clamped_height,
        anchor_row: start_row,
    })
}

#[cfg(test)]
mod geometry_tests {
    use super::fit_popup;

    /// A popup that fits below the anchor needs no scroll and keeps its row.
    #[test]
    fn popup_fits_without_scrolling() {
        let (scroll, start_row, height) = fit_popup(5, 24, 8);
        assert_eq!((scroll, start_row, height), (0, 5, 8));
    }

    /// A popup on the last row scrolls the screen instead of shrinking: the
    /// caller gets the full height and the rows above move to scrollback.
    #[test]
    fn popup_scrolls_instead_of_shrinking() {
        let (scroll, start_row, height) = fit_popup(23, 24, 10);
        assert_eq!(height, 10, "full height must be preserved");
        assert_eq!(scroll, 9, "shortfall must be scrolled out");
        assert_eq!(start_row, 14, "popup must be docked above the bottom");
        assert!(start_row + height <= 24);
    }

    /// The scrolled-out rows are exactly the shortfall, so content that still
    /// fits is never moved unnecessarily.
    #[test]
    fn scroll_amount_equals_shortfall() {
        let (scroll, start_row, height) = fit_popup(20, 24, 10);
        assert_eq!(scroll, 6);
        assert_eq!(start_row, 14);
        assert_eq!(height, 10);
    }

    /// A popup taller than the terminal is clamped; it then scrolls by
    /// whatever the clamped height still cannot fit below the anchor.
    #[test]
    fn popup_taller_than_terminal_is_clamped() {
        let (scroll, start_row, height) = fit_popup(3, 10, 40);
        assert_eq!(height, 10, "height is clamped to the screen");
        assert_eq!(scroll, 3, "shortfall is scrolled out");
        assert_eq!(start_row, 0);
    }

    /// A popup taller than the terminal anchored at the top needs no scroll.
    #[test]
    fn clamped_popup_at_top_does_not_scroll() {
        let (scroll, start_row, height) = fit_popup(0, 10, 40);
        assert_eq!((scroll, start_row, height), (0, 0, 10));
    }

    /// A degenerate zero-row terminal must not panic or underflow.
    #[test]
    fn zero_height_terminal_yields_one_row() {
        let (scroll, start_row, height) = fit_popup(0, 0, 8);
        assert_eq!(height, 1);
        assert_eq!(scroll, 0);
        assert_eq!(start_row, 0);
    }

    /// A height-1 popup on the last row scrolls exactly one row.
    #[test]
    fn single_row_popup_at_bottom_scrolls_one() {
        let (scroll, start_row, height) = fit_popup(23, 24, 1);
        assert_eq!((scroll, start_row, height), (0, 23, 1));
    }

    /// Including the last row: anchor + height must never exceed the screen.
    #[test]
    fn popup_never_overflows_footer() {
        for rows in 1..=60u16 {
            for height in 1..=12u16 {
                for anchor in 0..rows {
                    let (scroll, start_row, h) = fit_popup(anchor, rows, height);
                    assert_eq!(h, height.min(rows));
                    assert_eq!(
                        start_row + scroll,
                        anchor.min(rows - 1),
                        "scroll must exactly compensate the shift"
                    );
                    assert!(
                        start_row + h <= rows,
                        "rows={rows} height={height} anchor={anchor} -> {start_row}+{h}"
                    );
                }
            }
        }
    }

    /// Re-anchoring at the post-scroll row is idempotent: a second fit at the
    /// same terminal height must not scroll again. Anchoring at the pre-scroll
    /// row instead would keep scrolling on every resize event.
    #[test]
    fn refit_at_post_scroll_anchor_scrolls_once() {
        let height = 9u16;
        let rows = 24u16;
        let anchor = 22u16;

        let (scroll, start_row, _) = fit_popup(anchor, rows, height);
        assert_eq!(scroll, 7);

        // Second fit from the post-scroll row: nothing left to scroll.
        let (scroll2, start_row2, _) = fit_popup(start_row, rows, height);
        assert_eq!(scroll2, 0, "re-anchored refit must not scroll again");
        assert_eq!(start_row2, start_row);

        // Anchoring at the original row would have scrolled all over again.
        let (scroll3, _, _) = fit_popup(anchor, rows, height);
        assert_eq!(scroll3, 7, "stale anchor reproduces the bug");
    }

    /// Shrinking then growing back must not drift the popup upward: each step
    /// re-anchors, so the popup returns to the same fitted row.
    #[test]
    fn resize_cycle_returns_to_same_row() {
        let height = 9u16;
        let mut anchor = 15u16;

        let (scroll, row, _) = fit_popup(anchor, 20, height);
        assert_eq!(scroll, 4);
        anchor = row;
        assert_eq!(anchor, 11);

        let (scroll_up_back, row2, _) = fit_popup(anchor, 24, height);
        assert_eq!(scroll_up_back, 0, "growing back needs no scroll");
        assert_eq!(row2, 11, "popup stays where the scroll left it");
    }
}

pub(crate) fn longest_common_prefix(names: &[&str]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut prefix = String::new();
    for (idx, ch) in names[0].chars().enumerate() {
        if names[1..]
            .iter()
            .all(|name| name.chars().nth(idx) == Some(ch))
        {
            prefix.push(ch);
        } else {
            break;
        }
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn entry(name: &str, group_index: usize) -> SlashCommandEntry {
        SlashCommandEntry {
            name: name.to_string(),
            desc: format!("{name} description"),
            keywords: Vec::new(),
            group_index,
            fill_only: false,
            status: None,
        }
    }

    fn fill_entry(name: &str, group_index: usize) -> SlashCommandEntry {
        let mut e = entry(name, group_index);
        e.fill_only = true;
        e
    }

    fn disabled_entry(name: &str, group_index: usize, reason: &str) -> SlashCommandEntry {
        let mut e = entry(name, group_index);
        e.status = Some(SlashCommandStatus {
            enabled: false,
            status_text: String::new(),
            reason: reason.to_string(),
        });
        e
    }

    fn sample_commands() -> Vec<SlashCommandEntry> {
        vec![
            entry("/help", 0),
            fill_entry("/model", 1),
            fill_entry("/quit", 1),
        ]
    }

    fn all_slash_commands() -> Vec<SlashCommandEntry> {
        vec![
            entry("/help", 0),
            entry("/model", 1),
            entry("/setup", 1),
            entry("/setting", 1),
            entry("/plan", 2),
            entry("/token", 3),
            fill_entry("/resume", 4),
            entry("/feedback", 5),
            entry("/record", 5),
            entry("/quit", 6),
            entry("/doctor", 7),
            entry("/status", 7),
        ]
    }

    fn session_with_commands(input: &str, commands: &[SlashCommandEntry]) -> SlashInputSession {
        let mut session = SlashInputSession::new(commands.to_vec(), Vec::new(), "aish> ".into());
        session.input = input.to_string();
        session.cursor = input.len();
        session.update_filtered();
        session
    }

    fn session_with_input(input: &str) -> SlashInputSession {
        session_with_commands(input, &sample_commands())
    }

    #[test]
    fn a_name_match_outranks_another_command_keyword_match() {
        // Regression: typing `/report` used to select `/feedback`, because
        // `/feedback` carries a "report" keyword and `fuzzy_score` rewards
        // shorter targets, so that keyword hit (192 on "report") beat the real
        // name hit (191 on "/report"). A command's own name must win.
        let mut report = entry("/report", 5);
        report.keywords = vec!["report".into(), "html".into(), "artifact".into()];
        let mut feedback = entry("/feedback", 5);
        feedback.keywords = vec!["feedback".into(), "report".into(), "issue".into()];

        for order in [
            vec![report.clone(), feedback.clone()],
            vec![feedback.clone(), report.clone()],
        ] {
            let session = session_with_commands("/report", &order);
            assert_eq!(
                session.selected_entry().map(|e| e.name.as_str()),
                Some("/report"),
                "ranking must not depend on registration order"
            );
        }
    }

    #[test]
    fn a_description_match_ranks_below_name_and_keyword_matches() {
        // Three tiers: name > keyword > description. Otherwise a command whose
        // prose happens to contain the word would tie with the real command.
        let mut by_name = entry("/report", 0);
        by_name.keywords = vec!["report".into()];
        let mut by_keyword = entry("/feedback", 0);
        by_keyword.keywords = vec!["report".into()];
        let mut by_desc = entry("/xyz", 0);
        by_desc.desc = "report".to_string();

        let session = session_with_commands("/report", &[by_desc, by_keyword, by_name]);
        let order: Vec<&str> = session
            .filtered
            .iter()
            .map(|i| session.entries[*i].name.as_str())
            .collect();
        assert_eq!(order, vec!["/report", "/feedback", "/xyz"]);
    }

    #[test]
    fn enter_on_slash_alone_executes_highlighted_command() {
        let mut session = session_with_input("/");
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Command("/help".into()))
        );
    }

    #[test]
    fn enter_on_partial_prefix_executes_highlighted_command() {
        let mut session = session_with_input("/hel");
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Command("/help".into()))
        );
    }

    #[test]
    fn enter_on_exact_fill_command_fills() {
        let mut session = session_with_input("/quit");
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Fill("/quit".into()))
        );
    }

    #[test]
    fn enter_on_ambiguous_fill_prefix_fills_highlighted() {
        let commands = all_slash_commands();
        let mut session = session_with_commands("/r", &commands);
        // /resume sorts before /record within group 4 and is fill-only.
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Fill("/resume".into()))
        );
    }

    #[test]
    fn tab_completes_and_dismisses_popup() {
        let mut session = session_with_input("/mod");
        assert_eq!(
            session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/model ".into()))
        );
    }

    #[test]
    fn tab_on_slash_completes_highlighted_and_dismisses() {
        let mut session = session_with_input("/");
        assert_eq!(
            session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/help ".into()))
        );
    }

    #[test]
    fn tab_at_shared_prefix_completes_highlighted() {
        let commands = all_slash_commands();
        let mut session = session_with_commands("/re", &commands);
        assert_eq!(
            session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/resume ".into()))
        );
    }

    #[test]
    fn trailing_space_after_exact_command_hides_suggestion_list() {
        let session = session_with_input("/help ");
        assert!(session.filtered.is_empty());
    }

    #[test]
    fn enter_on_command_with_trailing_space_executes() {
        let mut session = session_with_input("/help ");
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Command("/help".into()))
        );
    }

    #[test]
    fn typing_args_after_command_dismisses_to_readline() {
        let mut session = session_with_input("/help ");
        assert_eq!(
            session.handle_event(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/help x".into()))
        );
    }

    #[test]
    fn tab_completes_unambiguous_prefixes() {
        let commands = all_slash_commands();
        let cases = [
            ("/hel", "/help "),
            ("/mod", "/model "),
            ("/tok", "/token "),
            ("/doc", "/doctor "),
            ("/stat", "/status "),
            ("/rec", "/record "),
            ("/qui", "/quit "),
        ];
        for (prefix, expected) in cases {
            let mut session = session_with_commands(prefix, &commands);
            assert_eq!(
                session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
                Some(SlashInputOutcome::Dismissed(expected.into())),
                "Tab on {prefix}",
            );
        }
    }

    #[test]
    fn enter_executes_each_exact_command() {
        let commands = all_slash_commands();
        for c in &commands {
            let mut session = session_with_commands(&c.name, &commands);
            let expected = if c.fill_only {
                SlashInputOutcome::Fill(c.name.clone())
            } else {
                SlashInputOutcome::Command(c.name.clone())
            };
            assert_eq!(
                session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
                Some(expected),
                "Enter on {}",
                c.name,
            );
        }
    }

    #[test]
    fn trailing_space_hides_list_for_each_command() {
        let commands = all_slash_commands();
        for c in &commands {
            let input = format!("{} ", c.name);
            let session = session_with_commands(&input, &commands);
            assert!(
                session.filtered.is_empty(),
                "list should hide for {input:?}",
            );
        }
    }

    /// Regression: typing `/se`, navigating Down to `/setting`, then pressing
    /// Enter must execute `/setting` — not dismiss with the partial `/se`.
    #[test]
    fn enter_after_down_arrow_executes_selected_command() {
        let commands = all_slash_commands();
        let mut session = session_with_commands("/se", &commands);
        // Fuzzy ranks /setting first for "/se".
        session.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Command("/setting".into()))
        );
    }

    #[test]
    fn tab_ambiguous_prefix_extends_common_prefix() {
        let commands = all_slash_commands();
        let mut session = session_with_commands("/r", &commands);
        assert!(session
            .handle_event(key(KeyCode::Tab, KeyModifiers::NONE))
            .is_none());
        assert_eq!(session.input, "/re");
    }

    #[test]
    fn longest_common_prefix_shared_by_record_and_resume() {
        assert_eq!(longest_common_prefix(&["/record", "/resume"]), "/re");
    }

    #[test]
    fn backspace_to_empty_cancels_to_readline() {
        let mut session = session_with_input("/");
        assert_eq!(
            session.handle_event(key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Cancelled(String::new()))
        );
    }

    #[test]
    fn esc_cancels_with_input_for_restore() {
        let mut session = session_with_input("/hel");
        assert_eq!(
            session.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Cancelled("/hel".into()))
        );
    }

    #[test]
    fn ctrl_c_cancels_with_empty_restore() {
        let mut session = session_with_input("/hel");
        assert_eq!(
            session.handle_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(SlashInputOutcome::Cancelled(String::new()))
        );
    }

    #[test]
    fn disabled_command_enter_is_noop() {
        let commands = vec![disabled_entry("/live", 0, "daemon off"), entry("/help", 0)];
        let mut session = session_with_commands("/live", &commands);
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        // Still open: dismissing requires Esc.
        assert_eq!(
            session.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Cancelled("/live".into()))
        );
    }

    #[test]
    fn group_headers_render_between_groups() {
        let commands = vec![entry("/alpha", 0), entry("/beta", 1), entry("/gamma", 1)];
        let mut session =
            SlashInputSession::new(commands, vec!["G0".into(), "G1".into()], "aish> ".into());
        session.input = "/".into();
        session.cursor = 1;
        session.update_filtered();
        let rows = session.display_rows();
        assert_eq!(
            rows,
            vec![
                DisplayRow::Header(0),
                DisplayRow::Command(0),
                DisplayRow::Header(1),
                DisplayRow::Command(1),
                DisplayRow::Command(2),
            ]
        );
    }

    #[test]
    fn description_query_matches_command() {
        let mut help = entry("/help", 0);
        help.desc = "Show help information".into();
        let commands = vec![help, entry("/model", 1)];
        let session = session_with_commands("/information", &commands);
        assert_eq!(
            session
                .filtered
                .first()
                .map(|&i| session.entries[i].name.as_str()),
            Some("/help")
        );
    }

    #[test]
    fn keyword_query_matches_command() {
        let mut doctor = entry("/doctor", 0);
        doctor.keywords = vec!["diagnose".into()];
        // new() sorts by (group, name): /doctor precedes /help.
        let commands = vec![doctor, entry("/help", 0)];
        let session = session_with_commands("/diagnose", &commands);
        assert_eq!(session.filtered, vec![0]);
    }

    #[test]
    fn fuzzy_ranks_prefix_above_scattered_subsequence() {
        let commands = all_slash_commands();
        let session = session_with_commands("/se", &commands);
        // /setting and /setup match; the word-initial /setting wins.
        assert_eq!(
            session.filtered.first().map(|&i| commands[i].name.as_str()),
            Some("/setting")
        );
    }

    #[test]
    fn display_rows_render_each_group_header_once() {
        // Score order may interleave groups; each header renders exactly
        // once at the group's first occurrence — never repeated mid-list.
        let mut g0 = entry("/aaa", 0);
        g0.desc = "zzz match".into();
        let g0b = entry("/bbb", 0);
        let mut g1 = entry("/zzz", 1);
        g1.desc = "aaa match".into();
        let commands = vec![g0, g1, g0b];
        let mut session =
            SlashInputSession::new(commands, vec!["G0".into(), "G1".into()], "aish> ".into());
        // Score order interleaves non-adjacent rows of the same group:
        // g0, g1, then g0 again — the old last-group check re-rendered
        // Header(0) at the tail. new() sorted entries, so /bbb is index 1.
        session.input = "/match".into();
        session.cursor = session.input.len();
        session.filtered = vec![0, 2, 1];
        let rows = session.display_rows();
        assert_eq!(
            rows,
            vec![
                DisplayRow::Header(0),
                DisplayRow::Command(0),
                DisplayRow::Header(1),
                DisplayRow::Command(2),
                DisplayRow::Command(1),
            ]
        );
    }

    #[test]
    fn arrow_keys_move_highlight_without_changing_input() {
        let mut session = session_with_input("/");
        session.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(session.input, "/");
        assert_eq!(
            session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/model ".into()))
        );
    }

    #[test]
    fn tab_completes_highlighted_after_down_arrow() {
        let mut session = session_with_input("/");
        session.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/model ".into()))
        );
    }

    #[test]
    fn double_tab_extends_then_completes_highlighted() {
        let commands = all_slash_commands();
        let mut session = session_with_commands("/r", &commands);
        assert!(session
            .handle_event(key(KeyCode::Tab, KeyModifiers::NONE))
            .is_none());
        assert_eq!(session.input, "/re");
        assert_eq!(
            session.handle_event(key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/resume ".into()))
        );
    }
}
