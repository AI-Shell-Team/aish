//! Composite skill manager panel — the single-screen `/skill` UI.
//!
//! A self-contained `PanelComponent` that mirrors `SettingsPanel`'s layout
//! (category chips + searchable flat list + contextual footer) but is tuned
//! for skill management across three domains that share a list shape but have
//! different action sets:
//!
//! - **Installed** — locally installed skills with trust/quarantine state.
//! - **Browse** — remote registry search; a virtual `SearchTrigger` row at the
//!   top emits a `Search` outcome so the caller runs the blocking HTTP query
//!   outside the panel's event loop, then rebuilds with the results.
//! - **Registries** — configured registry sources; `Space` toggles enable.
//!
//! Like `SettingsPanel`, this panel is free of side effects: every action
//! (install, trust, remove, registry edit) is emitted as an outcome and run by
//! the shell layer, which rebuilds the panel afterward. That keeps the
//! component unit-testable and decoupled from the skills/config crates.

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

/// Distinguishes the virtual search row (Browse tab) from real data rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillItemKind {
    /// A normal data row — `Enter` emits [`SkillOutcome::Activate`].
    Normal,
    /// The Browse-tab search row — `Enter` emits [`SkillOutcome::Search`] with
    /// the current query. Always visible within its category regardless of the
    /// filter, so the user can always re-run a search.
    SearchTrigger,
}

/// One row in the panel. The caller builds these from the live skills/config
/// state; `status` is a pre-rendered, at-a-glance state string (e.g.
/// `"✓ trusted"`, `"⚠ quarantined"`, `"1.2k installs"`, `"[enabled]"`).
#[derive(Debug, Clone)]
pub struct SkillItem {
    pub key: String,
    pub label: String,
    pub desc: String,
    /// Pre-rendered status text shown inline on the row.
    pub status: String,
    /// Color for the status text.
    pub status_color: Color,
    /// Index into the `categories` slice passed to the panel.
    pub category_index: usize,
    pub kind: SkillItemKind,
}

/// Category chip metadata. Mirrors `SettingsCategoryInfo` but kept independent
/// so this module does not depend on the settings panel.
#[derive(Debug, Clone)]
pub struct SkillCategoryInfo {
    pub label: String,
    pub icon: String,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOutcome {
    /// `Enter` on a normal row. The caller interprets the action by tab:
    /// Installed → vet/detail, Browse → install, Registries → action menu.
    Activate { key: String, active_category: usize },
    /// `Space` on a row in the toggle category (Registries). The caller flips
    /// the registry's `enabled` flag.
    Toggle { key: String, active_category: usize },
    /// `Enter` on the `SearchTrigger` row. Carries the current query so the
    /// caller can run the blocking registry search, then rebuild.
    Search {
        query: String,
        active_category: usize,
    },
    /// Esc at top level, Ctrl+C, or Ctrl+Q.
    Cancelled { active_category: usize },
}

impl SkillOutcome {
    /// The chip active when this outcome was produced. The caller restores the
    /// cursor's chip across panel re-opens.
    pub fn active_category(&self) -> usize {
        match self {
            SkillOutcome::Activate {
                active_category, ..
            }
            | SkillOutcome::Toggle {
                active_category, ..
            }
            | SkillOutcome::Search {
                active_category, ..
            }
            | SkillOutcome::Cancelled { active_category } => *active_category,
        }
    }
}

/// The composite panel itself.
#[derive(Debug, Clone)]
pub struct SkillPanel {
    title: String,
    categories: Vec<SkillCategoryInfo>,
    items: Vec<SkillItem>,
    /// Active chip index (0-based into `categories`; there is no "All" chip).
    active_category: usize,
    query: String,
    selected: usize,
    /// Category whose rows support `Space` toggle (Registries). `None` means
    /// `Space` is always a search character.
    toggle_category: Option<usize>,
    search_placeholder: String,
    /// Hint shown on the Browse search row when the query is empty.
    search_hint_empty: String,
    /// Hint shown on the Browse search row when a query is present; `{query}`
    /// is replaced with the live query at render time.
    search_hint_query: String,
    /// Suffix appended to the title when the list is filtered; `{total}` is
    /// replaced with the unfiltered count.
    filtered_suffix: String,
    /// One footer line per category; indexed by `active_category`.
    tab_footers: Vec<String>,
    /// One empty-state hint per category.
    tab_empty_hints: Vec<String>,
    error_msg: Option<String>,
}

impl SkillPanel {
    pub fn new(
        title: impl Into<String>,
        categories: Vec<SkillCategoryInfo>,
        items: Vec<SkillItem>,
    ) -> Self {
        let n = categories.len();
        Self {
            title: title.into(),
            categories,
            items,
            active_category: 0,
            query: String::new(),
            selected: 0,
            toggle_category: None,
            search_placeholder: "Type to filter…".to_string(),
            search_hint_empty: "Type a query above, then Enter here to search registries."
                .to_string(),
            search_hint_query: "Enter to search registries for: \"{query}\"".to_string(),
            filtered_suffix: "  · filtered from {total}".to_string(),
            tab_footers: vec![String::new(); n],
            tab_empty_hints: vec![String::new(); n],
            error_msg: None,
        }
    }

