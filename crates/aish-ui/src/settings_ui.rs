//! Composite settings panel — the single-screen `/setting` UI.
//!
//! A self-contained `PanelComponent` that renders category chips, a flat
//! searchable list with rich per-row state, and handles Bool/Choice editing
//! inline (no second panel round-trip). Text/Int/Float/Secret/StringList
//! fields emit `RequestExternalEdit` so the caller can pop its own input
//! prompt without leaving the panel's event loop on return.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::util::{padded_area, truncate_line, truncate_str};
use crate::{PanelComponent, PanelEvent};

const MIN_PANEL_HEIGHT: u16 = 8;
const MIN_LIST_ROWS: usize = 4;
const DESCRIPTION_INDENT: &str = "      ";

/// How a setting's value is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsValueKind {
    Bool,
    Choice,
    Text,
    Float,
    Int,
    Secret,
    StringList,
}

/// One row in the panel. Built by the caller from the live config + catalog.
#[derive(Debug, Clone)]
pub struct SettingsItem {
    pub key: String,
    pub label: String,
    pub desc: String,
    /// Raw current value (what `apply()` would consume).
    pub current_raw: String,
    /// Friendly current value for display (e.g. "开"/"关", masked secret).
    pub display_value: String,
    /// Factory default raw value — powers the `✱` changed marker and `r` reset.
    pub default_raw: String,
    /// Index into the `categories` slice passed to the panel.
    pub category_index: usize,
    pub kind: SettingsValueKind,
    /// For `Choice` only — the legal values in display order.
    pub options: Vec<String>,
    pub changed: bool,
    pub restart_required: bool,
}

/// Category chip metadata.
#[derive(Debug, Clone)]
pub struct SettingsCategoryInfo {
    pub label: String,
    pub icon: String,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsOutcome {
    /// Bool flip, Choice change, or external-edit submit applied.
    Applied {
        key: String,
        value: String,
        active_category: usize,
    },
    /// Reset-to-default requested (Ctrl+R on a row).
    Reset { key: String, active_category: usize },
    /// User hit Enter on a Text/Int/Float/Secret/StringList field.
    RequestExternalEdit { key: String, active_category: usize },
    /// User hit Enter on a Choice field — caller pops a one-shot selection
    /// list (the user wants to see all options at a glance, not cycle blind).
    /// `</>` still cycles inline for power users.
    RequestChoiceSelect { key: String, active_category: usize },
    /// Esc at top level, Ctrl+C, or Ctrl+Q.
    Cancelled { active_category: usize },
}

impl SettingsOutcome {
    /// The chip that was active when this outcome was produced. The caller
    /// uses it to restore the cursor's chip across panel re-opens.
    pub fn active_category(&self) -> usize {
        match self {
            SettingsOutcome::Applied {
                active_category, ..
            }
            | SettingsOutcome::Reset {
                active_category, ..
            }
            | SettingsOutcome::RequestExternalEdit {
                active_category, ..
            }
            | SettingsOutcome::RequestChoiceSelect {
                active_category, ..
            }
            | SettingsOutcome::Cancelled { active_category } => *active_category,
        }
    }
}

/// Color tint for the four state badges rendered on each row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Badge {
    /// Bool = true.
    BoolOn,
    /// Bool = false.
    BoolOff,
    /// Secret field with empty value.
    SecretUnset,
    /// Non-empty secret (masked).
    SecretSet,
}

/// The composite panel itself.
#[derive(Debug, Clone)]
pub struct SettingsPanel {
    title: String,
    categories: Vec<SettingsCategoryInfo>,
    items: Vec<SettingsItem>,
    /// 0 = "All", 1..=N = the Nth category.
    active_category: usize,
    query: String,
    selected: usize,
    search_placeholder: String,
    footer_idle: String,
    footer_editing: String,
    error_msg: Option<String>,
}

