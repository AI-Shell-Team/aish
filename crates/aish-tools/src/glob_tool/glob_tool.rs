use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use aish_i18n;
use aish_llm::{CancellationToken, LlmSession, Tool, ToolResult};
use futures::future::FutureExt;

use super::prompt;

/// Directories excluded by default (VCS and common large generated trees).
/// Traversal PRUNES these directories instead of post-filtering results.
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

/// Hard wall-clock budget for one traversal. Guarantees the tool returns even
/// on pathological trees or stalled network filesystems.
const TRAVERSAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Check the stop flag and deadline every N yielded directory entries so the
/// per-entry overhead stays negligible inside very large directories.
const STOP_CHECK_INTERVAL: usize = 256;

/// Tool for enumerating files by glob pattern within a directory.
pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

/// Outcome of a pruned traversal.
struct WalkOutcome {
    matches: Vec<PathBuf>,
    /// Traversal was cancelled by the stop flag (user Ctrl+C).
    cancelled: bool,
    /// Traversal hit the wall-clock deadline.
    timed_out: bool,
}

/// Depth-first traversal with directory pruning, early exit and a stop flag.
///
/// - `start`: directory the traversal begins from (its children are yielded,
///   the start itself never is — matching `glob::glob` semantics).
/// - `strip`: prefix removed before pattern matching; `None` matches the full
///   path (used for absolute patterns).
/// - Symlinked directories are NOT followed (lstat semantics): this avoids
///   infinite loops on symlink cycles and redundant stats on network mounts.
fn walk_pattern(
    start: &Path,
    strip: Option<&Path>,
    pattern: &glob::Pattern,
    limit: usize,
    deadline: Instant,
    should_stop: &dyn Fn() -> bool,
) -> WalkOutcome {
    let matches_path = |path: &Path| -> bool {
        let rel: &Path = match strip {
            Some(p) => path.strip_prefix(p).unwrap_or(path),
            None => path,
        };
        pattern.matches_path(rel)
    };

    let mut out = WalkOutcome {
        matches: Vec::with_capacity(limit.min(1024)),
        cancelled: false,
        timed_out: false,
    };

    // The start directory itself is never yielded; if it sits inside the
    // exclude list the old post-filter behaviour was "no results".
    if start
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| DEFAULT_EXCLUDE_DIRS.contains(&n))
        .unwrap_or(false)
    {
        return out;
    }

    let mut stack: Vec<PathBuf> = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // Stop/deadline checks at directory granularity are nearly free and
        // guarantee termination even for trees of many small directories.
        if should_stop() {
            out.cancelled = true;
            return out;
        }
        if Instant::now() >= deadline {
            out.timed_out = true;
            return out;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut seen = 0usize;
        for entry in entries.flatten() {
            seen += 1;
            if seen.is_multiple_of(STOP_CHECK_INTERVAL)
                && (should_stop() || Instant::now() >= deadline)
            {
                if should_stop() {
                    out.cancelled = true;
                } else {
                    out.timed_out = true;
                }
                return out;
            }

            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            if file_type.is_dir() {
                if DEFAULT_EXCLUDE_DIRS.contains(&&*name_str) {
                    continue;
                }
                if matches_path(&path) {
                    out.matches.push(path.clone());
                    if out.matches.len() >= limit {
                        return out;
                    }
                }
                stack.push(path);
            } else {
                // Files, symlinks and other non-dir entries match by name.
                if matches_path(&path) {
                    out.matches.push(path);
                    if out.matches.len() >= limit {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// Run the glob query synchronously. Shared by `execute` and the
/// cancellable async path.
fn run_glob(args: &serde_json::Value, cancel: Option<&CancellationToken>) -> ToolResult {
    let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => return ToolResult::error(aish_i18n::t("tools.glob.missing_pattern")),
    };

    let root = normalize_root(args.get("root").and_then(|r| r.as_str()));
    if !root.exists() || !root.is_dir() {
        return ToolResult::error(format!(
            "Error: root directory not found: {}",
            root.display()
        ));
    }

    let compiled = match glob::Pattern::new(&pattern) {
        Ok(p) => p,
        Err(e) => {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("error".to_string(), e.to_string());
            return ToolResult::error(aish_i18n::t_with_args("tools.glob.invalid_glob", &args_map));
        }
    };

    // Absolute patterns are matched against the full path, but the walk
    // starts at the longest wildcard-free prefix so it does not traverse
    // the whole filesystem from `/` (matching the old `glob::glob`
    // behaviour).
    let (start, strip) = if pattern.starts_with('/') {
        let mut base = PathBuf::from("/");
        for comp in Path::new(&pattern).components().skip(1) {
            let comp = comp.as_os_str().to_string_lossy();
            if comp.contains(['*', '?', '[']) {
                break;
            }
            base.push(comp.as_ref());
        }
        (base, None)
    } else {
        let root = root.canonicalize().unwrap_or(root);
        (root.clone(), Some(root))
    };
    if !start.is_dir() {
        return ToolResult::error(format!(
            "Error: root directory not found: {}",
            start.display()
        ));
    }

    let should_stop: Box<dyn Fn() -> bool> = match cancel {
        Some(t) => Box::new(move || t.is_cancelled()),
        None => Box::new(|| false),
    };

    let started = Instant::now();
    let mut outcome = walk_pattern(
        &start,
        strip.as_deref(),
        &compiled,
        DEFAULT_MAX_RESULTS,
        started + TRAVERSAL_TIMEOUT,
        &should_stop,
    );
    outcome.matches.sort();

    // Cancelled walks report their (partial) matches as a normal success
    // with a note, whether or not anything matched: an error result would
    // make the agent retry, and the retry gets cancelled again.
    if outcome.cancelled {
        let body = if outcome.matches.is_empty() {
            "No files found.".to_string()
        } else {
            outcome
                .matches
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        return ToolResult::success(format!(
            "{body}\n(cancelled after {:.1}s, partial results)",
            started.elapsed().as_secs_f64()
        ));
    }

    if outcome.matches.is_empty() {
        if outcome.timed_out {
            // Never report a definitive negative result from an incomplete
            // walk — the agent would conclude the files do not exist.
            return ToolResult::success(format!(
                "No files found.\n(timed out after {:.0}s — narrow the pattern or root)",
                TRAVERSAL_TIMEOUT.as_secs()
            ));
        }
        return ToolResult::success("No files found.");
    }

    let truncated_note = if outcome.timed_out {
        format!(
            "\n(timed out after {:.0}s, partial results — narrow the pattern or root)",
            TRAVERSAL_TIMEOUT.as_secs()
        )
    } else if outcome.matches.len() >= DEFAULT_MAX_RESULTS {
        "\n(results truncated at 200)".to_string()
    } else {
        String::new()
    };

    let mut text: String = outcome
        .matches
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str(&truncated_note);
    ToolResult::success(text)
}

/// Resolve the root directory from the optional argument.
fn normalize_root(root: Option<&str>) -> PathBuf {
    match root {
        Some(r) if !r.trim().is_empty() => {
            let expanded = shellexpand::tilde(r).to_string();
            PathBuf::from(expanded)
        }
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
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
        run_glob(&args, None)
    }

    /// Runs the blocking traversal on a dedicated thread so a slow or stalled
    /// filesystem cannot freeze the async runtime, and checks the session
    /// cancellation token periodically so Ctrl+C interrupts the walk.
    fn execute_async_in_session<'a>(
        &'a self,
        args: serde_json::Value,
        session: &'a LlmSession,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let token = session.cancellation_token_arc();
        async move {
            let res = tokio::task::spawn_blocking(move || run_glob(&args, Some(&token))).await;
            match res {
                Ok(r) => r,
                Err(e) => ToolResult::error(format!("Error: glob task failed: {e}")),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tool() -> GlobTool {
        GlobTool::new()
    }

    fn mk_tree(root: &Path) {
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("a.rs"), "x").unwrap();
        fs::write(root.join("src/b.rs"), "x").unwrap();
        fs::write(root.join("src/deep/c.rs"), "x").unwrap();
        fs::write(root.join("target/debug/ignored.rs"), "x").unwrap();
    }

    #[test]
    fn nonexistent_absolute_literal_prefix_errors_without_full_walk() {
        // The walk must start at the literal prefix, so a nonexistent
        // prefix fails fast instead of scanning the filesystem from `/`.
        let out = tool().execute(serde_json::json!({
            "pattern": "/definitely/not/a/real/prefix/**/*.rs",
        }));
        assert!(!out.ok);
        assert!(
            out.output.contains("root directory not found"),
            "out: {}",
            out.output
        );
    }

    #[test]
    fn recursive_pattern_matches_top_level_and_prunes_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        mk_tree(tmp.path());
        let out = tool().execute(serde_json::json!({
            "pattern": "**/*.rs",
            "root": tmp.path().to_str().unwrap(),
        }));
        assert!(out.ok);
        let text = out.output;
        assert!(text.contains("a.rs"), "top-level must match: {text}");
        assert!(text.contains("b.rs") && text.contains("c.rs"));
        assert!(
            !text.contains("ignored.rs"),
            "target/ must be pruned: {text}"
        );
    }

    #[test]
    fn nonexistent_root_errors() {
        let out = tool().execute(serde_json::json!({
            "pattern": "**/*.rs",
            "root": "/definitely/not/a/real/path",
        }));
        assert!(!out.ok);
    }

    #[test]
    fn absolute_pattern_matches_full_path() {
        let tmp = tempfile::tempdir().unwrap();
        mk_tree(tmp.path());
        let pat = format!("{}/**/*.rs", tmp.path().display());
        let out = tool().execute(serde_json::json!({"pattern": pat}));
        assert!(out.ok);
        assert!(out.output.contains("b.rs"));
        assert!(!out.output.contains("ignored.rs"));
    }

    #[test]
    fn root_inside_exclude_list_returns_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("target")).unwrap();
        fs::write(tmp.path().join("target/x.rs"), "x").unwrap();
        let out = tool().execute(serde_json::json!({
            "pattern": "**/*.rs",
            "root": tmp.path().join("target").to_str().unwrap(),
        }));
        assert!(out.ok);
        assert_eq!(out.output, "No files found.");
    }

    #[test]
    fn early_stops_at_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("many");
        fs::create_dir_all(&dir).unwrap();
        for i in 0..500 {
            fs::write(dir.join(format!("f{i:03}.rs")), "x").unwrap();
        }
        let out = tool().execute(serde_json::json!({
            "pattern": "**/*.rs",
            "root": tmp.path().to_str().unwrap(),
        }));
        assert!(out.ok);
        // 200 file lines plus a trailing parenthesised truncation note.
        let file_lines = out.output.lines().filter(|l| !l.starts_with('(')).count();
        assert_eq!(file_lines, 200, "must cap at 200 result lines");
        assert!(
            out.output.contains("truncated"),
            "must note truncation: {}",
            out.output
        );
    }

    #[test]
    fn symlinked_directories_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("inside.rs"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();
        let out = tool().execute(serde_json::json!({
            "pattern": "**/*.rs",
            "root": tmp.path().to_str().unwrap(),
        }));
        assert!(out.ok);
        assert!(out.output.contains("inside.rs"));
        // The file is reachable via `real/`; the symlink path itself must not
        // produce a second, duplicate traversal.
        let count = out.output.matches("inside.rs").count();
        assert_eq!(count, 1, "no duplicate via symlink: {}", out.output);
    }

    #[test]
    fn cancelled_before_start_reports_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        mk_tree(tmp.path());
        let token = CancellationToken::new();
        token.cancel();
        let out = run_glob(
            &serde_json::json!({
                "pattern": "**/*.rs",
                "root": tmp.path().to_str().unwrap(),
            }),
            Some(&token),
        );
        // Cancelled before start: the per-directory check fires immediately.
        // Cancellation is reported as success + note, never as an error, so
        // the agent does not retry into another cancelled walk.
        assert!(out.ok);
        assert!(out.output.contains("cancelled"), "out: {}", out.output);
    }

    #[test]
    fn timeout_returns_partial_note() {
        let tmp = tempfile::tempdir().unwrap();
        mk_tree(tmp.path());
        let compiled = glob::Pattern::new("**/*.rs").unwrap();
        let start = tmp.path().canonicalize().unwrap();
        let outcome = walk_pattern(
            &start,
            Some(&start),
            &compiled,
            DEFAULT_MAX_RESULTS,
            Instant::now() - Duration::from_secs(1),
            &|| false,
        );
        assert!(outcome.timed_out, "expired deadline must stop the walk");
    }
}
