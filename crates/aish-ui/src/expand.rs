use std::cell::Cell;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

use crate::{PanelComponent, PanelEvent};

/// A scrollable text viewer panel for displaying collapsed/expanded output.
pub struct ExpandPanel {
    lines: Vec<Line<'static>>,
    scroll_offset: u16,
    title: String,
    footer_hint: String,
    /// Inner area height saved during the last render, used for scroll clamping.
    last_visible_height: Cell<u16>,
}

impl ExpandPanel {
    pub fn new(title: impl Into<String>, content: &str) -> Self {
        Self::with_footer(title, content, " ↑↓/PgUp/PgDn scroll · Ctrl+O/Esc close ")
    }

    /// Create with a custom footer hint (for i18n).
    pub fn with_footer(
        title: impl Into<String>,
        content: &str,
        footer_hint: impl Into<String>,
    ) -> Self {
        let lines = content.lines().map(|l| Line::from(l.to_owned())).collect();
        Self {
            lines,
            scroll_offset: 0,
            title: title.into(),
            footer_hint: footer_hint.into(),
            last_visible_height: Cell::new(1),
        }
    }

    fn max_scroll(&self, visible_height: u16) -> u16 {
        let total = self.lines.len();
        let visible = visible_height as usize;
        let max = total.saturating_sub(visible);
        max.min(u16::MAX as usize) as u16
    }
}

impl PanelComponent for ExpandPanel {
    type Output = ();

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        // +2 for top/bottom borders; leave 3 rows for the shell prompt
        let needed = self.lines.len().saturating_add(2).min(u16::MAX as usize) as u16;
        let available = terminal_height.saturating_sub(3);
        let desired = needed.min(available);
        // When content would fill the panel exactly (max_scroll=0), the user
        // sees arrow-key scrolling as "not working".  Shrink the panel so at
        // least 3 content lines remain below the fold.
        if self.lines.len() > 5 {
            let max_with_scroll = self
                .lines
                .len()
                .saturating_sub(3)
                .saturating_add(2)
                .min(u16::MAX as usize) as u16;
            desired.min(max_with_scroll).max(1)
        } else {
            desired
        }
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let title_span = ratatui::text::Span::styled(
            format!(" {} ", self.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        let footer_span =
            ratatui::text::Span::styled(&self.footer_hint, Style::default().fg(Color::DarkGray));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title_span)
            .title_bottom(footer_span);

        let inner = block.inner(area);
        self.last_visible_height.set(inner.height);

        let paragraph = Paragraph::new(self.lines.clone()).scroll((self.scroll_offset, 0));
        frame.render_widget(paragraph, inner);
        frame.render_widget(block, area);
    }

    fn handle_event(&mut self, event: Event) -> PanelEvent<Self::Output> {
        let Event::Key(key) = event else {
            return PanelEvent::Continue;
        };
        if key.kind != KeyEventKind::Press {
            return PanelEvent::Continue;
        }

        match key.code {
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PanelEvent::Cancel
            }
            KeyCode::Esc => PanelEvent::Cancel,
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                PanelEvent::Continue
            }
            KeyCode::Down => {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_add(1)
                    .min(self.max_scroll(self.last_visible_height.get()));
                PanelEvent::Continue
            }
            KeyCode::PageUp => {
                let step = self.last_visible_height.get().max(1);
                self.scroll_offset = self.scroll_offset.saturating_sub(step);
                PanelEvent::Continue
            }
            KeyCode::PageDown => {
                let step = self.last_visible_height.get().max(1);
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_add(step)
                    .min(self.max_scroll(self.last_visible_height.get()));
                PanelEvent::Continue
            }
            KeyCode::Home => {
                self.scroll_offset = 0;
                PanelEvent::Continue
            }
            KeyCode::End => {
                self.scroll_offset = self.max_scroll(self.last_visible_height.get());
                PanelEvent::Continue
            }
            _ => PanelEvent::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PanelEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ctrl_o() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
    }

    #[test]
    fn ctrl_o_closes_panel() {
        let mut panel = ExpandPanel::new("Test", "hello\nworld");
        assert_eq!(panel.handle_event(ctrl_o()), PanelEvent::Cancel);
    }

    #[test]
    fn esc_closes_panel() {
        let mut panel = ExpandPanel::new("Test", "hello");
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(panel.handle_event(esc), PanelEvent::Cancel);
    }

    #[test]
    fn scroll_down_increments_offset() {
        let mut panel = ExpandPanel::new("Test", "a\nb\nc\nd\ne");
        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(panel.handle_event(down), PanelEvent::Continue);
        assert_eq!(panel.scroll_offset, 1);
    }
}
