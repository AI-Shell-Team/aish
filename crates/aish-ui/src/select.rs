use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::{PanelComponent, PanelEvent};

const DEFAULT_VISIBLE_ITEMS: usize = 8;
const MIN_PANEL_HEIGHT: u16 = 7;
const SEARCH_RESERVED_LINES: u16 = 4;
const PANEL_PADDING_X: u16 = 2;
const DESCRIPTION_INDENT: &str = "    ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSelectItem {
    pub value: String,
    pub label: String,
    pub detail: Option<String>,
    pub badge: Option<String>,
    pub search_text: String,
}

impl SearchSelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        let value = value.into();
        let label = label.into();
        let search_text = format!("{value} {label}").to_lowercase();
        Self {
            value,
            label,
            detail: None,
            badge: None,
            search_text,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    pub fn with_search_text(mut self, search_text: impl Into<String>) -> Self {
        self.search_text = search_text.into().to_lowercase();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSelectOutcome {
    Selected(String),
}

#[derive(Debug, Clone)]
pub struct SearchSelectPanel {
    title: String,
    subtitle: Option<String>,
    search_placeholder: String,
    footer: Option<String>,
    empty_message: String,
    items: Vec<SearchSelectItem>,
    allow_cancel: bool,
    query: String,
    selected: usize,
    max_visible_items: usize,
}

impl SearchSelectPanel {
    pub fn new(
        title: impl Into<String>,
        search_placeholder: impl Into<String>,
        items: Vec<SearchSelectItem>,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            search_placeholder: search_placeholder.into(),
            footer: None,
            empty_message: "No matches".to_string(),
            items,
            allow_cancel: true,
            query: String::new(),
            selected: 0,
            max_visible_items: DEFAULT_VISIBLE_ITEMS,
        }
    }

    pub fn with_footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        let subtitle = subtitle.into();
        self.subtitle = (!subtitle.trim().is_empty()).then_some(subtitle);
        self
    }

    pub fn with_empty_message(mut self, empty_message: impl Into<String>) -> Self {
        self.empty_message = empty_message.into();
        self
    }

    pub fn with_allow_cancel(mut self, allow_cancel: bool) -> Self {
        self.allow_cancel = allow_cancel;
        self
    }

    pub fn with_max_visible_items(mut self, max_visible_items: usize) -> Self {
        self.max_visible_items = max_visible_items.max(1);
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

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn filtered_entries(&self) -> Vec<SelectEntry> {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            (0..self.items.len()).map(SelectEntry::Item).collect()
        } else {
            self.items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    item.search_text
                        .contains(&query)
                        .then_some(SelectEntry::Item(index))
                })
                .collect()
        }
    }

    fn select_current(&self) -> Option<SearchSelectOutcome> {
        let entries = self.filtered_entries();
        self.select_entry(entries.get(self.selected)?)
    }

    fn select_entry(&self, entry: &SelectEntry) -> Option<SearchSelectOutcome> {
        match entry {
            SelectEntry::Item(index) => Some(SearchSelectOutcome::Selected(
                self.items.get(*index)?.value.clone(),
            )),
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered_entries().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    fn visible_limit(&self, list_height: usize) -> usize {
        let row_height = self.estimated_entry_height() as usize;
        (list_height / row_height)
            .max(1)
            .clamp(1, self.max_visible_items)
    }

    pub fn scroll_offset(&self, list_height: usize) -> usize {
        let visible_limit = self.visible_limit(list_height);
        self.selected
            .saturating_add(1)
            .saturating_sub(visible_limit)
    }

    fn move_down(&mut self, amount: usize) {
        let len = self.filtered_entries().len();
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectEntry {
    Item(usize),
}

impl PanelComponent for SearchSelectPanel {
    type Output = SearchSelectOutcome;

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        let item_count = self.items.len();
        let visible_rows =
            item_count.min(self.max_visible_items).max(1) as u16 * self.estimated_entry_height();
        let reserved_lines = SEARCH_RESERVED_LINES + u16::from(self.subtitle.is_some());
        (visible_rows + reserved_lines).clamp(MIN_PANEL_HEIGHT, terminal_height.max(1))
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(u16::from(self.subtitle.is_some())),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_divider(frame, chunks[0]);
        self.render_title(frame, chunks[1]);
        self.render_subtitle(frame, chunks[2]);
        self.render_search_line(frame, chunks[3]);
        self.render_entries(frame, chunks[4]);
        self.render_footer(frame, chunks[5]);
    }

    fn handle_event(&mut self, event: Event) -> PanelEvent<Self::Output> {
        let Event::Key(key) = event else {
            return PanelEvent::Continue;
        };
        if key.kind == KeyEventKind::Release {
            return PanelEvent::Continue;
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
            KeyCode::Enter => self
                .select_current()
                .map(PanelEvent::Submit)
                .unwrap_or(PanelEvent::Continue),
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
                let len = self.filtered_entries().len();
                self.selected = len.saturating_sub(1);
                PanelEvent::Continue
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
                PanelEvent::Continue
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.push(ch);
                self.selected = 0;
                self.clamp_selection();
                PanelEvent::Continue
            }
            _ => PanelEvent::Continue,
        }
    }
}

impl SearchSelectPanel {
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

    fn render_subtitle(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let Some(subtitle) = &self.subtitle else {
            return;
        };
        let area = padded_area(area);
        frame.render_widget(
            Paragraph::new(truncate_display(subtitle, area.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn render_search_line(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        let search_text = if self.query.is_empty() {
            self.search_placeholder.as_str()
        } else {
            self.query.as_str()
        };
        let search_style = if self.query.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Search: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_display(search_text, area.width.saturating_sub(8) as usize),
                    search_style,
                ),
            ])),
            area,
        );
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let Some(footer) = &self.footer else {
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
        let entries = self.filtered_entries();
        if entries.is_empty() {
            frame.render_widget(
                Paragraph::new(self.empty_message.as_str())
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        let visible_limit = self.visible_limit(area.height as usize);
        let scroll = self.scroll_offset(area.height as usize);
        let end = (scroll + visible_limit).min(entries.len());
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);

        for (row, entry) in entries[scroll..end].iter().enumerate() {
            if y >= bottom {
                break;
            }
            let absolute_index = scroll + row;
            let selected = absolute_index == self.selected;
            let marker = self.entry_marker(selected, absolute_index, scroll, end, entries.len());
            let lines = self.entry_lines(*entry, selected, marker);
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
        total: usize,
    ) -> &'static str {
        if selected {
            "> "
        } else if absolute_index == scroll && scroll > 0 {
            "^ "
        } else if absolute_index + 1 == end && end < total {
            "v "
        } else {
            "  "
        }
    }

    fn entry_lines(
        &self,
        entry: SelectEntry,
        selected: bool,
        marker: &'static str,
    ) -> Vec<Line<'_>> {
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
        let detail_style = Style::default().fg(Color::DarkGray);

        match entry {
            SelectEntry::Item(index) => {
                let item = &self.items[index];
                let mut spans = vec![Span::styled(marker, marker_style)];
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
                        Span::styled(detail.as_str(), detail_style),
                    ]));
                }
                lines
            }
        }
    }
}

