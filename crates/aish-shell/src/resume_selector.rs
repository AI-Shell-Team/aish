use std::io::{self, Write};
use std::path::Path;

use aish_i18n::t;
use aish_session::SessionRecord;
use chrono::{DateTime, Utc};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute, queue,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use unicode_width::UnicodeWidthStr;

const MAX_VISIBLE_ITEMS: usize = 8;
const PANEL_RESERVED_LINES: usize = 10;

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

    let _guard = RawModeGuard::enter()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        cursor::MoveTo(0, 0),
        cursor::SavePosition,
        cursor::Hide
    )?;

    let mut query = String::new();
    let mut selected = 0usize;
    let result = loop {
        let filtered = filtered_indices(items, &query);
        if selected >= filtered.len() {
            selected = filtered.len().saturating_sub(1);
        }
        render(&mut stdout, items, &filtered, selected, &query)?;

        match event::read()? {
            Event::Key(key) => match key.code {
                KeyCode::Esc => break None,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break None,
                KeyCode::Enter => {
                    if let Some(item_index) = filtered.get(selected) {
                        break Some(items[*item_index].session_id.clone());
                    }
                }
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    if selected + 1 < filtered.len() {
                        selected += 1;
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                }
                KeyCode::Char(ch) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        query.push(ch);
                        selected = 0;
                    }
                }
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    };

    execute!(
        stdout,
        cursor::RestorePosition,
        terminal::Clear(terminal::ClearType::FromCursorDown),
        LeaveAlternateScreen,
        cursor::Show
    )?;
    write_blank_line(&mut stdout)?;
    Ok(result)
}

fn render(
    stdout: &mut io::Stdout,
    items: &[ResumeSessionItem],
    filtered: &[usize],
    selected: usize,
    query: &str,
) -> io::Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((100, 24));
    let width = cols.saturating_sub(4).max(40) as usize;
    let visible_limit = visible_item_limit(rows as usize);
    let visible_count = filtered.len().min(visible_limit);

    queue!(
        stdout,
        cursor::RestorePosition,
        terminal::Clear(terminal::ClearType::FromCursorDown)
    )?;
    write_line(
        stdout,
        format_args!("\x1b[36m{}\x1b[0m", t("shell.resume.selector_title")),
    )?;
    write_blank_line(stdout)?;
    write_line(
        stdout,
        format_args!("┌{}┐", "─".repeat(width.saturating_sub(2))),
    )?;
    let search = if query.is_empty() {
        format!("⌕ {}", t("shell.resume.search_placeholder"))
    } else {
        format!("⌕ {query}")
    };
    write_line(
        stdout,
        format_args!("│{}│", pad_or_truncate(&search, width.saturating_sub(2))),
    )?;
    write_line(
        stdout,
        format_args!("└{}┘", "─".repeat(width.saturating_sub(2))),
    )?;
    write_blank_line(stdout)?;

    if filtered.is_empty() {
        write_line(
            stdout,
            format_args!("  \x1b[2m{}\x1b[0m", t("shell.resume.no_matches")),
        )?;
    } else {
        let start = selected.saturating_sub(MAX_VISIBLE_ITEMS.saturating_sub(1));
        let end = (start + visible_count).min(filtered.len());
        let project = filtered
            .get(selected)
            .and_then(|index| items.get(*index))
            .map(|item| item.project.as_str())
            .unwrap_or("sessions");
        write_line(
            stdout,
            format_args!(
                "  \x1b[2m{}\x1b[0m",
                truncate_display(project, width.saturating_sub(4))
            ),
        )?;
        write_blank_line(stdout)?;

        for (visible_index, item_index) in filtered[start..end].iter().enumerate() {
            let absolute_index = start + visible_index;
            let item = &items[*item_index];
            let marker = if absolute_index == selected { ">" } else { " " };
            let current = if item.is_current {
                format!(" {}", t("shell.resume.current_marker"))
            } else {
                String::new()
            };
            let title = truncate_display(
                &format!("{}{}", item.title, current),
                width.saturating_sub(4),
            );
            let detail = truncate_display(&item.detail, width.saturating_sub(4));
            if absolute_index == selected {
                write_line(stdout, format_args!("\x1b[94m{} {}\x1b[0m", marker, title))?;
            } else {
                write_line(stdout, format_args!("{} {}", marker, title))?;
            }
            write_line(stdout, format_args!("  \x1b[2m{}\x1b[0m", detail))?;
        }
    }

    write_blank_line(stdout)?;
    write_line(
        stdout,
        format_args!(
            "\x1b[2m{}\x1b[0m",
            truncate_display(&t("shell.resume.selector_footer"), width)
        ),
    )?;
    stdout.flush()
}

fn write_line(stdout: &mut io::Stdout, args: std::fmt::Arguments<'_>) -> io::Result<()> {
    stdout.write_fmt(args)?;
    stdout.write_all(b"\r\n")
}

fn write_blank_line(stdout: &mut io::Stdout) -> io::Result<()> {
    stdout.write_all(b"\r\n")
}

fn filtered_indices(items: &[ResumeSessionItem], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..items.len()).collect();
    }

    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.search_text.contains(&query).then_some(index))
        .collect()
}

fn visible_item_limit(rows: usize) -> usize {
    let available = rows.saturating_sub(PANEL_RESERVED_LINES);
    let limit = available / 2;
    limit.clamp(1, MAX_VISIBLE_ITEMS)
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

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated = truncate_display(value, width);
    let padding = width.saturating_sub(UnicodeWidthStr::width(truncated.as_str()));
    format!("{}{}", truncated, " ".repeat(padding))
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

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_title_and_session_id() {
        let items = vec![
            ResumeSessionItem {
                session_id: "abc123".to_string(),
                title: "deploy docs".to_string(),
                detail: "now".to_string(),
                search_text: "abc123 deploy docs".to_string(),
                project: "aish".to_string(),
                is_current: false,
            },
            ResumeSessionItem {
                session_id: "def456".to_string(),
                title: "fix parser".to_string(),
                detail: "now".to_string(),
                search_text: "def456 fix parser".to_string(),
                project: "aish".to_string(),
                is_current: false,
            },
        ];

        assert_eq!(filtered_indices(&items, "parser"), vec![1]);
        assert_eq!(filtered_indices(&items, "abc"), vec![0]);
        assert_eq!(filtered_indices(&items, ""), vec![0, 1]);
    }

    #[test]
    fn visible_limit_respects_terminal_height() {
        assert_eq!(visible_item_limit(24), 7);
        assert_eq!(visible_item_limit(40), 8);
        assert_eq!(visible_item_limit(10), 1);
    }
}
