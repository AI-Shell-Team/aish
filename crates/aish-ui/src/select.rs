use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{PanelComponent, PanelEvent};

const DEFAULT_VISIBLE_ITEMS: usize = 32;
const MIN_LIST_ROWS: usize = 5;
const MIN_PANEL_HEIGHT: u16 = 6;
const PANEL_PADDING_X: u16 = 2;
const DESCRIPTION_INDENT: &str = "    ";

// Shimmer sweep tunables (mirrors aish-shell's theme::shimmer_text): a cosine
// bump band travels left→right across the highlighted row's text.
const SHIMMER_SPEED: f64 = 30.0; // cells per second
const SHIMMER_PADDING: f64 = 10.0; // virtual padding for smooth enter/exit
const SHIMMER_BAND_HALF: f64 = 6.0; // half-width of the cosine bump
const SHIMMER_TICK_MS: u64 = 33; // ~30fps animation tick

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSelectItem {
    pub value: String,
    pub label: String,
    /// Optional emphasized prefix rendered in bold accent color before the
    /// label (e.g. a session's custom name), so it stands out at a glance.
    pub highlight: Option<String>,
    pub detail: Option<String>,
    pub badge: Option<String>,
    pub search_text: String,
    /// Whether Ctrl+E rename is allowed for this item (default: false).
    pub renamable: bool,
}