impl SettingsPanel {
    pub fn new(
        title: impl Into<String>,
        categories: Vec<SettingsCategoryInfo>,
        items: Vec<SettingsItem>,
    ) -> Self {
        Self {
            title: title.into(),
            categories,
            items,
            active_category: 0,
            query: String::new(),
            selected: 0,
            search_placeholder: "Type to filter…".to_string(),
            footer_idle: "↑↓ move · ←/→ category · Space/Enter edit · Ctrl+R reset · Esc exit"
                .to_string(),
            footer_editing: "Enter apply · Esc cancel".to_string(),
            error_msg: None,
        }
    }

    pub fn with_search_placeholder(mut self, s: impl Into<String>) -> Self {
        self.search_placeholder = s.into();
        self
    }

    pub fn with_footer_idle(mut self, s: impl Into<String>) -> Self {
        self.footer_idle = s.into();
        self
    }

    pub fn with_footer_editing(mut self, s: impl Into<String>) -> Self {
        self.footer_editing = s.into();
        self
    }

    /// Position the cursor on the row whose key matches, if visible.
    pub fn with_selected_key(mut self, key: Option<&str>) -> Self {
        if let Some(k) = key {
            if let Some(idx) = self
                .visible_indices()
                .iter()
                .position(|i| self.items[*i].key == k)
            {
                self.selected = idx;
            }
        }
        self
    }

    /// Restore the active category chip across panel re-opens (so editing a
    /// row in category N does not bounce the user back to "All").
    pub fn with_active_category(mut self, idx: usize) -> Self {
        self.set_active_category(idx);
        self
    }

    /// Show a transient red error line below the list (cleared on next key).
    pub fn with_error(mut self, msg: Option<String>) -> Self {
        self.error_msg = msg.filter(|m| !m.trim().is_empty());
        self
    }

    pub fn active_category(&self) -> usize {
        self.active_category
    }

    pub fn set_active_category(&mut self, idx: usize) {
        if idx < self.categories.len() + 1 {
            self.active_category = idx;
            self.selected = 0;
        }
    }

    /// Whether the item matches the search query (independent of the
    /// active-category filter). Used for chip counts so they reflect the
    /// query across all categories, not just the active one.
    fn item_matches_query(&self, idx: usize) -> bool {
        let item = &self.items[idx];
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        let hay = format!("{} {} {} {}", item.label, item.desc, item.key, {
            self.categories
                .get(item.category_index)
                .map(|c| c.label.as_str())
                .unwrap_or("")
        })
        .to_lowercase();
        q.split_whitespace().all(|tok| hay.contains(tok))
    }

    fn item_matches_filter(&self, idx: usize) -> bool {
        let item = &self.items[idx];
        if self.active_category != 0 && item.category_index + 1 != self.active_category {
            return false;
        }
        self.item_matches_query(idx)
    }

