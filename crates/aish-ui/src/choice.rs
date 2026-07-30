use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::util::padded_area;
use crate::{PanelComponent, PanelEvent, SearchSelectItem};

const DEFAULT_VISIBLE_ITEMS: usize = 8;
const MIN_PANEL_HEIGHT: u16 = 7;
const RESERVED_LINES: u16 = 3;
const DESCRIPTION_INDENT: &str = "    ";
const FOOTER_WITH_CANCEL: &str = "1-9 quick select | Up/Down navigate | Enter select | Esc cancel";
const FOOTER_NO_CANCEL: &str = "1-9 quick select | Up/Down navigate | Enter select";
const CUSTOM_INPUT_FOOTER: &str = "Type custom answer | Enter submit | Esc cancel";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceOutcome {
    Selected(String),
    CustomInput(String),
}

#[derive(Debug, Clone)]
pub struct ChoicePanel {
    title: String,
    question: Option<String>,
    footer: Option<String>,
    custom_input_footer: String,
    items: Vec<SearchSelectItem>,
    custom_label: Option<String>,
    allow_cancel: bool,
    selected: usize,
    max_visible_items: usize,
    custom_input_active: bool,
    custom_input: String,
    /// When true, an empty custom input submits as `CustomInput("")` instead of
    /// staying in input mode. Off by default to preserve existing behaviour.
    allow_empty_custom_input: bool,
}

impl ChoicePanel {
    pub fn new(
        title: impl Into<String>,
        question: impl Into<String>,
        items: Vec<SearchSelectItem>,
    ) -> Self {
        let mut title = title.into();
        let mut question = question.into();
        let question = if title.trim().is_empty() {
            title = question;
            None
        } else {
            (!question.trim().is_empty()).then_some(std::mem::take(&mut question))
        };

        Self {
            title,
            question,
            footer: Some(FOOTER_WITH_CANCEL.to_string()),
            custom_input_footer: CUSTOM_INPUT_FOOTER.to_string(),
            items,
            custom_label: None,
            allow_cancel: true,
            selected: 0,
            max_visible_items: DEFAULT_VISIBLE_ITEMS,
            custom_input_active: false,
            custom_input: String::new(),
            allow_empty_custom_input: false,
        }
    }

    pub fn with_custom_label(mut self, custom_label: impl Into<String>) -> Self {
        self.custom_label = Some(custom_label.into());
        self
    }

    pub fn with_allow_cancel(mut self, allow_cancel: bool) -> Self {
        self.allow_cancel = allow_cancel;
        self.footer = Some(Self::default_footer(allow_cancel).to_string());
        self
    }

    pub fn with_footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub fn with_custom_input_footer(mut self, footer: impl Into<String>) -> Self {
        self.custom_input_footer = footer.into();
        self
    }

    pub fn with_selected_value(mut self, value: Option<&str>) -> Self {
        if let Some(value) = value {
            if let Some(index) = self.items.iter().position(|item| item.value == value) {
                self.selected = index;
            }
        }
        self
    }

    pub fn with_max_visible_items(mut self, max_visible_items: usize) -> Self {
        self.max_visible_items = max_visible_items.max(1);
        self
    }

    /// Allow an empty custom input to submit (as `CustomInput("")`).
    /// Default is `false` — empty input stays in edit mode.
    pub fn with_allow_empty_custom_input(mut self, allow: bool) -> Self {
        self.allow_empty_custom_input = allow;
        self
    }

    pub fn default_footer(allow_cancel: bool) -> &'static str {
        if allow_cancel {
            FOOTER_WITH_CANCEL
        } else {
            FOOTER_NO_CANCEL
        }
    }

    fn entries_len(&self) -> usize {
        self.items.len() + usize::from(self.custom_label.is_some())
    }

    fn selected_is_custom(&self) -> bool {
        self.custom_label.is_some() && self.selected == self.items.len()
    }

    fn visible_limit(&self, list_height: usize) -> usize {
        let row_height = self.estimated_entry_height() as usize;
        (list_height / row_height)
            .max(1)
            .clamp(1, self.max_visible_items)
    }

    fn scroll_offset(&self, list_height: usize) -> usize {
        let visible_limit = self.visible_limit(list_height);
        self.selected
            .saturating_add(1)
            .saturating_sub(visible_limit)
    }

    fn move_down(&mut self, amount: usize) {
        let len = self.entries_len();
        if len > 0 {
            self.selected = (self.selected + amount).min(len - 1);
        }
    }

    fn move_up(&mut self, amount: usize) {
        self.selected = self.selected.saturating_sub(amount);
    }

    fn estimated_entry_height(&self) -> u16 {
        if self.items.iter().any(|item| item.detail.is_some()) {
            2
        } else {
            1
        }
    }

    fn selected_outcome(&mut self) -> PanelEvent<ChoiceOutcome> {
        if self.selected_is_custom() {
            self.custom_input_active = true;
            return PanelEvent::Continue;
        }

        self.items
            .get(self.selected)
            .map(|item| PanelEvent::Submit(ChoiceOutcome::Selected(item.value.clone())))
            .unwrap_or(PanelEvent::Continue)
    }

    fn submit_custom_input(&mut self) -> PanelEvent<ChoiceOutcome> {
        let trimmed = self.custom_input.trim().to_string();
        if trimmed.is_empty() {
            if self.allow_empty_custom_input {
                return PanelEvent::Submit(ChoiceOutcome::CustomInput(String::new()));
            }
            self.custom_input_active = false;
            return PanelEvent::Continue;
        }

        PanelEvent::Submit(ChoiceOutcome::CustomInput(trimmed))
    }

    fn handle_active_custom_input(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> PanelEvent<ChoiceOutcome> {
        match code {
            KeyCode::Esc => {
                if self.allow_cancel {
                    PanelEvent::Cancel
                } else {
                    self.custom_input_active = false;
                    PanelEvent::Continue
                }
            }
            KeyCode::Enter => self.submit_custom_input(),
            KeyCode::Backspace => {
                self.custom_input.pop();
                PanelEvent::Continue
            }
            KeyCode::Up => {
                self.custom_input_active = false;
                self.move_up(1);
                PanelEvent::Continue
            }
            KeyCode::Down => {
                self.custom_input_active = false;
                self.move_down(1);
                PanelEvent::Continue
            }
            KeyCode::Char('c')
                if modifiers.contains(KeyModifiers::CONTROL) && self.allow_cancel =>
            {
                PanelEvent::Cancel
            }
            KeyCode::Char(ch)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.custom_input.push(ch);
                PanelEvent::Continue
            }
            _ => PanelEvent::Continue,
        }
    }
}