impl SearchSelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        let value = value.into();
        let label = label.into();
        let search_text = format!("{value} {label}").to_lowercase();
        Self {
            value,
            label,
            highlight: None,
            detail: None,
            badge: None,
            search_text,
            renamable: false,
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

    /// Set the emphasized prefix rendered in bold accent color before the
    /// label. Used to make a key field (e.g. a custom session name) visually
    /// distinct from the rest of the label.
    pub fn with_highlight(mut self, highlight: impl Into<String>) -> Self {
        let highlight = highlight.into();
        self.highlight = (!highlight.trim().is_empty()).then_some(highlight);
        self
    }

    pub fn with_search_text(mut self, search_text: impl Into<String>) -> Self {
        self.search_text = search_text.into().to_lowercase();
        self
    }
    /// Allow Ctrl+E rename for this item.
    pub fn with_renamable(mut self) -> Self {
        self.renamable = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSelectOutcome {
    Selected(String),
    /// Rename the item carrying the given value (triggered by Ctrl+E).
    Rename(String),
    /// Exit the panel (triggered by Ctrl+Q). Callers decide exit semantics.
    Quit,
    /// A configured single-key action was pressed (e.g. 'a' add, 'd' delete).
    /// Action keys are intercepted before the search buffer.
    Action(char, String),
}

/// Relevance score for `query` against `search_text`: every whitespace-separated
/// token must be a substring (AND), but exact matches beat prefix matches beat
/// substring matches, so the most relevant entry floats to the top. Returns
/// `None` when any token is absent.
fn match_score(search_text: &str, query: &str) -> Option<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let st = search_text.to_lowercase();
    let mut total = 0;
    for token in query.split_whitespace() {
        if st == token {
            total += 100;
        } else if st.starts_with(token) {
            total += 50;
        } else if st.contains(token) {
            total += 10;
        } else {
            return None;
        }
    }
    Some(total)
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
    /// Value of the item whose row gets the animated shimmer sweep (the live
    /// "current" session). `None` disables animation.
    shimmer_value: Option<String>,
    /// Animation clock in milliseconds, advanced one tick per redraw.
    anim_ms: u64,
    /// Configured single-key actions (key, label). These keys are intercepted
    /// before the search buffer so they trigger `Action(char)`, not typing.
    actions: Vec<(char, String)>,
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
            shimmer_value: None,
            actions: Vec::new(),
            anim_ms: 0,
        }
    }

    /// Register a single-key action. The key is intercepted before the search
    /// buffer and emitted as `SearchSelectOutcome::Action(char)`.
    pub fn with_action(mut self, key: char, label: impl Into<String>) -> Self {
        self.actions.push((key.to_ascii_lowercase(), label.into()));
        self
    }

    pub fn with_footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Mark the item with `value` as the "current" row that gets an animated
    /// shimmer sweep. Pass `None` (or omit) for a static panel.
    pub fn with_shimmer(mut self, value: Option<&str>) -> Self {
        self.shimmer_value = value.map(|v| v.to_string());
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
        let query = self.query.trim();
        if query.is_empty() {
            (0..self.items.len()).map(SelectEntry::Item).collect()
        } else {
            let mut scored: Vec<(usize, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    match_score(&item.search_text, query).map(|s| (index, s))
                })
                .collect();
            // Highest score first; ties keep original order for stability.
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            scored
                .into_iter()
                .map(|(i, _)| SelectEntry::Item(i))
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

    /// Build a `Rename` outcome for the currently highlighted entry.
    /// Returns `None` when no entry is highlighted (empty list), so callers
    /// can treat Ctrl+E on an empty list as a no-op.
    fn rename_current(&self) -> Option<SearchSelectOutcome> {
        let entries = self.filtered_entries();
        let entry = entries.get(self.selected)?;
        match entry {
            SelectEntry::Item(index) => {
                let item = self.items.get(*index)?;
                if !item.renamable {
                    return None;
                }
                Some(SearchSelectOutcome::Rename(item.value.clone()))
            }
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

    fn chrome_lines(&self) -> u16 {
        // divider + title + search + footer (+ optional subtitle)
        4 + u16::from(self.subtitle.is_some())
    }

    fn effective_max_visible_items(&self, terminal_height: u16) -> usize {
        let row_height = self.estimated_entry_height().max(1) as usize;
        let budget =
            (terminal_height as usize).saturating_sub(self.chrome_lines() as usize) / row_height;
        budget.max(MIN_LIST_ROWS).min(self.max_visible_items)
    }

    fn visible_limit(&self, list_height: usize) -> usize {
        let row_height = self.estimated_entry_height() as usize;
        (list_height / row_height).max(1)
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
        if self
            .items
            .iter()
            .any(|item| item.detail.as_ref().is_some_and(|d| !d.trim().is_empty()))
        {
            2
        } else {
            1
        }
    }

    fn entry_detail_lines<'a>(&self, item: &'a SearchSelectItem) -> Vec<Line<'a>> {
        let detail_style = Style::default().fg(Color::DarkGray);
        let Some(detail) = &item.detail else {
            return Vec::new();
        };
        if detail.trim().is_empty() {
            return Vec::new();
        }

        vec![Line::from(vec![
            Span::raw(DESCRIPTION_INDENT),
            Span::styled(detail.as_str(), detail_style),
        ])]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectEntry {
    Item(usize),
}

impl PanelComponent for SearchSelectPanel {
    type Output = SearchSelectOutcome;

    fn desired_height(&self, _terminal_width: u16, terminal_height: u16) -> u16 {
        let max_visible = self.effective_max_visible_items(terminal_height);
        let visible_count = self.items.len().min(max_visible).max(1);
        let list_height = visible_count as u16 * self.estimated_entry_height();
        (list_height + self.chrome_lines()).clamp(MIN_PANEL_HEIGHT, terminal_height.max(1))
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

    fn tick_interval(&self) -> Option<Duration> {
        // Only animate when a shimmer row is configured, so static panels
        // still block on input (no busy redraw loop).
        self.shimmer_value
            .as_ref()
            .map(|_| Duration::from_millis(SHIMMER_TICK_MS))
    }

    fn tick(&mut self) {
        self.anim_ms = self.anim_ms.wrapping_add(SHIMMER_TICK_MS);
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
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => self
                .rename_current()
                .map(PanelEvent::Submit)
                .unwrap_or(PanelEvent::Continue),
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PanelEvent::Submit(SearchSelectOutcome::Quit)
            }
            KeyCode::Up => {
                self.move_up(1);
                PanelEvent::Continue
            }
            KeyCode::Down => {
                self.move_down(1);
                PanelEvent::Continue
            }
            KeyCode::PageUp => {
                let page = terminal::size()
                    .map(|(_, rows)| self.effective_max_visible_items(rows))
                    .unwrap_or(self.max_visible_items);
                self.move_up(page);
                PanelEvent::Continue
            }
            KeyCode::PageDown => {
                let page = terminal::size()
                    .map(|(_, rows)| self.effective_max_visible_items(rows))
                    .unwrap_or(self.max_visible_items);
                self.move_down(page);
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
                let lower = ch.to_ascii_lowercase();
                // Action keys only fire on an empty query; once the user is
                // typing a search, letters must filter instead of triggering a
                // panel action (otherwise typing "model" would hit the `m`
                // manage action).
                if self.query.is_empty() {
                    if let Some((action_key, _)) = self.actions.iter().find(|(k, _)| *k == lower) {
                        let Some(value) =
                            self.filtered_entries()
                                .get(self.selected)
                                .and_then(|e| match e {
                                    SelectEntry::Item(i) => {
                                        self.items.get(*i).map(|it| it.value.clone())
                                    }
                                })
                        else {
                            return PanelEvent::Continue;
                        };
                        return PanelEvent::Submit(SearchSelectOutcome::Action(*action_key, value));
                    }
                }
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
            Paragraph::new(Line::from(vec![Span::styled(
                truncate_display(search_text, area.width as usize),
                search_style,
            )])),
            area,
        );
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        // Compose the footer from the registered single-key actions — each
        // rendered as "key label" (e.g. "a add") — followed by any explicit
        // footer hint. Actions are the non-obvious shortcuts, so they lead;
        // the explicit footer carries the conventional Esc/Enter hint. This
        // keeps the visible hints in sync with the registered actions instead
        // of duplicating them in a static footer string.
        let mut parts: Vec<String> = self
            .actions
            .iter()
            .map(|(key, label)| format!("{key} {label}"))
            .collect();
        if let Some(footer) = &self.footer {
            parts.push(footer.clone());
        }
        if parts.is_empty() {
            return;
        }
        let area = padded_area(area);
        if area.width == 0 {
            return;
        }
        let text = parts.join("  ");
        frame.render_widget(
            Paragraph::new(truncate_display(&text, area.width as usize))
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

        match entry {
            SelectEntry::Item(index) => {
                let item = &self.items[index];
                let mut spans = vec![Span::styled(marker, marker_style)];
                let is_shimmer = self
                    .shimmer_value
                    .as_deref()
                    .is_some_and(|v| v == item.value);
                if is_shimmer {
                    // Animated rainbow sweep across the row's text.
                    let text = match &item.highlight {
                        Some(h) if !h.is_empty() => format!("{h}  {}", item.label),
                        _ => item.label.clone(),
                    };
                    spans.extend(shimmer_spans(&text, self.anim_ms));
                } else {
                    if let Some(highlight) = &item.highlight {
                        spans.push(Span::styled(
                            highlight.as_str(),
                            Style::default()
                                // Accent electric blue — aish's signature colour
                                // (matches theme::accent rgb 0,180,255), bold so
                                // the note stands out from id + cwd.
                                .fg(Color::Rgb(0, 180, 255))
                                .add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::raw("  "));
                    }
                    spans.push(Span::styled(item.label.as_str(), label_style));
                }
                if let Some(badge) = &item.badge {
                    spans.push(Span::styled(
                        format!(" {badge}"),
                        Style::default().fg(Color::Yellow),
                    ));
                }
                let mut lines = vec![Line::from(spans)];
                lines.extend(self.entry_detail_lines(item));
                lines
            }
        }
    }
}

/// Build per-character ratatui spans with a cosine-bump rainbow sweep, the
/// same algorithm as `aish_shell::theme::shimmer_text` but emitted as styled
/// `Span`s (ratatui does not render raw ANSI). Each char gets its own
/// `Color::Rgb` from an HSL hue that drifts with position and time, modulated
/// by a travelling brightness bump.
fn shimmer_spans(text: &str, time_ms: u64) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut total_width = 0.0_f64;
    let char_pos: Vec<f64> = chars
        .iter()
        .map(|&ch| {
            let start = total_width;
            total_width += UnicodeWidthChar::width(ch).unwrap_or(0) as f64;
            start
        })
        .collect();
    let period = total_width + SHIMMER_PADDING * 2.0;
    let pos = ((time_ms as f64 / 1000.0) * SHIMMER_SPEED) % period;

    chars
        .iter()
        .enumerate()
        .map(|(i, &ch)| {
            let idx = char_pos[i];
            let dist = (idx + SHIMMER_PADDING - pos).abs();
            let intensity = if dist >= SHIMMER_BAND_HALF {
                0.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * dist / SHIMMER_BAND_HALF).cos())
            };
            let hue = (i as f64) * 28.0 + time_ms as f64 * 0.05;
            let sat = 0.45 + intensity * 0.45;
            let light = 0.30 + intensity * 0.42;
            let (r, g, b) = hsl_to_rgb(hue, sat, light);
            Span::styled(ch.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

/// HSL → 8-bit RGB. Ported from `aish_shell::theme` so this crate stays
/// independent of the shell layer.
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 360.0;
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |t: f64| -> f64 {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (
        (f(h + 1.0 / 3.0) * 255.0).round() as u8,
        (f(h) * 255.0).round() as u8,
        (f(h - 1.0 / 3.0) * 255.0).round() as u8,
    )
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

    fn ctrl(ch: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL))
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
    fn filters_with_tokenized_and_query() {
        let mut panel = SearchSelectPanel::new(
            "Providers",
            "Search",
            vec![
                item("openrouter", "OpenRouter", "openrouter open router"),
                item("openai", "OpenAI", "openai open ai"),
            ],
        );

        panel.handle_event(key(KeyCode::Char('o')));
        panel.handle_event(key(KeyCode::Char('p')));
        panel.handle_event(key(KeyCode::Char('e')));
        panel.handle_event(key(KeyCode::Char('n')));
        panel.handle_event(key(KeyCode::Char(' ')));
        panel.handle_event(key(KeyCode::Char('r')));
        panel.handle_event(key(KeyCode::Char('o')));
        panel.handle_event(key(KeyCode::Char('u')));
        panel.handle_event(key(KeyCode::Char('t')));
        panel.handle_event(key(KeyCode::Char('e')));
        panel.handle_event(key(KeyCode::Char('r')));

        assert_eq!(panel.filtered_entries(), vec![SelectEntry::Item(0)]);
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

    #[test]
    fn ctrl_e_emits_rename_for_selected_item() {
        let mut panel = SearchSelectPanel::new(
            "Sessions",
            "Search",
            vec![
                item("session:0", "Alpha", "session:0 alpha").with_renamable(),
                item("session:1", "Beta", "session:1 beta").with_renamable(),
            ],
        );

        // Default selection is the first item → Ctrl+E renames it.
        assert_eq!(
            panel.handle_event(ctrl('e')),
            PanelEvent::Submit(SearchSelectOutcome::Rename("session:0".to_string()))
        );

        // Move down, then Ctrl+E renames the now-highlighted second item.
        panel.handle_event(key(KeyCode::Down));
        assert_eq!(
            panel.handle_event(ctrl('e')),
            PanelEvent::Submit(SearchSelectOutcome::Rename("session:1".to_string()))
        );
    }

    #[test]
    fn ctrl_e_on_empty_list_is_noop() {
        let mut panel =
            SearchSelectPanel::new("Sessions", "Search", vec![item("a", "Alpha", "a alpha")]);
        // Filter down to nothing, then Ctrl+E must not submit.
        panel.handle_event(key(KeyCode::Char('z')));
        assert!(panel.filtered_entries().is_empty());
        assert_eq!(panel.handle_event(ctrl('e')), PanelEvent::Continue);
    }
    #[test]
    fn ctrl_e_noop_for_non_renamable_item() {
        let mut panel = SearchSelectPanel::new(
            "Sessions",
            "Search",
            vec![item("session:0", "Alpha", "session:0 alpha")],
        );
        // renamable defaults to false → Ctrl+E must be a no-op.
        assert_eq!(panel.handle_event(ctrl('e')), PanelEvent::Continue);
    }

    #[test]
    fn ctrl_q_emits_quit() {
        let mut panel = SearchSelectPanel::new(
            "Sessions",
            "Search",
            vec![item("session:0", "Alpha", "session:0 alpha")],
        );
        assert_eq!(
            panel.handle_event(ctrl('q')),
            PanelEvent::Submit(SearchSelectOutcome::Quit)
        );
    }

    #[test]
    fn shimmer_only_animates_when_configured() {
        // No shimmer configured → no tick interval (static, blocks on input).
        let panel =
            SearchSelectPanel::new("Sessions", "Search", vec![item("a", "Alpha", "a alpha")]);
        assert_eq!(panel.tick_interval(), None);

        // Shimmer configured → tick interval present, tick advances the clock.
        let mut panel = panel.with_shimmer(Some("a"));
        assert_eq!(
            panel.tick_interval(),
            Some(std::time::Duration::from_millis(SHIMMER_TICK_MS))
        );
        assert_eq!(panel.anim_ms, 0);
        panel.tick();
        assert_eq!(panel.anim_ms, SHIMMER_TICK_MS);
    }

    #[test]
    fn shimmer_spans_produces_one_span_per_char() {
        let spans = shimmer_spans("abc", 0);
        assert_eq!(spans.len(), 3);
        // Empty input → no spans.
        assert!(shimmer_spans("", 500).is_empty());
    }
}
