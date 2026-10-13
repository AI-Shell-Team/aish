//! Tab completion: local paths + PTY JSON, rustyline bash-style listing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustyline::completion::{FilenameCompleter, Pair};

use aish_pty::readline_tab::{clamp_pos, should_complete_path_locally, word_start_at};

const PTY_COMPLETION_TIMEOUT: Duration = Duration::from_millis(1200);

/// `/mcp` subcommands, mirroring `aish_mcp::commands::parse_command`.
const MCP_SUBCOMMANDS: &[&str] = &[
    "add",
    "disable",
    "enable",
    "help",
    "list",
    "reconnect",
    "reload",
    "remove",
    "rm",
    "status",
    "test",
    "tool",
    "tools",
    "trust",
];

/// `/mcp` subcommands whose first argument names a configured server.
const MCP_SERVER_ARG_SUBCOMMANDS: &[&str] = &[
    "disable",
    "enable",
    "reconnect",
    "remove",
    "rm",
    "test",
    "tool",
];

pub struct CompletionEngine {
    pty: Arc<Mutex<aish_pty::PersistentPty>>,
    /// Server names for `/mcp <sub> <server>` completion, refreshed by the
    /// shell. `None` disables `/mcp` argument completion entirely.
    mcp_names: Option<Arc<Mutex<Vec<String>>>>,
}

impl CompletionEngine {
    pub fn new(pty: Arc<Mutex<aish_pty::PersistentPty>>) -> Self {
        Self {
            pty,
            mcp_names: None,
        }
    }