impl PanelComponent for ChoicePanel {
    type Output = ChoiceOutcome;

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        let visible_rows = self.entries_len().min(self.max_visible_items).max(1) as u16
            * self.estimated_entry_height();
        let reserved_lines = RESERVED_LINES + u16::from(self.question.is_some());
        (visible_rows + reserved_lines).clamp(MIN_PANEL_HEIGHT, terminal_height.max(1))
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(u16::from(self.question.is_some())),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_divider(frame, chunks[0]);
        self.render_title(frame, chunks[1]);
        self.render_question(frame, chunks[2]);
        self.render_entries(frame, chunks[3]);
        self.render_footer(frame, chunks[4]);
    }

    fn handle_event(&mut self, event: Event) -> PanelEvent<Self::Output> {
        let Event::Key(key) = event else {
            return PanelEvent::Continue;
        };
        if key.kind == KeyEventKind::Release {
            return PanelEvent::Continue;
        }

        if self.custom_input_active {
            return self.handle_active_custom_input(key.code, key.modifiers);
        }

        match key.code {
            KeyCode::Esc => {
                if self.allow_cancel {
                    PanelEvent::Cancel
                } else {
                    PanelEvent::Continue
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.allow_cancel {
                    PanelEvent::Cancel
                } else {
                    PanelEvent::Continue
                }
            }
            KeyCode::Enter => self.selected_outcome(),
            KeyCode::Up => {
                self.move_up(1);
                PanelEvent::Continue
            }
            KeyCode::Down => {
                self.move_down(1);
                PanelEvent::Continue
            }
            KeyCode::PageUp => {
                self.move_up(self.max_visible_items);
                PanelEvent::Continue
            }
            KeyCode::PageDown => {
                self.move_down(self.max_visible_items);
                PanelEvent::Continue
            }
            KeyCode::Home => {
                self.selected = 0;
                PanelEvent::Continue
            }
            KeyCode::End => {
                self.selected = self.entries_len().saturating_sub(1);
                PanelEvent::Continue
            }
            KeyCode::Char(ch)
                if ch.is_ascii_digit()
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                let Some(digit) = ch.to_digit(10) else {
                    return PanelEvent::Continue;
                };
                if digit == 0 {
                    return PanelEvent::Continue;
                }
                let index = digit as usize - 1;
                if index < self.entries_len() {
                    self.selected = index;
                    return self.selected_outcome();
                }
                PanelEvent::Continue
            }
            KeyCode::Char(ch)
                if self.selected_is_custom()
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.custom_input_active = true;
                self.custom_input.push(ch);
                PanelEvent::Continue
            }
            _ => PanelEvent::Continue,
        }
    }
}

impl ChoicePanel {
    fn render_divider(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let width = area.width as usize;
        if width == 0 {
            return;
        }
        frame.render_widget(
            Paragraph::new("-".repeat(width)).style(Style::default().fg(Color::Cyan)),
            area,
        );
    }

    fn render_title(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        frame.render_widget(
            Paragraph::new(truncate_display(&self.title, area.width as usize)).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            area,
        );
    }