fn padded_area(area: Rect) -> Rect {
    let padding = PANEL_PADDING_X.min(area.width / 2);
    Rect::new(
        area.x.saturating_add(padding),
        area.y,
        area.width.saturating_sub(padding.saturating_mul(2)),
        area.height,
    )
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::{PanelComponent, PanelEvent};

    fn item(value: &str, label: &str, search_text: &str) -> SearchSelectItem {
        SearchSelectItem::new(value, label).with_search_text(search_text)
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn filters_by_search_text() {
        let mut panel = SearchSelectPanel::new(
            "Sessions",
            "Search",
            vec![
                item("a", "Deploy docs", "a deploy docs"),
                item("b", "Fix parser", "b fix parser"),
            ],
        );

        panel.handle_event(key(KeyCode::Char('p')));
        panel.handle_event(key(KeyCode::Char('a')));

        assert_eq!(panel.filtered_entries(), vec![SelectEntry::Item(1)]);
    }

    #[test]
    fn default_cursor_uses_matching_value() {
        let panel = SearchSelectPanel::new(
            "Question",
            "Search",
            vec![item("yes", "Yes", "yes"), item("no", "No", "no")],
        )
        .with_selected_value(Some("no"));

        assert_eq!(panel.selected, 1);
    }

    #[test]
    fn selection_is_clamped_after_filtering() {
        let mut panel = SearchSelectPanel::new(
            "Question",
            "Search",
            vec![item("a", "Alpha", "alpha"), item("b", "Beta", "beta")],
        )
        .with_selected_value(Some("b"));

        panel.handle_event(key(KeyCode::Char('h')));

        assert_eq!(panel.selected, 0);
        assert_eq!(panel.filtered_entries(), vec![SelectEntry::Item(0)]);
    }

    #[test]
    fn scroll_offset_keeps_selected_visible() {
        let panel = SearchSelectPanel::new(
            "Question",
            "Search",
            (0..10)
                .map(|i| item(&i.to_string(), &format!("Item {i}"), &format!("item {i}")))
                .collect(),
        )
        .with_selected_value(Some("7"));

        assert_eq!(panel.scroll_offset(4), 4);
    }

    #[test]
    fn escape_respects_cancel_setting() {
        let mut cancel_panel =
            SearchSelectPanel::new("Question", "Search", vec![item("a", "Alpha", "alpha")]);
        assert_eq!(
            cancel_panel.handle_event(key(KeyCode::Esc)),
            PanelEvent::Cancel
        );

        let mut required_panel =
            SearchSelectPanel::new("Question", "Search", vec![item("a", "Alpha", "alpha")])
                .with_allow_cancel(false);
        assert_eq!(
            required_panel.handle_event(key(KeyCode::Esc)),
            PanelEvent::Continue
        );
    }
}
