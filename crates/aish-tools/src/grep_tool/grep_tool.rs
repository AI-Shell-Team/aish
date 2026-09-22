use std::fs::File;
use std::future::Future;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, Instant};

use aish_i18n;
use aish_llm::{CancellationToken, LlmSession, Tool, ToolResult};
use futures::future::FutureExt;

use super::prompt;

/// Directories excluded by default (shared with GlobTool).
const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    ".bzr",
    ".jj",
    ".sl",
    "node_modules",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    "venv",
    "target",
    "build",
    "dist",
];

const DEFAULT_MAX_RESULTS: usize = 200;
const MAX_LINE_LENGTH: usize = 500;

/// Wall-clock budget for one search, matching GlobTool's traversal budget.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Check the stop flag every N files so the per-file overhead stays small.
const STOP_CHECK_INTERVAL: usize = 64;

/// Tool for searching file contents by regex pattern.
pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        run_grep(&args, None)
    }

    /// Runs the blocking scan on a dedicated thread so a huge or stalled
    /// filesystem cannot freeze the async runtime, and checks the session
    /// cancellation token periodically so Ctrl+C interrupts the walk
    /// (issue #547).
    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let token = session.cancellation_token_arc();
        async move {
            let res = tokio::task::spawn_blocking(move || run_grep(&args, Some(&token))).await;
            match res {
                Ok(r) => r,
                Err(e) => ToolResult::error(format!("Error: grep task failed: {e}")),
            }
        }
        .boxed()
    }
}

/// Search outcome flags for result wording.
struct SearchStop {
    cancelled: bool,
    timed_out: bool,
}

fn run_grep(args: &serde_json::Value, cancel: Option<&CancellationToken>) -> ToolResult {
    let pattern_str = match args.get("pattern").and_then(|p| p.as_str()) {
        Some(p) if !p.trim().is_empty() => p,
        _ => return ToolResult::error(aish_i18n::t("tools.grep.missing_pattern")),
    };

    let re = match regex::Regex::new(pattern_str) {
        Ok(re) => re,
        Err(e) => {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("error".to_string(), e.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.grep.invalid_regex",
                &args_map,
            ));
        }
    };

    let root = normalize_root(args.get("root").and_then(|r| r.as_str()));
    if !root.exists() || !root.is_dir() {
        return ToolResult::error(format!(
            "Error: root directory not found: {}",
            root.display()
        ));
    }

    let include_glob = args
        .get("include")
        .and_then(|g| g.as_str())
        .filter(|g| !g.trim().is_empty());

    let started = Instant::now();
    let deadline = started + SEARCH_TIMEOUT;
    let should_stop: Box<dyn Fn() -> bool> = match cancel {
        Some(t) => Box::new(move || t.is_cancelled()),
        None => Box::new(|| false),
    };

    let mut matches: Vec<String> = Vec::with_capacity(DEFAULT_MAX_RESULTS);
    let mut stop = SearchStop {
        cancelled: false,
        timed_out: false,
    };
    // The walk itself is cancellable now: a stopped walk lands in the same
    // partial-results path as a stopped scan loop.
    let files = match walk_files(&root, should_stop.as_ref(), deadline) {
        Some(f) => f,
        None => {
            stop.cancelled = should_stop();
            stop.timed_out = !stop.cancelled && Instant::now() >= deadline;
            Vec::new()
        }
    };

    'outer: for (visited, file_path) in files.into_iter().enumerate() {
        // Periodic stop check: cancel / wall-clock budget (issue #547).
        if visited % STOP_CHECK_INTERVAL == 0 && (should_stop() || Instant::now() >= deadline) {
            stop.cancelled = should_stop();
            stop.timed_out = Instant::now() >= deadline && !stop.cancelled;
            break 'outer;
        }
        if matches.len() >= DEFAULT_MAX_RESULTS {
            break;
        }

        // Apply include filter
        if let Some(glob_pattern) = include_glob {
            let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !glob_match(glob_pattern, file_name) {
                continue;
            }
        }

        // Skip binary / unreadable files
        let Ok(file) = File::open(&file_path) else {
            continue;
        };
        // Skip large files (>1MB)
        if file.metadata().map(|m| m.len()).unwrap_or(0) > 1_048_576 {
            continue;
        }

        let reader = BufReader::new(file);
        let rel_path = file_path
            .strip_prefix(&root)
            .unwrap_or(&file_path)
            .display()
            .to_string();

        for (line_no, line_result) in reader.lines().enumerate() {
            if matches.len() >= DEFAULT_MAX_RESULTS {
                break;
            }
            let Ok(line) = line_result else {
                continue;
            };
            if re.is_match(&line) {
                let truncated = if line.len() > MAX_LINE_LENGTH {
                    let end = line
                        .char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= MAX_LINE_LENGTH)
                        .last()
                        .unwrap_or(0);
                    format!("{}...", &line[..end])
                } else {
                    line
                };
                matches.push(format!("{}:{}: {}", rel_path, line_no + 1, truncated));
            }
        }
    }

    // A stopped search must never report a definitive negative: cancelled
    // returns partial results with a note (like glob), timeout states the
    // budget so the agent narrows the search instead of retrying blindly.
    let stopped_note = if stop.cancelled {
        Some(format!(
            "(cancelled after {:.1}s, partial results)",
            started.elapsed().as_secs_f64()
        ))
    } else if stop.timed_out {
        Some(format!(
            "(timed out after {:.0}s, partial results — narrow the pattern or root)",
            SEARCH_TIMEOUT.as_secs()
        ))
    } else {
        None
    };

    if matches.is_empty() && stopped_note.is_none() {
        return ToolResult::success("No matches found.");
    }
    if matches.is_empty() {
        return ToolResult::success(format!("No matches found.\n{}", stopped_note.unwrap()));
    }

    let truncated = matches.len() >= DEFAULT_MAX_RESULTS;
    let mut output = matches.join("\n");
    if truncated {
        output.push_str("\n(results truncated at 200)");
    }
    if let Some(note) = stopped_note {
        output.push('\n');
        output.push_str(&note);
    }

    ToolResult::success(output)
}