    fn render_question(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let Some(question) = &self.question else {
            return;
        };
        let area = padded_area(area);
        frame.render_widget(
            Paragraph::new(truncate_display(question, area.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let footer = if self.custom_input_active {
            Some(self.custom_input_footer.as_str())
        } else {
            self.footer.as_deref()
        };
        let Some(footer) = footer else {
            return;
        };
        let area = padded_area(area);
        if area.width == 0 {
            return;
        }
        frame.render_widget(
            Paragraph::new(truncate_display(footer, area.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn render_entries(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        if self.entries_len() == 0 {
            return;
        }

        let visible_limit = self.visible_limit(area.height as usize);
        let scroll = self.scroll_offset(area.height as usize);
        let end = (scroll + visible_limit).min(self.entries_len());
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);

        for absolute_index in scroll..end {
            if y >= bottom {
                break;
            }
            let selected = absolute_index == self.selected;
            let marker = self.entry_marker(selected, absolute_index, scroll, end);
            let lines = self.entry_lines(absolute_index, selected, marker);
            for line in lines {
                if y >= bottom {
                    break;
                }
                frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
                y = y.saturating_add(1);
            }
        }
    }

    fn entry_marker(
        &self,
        selected: bool,
        absolute_index: usize,
        scroll: usize,
        end: usize,
    ) -> &'static str {
        if selected {
            "> "
        } else if absolute_index == scroll && scroll > 0 {
            "^ "
        } else if absolute_index + 1 == end && end < self.entries_len() {
            "v "
        } else {
            "  "
        }
    }

    fn entry_lines(&self, index: usize, selected: bool, marker: &'static str) -> Vec<Line<'_>> {
        let marker_style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let label_style = if selected {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(marker, marker_style),
            Span::styled(
                format!("{}. ", index + 1),
                Style::default().fg(Color::DarkGray),
            ),
        ];

        if let Some(item) = self.items.get(index) {
            spans.push(Span::styled(item.label.as_str(), label_style));
            if let Some(badge) = &item.badge {
                spans.push(Span::styled(
                    format!(" {badge}"),
                    Style::default().fg(Color::Yellow),
                ));
            }
            let mut lines = vec![Line::from(spans)];
            if let Some(detail) = &item.detail {
                lines.push(Line::from(vec![
                    Span::raw(DESCRIPTION_INDENT),
                    Span::styled(detail.as_str(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            return lines;
        }

        if self.custom_input_active {
            spans.push(Span::styled(
                self.custom_input.as_str(),
                Style::default().fg(Color::White),
            ));
            spans.push(Span::styled("|", Style::default().fg(Color::White)));
        } else if !self.custom_input.is_empty() {
            spans.push(Span::styled(
                self.custom_input.as_str(),
                if selected {
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        } else {
            spans.push(Span::styled(
                self.custom_label.as_deref().unwrap_or("Custom input"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        vec![Line::from(spans)]
    }
}

fn truncate_display(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }

    let mut output = String::new();
    let target = max_width.saturating_sub(3);
    for ch in value.chars() {
        let next = format!("{}{}", output, ch);
        if UnicodeWidthStr::width(next.as_str()) > target {
            break;
        }
        output.push(ch);
    }
    output.push_str("...");
    output
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::PanelEvent;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn choice_panel_supports_numeric_shortcuts() {
        let mut panel = ChoicePanel::new(
            "Question",
            "Pick one",
            vec![
                SearchSelectItem::new("a", "Alpha"),
                SearchSelectItem::new("b", "Beta"),
            ],
        );

        assert_eq!(
            panel.handle_event(key(KeyCode::Char('2'))),
            PanelEvent::Submit(ChoiceOutcome::Selected("b".to_string()))
        );
    }

    #[test]
    fn custom_option_accepts_inline_input() {
        let mut panel = ChoicePanel::new(
            "Question",
            "Pick one",
            vec![SearchSelectItem::new("a", "Alpha")],
        )
        .with_custom_label("Custom");

        assert_eq!(
            panel.handle_event(key(KeyCode::Char('2'))),
            PanelEvent::Continue
        );
        assert_eq!(
            panel.handle_event(key(KeyCode::Char('h'))),
            PanelEvent::Continue
        );
        assert_eq!(
            panel.handle_event(key(KeyCode::Char('i'))),
            PanelEvent::Continue
        );
        assert_eq!(
            panel.handle_event(key(KeyCode::Enter)),
            PanelEvent::Submit(ChoiceOutcome::CustomInput("hi".to_string()))
        );
    }

    #[test]
    fn active_custom_input_replaces_custom_label() {
        let mut panel = ChoicePanel::new(
            "Question",
            "Pick one",
            vec![SearchSelectItem::new("a", "Alpha")],
        )
        .with_custom_label("Custom");

        panel.handle_event(key(KeyCode::Char('2')));
        panel.handle_event(key(KeyCode::Char('h')));
        panel.handle_event(key(KeyCode::Char('i')));

        let line = panel.entry_lines(1, true, "> ").remove(0);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(rendered, "> 2. hi|");
    }

    #[test]
    fn escape_in_inline_input_cancels_panel() {
        let mut panel = ChoicePanel::new(
            "Question",
            "Pick one",
            vec![SearchSelectItem::new("a", "Alpha")],
        )
        .with_custom_label("Custom");

        panel.handle_event(key(KeyCode::Char('2')));
        panel.handle_event(key(KeyCode::Char('h')));

        assert_eq!(panel.handle_event(key(KeyCode::Esc)), PanelEvent::Cancel);
    }
}
