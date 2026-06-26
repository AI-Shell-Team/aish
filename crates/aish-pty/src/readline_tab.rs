//! Parse bash/readline Tab completion output captured from the PTY master fd.

use std::time::Duration;

use crate::offload::strip_ansi_escapes;

pub const TAB_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineTabResult {
    pub word_start: usize,
    pub line_after: Option<String>,
    pub candidates: Vec<String>,
}

impl ReadlineTabResult {
    pub fn is_useful(&self, input_line: &str) -> bool {
        let extended = self.line_after.as_ref().is_some_and(|line| {
            !line.contains("^U")
                && line != input_line
                && (line.len() > input_line.len() || line.starts_with(input_line.trim_end()))
        });
        extended || !self.candidates.is_empty()
    }
}

pub fn clamp_pos(line: &str, pos: usize) -> usize {
    if line.is_char_boundary(pos) {
        pos
    } else {
        (0..=pos)
            .rev()
            .find(|&p| line.is_char_boundary(p))
            .unwrap_or(0)
    }
}

pub fn word_start_at(line: &str, pos: usize) -> usize {
    let before = line.get(..pos).unwrap_or(line);
    before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0)
}

fn is_path_like_token(token: &str) -> bool {
    !token.is_empty()
        && (token.starts_with('/')
            || token.starts_with("./")
            || token.starts_with("../")
            || token.starts_with('~')
            || token.contains('/'))
}

fn is_quoted_token(token: &str) -> bool {
    token.starts_with('"') || token.starts_with('\'')
}

fn looks_like_remote_path(token: &str) -> bool {
    token.contains("://") || (token.contains('@') && token.contains(':'))
}

/// True when Rust `FilenameCompleter` should handle the token (not PTY/bash path logic).
pub fn should_complete_path_locally(token: &str) -> bool {
    is_path_like_token(token) && !is_quoted_token(token) && !looks_like_remote_path(token)
}

/// Build keystrokes for PTY readline: optional Ctrl-U, line text, cursor, Tab×n.
pub fn build_readline_tab_payload(line: &str, pos: usize, tabs: u8, clear_line: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(line.len() + 8);
    if clear_line {
        buf.push(0x15);
    }
    buf.extend_from_slice(line.as_bytes());
    if pos < line.len() {
        buf.push(0x01);
        buf.extend(std::iter::repeat_n(0x06, line[..pos].chars().count()));
    }
    buf.extend(std::iter::repeat_n(0x09, tabs as usize));
    buf
}

pub fn parse_readline_tab_output(raw: &[u8], input_line: &str, cursor: usize) -> ReadlineTabResult {
    let word_start = word_start_at(input_line, cursor);
    let current_word = input_line.get(word_start..cursor).unwrap_or("");
    let text = normalize_tab_text(raw);

    let mut candidates = parse_column_candidates(&text, current_word);
    let line_after = parse_inline_line(&text, input_line);

    if candidates.is_empty() {
        if let Some(extended) = &line_after {
            if extended != input_line {
                if let Some(suffix) = extended.get(word_start..) {
                    if !suffix.is_empty() && suffix != current_word {
                        candidates.push(suffix.to_string());
                    }
                }
            }
        }
    }

    ReadlineTabResult {
        word_start,
        line_after,
        candidates,
    }
}

pub fn to_replacement_pairs(
    result: &ReadlineTabResult,
    line: &str,
    pos: usize,
) -> (usize, Vec<(String, String)>) {
    let word_start = result.word_start;
    let current = line.get(word_start..pos).unwrap_or("");

    if let Some(new_line) = &result.line_after {
        if new_line != line {
            if let Some(suffix) = new_line.get(word_start..) {
                if !suffix.is_empty() && suffix != current {
                    let s = suffix.to_string();
                    return (word_start, vec![(s.clone(), s)]);
                }
            }
        }
    }

    let pairs = result
        .candidates
        .iter()
        .filter(|c| !c.is_empty())
        .map(|c| {
            let rep = word_replacement(c);
            (c.trim_end_matches(' ').to_string(), rep)
        })
        .collect();
    (word_start, pairs)
}

fn word_replacement(word: &str) -> String {
    if word.ends_with('/') || word.contains('/') || word.ends_with(' ') {
        word.to_string()
    } else {
        format!("{word} ")
    }
}

fn normalize_tab_text(raw: &[u8]) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes(raw))
        .into_owned()
        .replace('\x07', "")
}

fn strip_ps1_line(line: &str) -> &str {
    if let Some(pos) = line.rfind("$ ").or_else(|| line.rfind("# ")) {
        line[pos + 2..].trim()
    } else {
        line.trim()
    }
}

fn parse_column_candidates(text: &str, current_word: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split(['\r', '\n']) {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.contains("Display all ")
            || trimmed.contains(" possibilities?")
            || !trimmed.contains("  ")
        {
            continue;
        }
        for token in trimmed.split("  ") {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if !current_word.is_empty() && !token.starts_with(current_word) {
                continue;
            }
            if !out.iter().any(|existing| existing == token) {
                out.push(token.to_string());
            }
        }
    }
    out
}

fn parse_inline_line(text: &str, input_line: &str) -> Option<String> {
    if text.contains("Display all ") {
        return None;
    }
    if text
        .split(['\r', '\n'])
        .any(|l| l.contains("  ") && l.matches(' ').count() >= 2)
    {
        return None;
    }

    for segment in text.replace('\r', "").split('\n').rev() {
        let seg = strip_ps1_line(segment);
        if seg.is_empty() || seg.contains("Display all") {
            continue;
        }
        if input_line.starts_with(seg)
            || seg.starts_with(input_line.trim_end())
            || seg.len() >= input_line.len()
        {
            return Some(seg.to_string());
        }
    }

    let flat = strip_ps1_line(text.replace('\r', "").as_str()).to_string();
    if !flat.is_empty()
        && flat != input_line
        && (flat.starts_with(input_line.trim_end()) || input_line.starts_with(&flat))
    {
        return Some(flat);
    }

    text.rsplit('\r').next().and_then(|last| {
        let last = strip_ps1_line(last.trim_matches('\n'));
        (!last.is_empty() && !last.contains("  ")).then(|| last.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_complete_path_locally_excludes_quoted_and_remote() {
        assert!(should_complete_path_locally("~/.config/"));
        assert!(!should_complete_path_locally("\"~/.config\""));
        assert!(!should_complete_path_locally("user@host:/path"));
    }

    #[test]
    fn parse_gi_list_and_inline_extend() {
        let list = b"\x07gi\x07\r\ngit                 git-receive-pack    \r\ngi";
        let list_result = parse_readline_tab_output(list, "gi", 2);
        assert!(list_result.candidates.iter().any(|c| c.starts_with("git")));

        let inline = b"\x07ls /home/";
        assert_eq!(
            parse_readline_tab_output(inline, "ls /ho", 6)
                .line_after
                .as_deref(),
            Some("ls /home/")
        );
    }

    #[test]
    fn word_replacement_respects_trailing_space() {
        assert_eq!(word_replacement("status"), "status ");
        assert_eq!(word_replacement("status "), "status ");
        assert_eq!(word_replacement("/home/"), "/home/");
    }
}
