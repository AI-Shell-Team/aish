//! Security confirmation dialog component for ratatui.
//!
//! Provides a dialog component for displaying security warnings and
//! collecting user decisions (Allow, Confirm, Cancel).

use crossterm::event::{KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Action taken by the user in a security dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    /// No action taken (navigation only).
    None,
    /// User chose to allow the operation.
    Allow,
    /// User chose to confirm with additional verification.
    Confirm,
    /// User cancelled or dismissed the dialog.
    Cancel,
}

/// Security confirmation dialog component.
///
/// Displays security information including:
/// - Tool name and target
/// - Warning message
/// - Risk level badge (colored)
/// - List of reasons
/// - Suggested alternatives (if any)
/// - Action buttons
pub struct SecurityDialog {
    /// Name of the tool/operation being executed.
    pub tool_name: String,
    /// Target of the operation (file, command, etc.).
    pub target: String,
    /// Main warning message.
    pub message: String,
    /// Risk level indicator (low, medium, high, critical).
    pub risk_level: Option<String>,
    /// List of reasons for the security warning.
    pub reasons: Vec<String>,
    /// Suggested alternative approaches.
    pub alternatives: Vec<String>,
    /// Currently selected button index (0=Allow, 1=Confirm, 2=Cancel).
    selected_button: usize,
}

