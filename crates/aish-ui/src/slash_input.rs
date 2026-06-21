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

    /// Override input after construction (for integration tests).
    #[doc(hidden)]
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
        let query = self.command_query().to_lowercase();
        if query.is_empty() || !query.starts_with('/') {
            self.filtered.clear();
            self.selected = 0;
            return;
        }
        self.filtered = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| name.to_lowercase().starts_with(&query))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
        self.clamp_selected();
    }

    fn matching_name_count(&self) -> usize {
        let query = self.command_query().to_lowercase();
        self.commands
            .iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&query))
            .count()
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

    /// Whether the current input is still a prefix of any command name.
    fn has_command_prefix(&self) -> bool {
        let query = self.command_query().to_lowercase();
        if query.is_empty() || !query.starts_with('/') {
            return false;
        }
        self.commands
            .iter()
            .any(|(name, _)| name.to_lowercase().starts_with(&query))
    }

    /// True when input is an exact command followed by a space (Tab completion or typed).
    /// Hide the suggestion list so Enter does not pick a different row.
    fn should_hide_command_list(&self) -> bool {
        let Some(space_idx) = self.input.find(' ') else {
            return false;
        };
        let command_name = &self.input[..space_idx];
        self.commands.iter().any(|(name, _)| name == command_name)
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

    /// Name of the currently highlighted command in the filtered list.
    fn selected_command_name(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .map(|&idx| self.commands[idx].0.as_str())
    }

    /// Enter: exact command match executes; otherwise accept the highlighted item.
    fn handle_submit(&mut self) -> Option<SlashInputOutcome> {
        let trimmed = self.input.trim();
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        let exact_match = self
            .commands
            .iter()
            .any(|(name, _)| first_word == name || trimmed == name);
        if exact_match {
            return Some(SlashInputOutcome::Command(trimmed.to_string()));
        }
        if !self.filtered.is_empty() {
            let query = self.command_query();
            let match_count = self.matching_name_count();
            // Ambiguous prefix (e.g. /r): return to readline instead of guessing.
            if match_count > 1 && query != "/" {
                return Some(SlashInputOutcome::Dismissed(self.input.clone()));
            }
            let Some(command) = self.selected_command_name().map(str::to_string) else {
                return Some(SlashInputOutcome::Dismissed(self.input.clone()));
            };
            return Some(SlashInputOutcome::Command(command));
        }
        Some(SlashInputOutcome::Dismissed(self.input.clone()))
    }

    /// Tab: extend shared prefix, or complete the highlighted command and dismiss.
    fn handle_tab_complete(&mut self) -> Option<SlashInputOutcome> {
        if self.filtered.is_empty() {
            return None;
        }
        if self.filtered.len() == 1 {
            let command = self.selected_command_name()?.to_string();
            let completed = Self::format_command_with_trailing_space(&command);
            return Some(SlashInputOutcome::Dismissed(completed));
        }

        let names: Vec<&str> = self
            .filtered
            .iter()
            .map(|&idx| self.commands[idx].0.as_str())
            .collect();
        let lcp = longest_common_prefix(&names);
        let query = self.command_query();
        if lcp.len() > query.len() {
            self.replace_command_token(&lcp);
            self.update_filtered();
            return None;
        }

        let command = self.selected_command_name()?.to_string();
        let completed = Self::format_command_with_trailing_space(&command);
        Some(SlashInputOutcome::Dismissed(completed))
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

fn open_inline_terminal(height: u16) -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
}

fn longest_common_prefix(names: &[&str]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn sample_commands() -> Vec<(String, String)> {
        vec![
            ("/help".into(), "Show help".into()),
            ("/model".into(), "Switch model".into()),
            ("/quit".into(), "Exit".into()),
        ]
    }

    fn all_slash_commands() -> Vec<(String, String)> {
        vec![
            ("/help".into(), "Show help information".into()),
            ("/model".into(), "Show or switch AI model".into()),
            ("/setup".into(), "Open setup wizard".into()),
            ("/plan".into(), "Plan mode control".into()),
            ("/token".into(), "Show token usage".into()),
            ("/resume".into(), "Resume previous session".into()),
            ("/feedback".into(), "Submit feedback".into()),
            (
                "/record".into(),
                "Record terminal session (start/stop)".into(),
            ),
            ("/quit".into(), "Exit AI Shell".into()),
            ("/doctor".into(), "Run system diagnostics".into()),
            ("/status".into(), "Show system environment status".into()),
        ]
    }

    fn session_with_commands(input: &str, commands: &[(String, String)]) -> SlashInputSession {
        let mut session = SlashInputSession::new(commands.to_vec(), "aish> ".into());
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
    fn enter_on_exact_command_executes_as_typed() {
        let mut session = session_with_input("/quit");
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Command("/quit".into()))
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
        // /resume precedes /record in SLASH_COMMANDS; selected defaults to 0.
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
        for (name, _) in &commands {
            let mut session = session_with_commands(name, &commands);
            assert_eq!(
                session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
                Some(SlashInputOutcome::Command(name.clone())),
                "Enter on {name}",
            );
        }
    }

    #[test]
    fn trailing_space_hides_list_for_each_command() {
        let commands = all_slash_commands();
        for (name, _) in &commands {
            let input = format!("{name} ");
            let session = session_with_commands(&input, &commands);
            assert!(
                session.filtered.is_empty(),
                "list should hide for {input:?}",
            );
        }
    }

    #[test]
    fn ambiguous_prefix_enter_dismisses_to_readline() {
        let commands = all_slash_commands();
        let mut session = session_with_commands("/r", &commands);
        assert_eq!(
            session.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SlashInputOutcome::Dismissed("/r".into()))
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
            Some(SlashInputOutcome::Cancelled)
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

    #[test]
    fn description_only_query_does_not_match() {
        let commands = all_slash_commands();
        let session = session_with_commands("/show", &commands);
        assert!(session.filtered.is_empty());
    }
}