    pub fn with_search_placeholder(mut self, s: impl Into<String>) -> Self {
        self.search_placeholder = s.into();
        self
    }

    /// Set the search-row hints (Browse tab). `query_hint` may contain
    /// `{query}`, replaced with the live query at render time.
    pub fn with_search_hints(
        mut self,
        empty: impl Into<String>,
        query_hint: impl Into<String>,
    ) -> Self {
        self.search_hint_empty = empty.into();
        self.search_hint_query = query_hint.into();
        self
    }

    /// Set the title suffix shown when the list is filtered. May contain
    /// `{total}`, replaced with the unfiltered row count at render time.
    pub fn with_filtered_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.filtered_suffix = suffix.into();
        self
    }

    /// Set the footer hint line for a given category index.
    pub fn with_tab_footer(mut self, category_index: usize, s: impl Into<String>) -> Self {
        if category_index < self.tab_footers.len() {
            self.tab_footers[category_index] = s.into();
        }
        self
    }

    /// Set the empty-state hint for a given category index.
    pub fn with_tab_empty_hint(mut self, category_index: usize, s: impl Into<String>) -> Self {
        if category_index < self.tab_empty_hints.len() {
            self.tab_empty_hints[category_index] = s.into();
        }
        self
    }

    /// Mark which category supports `Space` toggle (Registries).
    pub fn with_toggle_category(mut self, category_index: usize) -> Self {
        self.toggle_category = Some(category_index);
        self
    }

    /// Pre-fill the filter/search query (e.g. `/skill search grep`).
    pub fn with_query(mut self, q: impl Into<String>) -> Self {
        self.query = q.into();
        self.clamp_selected();
        self
    }

