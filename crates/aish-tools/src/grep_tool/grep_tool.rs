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

/// Files larger than this are skipped rather than scanned.
const MAX_SCAN_FILE_SIZE: u64 = 1_048_576;

/// Cap how many skipped paths are retained in memory. The report below shows
/// only a prefix of these, but the total count stays exact.
const MAX_TRACKED_SKIPS: usize = 200;

/// Cap the number of skipped paths listed in the tool output.
const MAX_REPORTED_SKIPS: usize = 20;

/// Search outcome flags for result wording.
struct SearchStop {
    cancelled: bool,
    timed_out: bool,
}

/// Paths that were intentionally or unavoidably not searched, with the
/// reason. Reporting these is what separates "definitely no match" from
/// "the search scope was incomplete" (issue #693).
#[derive(Default)]
struct SkippedPaths {
    entries: Vec<(PathBuf, String)>,
    total: usize,
}

impl SkippedPaths {
    fn push(&mut self, path: PathBuf, reason: String) {
        self.total += 1;
        if self.entries.len() < MAX_TRACKED_SKIPS {
            self.entries.push((path, reason));
        }
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// Render the "search incomplete" note, or `None` when every candidate was
/// checked.
fn skipped_note(root: &std::path::Path, skipped: &SkippedPaths) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let mut lines = vec!["(search incomplete — some paths were not checked)".to_string()];
    let shown = skipped.entries.len().min(MAX_REPORTED_SKIPS);
    for (path, reason) in skipped.entries.iter().take(shown) {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let shown_path = if rel.as_os_str().is_empty() {
            path.display().to_string()
        } else {
            rel.display().to_string()
        };
        lines.push(format!("- {shown_path}: {reason}"));
    }
    if skipped.total > shown {
        lines.push(format!("- ... and {} more", skipped.total - shown));
    }
    Some(lines.join("\n"))
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

    let include_pattern = match args
        .get("include")
        .and_then(|g| g.as_str())
        .filter(|g| !g.trim().is_empty())
    {
        Some(glob_pattern) => match glob::Pattern::new(glob_pattern) {
            Ok(pattern) => Some(pattern),
            Err(e) => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("error".to_string(), e.to_string());
                return ToolResult::error(aish_i18n::t_with_args(
                    "tools.grep.invalid_glob",
                    &args_map,
                ));
            }
        },
        None => None,
    };

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
    let FileWalk {
        files,
        skipped,
        complete,
    } = walk_files(&root, should_stop.as_ref(), deadline);
    let mut skipped = skipped;
    let files = if complete {
        files
    } else {
        stop.cancelled = should_stop();
        stop.timed_out = !stop.cancelled && Instant::now() >= deadline;
        Vec::new()
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

        // Apply include filter. Use the glob crate so a wildcard in the
        // middle of the pattern (app*.log) matches app-prod.log instead of
        // falling back to exact-name comparison (issue #695).
        if let Some(pattern) = &include_pattern {
            let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !pattern.matches(file_name) {
                continue;
            }
        }

        // Unreadable files and files over the scan limit are not searched;
        // keep their paths so the result can say the scope was incomplete.
        let file = match File::open(&file_path) {
            Ok(file) => file,
            Err(e) => {
                skipped.push(file_path, format!("could not open file: {e}"));
                continue;
            }
        };
        if file.metadata().map(|m| m.len()).unwrap_or(0) > MAX_SCAN_FILE_SIZE {
            skipped.push(
                file_path,
                format!("file exceeds the {MAX_SCAN_FILE_SIZE} byte scan limit"),
            );
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

    let incomplete_note = skipped_note(&root, &skipped);
    if matches.is_empty() {
        let mut output = if incomplete_note.is_some() {
            "No matches found in the files that were checked.".to_string()
        } else {
            "No matches found.".to_string()
        };
        for note in [stopped_note, incomplete_note].into_iter().flatten() {
            output.push('\n');
            output.push_str(&note);
        }
        return ToolResult::success(output);
    }

    let truncated = matches.len() >= DEFAULT_MAX_RESULTS;
    let mut output = matches.join("\n");
    if truncated {
        output.push_str("\n(results truncated at 200)");
    }
    for note in [stopped_note, incomplete_note].into_iter().flatten() {
        output.push('\n');
        output.push_str(&note);
    }

    ToolResult::success(output)
}

/// Result of a directory walk: candidate files plus any paths that could not
/// be traversed, and whether the walk itself ran to completion.
struct FileWalk {
    files: Vec<PathBuf>,
    skipped: SkippedPaths,
    complete: bool,
}

/// Walk directory tree, collecting file paths (excluding default dirs).
/// The walk itself is cancellable: `should_stop` is checked per directory
/// and the deadline bounds slow/unresponsive filesystems (issue #551
/// review — the previously collected list was built by an uncancellable
/// traversal). Unreadable directories are recorded so the caller never
/// reports a complete zero-match result for a partial scan (issue #693).
fn walk_files(
    root: &std::path::Path,
    should_stop: &dyn Fn() -> bool,
    deadline: Instant,
) -> FileWalk {
    let mut files = Vec::new();
    let mut skipped = SkippedPaths::default();
    let complete = walk_dir_recursive(
        root,
        &mut files,
        &mut skipped,
        should_stop,
        deadline,
        &mut 0,
    );
    if complete {
        files.sort();
    }
    FileWalk {
        files,
        skipped,
        complete,
    }
}

/// Returns `true` when the walk finished, `false` when stopped early.
fn walk_dir_recursive(
    dir: &std::path::Path,
    result: &mut Vec<PathBuf>,
    skipped: &mut SkippedPaths,
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
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        skipped.push(
                            dir.to_path_buf(),
                            format!("could not read directory entry: {e}"),
                        );
                        continue;
                    }
                };
                let path = entry.path();
                if path.is_dir() {
                    if !is_excluded_dir_name(&path)
                        && !walk_dir_recursive(
                            &path,
                            result,
                            skipped,
                            should_stop,
                            deadline,
                            visited_dirs,
                        )
                    {
                        return false;
                    }
                } else if path.is_file() {
                    result.push(path);
                }
            }
        }
        Err(e) => skipped.push(dir.to_path_buf(), format!("could not read directory: {e}")),
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
    fn test_include_glob_matching() {
        let patterns = |text: &str| glob::Pattern::new(text).unwrap();
        assert!(patterns("*.rs").matches("main.rs"));
        assert!(patterns("*.rs").matches("lib.rs"));
        assert!(!patterns("*.rs").matches("main.py"));
        assert!(patterns("test_*").matches("test_foo"));
        assert!(!patterns("test_*").matches("prod_foo"));
        // Issue #695: a wildcard in the middle must match across the dash.
        assert!(patterns("app*.log").matches("app-prod.log"));
        assert!(!patterns("app*.log").matches("db-prod.log"));
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

    #[test]
    fn issue_695_include_glob_matches_wildcard_in_the_middle() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("app-prod.log"), "ERROR backend failed\n").unwrap();
        std::fs::write(tmp.path().join("db-prod.log"), "ERROR db failed\n").unwrap();

        let tool = GrepTool::new();
        let app = tool.execute(serde_json::json!({
            "pattern": "ERROR",
            "root": tmp.path().to_str().unwrap(),
            "include": "app*.log"
        }));
        assert!(app.ok);
        assert!(
            app.output.contains("app-prod.log:1: ERROR backend failed"),
            "app*.log must match app-prod.log, got: {}",
            app.output
        );
        assert!(
            !app.output.contains("db-prod.log"),
            "app*.log must not match db-prod.log, got: {}",
            app.output
        );

        let all = tool.execute(serde_json::json!({
            "pattern": "ERROR",
            "root": tmp.path().to_str().unwrap(),
            "include": "*.log"
        }));
        assert!(all.ok);
        assert!(all.output.contains("app-prod.log:1:"));
        assert!(all.output.contains("db-prod.log:1:"));
    }

    #[test]
    fn issue_693_large_file_skip_is_reported_with_hits() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("small.log"), "ERROR small\n").unwrap();
        let big = format!("ERROR skipped\n{}", "x".repeat(MAX_SCAN_FILE_SIZE as usize));
        std::fs::write(tmp.path().join("big.log"), big).unwrap();

        let result = run_grep(
            &serde_json::json!({
                "pattern": "ERROR",
                "root": tmp.path().to_str().unwrap()
            }),
            None,
        );
        assert!(result.ok);
        assert!(
            result.output.contains("small.log:1: ERROR small"),
            "matches from checked files must be kept: {}",
            result.output
        );
        assert!(
            result.output.contains("search incomplete"),
            "skipped file must mark the search incomplete: {}",
            result.output
        );
        assert!(
            result.output.contains("big.log") && result.output.contains("scan limit"),
            "the skipped file and reason must be named: {}",
            result.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn issue_693_unreadable_directory_is_reported() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("error.log"), "SERVICE_FAILED\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Root bypasses permission bits; skip rather than assert something
        // the kernel does not enforce for this process.
        if std::fs::read_dir(&locked).is_ok() {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: running with permission-bypassing privileges");
            return;
        }

        let result = run_grep(
            &serde_json::json!({
                "pattern": "SERVICE_FAILED",
                "root": tmp.path().to_str().unwrap()
            }),
            None,
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.ok);
        assert!(
            result
                .output
                .contains("No matches found in the files that were checked."),
            "must not claim a complete zero-match scan: {}",
            result.output
        );
        assert!(
            result.output.contains("search incomplete") && result.output.contains("locked"),
            "the unreadable directory must be named: {}",
            result.output
        );
        assert!(
            result.output.contains("Permission denied") || result.output.contains("os error 13"),
            "the reason must be preserved: {}",
            result.output
        );
    }

    #[test]
    fn issue_693_complete_zero_match_stays_definitive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("app.log"), "SERVICE_OK\n").unwrap();

        let result = run_grep(
            &serde_json::json!({
                "pattern": "SERVICE_FAILED",
                "root": tmp.path().to_str().unwrap()
            }),
            None,
        );
        assert!(result.ok);
        assert_eq!(result.output, "No matches found.");
    }
}