impl SecurityDialog {
    /// Create a new security dialog.
    ///
    /// # Arguments
    ///
    /// * `tool_name` - Name of the tool/operation
    /// * `target` - Target of the operation
    /// * `message` - Main warning message
    /// * `risk_level` - Optional risk level indicator
    pub fn new(
        tool_name: &str,
        target: &str,
        message: &str,
        risk_level: Option<&str>,
    ) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            target: target.to_string(),
            message: message.to_string(),
            risk_level: risk_level.map(|s| s.to_string()),
            reasons: Vec::new(),
            alternatives: Vec::new(),
            selected_button: 0, // Default to Allow
        }
    }

    /// Set the reasons for this security warning.
    pub fn with_reasons(mut self, reasons: Vec<String>) -> Self {
        self.reasons = reasons;
        self
    }

    /// Set the alternative approaches for this warning.
    pub fn with_alternatives(mut self, alternatives: Vec<String>) -> Self {
        self.alternatives = alternatives;
        self
    }

    /// Get the current selected button index.
    pub fn selected_button(&self) -> usize {
        self.selected_button
    }

    /// Handle a key event and return the corresponding action.
    ///
    /// Supported keys:
    /// - Tab/Right Arrow: Move to next button
    /// - Shift+Tab/Left Arrow: Move to previous button
    /// - Enter: Select current button
    /// - Escape: Cancel
    pub fn handle_key(&mut self, key: KeyEvent) -> DialogAction {
        // Ignore key release and repeat events
        if key.kind != KeyEventKind::Press {
            return DialogAction::None;
        }

        match key.code {
            crossterm::event::KeyCode::Tab
            | crossterm::event::KeyCode::Right => {
                self.selected_button = (self.selected_button + 1) % 3;
                DialogAction::None
            }
            crossterm::event::KeyCode::BackTab
            | crossterm::event::KeyCode::Left => {
                self.selected_button = if self.selected_button == 0 {
                    2
                } else {
                    self.selected_button - 1
                };
                DialogAction::None
            }
            crossterm::event::KeyCode::Enter => match self.selected_button {
                0 => DialogAction::Allow,
                1 => DialogAction::Confirm,
                2 => DialogAction::Cancel,
                _ => DialogAction::None,
            },
            crossterm::event::KeyCode::Esc => DialogAction::Cancel,
            _ => DialogAction::None,
        }
    }

    /// Render the dialog to the given frame area.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Calculate risk level color
        let risk_color = self.risk_level_color();

        // Build title with risk badge
        let title = if let Some(risk) = &self.risk_level {
            format!("{}: {} [{:?}]", self.tool_name, self.target, risk)
        } else {
            format!("{}: {}", self.tool_name, self.target)
        };

        // Create main layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3),  // Title + risk badge
                Constraint::Min(3),     // Message
                Constraint::Min(2),     // Reasons (if any)
                Constraint::Min(2),     // Alternatives (if any)
                Constraint::Length(3),  // Buttons
            ])
            .split(area);

        // Render title block with risk badge
        self.render_title(frame, chunks[0], &title, risk_color);

        // Render message
        self.render_message(frame, chunks[1]);

        // Render reasons if present
        if !self.reasons.is_empty() {
            self.render_reasons(frame, chunks[2]);
        }

        // Render alternatives if present
        if !self.alternatives.is_empty() {
            self.render_alternatives(frame, chunks[3]);
        }

        // Render buttons
        self.render_buttons(frame, area);
    }

    fn risk_level_color(&self) -> Color {
        match self.risk_level.as_deref() {
            Some("low") => Color::Green,
            Some("medium") | Some("moderate") => Color::Yellow,
            Some("high") => Color::Red,
            Some("critical") => Color::Magenta,
            _ => Color::Gray,
        }
    }

    fn render_title(&self, frame: &mut Frame, area: Rect, title: &str, risk_color: Color) {
        let title_style = Style::default()
            .fg(risk_color)
            .add_modifier(Modifier::BOLD);

        let block = Block::default()
            .title(title)
            .title_style(title_style)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(block, area);
    }

    fn render_message(&self, frame: &mut Frame, area: Rect) {
        let text = Text::from(self.message.as_str());
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Color::White));

        frame.render_widget(paragraph, area);
    }

    fn render_reasons(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled("Reasons:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]));

        for reason in &self.reasons {
            lines.push(Line::from(vec![
                Span::raw("  • "),
                Span::styled(reason, Style::default().fg(Color::White)),
            ]));
        }

        let text: Text = lines.into();
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .style(Style::default());

        frame.render_widget(paragraph, area);
    }

    fn render_alternatives(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled("Alternatives:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]));

        for alt in &self.alternatives {
            lines.push(Line::from(vec![
                Span::raw("  → "),
                Span::styled(alt, Style::default().fg(Color::White)),
            ]));
        }

        let text: Text = lines.into();
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .style(Style::default());

        frame.render_widget(paragraph, area);
    }

    fn render_buttons(&self, frame: &mut Frame, area: Rect) {
        // Calculate button area at bottom
        let button_area = Rect {
            x: area.x + 2,
            y: area.bottom() - 5,
            width: area.width - 4,
            height: 3,
        };

        let buttons = ["Allow", "Confirm", "Cancel"];
        let total_width: u16 = buttons.iter().map(|b| b.len() as u16 + 4).sum();
        let spacing = (button_area.width - total_width) / 2;

        let mut x = button_area.x + spacing;
        let y = button_area.y + 1;

        for (i, button) in buttons.iter().enumerate() {
            let is_selected = i == self.selected_button;
            let button_width = button.len() as u16 + 4;

            let style = if is_selected {
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Gray)
            };

            let button_text = format!(" {} ", button);
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
            x += button_width + 2;
        }

        // Render help text
        let help_text = "← →: Move | Enter: Select | Esc: Cancel";
        let help_area = Rect {
            x: area.x + 2,
            y: area.bottom() - 2,
            width: area.width - 4,
            height: 1,
        };

        let help_paragraph = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);

        frame.render_widget(help_paragraph, help_area);
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
    fn test_security_dialog_creation() {
        let dialog = SecurityDialog::new("bash", "rm /tmp/x", "Dangerous operation", Some("high"));

        assert_eq!(dialog.tool_name, "bash");
        assert_eq!(dialog.target, "rm /tmp/x");
        assert_eq!(dialog.message, "Dangerous operation");
        assert_eq!(dialog.risk_level, Some("high".to_string()));
        assert_eq!(dialog.selected_button, 0);
        assert!(dialog.reasons.is_empty());
        assert!(dialog.alternatives.is_empty());
    }

    #[test]
    fn test_security_dialog_with_reasons() {
        let dialog = SecurityDialog::new("rm", "/tmp/x", "message", Some("medium"))
            .with_reasons(vec![
                "File deletion detected".to_string(),
                "No backup available".to_string(),
            ]);

        assert_eq!(dialog.reasons.len(), 2);
        assert_eq!(dialog.reasons[0], "File deletion detected");
    }

    #[test]
    fn test_security_dialog_with_alternatives() {
        let dialog = SecurityDialog::new("rm", "/tmp/x", "message", None)
            .with_alternatives(vec![
                "Use trash instead".to_string(),
                "Create backup first".to_string(),
            ]);

        assert_eq!(dialog.alternatives.len(), 2);
    }

    #[test]
    fn test_handle_key_navigation() {
        let mut dialog = SecurityDialog::new("test", "target", "message", None);

        // Tab moves forward
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(action, DialogAction::None);
        assert_eq!(dialog.selected_button, 1);

        // Shift+Tab moves backward
        let action = dialog.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(action, DialogAction::None);
        assert_eq!(dialog.selected_button, 0);

        // Arrow keys
        let _action = dialog.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(dialog.selected_button, 1);

        let _action = dialog.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(dialog.selected_button, 0);
    }

    #[test]
    fn test_handle_key_selection() {
        let mut dialog = SecurityDialog::new("test", "target", "message", None);

        // Enter on Allow (button 0)
        dialog.selected_button = 0;
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, DialogAction::Allow);

        // Enter on Confirm (button 1)
        dialog.selected_button = 1;
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, DialogAction::Confirm);

        // Enter on Cancel (button 2)
        dialog.selected_button = 2;
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, DialogAction::Cancel);
    }

    #[test]
    fn test_handle_key_escape() {
        let mut dialog = SecurityDialog::new("test", "target", "message", None);

        let action = dialog.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, DialogAction::Cancel);
    }

    #[test]
    fn test_handle_key_ignore_repeat() {
        let mut dialog = SecurityDialog::new("test", "target", "message", None);

        // Create a key event with kind=Repeat (should be ignored)
        let mut key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        key.kind = KeyEventKind::Repeat;

        let action = dialog.handle_key(key);
        assert_eq!(action, DialogAction::None);
        assert_eq!(dialog.selected_button, 0); // Should not change
    }

    #[test]
    fn test_risk_level_colors() {
        let dialog = SecurityDialog::new("test", "target", "message", Some("low"));
        assert_eq!(dialog.risk_level_color(), Color::Green);

        let dialog = SecurityDialog::new("test", "target", "message", Some("medium"));
        assert_eq!(dialog.risk_level_color(), Color::Yellow);

        let dialog = SecurityDialog::new("test", "target", "message", Some("high"));
        assert_eq!(dialog.risk_level_color(), Color::Red);

        let dialog = SecurityDialog::new("test", "target", "message", Some("critical"));
        assert_eq!(dialog.risk_level_color(), Color::Magenta);

        let dialog = SecurityDialog::new("test", "target", "message", None);
        assert_eq!(dialog.risk_level_color(), Color::Gray);
    }

    #[test]
    fn test_dialog_action_equality() {
        assert_eq!(DialogAction::Allow, DialogAction::Allow);
        assert_eq!(DialogAction::Confirm, DialogAction::Confirm);
        assert_eq!(DialogAction::Cancel, DialogAction::Cancel);
        assert_eq!(DialogAction::None, DialogAction::None);

        assert_ne!(DialogAction::Allow, DialogAction::Confirm);
        assert_ne!(DialogAction::Cancel, DialogAction::None);
    }
}
