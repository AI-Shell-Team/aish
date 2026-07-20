//! Tab completion: local paths + PTY JSON, rustyline bash-style listing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustyline::completion::{FilenameCompleter, Pair};

use aish_pty::readline_tab::{clamp_pos, should_complete_path_locally, word_start_at};

const PTY_COMPLETION_TIMEOUT: Duration = Duration::from_millis(1200);

pub struct CompletionEngine {
    pty: Arc<Mutex<aish_pty::PersistentPty>>,
}

impl CompletionEngine {
    pub fn new(pty: Arc<Mutex<aish_pty::PersistentPty>>) -> Self {
        Self { pty }
    }

    /// Return `(word_start, candidates)` for rustyline to extend or list below the line.
    pub fn complete(&self, line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
        let pos = clamp_pos(line, pos);
        let before = line.get(..pos).unwrap_or(line);

        if before.starts_with(';') || before.starts_with('\u{ff1b}') || before.trim().is_empty() {
            return None;
        }

        // Built-in slash command completion: when the first word starts with
        // `/` and has no whitespace yet, complete from SLASH_COMMANDS. This
        // covers the case where the popup dismissed to readline (e.g. after
        // Tab-completing `/setup `) and the user backspaced to a partial
        // prefix — Tab in readline now completes slash commands natively.
        if before.starts_with('/') && !before.contains(char::is_whitespace) {
            let pairs = filter_extending_pairs(line, pos, 0, complete_slash_command(before));
            if !pairs.is_empty() {
                return Some((0, pairs));
            }
        }

        let (word_start, pairs) = if should_complete_path_locally(
            line.get(word_start_at(line, pos)..pos).unwrap_or(""),
        ) {
            Self::complete_path_local(line, pos)?
        } else {
            let word_start = word_start_at(line, pos);
            (word_start, self.complete_via_pty(line, pos)?)
        };

        let pairs = filter_extending_pairs(line, pos, word_start, pairs);
        if pairs.is_empty() {
            return None;
        }

        Some((word_start, pairs))
    }

    fn complete_via_pty(&self, line: &str, pos: usize) -> Option<Vec<Pair>> {
        let mut pty = self.pty.lock().ok()?;
        if !pty.is_running() {
            return None;
        }
        let resp = pty
            .query_completions(line, pos, PTY_COMPLETION_TIMEOUT)
            .ok()?;
        if resp.candidates.is_empty() {
            return None;
        }
        Some(
            resp.candidates
                .into_iter()
                .map(|c| Pair {
                    display: c.display,
                    replacement: c.replacement,
                })
                .collect(),
        )
    }

    /// Keep FilenameCompleter `replacement` values intact for line editing / LCP.
    fn complete_path_local(line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
        let pos = clamp_pos(line, pos);
        let (start, pairs) = FilenameCompleter::new().complete_path(line, pos).ok()?;
        if pairs.is_empty() {
            None
        } else {
            Some((start, pairs))
        }
    }
}

fn filter_extending_pairs(
    line: &str,
    pos: usize,
    word_start: usize,
    pairs: Vec<Pair>,
) -> Vec<Pair> {
    let current = line.get(word_start..pos).unwrap_or("");
    pairs
        .into_iter()
        .filter(|p| p.replacement != current)
        .collect()
}

/// Complete built-in slash commands for a `/`-prefixed first word.
///
/// `before_cursor` is the line text up to the cursor. Returns matching
/// command names as `Pair`s (display = name, replacement = name), or an
/// empty `Vec` when nothing matches. Callers should apply
/// `filter_extending_pairs` to drop exact matches.
fn complete_slash_command(before_cursor: &str) -> Vec<Pair> {
    let query = before_cursor.to_lowercase();
    crate::readline::SLASH_COMMANDS
        .iter()
        .filter(|(name, _)| name.to_lowercase().starts_with(&query))
        .map(|(name, _)| Pair {
            display: (*name).to_string(),
            replacement: (*name).to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyline::completion::FilenameCompleter;

    #[test]
    fn filter_extending_pairs_drops_exact_match() {
        let pairs = vec![Pair {
            display: "git".into(),
            replacement: "git".into(),
        }];
        let filtered = filter_extending_pairs("git", 3, 0, pairs);
        assert!(filtered.is_empty());
    }

    #[test]
    fn ls_usr_lcp_extends_with_full_paths_not_r_slash() {
        if !std::path::Path::new("/usr").is_dir() {
            return;
        }
        let line = "ls /usr";
        let pos = line.len();
        let (start, pairs) = FilenameCompleter::new().complete_path(line, pos).unwrap();
        assert_eq!(start, 3);
        assert!(!pairs.is_empty());
        for p in &pairs {
            assert!(
                p.replacement.starts_with("/usr/"),
                "expected full path replacement, got {:?}",
                p.replacement
            );
        }
        let lcp = rustyline::completion::longest_common_prefix(&pairs).unwrap_or("");
        assert!(
            lcp.starts_with("/usr"),
            "LCP should extend /usr prefix, got {lcp:?}"
        );
        assert_ne!(lcp, "r/");
    }

    #[test]
    fn slash_command_completes_partial_prefix() {
        let pairs = complete_slash_command("/set");
        let names: Vec<&str> = pairs.iter().map(|p| p.replacement.as_str()).collect();
        assert!(names.contains(&"/setup"), "expected /setup in {names:?}");
        assert!(
            names.contains(&"/setting"),
            "expected /setting in {names:?}"
        );
    }

    #[test]
    fn slash_command_completes_unambiguous_prefix() {
        let pairs = complete_slash_command("/setu");
        let names: Vec<&str> = pairs.iter().map(|p| p.replacement.as_str()).collect();
        assert_eq!(names, vec!["/setup"]);
    }

    #[test]
    fn slash_command_exact_match_filtered_out() {
        // `/setup` is an exact command — filter_extending_pairs drops it.
        let pairs = filter_extending_pairs("/setup", 6, 0, complete_slash_command("/setup"));
        assert!(pairs.is_empty(), "exact match should be filtered out");
    }

    #[test]
    fn slash_command_non_matching_prefix_returns_empty() {
        let pairs = complete_slash_command("/xyz");
        assert!(pairs.is_empty());
    }

    #[test]
    fn slash_command_slash_alone_matches_all() {
        let pairs = complete_slash_command("/");
        assert!(pairs.len() > 1, "bare `/` should match all commands");
    }
}