    /// Attach the live MCP server-name list used for `/mcp` argument completion.
    pub fn with_mcp_names(mut self, names: Arc<Mutex<Vec<String>>>) -> Self {
        self.mcp_names = Some(names);
        self
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

        // `/mcp <subcommand> [server ...]` argument completion. The slash
        // branch above still owns naked `/mcp` (no argument typed yet); this
        // one only fires once whitespace follows the command name.
        if let Some(found) = self.mcp_arg_candidates(before) {
            return Some(found);
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

    /// Candidates for a `/mcp` argument position, or `None` when the cursor is
    /// not inside a `/mcp` argument region or nothing matches.
    ///
    /// A missing (never injected) server-name list disables argument
    /// completion entirely, leaving `/mcp` to the slash-command branch.
    fn mcp_arg_candidates(&self, before: &str) -> Option<(usize, Vec<Pair>)> {
        let names = self.mcp_names.as_ref()?.lock().ok()?;
        mcp_arg_candidates_for(names.as_slice(), before)
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
        .filter(|c| c.name.to_lowercase().starts_with(&query))
        .map(|c| Pair {
            display: c.name.to_string(),
            replacement: c.name.to_string(),
        })
        .collect()
}

/// Candidate computation for `/mcp` arguments.
///
/// Kept free of the PTY so the rules can be unit tested without spawning a
/// shell. `before` is the line text up to the cursor; `word_start` is in line
/// coordinates, which are identical to `before` coordinates because `before`
/// is the line prefix.
///
/// * argument 1 — `/mcp` subcommands, filtered by the current token;
/// * argument 2 — configured server names for the server-taking subcommands;
/// * argument 3 — `trust <server> <tool>` offers `*` (per-tool names are not
///   tracked here);
/// * anything else — no candidates.
///
/// Returns `None` when nothing matches, so the caller falls back to its
/// regular path/PTY completion.
fn mcp_arg_candidates_for(names: &[String], before: &str) -> Option<(usize, Vec<Pair>)> {
    let trimmed = before.trim_start();
    let is_mcp = trimmed
        .get(..4)
        .is_some_and(|head| head.eq_ignore_ascii_case("/mcp"));
    // Naked `/mcpx` and `/mcp` stay with the slash-command branch: argument
    // positions only start after whitespace.
    if !is_mcp
        || !trimmed
            .as_bytes()
            .get(4)
            .is_some_and(|b| b.is_ascii_whitespace())
    {
        return None;
    }

    let word_start = word_start_at(before, before.len());
    let prefix = before.get(word_start..).unwrap_or("");
    let typed: Vec<&str> = before.split_whitespace().skip(1).collect();
    // A non-empty prefix means the cursor sits inside the current word, which
    // is not a completed argument yet.
    let arg_index = typed.len().saturating_sub(usize::from(!prefix.is_empty()));

    let candidates = match arg_index {
        0 => mcp_pairs(MCP_SUBCOMMANDS.iter().copied(), prefix),
        1 => {
            let sub = typed.first()?.to_ascii_lowercase();
            if !MCP_SERVER_ARG_SUBCOMMANDS.contains(&sub.as_str()) {
                return None;
            }
            mcp_pairs(names.iter().map(String::as_str), prefix)
        }
        2 => {
            let sub = typed.first()?.to_ascii_lowercase();
            if sub != "trust" {
                return None;
            }
            mcp_pairs(std::iter::once("*"), prefix)
        }
        _ => return None,
    };

    let pairs = filter_extending_pairs(before, before.len(), word_start, candidates);
    if pairs.is_empty() {
        return None;
    }
    Some((word_start, pairs))
}

/// Case-insensitive prefix filter; results are sorted and deduplicated.
fn mcp_pairs<'a>(names: impl Iterator<Item = &'a str>, prefix: &str) -> Vec<Pair> {
    let prefix = prefix.to_lowercase();
    let mut matched: Vec<&str> = names
        .filter(|name| !name.is_empty() && name.to_lowercase().starts_with(&prefix))
        .collect();
    matched.sort_unstable();
    matched.dedup();
    matched
        .into_iter()
        .map(|name| Pair {
            display: name.to_string(),
            replacement: name.to_string(),
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

    /// Argument completion for an injected server list, as `(word_start, names)`.
    fn mcp_opts(names: &[&str], before: &str) -> Option<(usize, Vec<String>)> {
        let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        mcp_arg_candidates_for(&owned, before)
            .map(|(start, pairs)| (start, pairs.into_iter().map(|p| p.replacement).collect()))
    }

    /// `(word_start, replacements)` of an argument completion, or a panic.
    fn mcp_result(names: &[&str], before: &str) -> (usize, Vec<String>) {
        mcp_opts(names, before).expect("expected /mcp candidates")
    }

    #[test]
    fn mcp_lists_subcommands_after_trailing_space() {
        let (start, names) = mcp_result(&[], "/mcp ");
        assert_eq!(start, 5);
        assert_eq!(
            names,
            vec![
                "add",
                "disable",
                "enable",
                "help",
                "list",
                "reconnect",
                "reload",
                "remove",
                "rm",
                "status",
                "test",
                "tool",
                "tools",
                "trust"
            ]
        );
    }

    #[test]
    fn mcp_filters_subcommands_by_prefix() {
        assert_eq!(mcp_result(&[], "/mcp en").1, vec!["enable"]);
        assert_eq!(
            mcp_result(&[], "/mcp re").1,
            vec!["reconnect", "reload", "remove"]
        );
        // `rm` alone is an exact match, so it is dropped by
        // `filter_extending_pairs`; a shorter prefix still offers it.
        assert_eq!(
            mcp_result(&[], "/mcp r").1,
            vec!["reconnect", "reload", "remove", "rm"]
        );
    }

    #[test]
    fn mcp_exact_subcommand_is_not_reoffered() {
        // `/mcp enable` with the cursor at the end is a complete word.
        assert!(mcp_opts(&[], "/mcp enable").is_none());
    }

    #[test]
    fn mcp_lists_servers_for_server_taking_subcommands() {
        let servers = ["fixture", "fs"];
        for sub in [
            "enable",
            "disable",
            "remove",
            "rm",
            "reconnect",
            "test",
            "tool",
        ] {
            let before = format!("/mcp {sub} ");
            let (start, names) = mcp_result(&servers, &before);
            assert_eq!(start, before.len(), "sub = {sub}");
            assert_eq!(names, vec!["fixture", "fs"], "sub = {sub}");
        }
    }

    #[test]
    fn mcp_filters_servers_by_prefix() {
        let (start, names) = mcp_result(&["fixture", "fs"], "/mcp enable fi");
        assert_eq!(start, 12);
        assert_eq!(names, vec!["fixture"]);
    }

    #[test]
    fn mcp_server_prefix_is_case_insensitive() {
        assert_eq!(
            mcp_result(&["fixture"], "/mcp enable FIX").1,
            vec!["fixture"]
        );
    }

    #[test]
    fn mcp_trust_third_arg_offers_star() {
        let (start, names) = mcp_result(&["fixture"], "/mcp trust fixture ");
        assert_eq!(start, 19);
        assert_eq!(names, vec!["*"]);
    }

    #[test]
    fn mcp_tools_takes_no_server_argument() {
        assert!(mcp_opts(&[], "/mcp tools ").is_none());
        assert!(mcp_opts(&["fixture"], "/mcp tools fi").is_none());
    }

    #[test]
    fn mcp_without_servers_offers_nothing() {
        assert!(mcp_opts(&[], "/mcp enable ").is_none());
        assert!(mcp_opts(&[], "/mcp enable fi").is_none());
    }

    #[test]
    fn mcp_deeper_or_other_positions_offer_nothing() {
        // `trust <server> <tool> allow` is already past the completion point.
        assert!(mcp_opts(&[], "/mcp trust fixture * ").is_none());
        // `add <name>` takes a new server name, not a configured one.
        assert!(mcp_opts(&["fixture"], "/mcp add ").is_none());
        assert!(mcp_opts(&["fixture"], "/mcp status fi").is_none());
    }

    #[test]
    fn mcp_naked_command_stays_with_slash_completion() {
        assert!(mcp_opts(&[], "/mcp").is_none());
        assert!(mcp_opts(&[], "/mcpx").is_none());
        let names: Vec<String> = complete_slash_command("/mcp")
            .iter()
            .map(|p| p.replacement.clone())
            .collect();
        assert_eq!(names, vec!["/mcp"]);
    }

    #[test]
    fn mcp_ignores_lines_where_mcp_is_not_the_command() {
        assert!(mcp_opts(&[], "git /mcp en").is_none());
        assert!(mcp_opts(&[], "echo /mcp enable ").is_none());
    }
}
