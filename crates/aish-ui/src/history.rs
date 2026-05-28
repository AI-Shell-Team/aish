use std::cell::Cell;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::{PanelComponent, PanelEvent};

const FOOTER_HINT: &str = " ↑↓ Select · Enter View · Ctrl+O/Esc Close ";
const MAX_VISIBLE: usize = 8;

/// Record for display in the history panel.
#[derive(Clone)]
pub struct HistoryRecord {
    pub command: String,
    pub line_count: usize,
    pub time: String,
}

/// The selected outcome when user picks a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryOutcome {
    pub selected_index: usize,
}

/// A selectable list of collapsed output records.
pub struct HistoryPanel {
    records: Vec<HistoryRecord>,
    selected: usize,
    scroll_offset: usize,
    title: String,
    /// Visible row count saved during the last render, used for scroll calculations.
    last_visible_limit: Cell<usize>,
}

impl HistoryPanel {
    pub fn new(title: impl Into<String>, records: Vec<HistoryRecord>) -> Self {
        Self {
            selected: 0,
            scroll_offset: 0,
            title: title.into(),
            records,
            last_visible_limit: Cell::new(MAX_VISIBLE),
        }
    }

    fn visible_limit(&self, height: u16) -> usize {
        // height is already the inner area (borders removed by block.inner)
        (height as usize)
            .max(1)
            .min(MAX_VISIBLE)
            .min(self.records.len())
    }

    fn ensure_selected_visible(&mut self, visible_limit: usize) {
        if self.selected >= self.scroll_offset + visible_limit {
            self.scroll_offset = self.selected.saturating_sub(visible_limit - 1);
        }
    }
}

impl PanelComponent for HistoryPanel {
    type Output = HistoryOutcome;

    fn desired_height(&self, _tw: u16, th: u16) -> u16 {
        let needed = self.records.len().min(MAX_VISIBLE) as u16 + 2;
        needed.min(th.saturating_sub(3)).max(1)
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let title_span = ratatui::text::Span::styled(
            format!(" {} ", self.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        let footer_span =
            ratatui::text::Span::styled(FOOTER_HINT, Style::default().fg(Color::DarkGray));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title_span)
            .title_bottom(footer_span);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let visible = self.visible_limit(inner.height);
        self.last_visible_limit.set(visible);
        let mut lines: Vec<Line<'_>> = Vec::new();
        for i in self.scroll_offset..self.records.len() {
            if lines.len() >= visible {
                break;
            }
            let rec = &self.records[i];
            let selected = i == self.selected;
            let marker = if selected { "> " } else { "  " };
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let dim = Style::default().fg(Color::DarkGray);

            let max_cmd = (inner.width as usize).saturating_sub(30);
            let cmd_display = if rec.command.chars().count() > max_cmd && max_cmd > 3 {
                let trunc: String = rec.command.chars().take(max_cmd.saturating_sub(3)).collect();
                format!("{trunc}...")
            } else {
                rec.command.clone()
            };

            let badge = format!("{} lines", rec.line_count);

            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(cmd_display, style),
                Span::styled(format!("   {}  {}", badge, rec.time), dim),
            ]));
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }

    fn handle_event(&mut self, event: Event) -> PanelEvent<Self::Output> {
        let Event::Key(key) = event else {
            return PanelEvent::Continue;
        };
        if key.kind != KeyEventKind::Press {
            return PanelEvent::Continue;
        }

        match key.code {
            KeyCode::Esc => PanelEvent::Cancel,
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PanelEvent::Cancel
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                if self.selected < self.scroll_offset {
                    self.scroll_offset = self.selected;
                }
                PanelEvent::Continue
            }
            KeyCode::Down => {
                if !self.records.is_empty() {
                    self.selected = (self.selected + 1).min(self.records.len() - 1);
                    self.ensure_selected_visible(self.last_visible_limit.get());
                }
                PanelEvent::Continue
            }
            KeyCode::PageUp => {
                let vl = self.last_visible_limit.get();
                self.selected = self.selected.saturating_sub(vl);
                if self.selected < self.scroll_offset {
                    self.scroll_offset = self.selected;
                }
                PanelEvent::Continue
            }
            KeyCode::PageDown => {
                if !self.records.is_empty() {
                    let vl = self.last_visible_limit.get();
                    self.selected = (self.selected + vl).min(self.records.len() - 1);
                    self.ensure_selected_visible(vl);
                }
                PanelEvent::Continue
            }
            KeyCode::Home => {
                self.selected = 0;
                self.scroll_offset = 0;
                PanelEvent::Continue
            }
            KeyCode::End => {
                if !self.records.is_empty() {
                    self.selected = self.records.len() - 1;
                    self.ensure_selected_visible(self.last_visible_limit.get());
                }
                PanelEvent::Continue
            }
            KeyCode::Enter => {
                if self.records.is_empty() {
                    PanelEvent::Cancel
                } else if self.selected < self.records.len() {
                    PanelEvent::Submit(HistoryOutcome {
                        selected_index: self.selected,
                    })
                } else {
                    PanelEvent::Continue
                }
            }
            _ => PanelEvent::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use super::*;
    use crate::PanelEvent;

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn sample_records() -> Vec<HistoryRecord> {
        vec![
            HistoryRecord { command: "ls -la".into(), line_count: 45, time: "12:30:15".into() },
            HistoryRecord { command: "cat README.md".into(), line_count: 120, time: "12:31:42".into() },
            HistoryRecord { command: "make build".into(), line_count: 200, time: "12:35:10".into() },
        ]
    }

    #[test]
    fn ctrl_o_closes() {
        let mut panel = HistoryPanel::new("History", sample_records());
        assert_eq!(
            panel.handle_event(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            PanelEvent::Cancel
        );
    }

    #[test]
    fn esc_closes() {
        let mut panel = HistoryPanel::new("History", sample_records());
        assert_eq!(
            panel.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)),
            PanelEvent::Cancel
        );
    }

    #[test]
    fn enter_selects() {
        let mut panel = HistoryPanel::new("History", sample_records());
        panel.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        let result = panel.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            result,
            PanelEvent::Submit(HistoryOutcome { selected_index: 1 })
        );
    }

    #[test]
    fn navigate_up_down() {
        let mut panel = HistoryPanel::new("History", sample_records());
        assert_eq!(panel.selected, 0);
        panel.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(panel.selected, 1);
        panel.handle_event(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(panel.selected, 0);
    }
}