    fn visible_indices(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|i| self.item_matches_filter(*i))
            .collect()
    }

    fn current_visible(&self) -> Option<usize> {
        self.visible_indices().get(self.selected).copied()
    }

    fn clamp_selected(&mut self) {
        let len = self.visible_indices().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.visible_indices().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let mut next = self.selected as isize + delta;
        if next < 0 {
            next = 0;
        }
        if next as usize >= len {
            next = (len - 1) as isize;
        }
        self.selected = next as usize;
    }

    fn switch_category(&mut self, dir: isize) {
        let n = self.categories.len() + 1; // +1 for "All"
        let mut next = self.active_category as isize + dir;
        if next < 0 {
            next = (n - 1) as isize;
        }
        if next as usize >= n {
            next = 0;
        }
        self.active_category = next as usize;
        self.selected = 0;
    }

    fn apply_bool_toggle(&self, item: &SettingsItem) -> SettingsOutcome {
        let next = match item.current_raw.as_str() {
            "true" => "false".to_string(),
            _ => "true".to_string(),
        };
        SettingsOutcome::Applied {
            key: item.key.clone(),
            value: next,
            active_category: self.active_category,
        }
    }

    fn apply_choice_step(&self, item: &SettingsItem, forward: bool) -> Option<SettingsOutcome> {
        if item.options.is_empty() {
            return None;
        }
        let current = item
            .options
            .iter()
            .position(|o| o.eq_ignore_ascii_case(&item.current_raw))
            .unwrap_or(0);
        let n = item.options.len();
        let next = if forward {
            (current + 1) % n
        } else {
            (current + n - 1) % n
        };
        Some(SettingsOutcome::Applied {
            key: item.key.clone(),
            value: item.options[next].clone(),
            active_category: self.active_category,
        })
    }

    fn reset_current(&self) -> Option<SettingsOutcome> {
        let idx = self.current_visible()?;
        Some(SettingsOutcome::Reset {
            key: self.items[idx].key.clone(),
            active_category: self.active_category,
        })
    }

    fn activate_current(&self) -> Option<SettingsOutcome> {
        let idx = self.current_visible()?;
        let item = &self.items[idx];
        match item.kind {
            SettingsValueKind::Bool => Some(self.apply_bool_toggle(item)),
            SettingsValueKind::Choice => Some(SettingsOutcome::RequestChoiceSelect {
                key: item.key.clone(),
                active_category: self.active_category,
            }),
            SettingsValueKind::Text
            | SettingsValueKind::Float
            | SettingsValueKind::Int
            | SettingsValueKind::Secret
            | SettingsValueKind::StringList => Some(SettingsOutcome::RequestExternalEdit {
                key: item.key.clone(),
                active_category: self.active_category,
            }),
        }
    }

    // ---------------- rendering helpers -------------------------------------

    fn chrome_lines(&self) -> u16 {
        // divider + title + chips + search + footer + desc + (optional error)
        6 + u16::from(self.error_msg.is_some())
    }

    fn render_divider(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let width = area.width as usize;
        if width == 0 {
            return;
        }
        frame.render_widget(
            Paragraph::new("─".repeat(width)).style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn render_title(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        let total = self.items.len();
        let shown = self.visible_indices().len();
        let title = format!("{}  ({shown}/{total})", self.title);
        let mut spans = vec![Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )];
        if shown < total {
            spans.push(Span::styled(
                format!("  · filtered {}", shown),
                Style::default().fg(Color::DarkGray),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_chips(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        use unicode_width::UnicodeWidthStr;
        let area = padded_area(area);
        if area.width < 8 {
            return;
        }
        // Count per real category among query-matching items, IGNORING the
        // active-category filter — otherwise selecting a category collapses
        // every other chip to (0) and All(N) shows only the active count.
        let mut counts = vec![0usize; self.categories.len()];
        let mut total = 0usize;
        for i in 0..self.items.len() {
            if !self.item_matches_query(i) {
                continue;
            }
            total += 1;
            let ci = self.items[i].category_index;
            if ci < counts.len() {
                counts[ci] += 1;
            }
        }

        // Build chip descriptors: (text, style, display_width). Index 0 = All.
        let all_active = self.active_category == 0;
        let all_style = if all_active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let all_text = format!(" All({}) ", total);
        let mut chips: Vec<(String, Style, usize)> =
            vec![(all_text.clone(), all_style, all_text.width())];
        for (i, cat) in self.categories.iter().enumerate() {
            let active = self.active_category == i + 1;
            let style = if active {
                Style::default()
                    .fg(Color::Black)
                    .bg(cat.color)
                    .add_modifier(Modifier::BOLD)
            } else if counts[i] == 0 {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(cat.color)
            };
            // Separator space goes on the right of each chip.
            let body = format!(" {} {}({})  ", cat.icon, cat.label, counts[i]);
            let w = body.width();
            chips.push((body, style, w));
        }
        let active_idx = self.active_category.min(chips.len() - 1);

        // Window around the active chip. Start with just active, then expand
        // outward as long as we stay within `area.width`. This guarantees the
        // active chip is always fully visible — no mid-chip truncation.
        let area_w = area.width as usize;
        let mut first = active_idx;
        let mut last = active_idx;
        let mut w = chips[active_idx].2;
        // Expand left first (bias toward showing the predecessor).
        while first > 0 {
            let nw = w + chips[first - 1].2;
            if nw > area_w {
                break;
            }
            first -= 1;
            w = nw;
        }
        // Then expand right.
        while last + 1 < chips.len() {
            let nw = w + chips[last + 1].2;
            if nw > area_w {
                break;
            }
            last += 1;
            w = nw;
        }

        let left_more = first > 0;
        let right_more = last + 1 < chips.len();

        let mut spans: Vec<Span<'_>> = Vec::new();
        if left_more {
            spans.push(Span::styled(
                "‹ ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        for (text, style, _) in chips.iter().take(last + 1).skip(first) {
            spans.push(Span::styled(text.clone(), *style));
        }
        if right_more {
            spans.push(Span::styled(
                " ›",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_search(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        let prompt_span = Span::styled(
            "search ❯ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        let (text, style) = if self.query.is_empty() {
            (
                self.search_placeholder.clone(),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            (self.query.clone(), Style::default().fg(Color::White))
        };
        let mut spans = vec![prompt_span, Span::styled(text, style)];
        if !self.query.is_empty() {
            // Caret.
            spans.push(Span::styled("▏", Style::default().fg(Color::Cyan)));
        }
        let line = truncate_line(spans, area.width as usize);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_entries(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        let entries = self.visible_indices();
        if entries.is_empty() {
            frame.render_widget(
                Paragraph::new("  No matches — press Esc to clear the filter.")
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        // Two lines per row: value line + desc line.
        let row_h = 2usize;
        let visible_limit = ((area.height as usize) / row_h).max(1);
        let scroll = (self.selected / visible_limit) * visible_limit;
        let end = (scroll + visible_limit).min(entries.len());

        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);

        for row in scroll..end {
            if y >= bottom {
                break;
            }
            let abs = row;
            let item_idx = entries[abs];
            let selected = abs == self.selected;
            // marker line
            let line1 = self.render_item_line(item_idx, selected, abs, scroll, end, entries.len());
            frame.render_widget(Paragraph::new(line1), Rect::new(area.x, y, area.width, 1));
            y = y.saturating_add(1);
            if y >= bottom {
                break;
            }
            // desc line
            let item = &self.items[item_idx];
            let desc_style = if selected {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let desc = truncate_str(
                &item.desc,
                (area.width as usize).saturating_sub(DESCRIPTION_INDENT.width()),
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(DESCRIPTION_INDENT),
                    Span::styled(desc, desc_style),
                ])),
                Rect::new(area.x, y, area.width, 1),
            );
            y = y.saturating_add(1);
        }
    }

    fn render_item_line(
        &self,
        item_idx: usize,
        selected: bool,
        abs: usize,
        scroll: usize,
        end: usize,
        total: usize,
    ) -> Line<'_> {
        let item = &self.items[item_idx];
        let marker = if selected {
            "▶"
        } else if abs == scroll && scroll > 0 {
            "▲"
        } else if abs + 1 == end && end < total {
            "▼"
        } else {
            " "
        };
        let marker_style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let cat = self
            .categories
            .get(item.category_index)
            .filter(|_| self.active_category == 0);
        let icon_style = if selected {
            Style::default()
                .fg(cat.map(|c| c.color).unwrap_or(Color::Cyan))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(cat.map(|c| c.color).unwrap_or(Color::DarkGray))
        };
        let icon = cat.map(|c| c.icon.as_str()).unwrap_or("·");

        let label_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        let mut spans = vec![
            Span::styled(format!("{marker} "), marker_style),
            Span::styled(format!("{icon} "), icon_style),
            Span::styled(item.label.as_str(), label_style),
        ];

        // Value column — pushed to the right would need width math; keep it
        // inline but visually separated. For Bool we show a colored dot before
        // the value text to give the row an at-a-glance state.
        let badge = match item.kind {
            SettingsValueKind::Bool => {
                if item.current_raw == "true" {
                    Some(Badge::BoolOn)
                } else {
                    Some(Badge::BoolOff)
                }
            }
            SettingsValueKind::Secret => {
                if item.current_raw.is_empty() {
                    Some(Badge::SecretUnset)
                } else {
                    Some(Badge::SecretSet)
                }
            }
            _ => None,
        };
        if let Some(b) = badge {
            let (glyph, color) = match b {
                Badge::BoolOn => ("●", Color::LightGreen),
                Badge::BoolOff => ("○", Color::DarkGray),
                Badge::SecretUnset => ("△", Color::Yellow),
                Badge::SecretSet => ("◆", Color::LightMagenta),
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(glyph, Style::default().fg(color)));
        }

        // Value text (dim for unselected, accent for selected).
        let value_style = if selected {
            Style::default().fg(Color::LightCyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(item.display_value.as_str(), value_style));

        // Trailing state badges.
        if item.changed {
            spans.push(Span::styled(
                " ✱",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if item.restart_required {
            spans.push(Span::styled(" ↻", Style::default().fg(Color::LightRed)));
        }

        Line::from(spans)
    }

    fn render_desc_or_error(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        if let Some(err) = &self.error_msg {
            frame.render_widget(
                Paragraph::new(truncate_str(err, area.width as usize)).style(
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                ),
                area,
            );
            return;
        }
        if let Some(idx) = self.current_visible() {
            let item = &self.items[idx];
            // Show default as a hint when changed.
            let hint = if item.changed && !item.default_raw.is_empty() {
                format!("   [default: {}]", item.default_raw)
            } else {
                String::new()
            };
            let line = Line::from(vec![
                Span::styled("↳ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_str(
                        &item.desc,
                        (area.width as usize).saturating_sub(hint.width() + 3),
                    ),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(hint, Style::default().fg(Color::DarkGray)),
            ]);
            frame.render_widget(Paragraph::new(line), area);
        }
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        // Contextual footer: depends on the selected row's kind.
        let footer = self
            .current_visible()
            .map(|i| self.context_footer(&self.items[i]))
            .unwrap_or_else(|| self.footer_idle.clone());
        frame.render_widget(
            Paragraph::new(truncate_str(&footer, area.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn context_footer(&self, item: &SettingsItem) -> String {
        let base = match item.kind {
            SettingsValueKind::Bool => {
                "Space/Enter toggle · Ctrl+R reset · ←/→ category · Esc exit"
            }
            SettingsValueKind::Choice => {
                "</> step · Enter select · Ctrl+R reset · ←/→ category · Esc exit"
            }
            _ => "Enter edit · Ctrl+R reset · ←/→ category · Esc exit",
        };
        if item.changed {
            format!("{base}   · ✱ changed")
        } else {
            base.to_string()
        }
    }
}

impl PanelComponent for SettingsPanel {
    type Output = SettingsOutcome;

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        // Estimate: up to 10 visible rows (2 lines each) + chrome.
        let rows = self.items.len().clamp(MIN_LIST_ROWS, 10);
        let h = (rows as u16) * 2 + self.chrome_lines();
        h.clamp(MIN_PANEL_HEIGHT, terminal_height.max(1))
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // divider
                Constraint::Length(1), // title
                Constraint::Length(1), // chips
                Constraint::Length(1), // search
                Constraint::Min(1),    // entries
                Constraint::Length(1), // desc / error
                Constraint::Length(1), // footer
            ])
            .split(area);

        self.render_divider(frame, chunks[0]);
        self.render_title(frame, chunks[1]);
        self.render_chips(frame, chunks[2]);
        self.render_search(frame, chunks[3]);
        self.render_entries(frame, chunks[4]);
        self.render_desc_or_error(frame, chunks[5]);
        self.render_footer(frame, chunks[6]);
    }

    fn handle_event(&mut self, event: Event) -> PanelEvent<Self::Output> {
        self.error_msg = None; // clear transient error on any key
        let Event::Key(key) = event else {
            return PanelEvent::Continue;
        };
        if key.kind == KeyEventKind::Release {
            return PanelEvent::Continue;
        }

        match key.code {
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.selected = 0;
                    PanelEvent::Continue
                } else {
                    PanelEvent::Submit(SettingsOutcome::Cancelled {
                        active_category: self.active_category,
                    })
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PanelEvent::Submit(SettingsOutcome::Cancelled {
                    active_category: self.active_category,
                })
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PanelEvent::Submit(SettingsOutcome::Cancelled {
                    active_category: self.active_category,
                })
            }
            KeyCode::Tab => {
                self.switch_category(1);
                PanelEvent::Continue
            }
            KeyCode::BackTab => {
                self.switch_category(-1);
                PanelEvent::Continue
            }
            KeyCode::Up => {
                self.move_cursor(-1);
                PanelEvent::Continue
            }
            KeyCode::Down => {
                self.move_cursor(1);
                PanelEvent::Continue
            }
            KeyCode::PageUp => {
                self.move_cursor(-5);
                PanelEvent::Continue
            }
            KeyCode::PageDown => {
                self.move_cursor(5);
                PanelEvent::Continue
            }
            KeyCode::Left => {
                self.switch_category(-1);
                PanelEvent::Continue
            }
            KeyCode::Right => {
                self.switch_category(1);
                PanelEvent::Continue
            }
            KeyCode::Home => {
                self.selected = 0;
                PanelEvent::Continue
            }
            KeyCode::End => {
                self.clamp_selected();
                let len = self.visible_indices().len();
                if len > 0 {
                    self.selected = len - 1;
                }
                PanelEvent::Continue
            }
            KeyCode::Enter => self
                .activate_current()
                .map(PanelEvent::Submit)
                .unwrap_or(PanelEvent::Continue),
            KeyCode::Char(' ') => {
                // Space toggles Bool on the current row; otherwise it's a
                // search character.
                if let Some(idx) = self.current_visible() {
                    if self.items[idx].kind == SettingsValueKind::Bool {
                        return PanelEvent::Submit(self.apply_bool_toggle(&self.items[idx]));
                    }
                }
                self.push_query(' ');
                PanelEvent::Continue
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.clamp_selected();
                PanelEvent::Continue
            }
            KeyCode::Char('<') | KeyCode::Char(',') => {
                if let Some(idx) = self.current_visible() {
                    if self.items[idx].kind == SettingsValueKind::Choice {
                        if let Some(o) = self.apply_choice_step(&self.items[idx], false) {
                            return PanelEvent::Submit(o);
                        }
                    }
                }
                self.push_query(if key.code == KeyCode::Char('<') {
                    '<'
                } else {
                    ','
                });
                PanelEvent::Continue
            }
            KeyCode::Char('>') | KeyCode::Char('.') => {
                if let Some(idx) = self.current_visible() {
                    if self.items[idx].kind == SettingsValueKind::Choice {
                        if let Some(o) = self.apply_choice_step(&self.items[idx], true) {
                            return PanelEvent::Submit(o);
                        }
                    }
                }
                self.push_query(if key.code == KeyCode::Char('>') {
                    '>'
                } else {
                    '.'
                });
                PanelEvent::Continue
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self
                .reset_current()
                .map(PanelEvent::Submit)
                .unwrap_or(PanelEvent::Continue),
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.push_query(ch);
                PanelEvent::Continue
            }
            _ => PanelEvent::Continue,
        }
    }
}

impl SettingsPanel {
    fn push_query(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.clamp_selected();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(label: &str, color: Color) -> SettingsCategoryInfo {
        SettingsCategoryInfo {
            label: label.into(),
            icon: "◆".into(),
            color,
        }
    }

    fn bool_item(key: &str, label: &str, cur: &str) -> SettingsItem {
        SettingsItem {
            key: key.into(),
            label: label.into(),
            desc: "desc".into(),
            current_raw: cur.into(),
            display_value: if cur == "true" {
                "on".into()
            } else {
                "off".into()
            },
            default_raw: "false".into(),
            category_index: 0,
            kind: SettingsValueKind::Bool,
            options: vec![],
            changed: cur != "false",
            restart_required: false,
        }
    }

    fn choice_item(key: &str, cur: &str, opts: &[&str]) -> SettingsItem {
        SettingsItem {
            key: key.into(),
            label: key.into(),
            desc: "desc".into(),
            current_raw: cur.into(),
            display_value: cur.into(),
            default_raw: opts[0].into(),
            category_index: 0,
            kind: SettingsValueKind::Choice,
            options: opts.iter().map(|s| s.to_string()).collect(),
            changed: cur != opts[0],
            restart_required: false,
        }
    }

    #[test]
    fn bool_toggle_emits_inverse() {
        let panel = SettingsPanel::new(
            "t",
            vec![cat("A", Color::Cyan)],
            vec![bool_item("b", "B", "true")],
        );
        let o = panel.activate_current().unwrap();
        match o {
            SettingsOutcome::Applied { key, value, .. } if key == "b" && value == "false" => {}
            other => panic!("expected Applied(b,false), got {:?}", other),
        }
    }

    #[test]
    fn choice_step_wraps_around() {
        // apply_choice_step (driven by `<`/`>`) still cycles through values.
        let panel = SettingsPanel::new(
            "t",
            vec![cat("A", Color::Cyan)],
            vec![choice_item("c", "low", &["low", "medium", "high"])],
        );
        // forward from index 0 → 1
        match panel.apply_choice_step(&panel.items[0], true).unwrap() {
            SettingsOutcome::Applied { key, value, .. } if key == "c" && value == "medium" => {}
            other => panic!("expected Applied(c,medium), got {:?}", other),
        }
        // Forward from the last wraps to first.
        let panel2 = SettingsPanel::new(
            "t",
            vec![cat("A", Color::Cyan)],
            vec![choice_item("c", "high", &["low", "medium", "high"])],
        );
        match panel2.apply_choice_step(&panel2.items[0], true).unwrap() {
            SettingsOutcome::Applied { key, value, .. } if key == "c" && value == "low" => {}
            other => panic!("expected Applied(c,low), got {:?}", other),
        }
    }

    /// Enter on a Choice row now emits RequestChoiceSelect (caller pops a list),
    /// not an inline cycle. Power users still cycle inline with `<`/`>`.
    #[test]
    fn choice_activate_emits_request_choice_select() {
        let panel = SettingsPanel::new(
            "t",
            vec![cat("A", Color::Cyan)],
            vec![choice_item("c", "low", &["low", "medium", "high"])],
        );
        match panel.activate_current().unwrap() {
            SettingsOutcome::RequestChoiceSelect { key, .. } if key == "c" => {}
            other => panic!("expected RequestChoiceSelect, got {:?}", other),
        }
    }

    #[test]
    fn filter_by_query_and_category() {
        let mut items = vec![
            bool_item("a", "Alpha", "true"),
            bool_item("b", "Beta", "false"),
            choice_item("c", "low", &["low", "high"]),
        ];
        items[2].category_index = 1;
        let cats = vec![cat("A", Color::Cyan), cat("B", Color::Magenta)];
        let panel = SettingsPanel::new("t", cats, items);
        // No filter: all 3 visible.
        assert_eq!(panel.visible_indices().len(), 3);
        // Category filter to B (index 2 = category_index 1).
        let mut p2 = panel.clone();
        p2.set_active_category(2);
        let vis = p2.visible_indices();
        assert_eq!(vis, vec![2]);
    }

    /// Regression: chip counts must use the query filter only, NOT the
    /// active-category filter. Before the fix, selecting a category
    /// collapsed every other chip to (0) and `All(N)` shrank to the
    /// active count, misleading users about cross-category matches.
    #[test]
    fn chip_counts_ignore_active_category() {
        let mut items = vec![
            bool_item("a", "Alpha", "true"),
            bool_item("b", "Beta", "false"),
            choice_item("c", "low", &["low", "high"]),
        ];
        items[0].category_index = 0; // A
        items[1].category_index = 0; // A
        items[2].category_index = 1; // B
        let cats = vec![cat("A", Color::Cyan), cat("B", Color::Magenta)];
        let mut panel = SettingsPanel::new("t", cats, items);
        panel.set_active_category(2); // focus B
                                      // item_matches_query is the chip-count predicate; it must NOT
                                      // depend on active_category. All 3 items still match an empty query.
        let q_matches: Vec<usize> = (0..panel.items.len())
            .filter(|&i| panel.item_matches_query(i))
            .collect();
        assert_eq!(q_matches, vec![0, 1, 2]);
        // But item_matches_filter (list visibility) honors active_category.
        let f_matches: Vec<usize> = (0..panel.items.len())
            .filter(|&i| panel.item_matches_filter(i))
            .collect();
        assert_eq!(f_matches, vec![2]);
    }

    /// Categorize/Left/Right/Tab all funnel through `switch_category`; verify
    /// the resulting `active_category` and that `selected` resets to 0.
    #[test]
    fn switch_category_rotates_and_resets_cursor() {
        let items = vec![
            bool_item("a", "Alpha", "true"),
            bool_item("b", "Beta", "false"),
        ];
        let cats = vec![cat("A", Color::Cyan), cat("B", Color::Magenta)];
        let mut panel = SettingsPanel::new("t", cats, items);
        panel.selected = 1;
        // Forward through All → A → B → wrap to All.
        panel.switch_category(1);
        assert_eq!(panel.active_category, 1);
        assert_eq!(panel.selected, 0);
        panel.switch_category(1);
        assert_eq!(panel.active_category, 2);
        panel.switch_category(1);
        assert_eq!(panel.active_category, 0); // wrapped
                                              // Backward from All wraps to last.
        panel.switch_category(-1);
        assert_eq!(panel.active_category, 2);
    }

    /// Ctrl+R on a row should emit `Reset { key }`.
    #[test]
    fn ctrl_r_resets_current_row() {
        let items = vec![bool_item("b", "B", "true")];
        let panel = SettingsPanel::new("t", vec![cat("A", Color::Cyan)], items);
        let outcome = panel.reset_current().unwrap();
        match outcome {
            SettingsOutcome::Reset { key, .. } if key == "b" => {}
            other => panic!("expected Reset(b), got {:?}", other),
        }
    }

    /// `<` on a Choice row steps backward; `>` steps forward.
    #[test]
    fn choice_step_backward_and_forward() {
        let items = vec![choice_item("c", "medium", &["low", "medium", "high"])];
        let panel = SettingsPanel::new("t", vec![cat("A", Color::Cyan)], items);
        // Backward from medium → low.
        let back = panel.apply_choice_step(&panel.items[0], false).unwrap();
        match back {
            SettingsOutcome::Applied { key, value, .. } if key == "c" && value == "low" => {}
            other => panic!("expected Applied(c,low), got {:?}", other),
        }
        // Forward from medium → high.
        let fwd = panel.apply_choice_step(&panel.items[0], true).unwrap();
        match fwd {
            SettingsOutcome::Applied { key, value, .. } if key == "c" && value == "high" => {}
            other => panic!("expected Applied(c,high), got {:?}", other),
        }
    }

    /// Text-kind rows emit RequestExternalEdit on activate (delegated to caller).
    #[test]
    fn text_kind_requests_external_edit() {
        use crate::SettingsItem;
        let item = SettingsItem {
            key: "x".into(),
            label: "X".into(),
            desc: "d".into(),
            current_raw: "v".into(),
            display_value: "v".into(),
            default_raw: String::new(),
            category_index: 0,
            kind: SettingsValueKind::Text,
            options: vec![],
            changed: false,
            restart_required: false,
        };
        let panel = SettingsPanel::new("t", vec![cat("A", Color::Cyan)], vec![item]);
        match panel.activate_current().unwrap() {
            SettingsOutcome::RequestExternalEdit { key, .. } if key == "x" => {}
            other => panic!("expected RequestExternalEdit, got {:?}", other),
        }
    }

    /// An empty items list renders the "no matches" branch without panic,
    /// and cursor math stays in bounds.
    #[test]
    fn empty_items_is_safe() {
        let panel: SettingsPanel = SettingsPanel::new("t", vec![cat("A", Color::Cyan)], Vec::new());
        assert!(panel.visible_indices().is_empty());
        assert!(panel.current_visible().is_none());
        assert!(panel.reset_current().is_none());
        // activate_current on empty list is a no-op (None).
        assert!(panel.activate_current().is_none());
    }

    #[test]
    fn changed_marker_detected() {
        let item = bool_item("b", "B", "true");
        assert!(item.changed);
        let item2 = bool_item("b", "B", "false");
        assert!(!item2.changed);
    }
}
