//! `@path` file-mention popup: a fuzzy file picker for AI-mode input.
//!
//! Opened by typing `@` while composing an AI prompt (`;` prefix). Lists
//! files and directories under the current working directory, fuzzy-filtered
//! by whatever the user types after `@`. Selecting a directory descends into
//! it (seeds the query with `dir/` and refreshes); selecting a file — or
//! pressing Enter on free-typed text — yields the path to the caller, which
//! splices it back into the readline buffer next to the `@`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crossterm::{
    cursor, event,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::slash_input::{
    drain_pending_events, longest_common_prefix, open_inline_terminal, strip_ansi, RawModeGuard,
};

/// Outcome of the file-mention session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMentionOutcome {
    /// User selected a path (relative to cwd; directories keep a trailing `/`).
    Selected(String),
    /// User cancelled (Esc / Ctrl+C / Backspace past the `@`).
    Cancelled,
}

const MAX_VISIBLE_FILES: usize = 10;
const MAX_PANEL_HEIGHT: u16 = 12;
/// Cap the recursive scan so enormous trees cannot stall the popup. Hit
/// early on projects with vendored dependencies; raised once `.gitignore`-
/// style skipping trims the obvious offenders.
const MAX_FILES_SCANNED: usize = 5000;
const MAX_SCAN_DEPTH: usize = 10;
/// Bound the filtered list; the popup scrolls within `MAX_VISIBLE_FILES`.
const MAX_FILTERED: usize = 50;

/// Build artifacts, VCS state, and dependency trees skipped by the scan.
/// Hidden directories (`.foo`) are skipped separately.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    "coverage",
    ".mypy_cache",
    ".pytest_cache",
    ".idea",
    ".vscode",
    ".sass-cache",
    "out",
    "deps",
    "_build",
];

#[derive(Debug, Clone)]
pub(crate) struct FileCandidate {
    /// Relative path from cwd with `/` separators. Directories keep a
    /// trailing `/` so display, filtering, and descent stay consistent.
    path: String,
    is_dir: bool,
}

/// Inline `@path` file-mention popup.
///
/// Renders the shell prompt + the fixed line text before `@` + the editable
/// `@query` on the first row, and a fuzzy-filtered file list below it.
pub struct FileMentionSession {
    prompt: String,
    /// Fixed text on the line before `@` (e.g. `"; analyze "`). Shown for
    /// context; not editable from within the popup.
    prefix: String,
    /// Editable text after `@`.
    query: String,
    cursor: usize,
    selected: usize,
    filtered: Vec<usize>,
    candidates: Vec<FileCandidate>,
}

impl FileMentionSession {
    /// Build a new session rooted at `cwd`. `prefix` is the text already on
    /// the line before the `@` (shown for context, not editable here).
    pub fn new(cwd: &Path, prompt: String, prefix: String) -> Self {
        let candidates = scan_files(cwd);
        let mut session = Self {
            prompt: strip_ansi(&prompt),
            prefix,
            query: String::new(),
            cursor: 0,
            selected: 0,
            filtered: Vec::new(),
            candidates,
        };
        session.update_filtered();
        session
    }

