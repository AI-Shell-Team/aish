//! Secret detection dialog component for ratatui.
//!
//! Provides a dialog for handling detected secrets with three options:
//! Redact (default/safest), Allow, or Abort.

use crossterm::event::{KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Text,
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Choice returned by the secret detection dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretDialogChoice {
    /// Replace secrets with environment variable placeholders.
    Redact,
    /// Allow sending original plaintext to AI.
    Allow,
    /// Abort the operation.
    Abort,
}

/// Secret detection dialog component.
///
/// Displays a warning about detected secrets and offers three choices:
/// - Redact: Replace secrets with placeholders (safest, default)
/// - Allow: Send original content as-is
/// - Abort: Cancel the operation
pub struct SecretDialog {
    /// Dialog title.
    pub title: String,
    /// Warning message to display.
    pub message: String,
    /// Currently selected option (0=Redact, 1=Allow, 2=Abort).
    selected: usize,
}

impl SecretDialog {
    /// Create a new secret detection dialog.
    ///
    /// # Arguments
    ///
    /// * `title` - Dialog title (e.g., "Secret Detected")
    /// * `message` - Warning message describing the detected secret
    ///
    /// # Example
    ///
    /// ```
    /// use aish_shell::tui::SecretDialog;
    ///
    /// let dialog = SecretDialog::new(
    ///     "Secret Detected",
    ///     "Found API key in command: export API_KEY=sk-..."
    /// );
    /// ```
    pub fn new(title: &str, message: &str) -> Self {
        Self {
            title: title.to_string(),
            message: message.to_string(),
            selected: 0, // Default to Redact (safest option)
        }
    }

    /// Get the currently selected option.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Handle a key event, returning `Some(choice)` on definitive selection.
    ///
    /// Navigation keys (Tab, arrows) return `None`. Only Enter or Escape
    /// produce a final choice.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SecretDialogChoice> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match key.code {
            crossterm::event::KeyCode::Tab | crossterm::event::KeyCode::Right => {
                self.selected = (self.selected + 1) % 3;
                None
            }
            crossterm::event::KeyCode::BackTab | crossterm::event::KeyCode::Left => {
                self.selected = if self.selected == 0 { 2 } else { self.selected - 1 };
                None
            }
            crossterm::event::KeyCode::Enter => Some(match self.selected {
                0 => SecretDialogChoice::Redact,
                1 => SecretDialogChoice::Allow,
                _ => SecretDialogChoice::Abort,
            }),
            crossterm::event::KeyCode::Esc => Some(SecretDialogChoice::Abort),
            _ => None,
        }
    }

    /// Render the dialog to the given frame area.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Create main layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3),  // Title
                Constraint::Min(3),     // Message
                Constraint::Length(4),  // Options
                Constraint::Length(3),  // Help text
            ])
            .split(area);

        // Render title block
        self.render_title(frame, chunks[0]);

        // Render message
        self.render_message(frame, chunks[1]);

        // Render option buttons
        self.render_options(frame, chunks[2]);

        // Render help text
        self.render_help(frame, chunks[3]);
    }

    fn render_title(&self, frame: &mut Frame, area: Rect) {
        let title_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);

        let block = Block::default()
            .title(self.title.as_str())
            .title_style(title_style)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        frame.render_widget(block, area);
    }

    fn render_message(&self, frame: &mut Frame, area: Rect) {
        let text = Text::from(self.message.as_str());
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Color::White));

        frame.render_widget(paragraph, area);
    }

    fn render_options(&self, frame: &mut Frame, area: Rect) {
        let options = [
            ("Redact", "Replace with placeholders"),
            ("Allow", "Send original content"),
            ("Abort", "Cancel operation"),
        ];

        let total_width: u16 = options.iter().map(|(name, _)| name.len() as u16 + 4).sum();
        let spacing = (area.width - total_width) / 2;

        let mut x = area.x + spacing;
        let y = area.y + 1;

        for (i, (name, desc)) in options.iter().enumerate() {
            let is_selected = i == self.selected;
            let button_width = name.len() as u16 + 4;

            let style = if is_selected {
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Gray)
            };

            let button_text = format!(" {} ", name);
            let paragraph = Paragraph::new(button_text.as_str())
                .style(style)
                .alignment(Alignment::Center);

            let button_rect = Rect {
                x,
                y,
                width: button_width,
                height: 1,
            };

            frame.render_widget(paragraph, button_rect);

            // Render description below button
            let desc_paragraph = Paragraph::new(*desc)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);

            let desc_rect = Rect {
                x,
                y: y + 1,
                width: button_width,
                height: 1,
            };

            frame.render_widget(desc_paragraph, desc_rect);

            x += button_width + 2;
        }
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help_text = "← →: Move | Enter: Select | Esc: Abort";
        let paragraph = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);

        frame.render_widget(paragraph, area);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn test_secret_dialog_creation() {
        let dialog = SecretDialog::new(
            "Secret Detected",
            "Found API key in command"
        );

        assert_eq!(dialog.title, "Secret Detected");
        assert_eq!(dialog.message, "Found API key in command");
        assert_eq!(dialog.selected, 0); // Default to Redact
    }

    #[test]
    fn test_handle_key_navigation() {
        let mut dialog = SecretDialog::new("Secret", "message");

        // Tab moves forward
        dialog.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(dialog.selected, 1);

        dialog.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(dialog.selected, 2);

        // Wrap around
        dialog.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(dialog.selected, 0);

        // BackTab moves backward
        dialog.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(dialog.selected, 2);

        // Arrow keys
        dialog.selected = 0;
        dialog.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(dialog.selected, 1);

        dialog.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(dialog.selected, 0);
    }

    #[test]
    fn test_handle_key_selection() {
        let mut dialog = SecretDialog::new("Secret", "message");

        // Redact (option 0)
        dialog.selected = 0;
        let choice = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(choice, Some(SecretDialogChoice::Redact));

        // Allow (option 1)
        dialog.selected = 1;
        let choice = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(choice, Some(SecretDialogChoice::Allow));

        // Abort (option 2)
        dialog.selected = 2;
        let choice = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(choice, Some(SecretDialogChoice::Abort));
    }

    #[test]
    fn test_handle_key_escape() {
        let mut dialog = SecretDialog::new("Secret", "message");

        let choice = dialog.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(choice, Some(SecretDialogChoice::Abort));
    }

    #[test]
    fn test_handle_key_navigation_returns_none() {
        let mut dialog = SecretDialog::new("Secret", "message");

        assert_eq!(dialog.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), None);
        assert_eq!(dialog.selected, 1);

        assert_eq!(dialog.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)), None);
        assert_eq!(dialog.selected, 0);
    }

    #[test]
    fn test_handle_key_ignore_repeat() {
        let mut dialog = SecretDialog::new("Secret", "message");

        // Create a key event with kind=Repeat (should be ignored)
        let mut key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        key.kind = KeyEventKind::Repeat;

        dialog.handle_key(key);
        assert_eq!(dialog.selected, 0); // Should not change
    }

    #[test]
    fn test_secret_dialog_choice_equality() {
        assert_eq!(SecretDialogChoice::Redact, SecretDialogChoice::Redact);
        assert_eq!(SecretDialogChoice::Allow, SecretDialogChoice::Allow);
        assert_eq!(SecretDialogChoice::Abort, SecretDialogChoice::Abort);

        assert_ne!(SecretDialogChoice::Redact, SecretDialogChoice::Allow);
        assert_ne!(SecretDialogChoice::Allow, SecretDialogChoice::Abort);
        assert_ne!(SecretDialogChoice::Abort, SecretDialogChoice::Redact);
    }

    #[test]
    fn test_default_selection_is_redact() {
        let dialog = SecretDialog::new("Secret", "message");
        assert_eq!(dialog.selected(), 0); // Redact is safest and default
    }
}
