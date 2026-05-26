use std::io;
use std::path::Path;

use aish_i18n::t;
use aish_session::SessionRecord;
use aish_ui::{
    PanelOutcome, PanelRuntime, SearchSelectItem, SearchSelectOutcome, SearchSelectPanel,
};
use chrono::{DateTime, Utc};
use unicode_width::UnicodeWidthStr;

const MAX_VISIBLE_ITEMS: usize = 8;

#[derive(Debug, Clone)]
pub struct ResumeSessionItem {
    pub session_id: String,
    pub title: String,
    pub detail: String,
    pub search_text: String,
    pub project: String,
    pub is_current: bool,
}

impl ResumeSessionItem {
    pub fn from_record(record: &SessionRecord, current_session_id: &str) -> Self {
        let snapshot = record.state_snapshot();
        let cwd = snapshot.cwd.as_deref().unwrap_or("-");
        let title = snapshot
            .summary_preview
            .as_deref()
            .filter(|summary| !summary.trim().is_empty())
            .map(|summary| truncate_display(summary, 72))
            .unwrap_or_else(|| short_session_id(&record.session_uuid));
        let updated_at = snapshot.updated_at.unwrap_or(record.created_at);
        let size = session_size_label(record);
        let detail = format!(
            "{} · {} · {} · {}",
            relative_time(updated_at),
            record.model,
            size,
            truncate_display(cwd, 64)
        );
        let project = project_name(cwd);
        let search_text = format!(
            "{} {} {} {} {}",
            record.session_uuid, title, record.model, cwd, project
        )
        .to_lowercase();

        Self {
            session_id: record.session_uuid.clone(),
            title,
            detail,
            search_text,
            project,
            is_current: record.session_uuid == current_session_id,
        }
    }
}

pub fn select_resume_session(items: &[ResumeSessionItem]) -> io::Result<Option<String>> {
    if items.is_empty() {
        return Ok(None);
    }

    let panel_items = items.iter().map(to_search_select_item).collect();
    let panel = SearchSelectPanel::new(
        t("shell.resume.selector_title"),
        t("shell.resume.search_placeholder"),
        panel_items,
    )
    .with_empty_message(t("shell.resume.no_matches"))
    .with_footer(t("shell.resume.selector_footer"))
    .with_max_visible_items(MAX_VISIBLE_ITEMS);

    match PanelRuntime::new().run(panel).map_err(io::Error::other)? {
        PanelOutcome::Submitted(SearchSelectOutcome::Selected(session_id)) => Ok(Some(session_id)),
        PanelOutcome::Cancelled => Ok(None),
    }
}

fn to_search_select_item(item: &ResumeSessionItem) -> SearchSelectItem {
    let mut select_item = SearchSelectItem::new(item.session_id.clone(), item.title.clone())
        .with_detail(item.detail.clone())
        .with_search_text(item.search_text.clone());
    if item.is_current {
        select_item = select_item.with_badge(t("shell.resume.current_marker"));
    }
    select_item
}

fn relative_time(updated_at: DateTime<Utc>) -> String {
    let elapsed = Utc::now().signed_duration_since(updated_at);
    if elapsed.num_seconds() < 60 {
        "now".to_string()
    } else if elapsed.num_minutes() < 60 {
        format!("{}m ago", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h ago", elapsed.num_hours())
    } else {
        format!("{}d ago", elapsed.num_days())
    }
}

fn session_size_label(record: &SessionRecord) -> String {
    let bytes = record.state.to_string().len();
    if bytes < 1024 {
        format!("{}B", bytes)
    } else {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    }
}

fn short_session_id(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("sessions")
        .to_string()
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
    use super::*;

    #[test]
    fn converts_resume_item_to_search_select_item() {
        let item = ResumeSessionItem {
            session_id: "abc123".to_string(),
            title: "deploy docs".to_string(),
            detail: "now · model · 1KB".to_string(),
            search_text: "abc123 deploy docs".to_string(),
            project: "aish".to_string(),
            is_current: false,
        };

        let select_item = to_search_select_item(&item);

        assert_eq!(select_item.value, "abc123");
        assert_eq!(select_item.label, "deploy docs");
        assert_eq!(select_item.detail.as_deref(), Some("now · model · 1KB"));
        assert_eq!(select_item.search_text, "abc123 deploy docs");
    }
}