/// Simple glob matching for the include filter (supports * wildcard only).
fn glob_match(pattern: &str, name: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

/// Walk directory tree, collecting file paths (excluding default dirs).
/// The walk itself is cancellable: `should_stop` is checked per directory
/// and the deadline bounds slow/unresponsive filesystems (issue #551
/// review — the previously collected list was built by an uncancellable
/// traversal). Returns `None` when the walk stopped early.
fn walk_files(
    root: &std::path::Path,
    should_stop: &dyn Fn() -> bool,
    deadline: Instant,
) -> Option<Vec<PathBuf>> {
    let mut result = Vec::new();
    let complete = walk_dir_recursive(root, &mut result, should_stop, deadline, &mut 0);
    if !complete {
        return None;
    }
    result.sort();
    Some(result)
}

/// Returns `true` when the walk finished, `false` when stopped early.
fn walk_dir_recursive(
    dir: &std::path::Path,
    result: &mut Vec<PathBuf>,
    should_stop: &dyn Fn() -> bool,
    deadline: Instant,
    visited_dirs: &mut usize,
) -> bool {
    // Check per directory: a huge fan-out directory still yields entries
    // quickly via readdir, so directory-granularity bounds the walk well
    // enough while keeping the overhead negligible.
    *visited_dirs += 1;
    if (*visited_dirs).is_multiple_of(STOP_CHECK_INTERVAL)
        && (should_stop() || Instant::now() >= deadline)
    {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !is_excluded_dir_name(&path)
                    && !walk_dir_recursive(&path, result, should_stop, deadline, visited_dirs)
                {
                    return false;
                }
            } else if path.is_file() {
                result.push(path);
            }
        }
    }
    true
}

fn is_excluded_dir_name(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| DEFAULT_EXCLUDE_DIRS.contains(&s))
        .unwrap_or(false)
}

fn normalize_root(root: Option<&str>) -> PathBuf {
    match root {
        Some(r) if !r.trim().is_empty() => {
            let expanded = shellexpand::tilde(r).to_string();
            PathBuf::from(expanded)
        }
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grep_tool_missing_pattern() {
        let tool = GrepTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
    }

    #[test]
    fn test_grep_tool_invalid_regex() {
        let tool = GrepTool::new();
        let result = tool.execute(serde_json::json!({"pattern": "[invalid"}));
        assert!(!result.ok);
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(!glob_match("*.rs", "main.py"));
        assert!(glob_match("test_*", "test_foo"));
        assert!(!glob_match("test_*", "prod_foo"));
    }

    #[test]
    fn test_is_excluded_dir_name() {
        assert!(is_excluded_dir_name(PathBuf::from(".git").as_path()));
        assert!(is_excluded_dir_name(
            PathBuf::from("node_modules").as_path()
        ));
        assert!(!is_excluded_dir_name(PathBuf::from("src").as_path()));
    }

    #[test]
    fn cancel_stops_scan_quickly_on_large_tree() {
        // Issue #547 regression: a scan outliving the cancel must stop at
        // the next check interval instead of running to completion.
        let tmp = tempfile::tempdir().unwrap();
        for fan in 0..40 {
            let dir = tmp.path().join(format!("d{fan}"));
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..1000 {
                std::fs::write(dir.join(format!("f{i}.txt")), "needle\n").unwrap();
            }
        }
        let token = std::sync::Arc::new(CancellationToken::new());
        let canceller = {
            let token = std::sync::Arc::clone(&token);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                token.cancel();
            })
        };
        let started = Instant::now();
        let result = run_grep(
            &serde_json::json!({"pattern": "zzz_unlikely", "root": tmp.path().to_str()}),
            Some(&token),
        );
        canceller.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "scan ignored cancellation: {:?}",
            started.elapsed()
        );
        assert!(
            result.output.contains("cancelled"),
            "out: {}",
            result.output
        );
    }

    /// Issue #551 review: the walk phase itself must be cancellable, not
    /// just the per-file scan loop. A tree with many directories (where
    /// readdir dominates) plus an already-cancelled token must return
    /// immediately instead of walking to completion.
    #[test]
    fn walk_phase_honours_already_cancelled_token() {
        let tmp = tempfile::tempdir().unwrap();
        // Deep tree so a full uncancellable walk would take measurable time;
        // many directories so the per-directory check fires early.
        let mut dir = tmp.path().to_path_buf();
        for depth in 0..40 {
            dir = dir.join(format!("lvl{depth}"));
            std::fs::create_dir_all(&dir).unwrap();
            for fan in 0..50 {
                let sub = dir.join(format!("d{fan}"));
                std::fs::create_dir_all(&sub).unwrap();
                std::fs::write(sub.join("f.txt"), "x\n").unwrap();
            }
        }
        let token = std::sync::Arc::new(CancellationToken::new());
        token.cancel(); // cancelled before the walk even starts
        let started = Instant::now();
        let result = run_grep(
            &serde_json::json!({"pattern": "zzz_unlikely", "root": tmp.path().to_str()}),
            Some(&token),
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "walk phase ignored cancellation: {:?}",
            started.elapsed()
        );
        assert!(
            result.output.contains("cancelled"),
            "out: {}",
            result.output
        );
    }
}
