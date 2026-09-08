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
        let name = fuzzy_score(query, &self.name);
        let desc = fuzzy_score(query, &self.desc);
        let kw = self
            .keywords
            .iter()
            .filter_map(|k| fuzzy_score(query, k))
            .max();
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
            let _ = terminal.autoresize();
            terminal.draw(|frame| self.render(frame, frame.area()))?;

            let event = event::read()?;
            if let Some(result) = self.handle_event(event) {
                break result;
            }
        };

        // All terminal cleanup MUST happen while still in raw mode.
        // If terminal is dropped after termios restore, its Drop impl may
        // write escape sequences in cooked mode, corrupting the terminal
        // and causing the next SlashInputSession invocation to hang.
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

pub(crate) fn open_inline_terminal(
    height: u16,
) -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
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