    /// Pre-fill the query after construction (e.g. when reopening from
    /// readline with text already typed after `@`).
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = query.into();
        self.cursor = self.query.len();
        self.update_filtered();
        self
    }

    /// Run the session in raw mode. Returns the outcome.
    pub fn run(mut self) -> io::Result<FileMentionOutcome> {
        let _guard = RawModeGuard::enter()?;
        // Rustyline appends a newline after returning on Cmd::Interrupt;
        // move back to the original prompt line before ratatui queries the
        // cursor position for the inline viewport.
        execute!(io::stdout(), cursor::MoveToColumn(0), cursor::MoveUp(1))?;
        // Drain stale keyboard events before ratatui issues DSR queries.
        drain_pending_events()?;
        let mut viewport_height = self.panel_height();
        let mut terminal = open_inline_terminal(viewport_height)?;

        let outcome = loop {
            let desired = self.panel_height();
            if desired != viewport_height {
                let _ = terminal.clear();
                drop(terminal);
                viewport_height = desired;
                terminal = open_inline_terminal(viewport_height)?;
            }
            let _ = terminal.autoresize();
            terminal.draw(|frame| self.render(frame, frame.area()))?;

            let event = event::read()?;
            if let Some(result) = self.handle_event(event) {
                break result;
            }
        };

        // All terminal cleanup MUST happen while still in raw mode.
        let _ = terminal.clear();
        drop(terminal);
        drop(_guard);
        let _ = io::stdout().flush();
        Ok(outcome)
    }

    /// Dispatch a keyboard event without raw-mode TUI (for integration tests).
    #[doc(hidden)]
    pub fn dispatch_event(&mut self, event: Event) -> Option<FileMentionOutcome> {
        self.handle_event(event)
    }

    fn panel_height(&self) -> u16 {
        if self.filtered.is_empty() {
            return 1;
        }
        (1 + self.filtered.len().min(MAX_VISIBLE_FILES) as u16).min(MAX_PANEL_HEIGHT)
    }

    fn update_filtered(&mut self) {
        let query = self.query.trim().trim_start_matches("./");
        let mut scored: Vec<(i64, usize)> = self
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy_score(query, &c.path).map(|s| (s, i)))
            .collect();
        // Higher score first; ties break by shorter path then alphabetical.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| self.candidates[a.1].path.cmp(&self.candidates[b.1].path))
        });
        self.filtered = scored
            .into_iter()
            .map(|(_, i)| i)
            .take(MAX_FILTERED)
            .collect();
        self.selected = 0;
    }

    /// Enter: if a directory is highlighted, descend (stay in popup); if a
    /// file is highlighted, select it; otherwise accept typed text verbatim.
    fn handle_submit(&mut self) -> Option<FileMentionOutcome> {
        if let Some(&idx) = self.filtered.get(self.selected) {
            let cand = self.candidates[idx].clone();
            if cand.is_dir {
                // Descend: seed the query with the directory path and refresh.
                self.query = cand.path;
                self.cursor = self.query.len();
                self.update_filtered();
                return None;
            }
            return Some(FileMentionOutcome::Selected(cand.path));
        }
        // No matching candidate: accept the typed query verbatim if non-empty.
        let typed = self.query.trim();
        if typed.is_empty() {
            Some(FileMentionOutcome::Cancelled)
        } else {
            Some(FileMentionOutcome::Selected(typed.to_string()))
        }
    }

    /// Tab: extend the shared prefix among filtered paths, otherwise behave
    /// like submit (descend into / select the highlighted entry).
    fn handle_tab_complete(&mut self) -> Option<FileMentionOutcome> {
        if self.filtered.is_empty() {
            return None;
        }
        if self.filtered.len() == 1 {
            return self.handle_submit();
        }
        let names: Vec<&str> = self
            .filtered
            .iter()
            .map(|&i| self.candidates[i].path.as_str())
            .collect();
        let lcp = longest_common_prefix(&names);
        if lcp.len() > self.query.len() {
            self.query = lcp.to_string();
            self.cursor = self.query.len();
            self.update_filtered();
            return None;
        }
        self.handle_submit()
    }

    fn handle_event(&mut self, event: Event) -> Option<FileMentionOutcome> {
        let Event::Key(key) = event else {
            return None;
        };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(FileMentionOutcome::Cancelled);
        }

        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
                self.update_filtered();
                None
            }
            KeyCode::Backspace => {
                if self.cursor == 0 {
                    // Backspace past `@`: cancel and hand control back to readline.
                    return Some(FileMentionOutcome::Cancelled);
                }
                let prev = self.query[..self.cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.query.drain(prev..self.cursor);
                self.cursor = prev;
                self.update_filtered();
                None
            }
            KeyCode::Up => {
                if !self.filtered.is_empty() && self.selected > 0 {
                    self.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1).min(self.filtered.len() - 1);
                }
                None
            }
            KeyCode::Enter => self.handle_submit(),
            KeyCode::Tab => self.handle_tab_complete(),
            KeyCode::Home => {
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.cursor = self.query.len();
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.query[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.query.len() {
                    self.cursor += self.query[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
                None
            }
            KeyCode::Esc => Some(FileMentionOutcome::Cancelled),
            _ => None,
        }
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let input_area = Rect::new(area.x, area.y, area.width, 1);
        let list_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        self.render_input_line(frame, input_area);
        self.render_file_list(frame, list_area);
    }

    fn render_input_line(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let prompt_style = Style::default().fg(Color::Green);
        let prefix_style = Style::default().fg(Color::White);
        let at_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let input_style = Style::default().fg(Color::White);
        let cursor_style = Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD);

        let before = &self.query[..self.cursor];
        let cursor_char = self.query[self.cursor..].chars().next();
        let after_start = self.cursor + cursor_char.map_or(0, |c| c.len_utf8());
        let after = &self.query[after_start..];

        let mut spans = vec![
            Span::styled(self.prompt.clone(), prompt_style),
            Span::styled(self.prefix.clone(), prefix_style),
            Span::styled("@", at_style),
            Span::styled(before.to_string(), input_style),
        ];
        if let Some(ch) = cursor_char {
            spans.push(Span::styled(ch.to_string(), cursor_style));
            spans.push(Span::styled(after.to_string(), input_style));
        } else {
            spans.push(Span::styled(" ", cursor_style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_file_list(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if self.filtered.is_empty() || area.height == 0 {
            return;
        }
        let max_visible = area.height as usize;
        let scroll = self.scroll_offset(max_visible);
        let end = (scroll + max_visible).min(self.filtered.len());
        let lines: Vec<Line> = self.filtered[scroll..end]
            .iter()
            .enumerate()
            .map(|(row, &idx)| {
                let absolute_row = scroll + row;
                let cand = &self.candidates[idx];
                let is_selected = absolute_row == self.selected;
                let marker = if is_selected { "▸ " } else { "  " };
                let marker_style = if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else if cand.is_dir {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(vec![
                    Span::styled(marker, marker_style),
                    Span::styled(cand.path.clone(), name_style),
                ])
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn scroll_offset(&self, max_visible: usize) -> usize {
        if self.selected >= max_visible {
            self.selected - max_visible + 1
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Directory scan + fuzzy scoring
// ---------------------------------------------------------------------------

/// Recursively scan `cwd` for files and directories, returning relative
/// paths. Hidden entries, `IGNORED_DIRS`, and symlinked directories are
/// skipped. The result is sorted by path for stable ordering.
pub(crate) fn scan_files(cwd: &Path) -> Vec<FileCandidate> {
    let mut out = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Ok(canon) = cwd.canonicalize() {
        visited.insert(canon);
    }
    scan_dir(cwd, cwd, 0, &mut out, &mut visited);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<FileCandidate>,
    visited: &mut HashSet<PathBuf>,
) {
    if depth > MAX_SCAN_DEPTH || out.len() >= MAX_FILES_SCANNED {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        if out.len() >= MAX_FILES_SCANNED {
            return;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Skip symlinks to avoid walking into unrelated trees or cycles.
        if ft.is_symlink() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if ft.is_dir() {
            if IGNORED_DIRS.contains(&name_str) {
                continue;
            }
            // Cycle guard: track canonicalized directory paths.
            if let Ok(canon) = path.canonicalize() {
                if !visited.insert(canon) {
                    continue;
                }
            }
            out.push(FileCandidate {
                path: format!("{rel}/"),
                is_dir: true,
            });
            scan_dir(root, &path, depth + 1, out, visited);
        } else if ft.is_file() {
            out.push(FileCandidate {
                path: rel,
                is_dir: false,
            });
        }
    }
}

/// Fuzzy subsequence score. Returns `None` when `query` is not a subsequence
/// of `target` (case-insensitive). Matching is case-insensitive; exact-case
/// matches, contiguous runs, and word-start positions score higher, while
/// longer targets are penalized so specific short paths win.
pub(crate) fn fuzzy_score(query: &str, target: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let query: Vec<char> = query.chars().collect();
    let target: Vec<char> = target.chars().collect();
    let mut qi = 0usize;
    let mut score: i64 = 0;
    let mut prev_matched = false;
    let mut prev_char = '\0';
    for (pi, &pc) in target.iter().enumerate() {
        if qi >= query.len() {
            break;
        }
        let qc = query[qi];
        if pc.eq_ignore_ascii_case(&qc) {
            let mut s = 10i64;
            if pc == qc {
                s += 5; // exact-case bonus
            }
            if prev_matched {
                s += 15; // contiguous run
            }
            if pi == 0 || matches!(prev_char, '/' | '.' | '_' | '-' | ' ') {
                s += 30; // word-start
            }
            // Leading-position bonus: matches near the front rank higher.
            s -= (pi as i64) / 4;
            score += s;
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
        prev_char = pc;
    }
    if qi < query.len() {
        return None;
    }
    // Prefer shorter targets (more specific match).
    score -= (target.len() as i64) / 4;
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn type_str(session: &mut FileMentionSession, s: &str) {
        for ch in s.chars() {
            session.dispatch_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    /// Build a unique temp directory under the system temp dir so parallel
    /// test processes never collide.
    fn scratch_dir(name: &str) -> PathBuf {
        let id = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "aish_file_mention_{name}_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // -- fuzzy_score ---------------------------------------------------------

    #[test]
    fn fuzzy_score_rejects_non_subsequence() {
        assert!(fuzzy_score("xyz", "src/main.rs").is_none());
    }

    #[test]
    fn fuzzy_score_accepts_subsequence() {
        assert!(fuzzy_score("main", "src/main.rs").is_some());
    }

    #[test]
    fn fuzzy_score_prefers_word_start_and_short_target() {
        // `main` matches `main.rs` at position 0 (word start) and the path
        // is shorter, so it must outrank `src/main.rs`.
        let direct = fuzzy_score("main", "main.rs").unwrap();
        let nested = fuzzy_score("main", "src/main.rs").unwrap();
        assert!(direct > nested);
    }

    #[test]
    fn fuzzy_score_case_insensitive_with_exact_bonus() {
        let lower = fuzzy_score("read", "README.md").unwrap();
        let exact = fuzzy_score("READ", "README.md").unwrap();
        assert!(exact > lower);
    }

    // -- scan_files ----------------------------------------------------------

    #[test]
    fn scan_files_skips_hidden_and_ignored() {
        let dir = scratch_dir("scan");
        fs::write(dir.join("a.txt"), b"x").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("b.rs"), b"x").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git").join("config"), b"x").unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target").join("bin"), b"x").unwrap();

        let files = scan_files(&dir);
        let paths: Vec<&str> = files.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"sub/"));
        assert!(paths.contains(&"sub/b.rs"));
        assert!(!paths.iter().any(|p| p.starts_with(".git")));
        assert!(!paths.iter().any(|p| p.starts_with("target")));

        cleanup(&dir);
    }

    #[test]
    fn scan_files_marks_directories_with_trailing_slash() {
        let dir = scratch_dir("dirs");
        fs::create_dir_all(dir.join("pkg")).unwrap();
        fs::write(dir.join("pkg").join("f.rs"), b"x").unwrap();
        let files = scan_files(&dir);
        let pkg = files.iter().find(|c| c.path == "pkg/").unwrap();
        assert!(pkg.is_dir);
        let f = files.iter().find(|c| c.path == "pkg/f.rs").unwrap();
        assert!(!f.is_dir);
        cleanup(&dir);
    }

    // -- FileMentionSession behavior ----------------------------------------

    fn session_in(dir: &Path, prefix: &str) -> FileMentionSession {
        FileMentionSession::new(dir, "aish> ".into(), prefix.into())
    }

    #[test]
    fn typing_filters_and_enter_selects_file() {
        let dir = scratch_dir("select");
        fs::write(dir.join("main.rs"), b"x").unwrap();
        fs::write(dir.join("other.rs"), b"x").unwrap();
        let mut s = session_in(&dir, "; analyze ");
        type_str(&mut s, "main");
        let outcome = s.dispatch_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            outcome,
            Some(FileMentionOutcome::Selected("main.rs".into()))
        );
        cleanup(&dir);
    }

    #[test]
    fn fuzzy_matches_substring_in_nested_path() {
        // Typing `main` must surface `src/main.rs` even though the match
        // starts mid-path — the core oh-my-pi parity requirement.
        let dir = scratch_dir("fuzzy");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("main.rs"), b"x").unwrap();
        fs::write(dir.join("README.md"), b"x").unwrap();
        let mut s = session_in(&dir, "; ");
        type_str(&mut s, "main");
        // The filtered list must contain src/main.rs in the top hit.
        assert!(s
            .filtered
            .iter()
            .any(|&i| s.candidates[i].path == "src/main.rs"));
        cleanup(&dir);
    }

    #[test]
    fn enter_on_directory_descends_instead_of_selecting() {
        let dir = scratch_dir("descend");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("main.rs"), b"x").unwrap();
        let mut s = session_in(&dir, "; ");
        type_str(&mut s, "src");
        // `src/` is the highlighted directory; Enter descends.
        let outcome = s.dispatch_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(outcome, None); // session continues
        assert_eq!(s.query, "src/");
        // Now the list is scoped under src/.
        assert!(s
            .filtered
            .iter()
            .any(|&i| s.candidates[i].path == "src/main.rs"));
        cleanup(&dir);
    }

    #[test]
    fn tab_extends_shared_prefix() {
        let dir = scratch_dir("tab");
        fs::write(dir.join("config.toml"), b"x").unwrap();
        fs::write(dir.join("config.yaml"), b"x").unwrap();
        let mut s = session_in(&dir, "; ");
        type_str(&mut s, "con");
        let outcome = s.dispatch_event(key(KeyCode::Tab, KeyModifiers::NONE));
        // Shared prefix is `config.`; popup stays open.
        assert_eq!(outcome, None);
        assert_eq!(s.query, "config.");
        cleanup(&dir);
    }

    #[test]
    fn backspace_past_at_cancels() {
        let dir = scratch_dir("cancel");
        fs::write(dir.join("a.txt"), b"x").unwrap();
        let mut s = session_in(&dir, "; ");
        let outcome = s.dispatch_event(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(outcome, Some(FileMentionOutcome::Cancelled));
        cleanup(&dir);
    }

    #[test]
    fn escape_cancels() {
        let dir = scratch_dir("esc");
        fs::write(dir.join("a.txt"), b"x").unwrap();
        let mut s = session_in(&dir, "; ");
        type_str(&mut s, "a");
        let outcome = s.dispatch_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(outcome, Some(FileMentionOutcome::Cancelled));
        cleanup(&dir);
    }

    #[test]
    fn enter_on_empty_query_with_no_match_cancels() {
        let dir = scratch_dir("empty");
        fs::write(dir.join("a.txt"), b"x").unwrap();
        let mut s = session_in(&dir, "; ");
        let outcome = s.dispatch_event(key(KeyCode::Enter, KeyModifiers::NONE));
        // Empty query, first highlighted candidate is a file → selects it.
        // The cancellation branch fires only when nothing matches at all.
        assert!(matches!(outcome, Some(FileMentionOutcome::Selected(_))));
        cleanup(&dir);
    }
}