    /// Restore the active chip across panel re-opens.
    pub fn with_active_category(mut self, idx: usize) -> Self {
        self.set_active_category(idx);
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

    /// Show a transient red error line below the list (cleared on next key).
    pub fn with_error(mut self, msg: Option<String>) -> Self {
        self.error_msg = msg.filter(|m| !m.trim().is_empty());
        self
    }

    pub fn active_category(&self) -> usize {
        self.active_category
    }

    pub fn set_active_category(&mut self, idx: usize) {
        if idx < self.categories.len() {
            self.active_category = idx;
            self.selected = 0;
        }
    }

    // ------------------------------------------------------------------
    // Filtering / cursor helpers
    // ------------------------------------------------------------------

    fn item_matches_query(&self, idx: usize) -> bool {
        let item = &self.items[idx];
        // The search row is always query-relevant — it reflects the query.
        if item.kind == SkillItemKind::SearchTrigger {
            return true;
        }
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        let hay =
            format!("{} {} {} {}", item.label, item.desc, item.key, item.status).to_lowercase();
        q.split_whitespace().all(|tok| hay.contains(tok))
    }

    fn item_matches_filter(&self, idx: usize) -> bool {
        let item = &self.items[idx];
        if item.category_index != self.active_category {
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
        let n = self.categories.len();
        if n == 0 {
            return;
        }
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

    fn activate_current(&self) -> Option<SkillOutcome> {
        let idx = self.current_visible()?;
        let item = &self.items[idx];
        if item.kind == SkillItemKind::SearchTrigger {
            Some(SkillOutcome::Search {
                query: self.query.clone(),
                active_category: self.active_category,
            })
        } else {
            Some(SkillOutcome::Activate {
                key: item.key.clone(),
                active_category: self.active_category,
            })
        }
    }

    fn toggle_current(&self) -> Option<SkillOutcome> {
        let idx = self.current_visible()?;
        let item = &self.items[idx];
        if item.kind == SkillItemKind::SearchTrigger {
            return None;
        }
        Some(SkillOutcome::Toggle {
            key: item.key.clone(),
            active_category: self.active_category,
        })
    }

    fn push_query(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.clamp_selected();
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

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
        let total: usize = self
            .items
            .iter()
            .filter(|i| i.category_index == self.active_category)
            .count();
        let shown = self.visible_indices().len();
        let title = format!("{}  ({})", self.title, shown);
        let mut spans = vec![Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )];
        if shown < total {
            spans.push(Span::styled(
                self.filtered_suffix
                    .replacen("{total}", &total.to_string(), 1),
                Style::default().fg(Color::DarkGray),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_chips(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        if area.width < 8 || self.categories.is_empty() {
            return;
        }
        // Count per category among query-matching items, ignoring the
        // active-category filter so non-active chips still show their totals.
        let mut counts = vec![0usize; self.categories.len()];
        for i in 0..self.items.len() {
            if !self.item_matches_query(i) {
                continue;
            }
            let ci = self.items[i].category_index;
            if ci < counts.len() {
                counts[ci] += 1;
            }
        }

        let mut chips: Vec<(String, Style, usize)> = Vec::with_capacity(self.categories.len());
        for (i, cat) in self.categories.iter().enumerate() {
            let active = self.active_category == i;
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
            let body = format!(" {} {}({})  ", cat.icon, cat.label, counts[i]);
            let w = body.width();
            chips.push((body, style, w));
        }
        // `chips` is 1:1 with `categories`, and the early return above guards
        // the empty case — but saturating_sub keeps this panic-free if that
        // guard is ever refactored away.
        let active_idx = self.active_category.min(chips.len().saturating_sub(1));

        // Window around the active chip, expanding outward to fit.
        let area_w = area.width as usize;
        let mut first = active_idx;
        let mut last = active_idx;
        let mut w = chips[active_idx].2;
        while first > 0 {
            let nw = w + chips[first - 1].2;
            if nw > area_w {
                break;
            }
            first -= 1;
            w = nw;
        }
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
            "filter ❯ ",
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
            spans.push(Span::styled("▏", Style::default().fg(Color::Cyan)));
        }
        let line = truncate_line(spans, area.width as usize);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_entries(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        let entries = self.visible_indices();
        if entries.is_empty() {
            let hint = self
                .tab_empty_hints
                .get(self.active_category)
                .filter(|h| !h.is_empty())
                .map(|s| s.as_str())
                .unwrap_or("No matches — press Esc to clear the filter.");
            frame.render_widget(
                Paragraph::new(format!("  {hint}")).style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

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
            let item_idx = entries[row];
            let selected = row == self.selected;
            let line1 = self.render_item_line(item_idx, selected, row, scroll, end, entries.len());
            frame.render_widget(Paragraph::new(line1), Rect::new(area.x, y, area.width, 1));
            y = y.saturating_add(1);
            if y >= bottom {
                break;
            }
            let item = &self.items[item_idx];
            let desc_style = if selected {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            // The SearchTrigger row renders the live query as its description
            // instead of the static field, so the user sees what will be sent.
            let budget = (area.width as usize).saturating_sub(DESCRIPTION_INDENT.width());
            let desc = if item.kind == SkillItemKind::SearchTrigger {
                let q = self.query.trim();
                let hint = if q.is_empty() {
                    self.search_hint_empty.clone()
                } else {
                    self.search_hint_query.replacen("{query}", q, 1)
                };
                truncate_str(&hint, budget)
            } else {
                truncate_str(&item.desc, budget)
            };
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

        let cat = self.categories.get(item.category_index);
        let icon_style = if selected {
            Style::default()
                .fg(cat.map(|c| c.color).unwrap_or(Color::Cyan))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(cat.map(|c| c.color).unwrap_or(Color::DarkGray))
        };
        // SearchTrigger row uses a magnifier glyph regardless of category icon.
        let icon = if item.kind == SkillItemKind::SearchTrigger {
            "🔍"
        } else {
            cat.map(|c| c.icon.as_str()).unwrap_or("·")
        };

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

        // Inline status (colored by the caller-chosen status_color).
        if !item.status.is_empty() {
            let status_style = if selected {
                Style::default()
                    .fg(item.status_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(item.status_color)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(item.status.as_str(), status_style));
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
            let line = Line::from(vec![
                Span::styled("↳ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_str(&item.desc, (area.width as usize).saturating_sub(3)),
                    Style::default().fg(Color::Gray),
                ),
            ]);
            frame.render_widget(Paragraph::new(line), area);
        }
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = padded_area(area);
        let footer = self
            .tab_footers
            .get(self.active_category)
            .filter(|f| !f.is_empty())
            .map(|s| s.as_str())
            .unwrap_or("↑↓ move · ←/→ category · Enter select · Esc exit");
        frame.render_widget(
            Paragraph::new(truncate_str(footer, area.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }
}

impl PanelComponent for SkillPanel {
    type Output = SkillOutcome;

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        let rows = self
            .items
            .iter()
            .filter(|i| i.category_index == self.active_category)
            .count()
            .clamp(MIN_LIST_ROWS, 10);
        let h = (rows as u16) * 2 + self.chrome_lines();
        // Apply the upper bound first, then the minimum. A plain
        // `h.clamp(MIN_PANEL_HEIGHT, max_h)` panics when max_h < MIN_PANEL_HEIGHT
        // (tiny terminals), since u16::clamp requires min <= max.
        let max_h = terminal_height.max(1);
        h.max(MIN_PANEL_HEIGHT).min(max_h)
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
                    PanelEvent::Submit(SkillOutcome::Cancelled {
                        active_category: self.active_category,
                    })
                }
            }
            KeyCode::Char('c') | KeyCode::Char('q')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                PanelEvent::Submit(SkillOutcome::Cancelled {
                    active_category: self.active_category,
                })
            }
            KeyCode::Tab | KeyCode::Right => {
                self.switch_category(1);
                PanelEvent::Continue
            }
            KeyCode::BackTab | KeyCode::Left => {
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
                // Space toggles on the configured toggle category; otherwise it
                // is a search/filter character.
                if self.toggle_category == Some(self.active_category) {
                    self.toggle_current()
                        .map(PanelEvent::Submit)
                        .unwrap_or_else(|| {
                            self.push_query(' ');
                            PanelEvent::Continue
                        })
                } else {
                    self.push_query(' ');
                    PanelEvent::Continue
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.clamp_selected();
                PanelEvent::Continue
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(label: &str, color: Color) -> SkillCategoryInfo {
        SkillCategoryInfo {
            label: label.into(),
            icon: "◆".into(),
            color,
        }
    }

    fn row(key: &str, label: &str, ci: usize) -> SkillItem {
        SkillItem {
            key: key.into(),
            label: label.into(),
            desc: "desc".into(),
            status: String::new(),
            status_color: Color::Gray,
            category_index: ci,
            kind: SkillItemKind::Normal,
        }
    }

    fn search_row(ci: usize) -> SkillItem {
        SkillItem {
            key: "__search__".into(),
            label: "Search registries".into(),
            desc: String::new(),
            status: String::new(),
            status_color: Color::Gray,
            category_index: ci,
            kind: SkillItemKind::SearchTrigger,
        }
    }

    fn panel(items: &[SkillItem]) -> SkillPanel {
        SkillPanel::new(
            "Skills",
            vec![
                cat("Installed", Color::Green),
                cat("Browse", Color::Blue),
                cat("Registries", Color::Magenta),
            ],
            items.to_vec(),
        )
    }

    #[test]
    fn visible_indices_respects_category() {
        let p = panel(&[
            row("a", "alpha", 0),
            row("b", "bravo", 1),
            row("c", "charlie", 0),
        ]);
        // Default category 0 → only rows in category 0.
        let vis: Vec<String> = p
            .visible_indices()
            .into_iter()
            .map(|i| p.items[i].key.clone())
            .collect();
        assert_eq!(vis, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn query_filters_rows() {
        let mut p = panel(&[row("a", "alpha", 0), row("c", "charlie", 0)]);
        p.query = "char".to_string();
        let vis: Vec<String> = p
            .visible_indices()
            .into_iter()
            .map(|i| p.items[i].key.clone())
            .collect();
        assert_eq!(vis, vec!["c".to_string()]);
    }

    #[test]
    fn search_trigger_row_survives_filter() {
        let mut p = panel(&[search_row(1), row("grep", "grep skill", 1)]);
        p.set_active_category(1);
        p.query = "nomatch".to_string();
        let vis: Vec<String> = p
            .visible_indices()
            .into_iter()
            .map(|i| p.items[i].key.clone())
            .collect();
        // Search row is always visible; the real row is filtered out.
        assert_eq!(vis, vec!["__search__".to_string()]);
    }

    #[test]
    fn activate_on_search_row_emits_search() {
        let mut p = panel(&[search_row(1), row("grep", "grep", 1)]);
        p.set_active_category(1);
        p.query = "grep".to_string();
        // Cursor on the search row (index 0).
        assert_eq!(p.selected, 0);
        let outcome = p.activate_current().unwrap();
        match outcome {
            SkillOutcome::Search { query, .. } => assert_eq!(query, "grep"),
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn activate_on_data_row_emits_activate() {
        let mut p = panel(&[search_row(1), row("grep", "grep", 1)]);
        p.set_active_category(1);
        p.selected = 1; // data row
        let outcome = p.activate_current().unwrap();
        match outcome {
            SkillOutcome::Activate { key, .. } => assert_eq!(key, "grep"),
            other => panic!("expected Activate, got {other:?}"),
        }
    }

    #[test]
    fn space_toggles_only_on_toggle_category() {
        let mut p = panel(&[row("skills_sh", "skills.sh", 2)]).with_toggle_category(2);
        p.set_active_category(2);
        assert_eq!(p.selected, 0);
        let outcome = p.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        )));
        match outcome {
            PanelEvent::Submit(SkillOutcome::Toggle { key, .. }) => assert_eq!(key, "skills_sh"),
            other => panic!("expected Toggle, got {other:?}"),
        }

        // On a non-toggle category, Space is a filter character.
        let mut p2 = panel(&[row("a", "alpha", 0)]).with_toggle_category(2);
        p2.set_active_category(0);
        let outcome = p2.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        )));
        assert!(matches!(outcome, PanelEvent::Continue));
        assert_eq!(p2.query, " ");
    }

    #[test]
    fn esc_clears_query_first_then_cancels() {
        let mut p = panel(&[row("a", "alpha", 0)]);
        p.query = "xyz".to_string();
        let outcome = p.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(matches!(outcome, PanelEvent::Continue));
        assert!(p.query.is_empty());

        let outcome = p.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(matches!(
            outcome,
            PanelEvent::Submit(SkillOutcome::Cancelled { .. })
        ));
    }

    #[test]
    fn switch_category_wraps_around() {
        let p = panel(&[row("a", "a", 0), row("b", "b", 1), row("c", "c", 2)]);
        let mut p = p;
        assert_eq!(p.active_category, 0);
        p.switch_category(-1); // wrap to last
        assert_eq!(p.active_category, 2);
        p.switch_category(1); // wrap to first
        assert_eq!(p.active_category, 0);
    }

    #[test]
    fn with_selected_key_positions_cursor() {
        let p = panel(&[row("a", "a", 0), row("b", "b", 0), row("c", "c", 0)])
            .with_selected_key(Some("b"));
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn desired_height_never_panics_on_tiny_terminal() {
        // Regression for the u16 clamp panic: terminal heights below
        // MIN_PANEL_HEIGHT made `clamp(MIN_PANEL_HEIGHT, max_h)` panic with
        // min > max. The max/min ordering now keeps all heights safe.
        let panel = SkillPanel::new("t", Vec::new(), Vec::new());
        for h in 1u16..=16 {
            let _ = panel.desired_height(80, h);
        }
    }
}
