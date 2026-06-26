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
}
