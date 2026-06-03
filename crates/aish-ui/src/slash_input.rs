use std::io::{self, Write};

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
    /// User selected a slash command (e.g. "/model gpt-4").
    Command(String),
    /// Input no longer matches any command; return to normal readline with this text.
    Dismissed(String),
    /// User pressed Esc; cancel and return to empty readline.
    Cancelled,
}

const MAX_VISIBLE_COMMANDS: usize = 8;
const MAX_PANEL_HEIGHT: u16 = 10;

/// Inline slash command autocomplete popup.
///
/// Renders the shell prompt + input on the first line, and a filtered
/// command list below it — no focus mode switching, typing and popup
/// navigation work simultaneously.
pub struct SlashInputSession {
    commands: Vec<(String, String)>,
    prompt: String,
    input: String,
    cursor: usize,
    selected: usize,
    filtered: Vec<usize>,
}

impl SlashInputSession {
    pub fn new(commands: Vec<(String, String)>, prompt: String) -> Self {
        // Strip ANSI escape codes — ratatui renders via its own style system
        let prompt = strip_ansi(&prompt);
        let mut filtered = Vec::with_capacity(commands.len());
        filtered.extend(0..commands.len());
        Self {
            commands,
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
        let backend = CrosstermBackend::new(io::stdout());
        let height = self.panel_height();
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;

        let outcome = loop {
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

    fn panel_height(&self) -> u16 {
        let items = self.commands.len().clamp(1, MAX_VISIBLE_COMMANDS) as u16;
        (1 + items).min(MAX_PANEL_HEIGHT)
    }

    fn update_filtered(&mut self) {
        let query = self.input.to_lowercase();
        self.filtered = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, (name, desc))| {
                name.to_lowercase().starts_with(&query) || desc.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        self.clamp_selected();
    }

    fn clamp_selected(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    /// Whether the current input is still a prefix of any command name.
    fn has_command_prefix(&self) -> bool {
        let query = self.input.to_lowercase();
        self.commands
            .iter()
            .any(|(name, _)| name.to_lowercase().starts_with(&query))
    }

    /// Set input text and cursor to the currently selected command name.
    fn sync_input_to_selected(&mut self) {
        if let Some(&cmd_idx) = self.filtered.get(self.selected) {
            let (name, _) = &self.commands[cmd_idx];
            self.input = name.clone();
            self.cursor = self.input.len();
        }
    }

    /// Handle an event. Returns `Some(outcome)` when the session should end.
    fn handle_event(&mut self, event: Event) -> Option<SlashInputOutcome> {
        let Event::Key(key) = event else { return None };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return None;
        }

        // Ctrl+C always cancels
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(SlashInputOutcome::Cancelled);
        }

        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
                self.update_filtered();
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
                        return Some(SlashInputOutcome::Cancelled);
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
                    self.sync_input_to_selected();
                }
                None
            }
            KeyCode::Down => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1).min(self.filtered.len() - 1);
                    self.sync_input_to_selected();
                }
                None
            }
            KeyCode::Enter => {
                let trimmed = self.input.trim();
                let first_word = trimmed.split_whitespace().next().unwrap_or("");
                let is_command = self
                    .commands
                    .iter()
                    .any(|(name, _)| first_word == name || trimmed == name);
                if is_command {
                    return Some(SlashInputOutcome::Command(trimmed.to_string()));
                }
                // No match — dismiss to normal readline
                Some(SlashInputOutcome::Dismissed(self.input.clone()))
            }
            KeyCode::Tab => {
                // Complete to selected item and submit immediately
                if let Some(&cmd_idx) = self.filtered.get(self.selected) {
                    let (name, _) = &self.commands[cmd_idx];
                    return Some(SlashInputOutcome::Command(name.clone()));
                }
                None
            }
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
            KeyCode::Esc => Some(SlashInputOutcome::Cancelled),
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

    fn render_command_list(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if self.filtered.is_empty() || area.height == 0 {
            return;
        }

        let max_visible = area.height as usize;
        let scroll = self.scroll_offset(max_visible);
        let end = (scroll + max_visible).min(self.filtered.len());
        let lines: Vec<Line> = self.filtered[scroll..end]
            .iter()
            .enumerate()
            .map(|(row, &cmd_idx)| {
                let absolute_row = scroll + row;
                let (name, desc) = &self.commands[cmd_idx];
                let is_selected = absolute_row == self.selected;
                let marker = if is_selected { "▸ " } else { "  " };
                let marker_style = if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let desc_style = Style::default().fg(Color::DarkGray);

                Line::from(vec![
                    Span::styled(marker, marker_style),
                    Span::styled(name, name_style),
                    Span::styled(format!("  {desc}"), desc_style),
                ])
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn scroll_offset(&self, max_visible: usize) -> usize {
        if self.selected >= max_visible {
            self.selected - max_visible + 1
        } else {
            0
        }
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
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

fn drain_pending_events() -> io::Result<()> {
    while event::poll(std::time::Duration::from_millis(0))? {
        let _ = event::read()?;
    }
    Ok(())
}

/// Strip ANSI CSI/OSC escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: skip until final byte (0x40..=0x7E)
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: skip until BEL or ST
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}
