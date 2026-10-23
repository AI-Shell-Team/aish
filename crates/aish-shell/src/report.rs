//! Offline single-file HTML report generation (`/report`, issue #608).
//!
//! The generated document is self-contained: inline CSS only, no scripts, no
//! external resources, and a CSP that forbids everything except inline style.
//! It therefore opens in a browser with no network at all.
//!
//! Model output is Markdown (tables, bold, lists) and is rendered as such by
//! [`markdown_to_html`]. That helper never lets raw HTML through:
//! `pulldown_cmark::html::push_html` forwards `Event::Html` / `Event::InlineHtml`
//! verbatim, so a log line containing `<script>` would become live markup. Both
//! events are therefore downgraded to `Event::Text`, which `push_html` escapes —
//! the text stays visible instead of being silently dropped.
//!
//! Tool output is not Markdown (it is a `<stdout>`/`<return_code>` envelope
//! around arbitrary bytes), so evidence bodies go through [`escape_html`] into a
//! `<pre>` and keep their exact line structure.
//!
//! This module is pure (no I/O except the explicit `write_*` helpers) so the
//! redaction / escaping / gap accounting can be unit tested without a
//! session store.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use pulldown_cmark::{Event, Options, Parser};
use regex::Regex;

use aish_i18n::{t, t_with_args};
use aish_security::secret::{redact_secrets_with_hits, SecretScanner};
use aish_session::{HistoryEntry, SessionContextMessage};

/// Marker appended by `aish_llm::cap_tool_output` when a tool result was cut
/// before entering the transcript.
const TRUNCATION_MARKER: &str = "...(truncated)";

/// Maximum characters of the objective / findings block kept in the report.
/// The full conversation section below stays complete, so this only keeps the
/// summary readable.
const SUMMARY_CHAR_LIMIT: usize = 8_000;

// ---------------------------------------------------------------------------
// Session environment
// ---------------------------------------------------------------------------

/// `true` when the shell runs under an SSH login.
///
/// Detection is intentionally narrow (`SSH_CONNECTION` / `SSH_TTY`): a
/// forwarded X11 socket must not turn a remote shell into a "desktop session"
/// that auto-launches a browser on the server.
pub fn is_remote_session() -> bool {
    is_remote_session_from(
        std::env::var_os("SSH_CONNECTION").is_some(),
        std::env::var_os("SSH_TTY").is_some(),
    )
}

/// Testable core of [`is_remote_session`].
pub fn is_remote_session_from(has_connection: bool, has_tty: bool) -> bool {
    has_connection || has_tty
}

/// `true` when a local graphical session is reachable.
pub fn has_graphical_session() -> bool {
    has_graphical_session_from(
        cfg!(any(target_os = "macos", target_os = "windows")),
        std::env::var_os("DISPLAY").is_some(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
    )
}

/// Testable core of [`has_graphical_session`].
pub fn has_graphical_session_from(native_gui: bool, display: bool, wayland: bool) -> bool {
    native_gui || display || wayland
}

/// Whether an automatic open is allowed. Remote or headless shells only ever
/// print the path: launching a browser there is both useless and surprising.
pub fn can_auto_open(remote: bool, graphical: bool) -> bool {
    !remote && graphical
}

// ---------------------------------------------------------------------------
// Report output paths
// ---------------------------------------------------------------------------

/// Directory holding this session's report files.
///
/// Defaults to `<data_local_dir>/aish/reports/<session>` (`~/.local/share/...`
/// on Linux); `output_dir` overrides the base directory and has `~` expanded.
pub fn reports_dir(output_dir: Option<&str>, session_uuid: &str) -> PathBuf {
    let base = match output_dir {
        Some(dir) if !dir.trim().is_empty() => {
            PathBuf::from(shellexpand::tilde(dir.trim()).into_owned())
        }
        _ => dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aish")
            .join("reports"),
    };
    base.join(sanitize_component(session_uuid))
}

/// Keep the session id from escaping the report directory. Session ids are
/// generated UUIDs, but `/fork` and imported sessions are not guaranteed to
/// be, and a path component must never carry separators or `..`.
fn sanitize_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '_') {
        "session".to_string()
    } else {
        cleaned
    }
}

/// Write `bytes` to `path`, creating the parent directory owner-only.
///
/// Files are created 0600 and the containing directory 0700 from the start:
/// reports contain the full conversation and command history.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            create_private_dir(parent)?;
        }
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            // `mode` only affects creation; a pre-existing loosened file is
            // forced back to owner-only before any content is written.
            .mode(0o600)
            .open(path)
            .and_then(|mut file| {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file.write_all(bytes)
            })
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Auto-trigger heuristic
// ---------------------------------------------------------------------------

/// Report nouns that are unambiguous in any sentence.
///
/// English `report` is a common verb ("how do I report a bug") and Chinese
/// `报告` is too ("报告一下当前状态"), so those languages only accept request
/// shapes; the loanword/foreign nouns below are always nouns.
const REPORT_NOUNS: &str = r"レポート|bericht|informe|rapport|отчёт|отчет";

/// Chinese/Japanese logographs for "report", which double as verbs.
const CJK_NOUNS: &str = r"报告|報告";

/// Actions that turn a report noun into a request for the artifact.
const REQUEST_VERBS: &str = r"写|生成|做|出|整理|汇总|输出|导出|制作|起草|打印|给|要|来|整|打|发";

/// A request for a report.
///
/// English requires an explicit artifact request (`generate an HTML report`,
/// `write a report`, `report please`) because a bare `report` is far more often
/// a verb than an instruction to produce a document. The same holds for
/// `报告` / `報告`, which need a producing verb nearby.
static REPORT_REQUEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(?:{nouns}|\bhtml\s+reports?\b|\breports?\s+please\b|\b(?:generate|write|make|create|produce|prepare|draft)\w*\s+(?:me\s+)?(?:an?\s+|the\s+|some\s+)?(?:\w+\s+){{0,2}}reports?\b|(?:{verbs})[^。，,.!?！？;；\n]{{0,8}}(?:{cjk}))",
        nouns = REPORT_NOUNS,
        verbs = REQUEST_VERBS,
        cjk = CJK_NOUNS
    ))
    .expect("report request regex is valid")
});

/// Negation that precedes the noun, e.g. `不要写报告` / `don't write a report`.
///
/// A negation pattern rather than a fixed phrase list: the phrase list missed
/// `不要写报告` and `别报告了`, and both wrote a file *and* opened a browser
/// after the user explicitly asked not to. The gap is capped at 16 characters
/// and stops at punctuation so a negation in an earlier clause cannot veto a
/// later request (`别只给结论，写个报告`).
static REPORT_VETO_BEFORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(?:不要|不用|无需|不需要|别|停止|\bkein\w*|\bno\b|\bnot\b|\bne\b|\bне\b|don'?t|dont|do\s+not|\bwithout\b)[^。，,.!?！？;；\n]{{0,16}}(?:{nouns}|{cjk}|\breports?\b)",
        nouns = REPORT_NOUNS,
        cjk = CJK_NOUNS
    ))
    .expect("report veto regex is valid")
});

/// Negation that follows the noun, as Japanese and colloquial Chinese do:
/// `レポートはいらない` / `报告就不要了`. An `after` variant is required because
/// these languages negate by postfix rather than by a leading particle.
static REPORT_VETO_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(?:{nouns}|{cjk}|reports?)[^。，,.!?！？;；\n]{{0,8}}(?:不用了?|不要了?|不需要|いらない|いらん|要らない|不要|なし|not\s+(?:needed|necessary|required)|isn'?t\s+needed)",
        nouns = REPORT_NOUNS,
        cjk = CJK_NOUNS
    ))
    .expect("report postfix veto regex is valid")
});

/// `true` when the prompt explicitly asks for a report.
///
/// Deliberately a heuristic: no report is produced for ordinary questions and
/// shell commands, and a false negative costs one `/report` invocation. A veto
/// always wins, because acting against "don't write a report" is worse than
/// doing nothing.
pub fn should_auto_report(prompt: &str) -> bool {
    if REPORT_VETO_BEFORE.is_match(prompt) || REPORT_VETO_AFTER.is_match(prompt) {
        return false;
    }
    REPORT_REQUEST.is_match(prompt)
}

// ---------------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------------

/// HTML-escape text for element content and double-quoted attributes.
///
/// `pulldown-cmark-escape` is not a dependency of this crate, so the mapping
/// lives here; it must cover everything that can end a text node or break out
/// of an attribute.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Truncate on a char boundary, appending the visible cut marker.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str(TRUNCATION_MARKER);
    out
}

/// A Markdown fence longer than any backtick run inside `text`, so embedded
/// fences can never terminate the block early.
fn fenced(text: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}text\n{text}\n{fence}\n")
}

/// Render model-authored Markdown to HTML.
///
/// Raw HTML events are downgraded to `Event::Text`, which `push_html` escapes:
/// forwarding `Event::Html` verbatim would turn a model answer (or a log line it
/// quoted) into live markup.
fn markdown_to_html(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(text, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

/// Lines that only wrap a tool result and carry no information of their own.
const ENVELOPE_TAGS: [&str; 8] = [
    "<stdout>",
    "</stdout>",
    "<stderr>",
    "</stderr>",
    "<return_code>",
    "</return_code>",
    "<offload>",
    "</offload>",
];

/// One-line summary of a body, used as the collapsed `<summary>` text.
///
/// Envelope lines are skipped so the reader sees real content instead of
/// `<stdout>`, and the remaining lines are flattened until `limit` characters
/// have been collected, so pretty-printed JSON yields its keys rather than a
/// lone `{`.
fn preview_line(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || ENVELOPE_TAGS.contains(&line) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
        if out.chars().count() >= limit {
            break;
        }
    }
    clip(&out, limit).replace(['\n', '\t'], " ")
}

/// Escape a Markdown table cell.
///
/// `<` / `>` are escaped because many Markdown renderers pass raw HTML through;
/// a command containing `<script>` must not become live markup in the `.md`
/// copy. `&` is escaped first so the entities produced below are not
/// double-escaped.
fn table_cell(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "\\|")
        .replace('\n', " ")
        .replace('`', "\\`")
}

// ---------------------------------------------------------------------------
// Report model
// ---------------------------------------------------------------------------

/// Everything the renderer needs from the session.
#[derive(Debug, Clone, Copy)]
pub struct ReportInput<'a> {
    pub session_uuid: &'a str,
    pub model: &'a str,
    /// Pre-formatted session creation time (UTC).
    pub created_at: &'a str,
    pub cwd: Option<&'a str>,
    pub messages: &'a [SessionContextMessage],
    pub history: &'a [HistoryEntry],
    /// Transcript appends that failed for this session, if known.
    pub transcript_failed: usize,
}

/// Rendered artifacts plus the counters the terminal summary reports.
pub struct ReportBundle {
    pub html: String,
    pub markdown: String,
    pub messages: usize,
    pub evidence_rows: usize,
    pub gaps: usize,
    pub redaction_hits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceStatus {
    Ok,
    Truncated,
    Missing,
}

impl EvidenceStatus {
    fn key(self) -> &'static str {
        match self {
            Self::Ok => "shell.report.html.status_ok",
            Self::Truncated => "shell.report.html.status_truncated",
            Self::Missing => "shell.report.html.status_missing",
        }
    }

    fn css(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Truncated => "warn",
            Self::Missing => "bad",
        }
    }
}

struct EvidenceRow {
    index: usize,
    tool: String,
    detail: String,
    output: String,
    status: EvidenceStatus,
}

struct CommandRow {
    index: usize,
    source: String,
    rc: String,
    command: String,
    failed: bool,
}

struct ReportDoc {
    session_uuid: String,
    model: String,
    created_at: String,
    generated_at: String,
    cwd: Option<String>,
    objective: String,
    findings: Option<String>,
    evidence: Vec<EvidenceRow>,
    commands: Vec<CommandRow>,
    conversation: Vec<(String, String)>,
    gaps: Vec<String>,
    hits: usize,
    messages: usize,
}

/// Build the HTML and Markdown report for one session.
///
/// All session text is redacted with the same scanner `/export` uses; the
/// report has no raw mode (`/export --raw` remains the only escape hatch).
pub fn build_report(input: &ReportInput<'_>, scanner: &SecretScanner) -> ReportBundle {
    let mut hits = 0usize;

    let objective_raw = input
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let objective = clip(
        &redact_scoped(&objective_raw, scanner, &mut hits),
        SUMMARY_CHAR_LIMIT,
    );

    // The last assistant message with text is the model's final answer. A
    // turn that ended on a tool call leaves no conclusion — say so instead of
    // inventing one.
    let findings = input
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
        .map(|m| {
            clip(
                &redact_scoped(&m.content, scanner, &mut hits),
                SUMMARY_CHAR_LIMIT,
            )
        });

    let evidence = build_evidence(input.messages, scanner, &mut hits);
    let commands = build_commands(input.history, scanner, &mut hits);

    let mut conversation = Vec::new();
    for message in input.messages {
        let role = match message.role.as_str() {
            "user" => t("shell.report.html.role_user"),
            "assistant" => t("shell.report.html.role_assistant"),
            "system" => t("shell.report.html.role_system"),
            "tool" => t("shell.report.html.role_tool"),
            other => other.to_string(),
        };
        conversation.push((role, redact_scoped(&message.content, scanner, &mut hits)));
    }

    let gaps = build_gaps(input, &evidence, &commands, findings.is_none());

    let doc = ReportDoc {
        session_uuid: input.session_uuid.to_string(),
        model: input.model.to_string(),
        created_at: input.created_at.to_string(),
        generated_at: chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
        cwd: input.cwd.map(str::to_string),
        objective,
        findings,
        evidence,
        commands,
        conversation,
        gaps,
        hits,
        messages: input.messages.len(),
    };

    ReportBundle {
        html: render_html(&doc),
        markdown: render_markdown(&doc),
        messages: doc.messages,
        evidence_rows: doc.evidence.len(),
        gaps: doc.gaps.len(),
        redaction_hits: doc.hits,
    }
}

fn redact_scoped(text: &str, scanner: &SecretScanner, hits: &mut usize) -> String {
    let (out, count) = redact_secrets_with_hits(text, scanner);
    *hits += count;
    out
}

fn build_evidence(
    messages: &[SessionContextMessage],
    scanner: &SecretScanner,
    hits: &mut usize,
) -> Vec<EvidenceRow> {
    // Tool calls in message order: id -> (name, arguments). `std::HashMap`
    // would make the unanswered-call list non-deterministic in output.
    let mut calls: Vec<(String, String, String)> = Vec::new();
    for message in messages {
        if let Some(tool_calls) = &message.tool_calls {
            for call in tool_calls {
                calls.push((call.id.clone(), call.name.clone(), call.arguments.clone()));
            }
        }
    }
    let lookup = |id: &str| calls.iter().find(|(call_id, ..)| call_id == id);

    let mut rows: Vec<EvidenceRow> = Vec::new();
    let mut answered: Vec<String> = Vec::new();
    for message in messages {
        if message.role != "tool" {
            continue;
        }
        let id = message.tool_call_id.clone().unwrap_or_default();
        let found = lookup(&id);
        if !id.is_empty() {
            answered.push(id.clone());
        }
        let tool = found
            .map(|(_, name, _)| name.clone())
            .or_else(|| message.name.clone())
            .unwrap_or_else(|| t("shell.report.html.tool_unknown"));
        let detail = found
            .map(|(_, _, args)| pretty_args(args))
            .unwrap_or_default();
        let truncated = message.content.trim_end().ends_with(TRUNCATION_MARKER);
        let status = if message.content.trim().is_empty() {
            EvidenceStatus::Missing
        } else if truncated {
            EvidenceStatus::Truncated
        } else {
            EvidenceStatus::Ok
        };
        rows.push(EvidenceRow {
            index: rows.len() + 1,
            tool,
            detail: redact_scoped(&detail, scanner, hits),
            output: redact_scoped(&message.content, scanner, hits),
            status,
        });
    }

    for (id, name, args) in &calls {
        if answered.iter().any(|answered_id| answered_id == id) {
            continue;
        }
        rows.push(EvidenceRow {
            index: rows.len() + 1,
            tool: name.clone(),
            detail: redact_scoped(&pretty_args(args), scanner, hits),
            output: String::new(),
            status: EvidenceStatus::Missing,
        });
    }

    rows
}

/// Pretty-print JSON arguments when they parse; otherwise keep the raw text.
fn pretty_args(args: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(args) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| args.to_string()),
        Err(_) => args.to_string(),
    }
}

fn build_commands(
    history: &[HistoryEntry],
    scanner: &SecretScanner,
    hits: &mut usize,
) -> Vec<CommandRow> {
    history
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let rc = entry
                .returncode
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string());
            CommandRow {
                index: i + 1,
                source: entry.source.clone(),
                failed: entry.returncode.is_some_and(|code| code != 0),
                command: redact_scoped(&entry.command, scanner, hits),
                rc,
            }
        })
        .collect()
}

fn build_gaps(
    input: &ReportInput<'_>,
    evidence: &[EvidenceRow],
    commands: &[CommandRow],
    findings_missing: bool,
) -> Vec<String> {
    let mut gaps = Vec::new();
    let failed = commands.iter().filter(|row| row.failed).count();
    let truncated = evidence
        .iter()
        .filter(|row| row.status == EvidenceStatus::Truncated)
        .count();
    let missing = evidence
        .iter()
        .filter(|row| row.status == EvidenceStatus::Missing)
        .count();

    if failed > 0 {
        gaps.push(count_line("shell.report.html.gap_failed_commands", failed));
    }
    if truncated > 0 {
        gaps.push(count_line("shell.report.html.gap_truncated", truncated));
    }
    if missing > 0 {
        gaps.push(count_line("shell.report.html.gap_missing", missing));
    }
    if input.transcript_failed > 0 {
        gaps.push(count_line(
            "shell.report.html.gap_transcript_failed",
            input.transcript_failed,
        ));
    }
    if findings_missing {
        gaps.push(t("shell.report.html.gap_no_findings"));
    }
    if evidence.is_empty() && commands.is_empty() {
        gaps.push(t("shell.report.html.gap_no_evidence"));
    }
    // Both statements are always true of a generated report and are the two
    // ways a reader could misread it (as a fix, or as a live re-analysis).
    gaps.push(t("shell.report.html.gap_observations_only"));
    gaps.push(t("shell.report.html.gap_no_model_calls"));
    gaps
}

fn count_line(key: &str, count: usize) -> String {
    let mut args = std::collections::HashMap::new();
    args.insert("count".to_string(), count.to_string());
    t_with_args(key, &args)
}

// ---------------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------------

const HTML_STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --fg: #1c1c1e; --muted: #6b6b70; --bg: #ffffff; --surface: #f5f5f7;
  --border: #d8d8dd; --accent: #2a6df4; --error: #c0392b; --warning: #996400;
}
@media (prefers-color-scheme: dark) {
  :root {
    --fg: #e6e6e6; --muted: #a0a0a8; --bg: #141416; --surface: #1d1d20;
    --border: #303035; --accent: #6ea8ff; --error: #ff8577; --warning: #e0b341;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; padding: 2rem 1.25rem 4rem; background: var(--bg); color: var(--fg);
  font: 16px/1.55 system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
}
main, header { max-width: 60rem; margin: 0 auto; }
h1 { font-size: 1.6rem; margin: 0 0 .35rem; }
h2 { font-size: 1.15rem; margin: 2.25rem 0 .75rem; padding-bottom: .3rem; border-bottom: 1px solid var(--border); }
h3 { font-size: .95rem; margin: 0; }
p, dd { margin: .35rem 0; }
.muted { color: var(--muted); }
.meta dl { display: grid; grid-template-columns: max-content 1fr; gap: .2rem 1rem; margin: .75rem 0; }
.meta dt { color: var(--muted); }
.meta dd { margin: 0; overflow-wrap: anywhere; }
.banner {
  margin: 1rem 0; padding: .6rem .8rem; border-radius: .4rem;
  background: var(--surface); border: 1px solid var(--border); color: var(--muted);
  font-size: .9rem;
}
.meta dd code { word-break: break-all; }
pre {
  margin: .4rem 0; padding: .6rem .75rem; background: var(--surface); border: 1px solid var(--border);
  border-radius: .35rem; white-space: pre-wrap; overflow-wrap: anywhere;
  font: .86rem/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
details { border: 1px solid var(--border); border-radius: .4rem; padding: .5rem .7rem; margin: .5rem 0; }
details > summary { cursor: pointer; }
summary .evt { font-weight: 600; }
summary .preview { color: var(--muted); font: .82rem ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; margin-left: .6rem; }
summary > .badge { margin-left: .5rem; }
.badge { font-size: .76rem; padding: .05rem .4rem; border-radius: .3rem; border: 1px solid var(--border); background: var(--surface); }
.badge.ok { color: var(--fg); }
.badge.warn { color: var(--warning); border-color: var(--warning); }
.badge.bad { color: var(--error); border-color: var(--error); }
.label { color: var(--muted); font-size: .78rem; text-transform: uppercase; letter-spacing: .04em; margin-top: .7rem; }
.hint { color: var(--muted); margin: .5rem 0 0; font-size: .92rem; }
table { width: 100%; border-collapse: collapse; font-size: .9rem; }
th, td { text-align: left; padding: .35rem .5rem; border-bottom: 1px solid var(--border); vertical-align: top; }
th { color: var(--muted); font-weight: 600; }
td code { font: .85rem ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow-wrap: anywhere; }
tr.failed td.rc { color: var(--error); }
ul.gaps { padding-left: 1.2rem; }
ul.gaps li { margin: .3rem 0; }
.md > :first-child { margin-top: 0; }
.md > :last-child { margin-bottom: 0; }
.md h1, .md h2, .md h3, .md h4, .md h5, .md h6 {
  margin: 1.1rem 0 .45rem; padding: 0; border: 0; font-size: 1rem; line-height: 1.35;
}
.md h1, .md h2 { font-size: 1.08rem; }
.md ul, .md ol { margin: .5rem 0; padding-left: 1.4rem; }
.md li { margin: .22rem 0; }
.md li > p { margin: .2rem 0; }
.md code {
  font: .85rem ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  background: var(--surface); border: 1px solid var(--border); border-radius: .25rem; padding: .05rem .28rem;
}
.md pre code { background: none; border: 0; padding: 0; }
.md blockquote { margin: .7rem 0; padding: .1rem 0 .1rem .8rem; border-left: 3px solid var(--border); color: var(--muted); }
.md hr { border: 0; border-top: 1px solid var(--border); margin: 1.1rem 0; }
.md table { margin: .7rem 0; }
.md strong { font-weight: 700; }
.objective .md { color: var(--muted); font-size: .95rem; }
.findings .md { font-size: 1.02rem; }
footer { max-width: 60rem; margin: 3rem auto 0; color: var(--muted); font-size: .85rem; }
"#;

fn render_html(doc: &ReportDoc) -> String {
    let lang = aish_i18n::current_language();
    let title = format!(
        "{} — {}",
        t("shell.report.html.title"),
        &doc.session_uuid[..8.min(doc.session_uuid.len())]
    );
    let mut html = String::with_capacity(16 * 1024);
    let _ = write!(
        html,
        r#"<!DOCTYPE html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">
<meta name="generator" content="aish">
<title>{title}</title>
<style>{style}</style>
</head>
<body>
<header>
<h1>{title}</h1>
<div class="meta"><dl>
<dt>{k_session}</dt><dd><code>{v_session}</code></dd>
<dt>{k_model}</dt><dd>{v_model}</dd>
<dt>{k_created}</dt><dd>{v_created}</dd>
<dt>{k_generated}</dt><dd>{v_generated}</dd>
"#,
        lang = escape_html(&lang),
        title = escape_html(&title),
        style = HTML_STYLE,
        k_session = escape_html(&t("shell.report.html.session")),
        v_session = escape_html(&doc.session_uuid),
        k_model = escape_html(&t("shell.report.html.model")),
        v_model = escape_html(&doc.model),
        k_created = escape_html(&t("shell.report.html.created")),
        v_created = escape_html(&doc.created_at),
        k_generated = escape_html(&t("shell.report.html.generated")),
        v_generated = escape_html(&doc.generated_at),
    );
    if let Some(cwd) = &doc.cwd {
        let _ = writeln!(
            html,
            "<dt>{}</dt><dd><code>{}</code></dd>",
            escape_html(&t("shell.report.html.scope")),
            escape_html(cwd)
        );
    }
    html.push_str("</dl></div>\n");
    let _ = write!(
        html,
        "<p class=\"banner\">{}</p>\n</header>\n<main>\n",
        escape_html(&t_with_args("shell.report.html.redaction", &{
            let mut args = std::collections::HashMap::new();
            args.insert("hits".to_string(), doc.hits.to_string());
            args.insert(
                "rules".to_string(),
                aish_security::secret::REDACTION_RULES_VERSION.to_string(),
            );
            args
        }))
    );

    html.push_str(&format!(
        "<section id=\"objective\" class=\"objective\"><h2>{}</h2>\n<div class=\"md\">{}</div>\n</section>\n",
        escape_html(&t("shell.report.html.objective")),
        markdown_to_html(&doc.objective),
    ));

    html.push_str(&format!(
        "<section id=\"findings\" class=\"findings\"><h2>{}</h2>\n",
        escape_html(&t("shell.report.html.findings"))
    ));
    match &doc.findings {
        Some(text) => {
            let _ = writeln!(html, "<div class=\"md\">{}</div>", markdown_to_html(text));
        }
        None => {
            let _ = writeln!(
                html,
                "<p class=\"muted\">{}</p>",
                escape_html(&t("shell.report.html.findings_empty"))
            );
        }
    }
    html.push_str("</section>\n");

    // Evidence: one block per tool call, in call order. Rows start expanded so
    // nothing is hidden by default; the summary still allows collapsing noise.
    let _ = writeln!(
        html,
        "<section id=\"evidence\"><h2>{} ({})</h2>",
        escape_html(&t("shell.report.html.evidence")),
        doc.evidence.len()
    );
    if doc.evidence.is_empty() {
        let _ = writeln!(
            html,
            "<p class=\"muted\">{}</p>",
            escape_html(&t("shell.report.html.evidence_empty"))
        );
    } else {
        let _ = writeln!(
            html,
            "<p class=\"hint\">{}</p>",
            escape_html(&t_with_args("shell.report.html.evidence_hint", &{
                let mut args = std::collections::HashMap::new();
                args.insert("count".to_string(), doc.evidence.len().to_string());
                args
            }))
        );
    }
    for row in &doc.evidence {
        // Prefer the call arguments as the preview; fall back to the first line
        // of output when a tool call carried no arguments.
        let preview = if !row.detail.is_empty() {
            preview_line(&row.detail, 110)
        } else {
            preview_line(&row.output, 110)
        };
        let _ = writeln!(
            html,
            "<details open><summary><span class=\"evt\">E{} · {}</span><span class=\"badge {css}\">{status}</span><span class=\"preview\">{preview}</span></summary>",
            row.index,
            escape_html(&row.tool),
            css = row.status.css(),
            status = escape_html(&t(row.status.key())),
            preview = escape_html(&preview),
        );
        if !row.detail.is_empty() {
            let _ = writeln!(
                html,
                "<div class=\"label\">{}</div><pre>{}</pre>",
                escape_html(&t("shell.report.html.args_label")),
                escape_html(&row.detail)
            );
        }
        if row.output.is_empty() {
            let _ = writeln!(
                html,
                "<p class=\"muted\">{}</p>",
                escape_html(&t("shell.report.html.output_missing"))
            );
        } else {
            let _ = writeln!(
                html,
                "<div class=\"label\">{}</div><pre>{}</pre>",
                escape_html(&t("shell.report.html.output_label")),
                escape_html(&row.output)
            );
        }
        html.push_str("</details>\n");
    }
    html.push_str("</section>\n");

    let _ = writeln!(
        html,
        "<section id=\"commands\"><h2>{} ({})</h2>",
        escape_html(&t("shell.report.html.commands")),
        doc.commands.len()
    );
    if doc.commands.is_empty() {
        let _ = writeln!(
            html,
            "<p class=\"muted\">{}</p>\n<p class=\"hint\">{}</p>",
            escape_html(&t("shell.report.html.commands_empty")),
            escape_html(&t("shell.report.html.commands_empty_ai"))
        );
    } else {
        let _ = writeln!(
            html,
            "<table><thead><tr><th>#</th><th>{}</th><th>rc</th><th>{}</th></tr></thead><tbody>",
            escape_html(&t("shell.report.html.col_source")),
            escape_html(&t("shell.report.html.col_command"))
        );
        for row in &doc.commands {
            let _ = writeln!(
                html,
                "<tr class=\"{cls}\"><td>{idx}</td><td>{src}</td><td class=\"rc\">{rc}</td><td><code>{cmd}</code></td></tr>",
                cls = if row.failed { "failed" } else { "" },
                idx = row.index,
                src = escape_html(&row.source),
                rc = escape_html(&row.rc),
                cmd = escape_html(&row.command),
            );
        }
        html.push_str("</tbody></table>\n");
    }
    html.push_str("</section>\n");

    let _ = writeln!(
        html,
        "<section id=\"gaps\"><h2>{}</h2>\n<ul class=\"gaps\">",
        escape_html(&t("shell.report.html.gaps"))
    );
    for gap in &doc.gaps {
        let _ = writeln!(html, "<li>{}</li>", escape_html(gap));
    }
    html.push_str("</ul>\n</section>\n");

    let _ = writeln!(
        html,
        "<section id=\"conversation\"><h2>{} ({})</h2>",
        escape_html(&t("shell.report.html.conversation")),
        doc.conversation.len()
    );
    for (role, content) in &doc.conversation {
        let _ = writeln!(
            html,
            "<details><summary>{}</summary><pre>{}</pre></details>",
            escape_html(role),
            escape_html(content)
        );
    }
    html.push_str("</section>\n</main>\n");
    let _ = write!(
        html,
        "<footer>{}</footer>\n</body>\n</html>\n",
        escape_html(&t("shell.report.html.footer"))
    );
    html
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

fn render_markdown(doc: &ReportDoc) -> String {
    let mut md = String::with_capacity(8 * 1024);
    let _ = writeln!(md, "# {}", t("shell.report.html.title"));
    md.push_str(&crate::app::export_redaction_banner(true));
    let _ = writeln!(md, "- **session**: `{}`", doc.session_uuid);
    let _ = writeln!(md, "- **model**: `{}`", doc.model.replace('`', "'"));
    let _ = writeln!(md, "- **created**: `{}`", doc.created_at.replace('`', "'"));
    let _ = writeln!(md, "- **generated**: {}", doc.generated_at);
    if let Some(cwd) = &doc.cwd {
        let _ = writeln!(md, "- **cwd**: `{cwd}`");
    }
    let _ = writeln!(md);

    let _ = writeln!(md, "## {}", t("shell.report.html.objective"));
    let _ = writeln!(md, "{}", fenced(&doc.objective));

    let _ = writeln!(md, "## {}", t("shell.report.html.findings"));
    match &doc.findings {
        Some(text) => md.push_str(&fenced(text)),
        None => {
            let _ = writeln!(md, "{}\n", t("shell.report.html.findings_empty"));
        }
    }

    let _ = writeln!(
        md,
        "## {} ({})\n",
        t("shell.report.html.evidence"),
        doc.evidence.len()
    );
    if doc.evidence.is_empty() {
        let _ = writeln!(md, "{}\n", t("shell.report.html.evidence_empty"));
    }
    for row in &doc.evidence {
        let _ = writeln!(
            md,
            "### E{} · `{}` — {}\n",
            row.index,
            row.tool,
            t(row.status.key())
        );
        if !row.detail.is_empty() {
            let _ = writeln!(md, "_{}_", t("shell.report.html.args_label"));
            md.push_str(&fenced(&row.detail));
        }
        if row.output.is_empty() {
            let _ = writeln!(md, "{}\n", t("shell.report.html.output_missing"));
        } else {
            let _ = writeln!(md, "_{}_", t("shell.report.html.output_label"));
            md.push_str(&fenced(&row.output));
        }
    }

    let _ = writeln!(
        md,
        "## {} ({})\n",
        t("shell.report.html.commands"),
        doc.commands.len()
    );
    if doc.commands.is_empty() {
        let _ = writeln!(md, "{}\n", t("shell.report.html.commands_empty"));
    } else {
        let _ = writeln!(
            md,
            "| # | {} | rc | {} |\n|---|---|---|---|",
            t("shell.report.html.col_source"),
            t("shell.report.html.col_command")
        );
        for row in &doc.commands {
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} |",
                row.index,
                table_cell(&row.source),
                table_cell(&row.rc),
                table_cell(&row.command)
            );
        }
        md.push('\n');
    }

    let _ = writeln!(md, "## {}\n", t("shell.report.html.gaps"));
    for gap in &doc.gaps {
        let _ = writeln!(md, "- {gap}");
    }
    md.push('\n');

    let _ = writeln!(
        md,
        "## {} ({})\n",
        t("shell.report.html.conversation"),
        doc.conversation.len()
    );
    for (role, content) in &doc.conversation {
        let _ = writeln!(md, "### {role}\n");
        md.push_str(&fenced(content));
    }
    md
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aish_core::{ContextToolCall, MemoryType};
    use chrono::Utc;

    fn message(role: &str, content: &str) -> SessionContextMessage {
        SessionContextMessage {
            role: role.to_string(),
            content: content.to_string(),
            memory_type: MemoryType::Shell,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        }
    }

    fn tool_message(id: &str, name: &str, content: &str) -> SessionContextMessage {
        SessionContextMessage {
            name: Some(name.to_string()),
            tool_call_id: Some(id.to_string()),
            ..message("tool", content)
        }
    }

    fn call_message(id: &str, name: &str, args: &str) -> SessionContextMessage {
        SessionContextMessage {
            tool_calls: Some(vec![ContextToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: args.to_string(),
            }]),
            ..message("assistant", "")
        }
    }

    fn entry(command: &str, rc: i32) -> HistoryEntry {
        HistoryEntry {
            id: None,
            session_uuid: "s".to_string(),
            command: command.to_string(),
            source: "user".to_string(),
            returncode: Some(rc),
            stdout: None,
            stderr: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn escape_html_covers_attribute_and_text_breakers() {
        assert_eq!(
            escape_html("<script>alert('x' & \"y\")</script>"),
            "&lt;script&gt;alert(&#39;x&#39; &amp; &quot;y&quot;)&lt;/script&gt;"
        );
        assert_eq!(escape_html("plain text"), "plain text");
    }

    #[test]
    fn remote_and_graphical_detection_are_independent() {
        assert!(is_remote_session_from(true, false));
        assert!(is_remote_session_from(false, true));
        assert!(!is_remote_session_from(false, false));
        assert!(has_graphical_session_from(true, false, false));
        assert!(has_graphical_session_from(false, true, false));
        assert!(has_graphical_session_from(false, false, true));
        assert!(!has_graphical_session_from(false, false, false));
        assert!(can_auto_open(false, true));
        assert!(!can_auto_open(true, true));
        assert!(!can_auto_open(false, false));
    }

    #[test]
    fn reports_dir_honours_override_and_sanitises_session_id() {
        let dir = reports_dir(Some("/tmp/reports"), "../../etc/passwd");
        // Dots are not alphanumeric-or-underscore, so traversal cannot survive.
        assert_eq!(dir, PathBuf::from("/tmp/reports/______etc_passwd"));
        assert!(!dir.to_string_lossy().contains(".."));
        let empty = reports_dir(Some("   "), "abc-123");
        assert!(empty.ends_with("aish/reports/abc-123"));
    }

    #[test]
    fn auto_report_trigger_requires_an_explicit_request() {
        assert!(should_auto_report("分析系统状态并生成报告"));
        assert!(should_auto_report(
            "Analyze the slow server and generate an HTML report"
        ));
        assert!(should_auto_report("crée un rapport"));
        assert!(should_auto_report("生成一份报告"));
        assert!(!should_auto_report("ls -la /tmp"));
        assert!(!should_auto_report("how do I list files?"));
        assert!(!should_auto_report("不要生成报告，只告诉我结论"));
        assert!(!should_auto_report("no report please"));
    }

    #[test]
    fn auto_report_trigger_rejects_negations_beyond_fixed_phrases() {
        // Every case below matched the old fixed-phrase veto list and wrote a
        // file (plus opened a browser) despite asking for no report.
        for prompt in [
            "不要写报告，只给结论",
            "别报告了",
            "不用写报告",
            "不需要report",
            "don't write a report",
            "no need for a report",
            "do not produce a report",
            "keinen Bericht erstellen",
            "не нужен отчёт",
            "我该怎么做，不用报告",
            "no report",
        ] {
            assert!(
                !should_auto_report(prompt),
                "negated prompt must not trigger a report: {prompt}"
            );
        }
        // A postfix negation counts too; Japanese and Chinese use it.
        assert!(!should_auto_report("レポートはいらない"));
        assert!(!should_auto_report("报告就不要了"));
        // A negation in an earlier clause must not veto a later request.
        assert!(should_auto_report("别只给结论，写个报告"));
        assert!(should_auto_report("不要解释太多。生成报告"));
        // `stop` is ambiguous ("stop and write a report"), so it is not a veto.
        assert!(should_auto_report("stop and write a report"));
    }

    #[test]
    fn auto_report_trigger_ignores_verb_usage() {
        // `report` is far more often a verb here; only an artifact request
        // counts. Chinese 报告 behaves the same way.
        for prompt in [
            "how do I report a bug",
            "report the error to the maintainers",
            "the tool will report its status",
            "报告一下当前状态",
            "报告给管理员",
            "报告下载链接在哪",
            "write down the results",
        ] {
            assert!(
                !should_auto_report(prompt),
                "verb usage must not trigger a report: {prompt}"
            );
        }
        for prompt in [
            "write a report of the findings",
            "report please",
            "html report",
            "写个报告",
            "给我一份报告",
            "请汇总并输出报告",
        ] {
            assert!(
                should_auto_report(prompt),
                "artifact request must trigger a report: {prompt}"
            );
        }
    }

    #[test]
    fn markdown_table_cells_cannot_break_out_of_the_table() {
        assert_eq!(table_cell("a|b"), "a\\|b");
        assert_eq!(table_cell("line\nbreak"), "line break");
        assert_eq!(
            table_cell("<script>x</script>"),
            "&lt;script&gt;x&lt;/script&gt;"
        );
        assert_eq!(table_cell("a & b"), "a &amp; b");
        // `&` is escaped before the entities it introduces.
        assert_eq!(table_cell("<&>"), "&lt;&amp;&gt;");
    }

    #[test]
    fn html_never_contains_live_script_markup() {
        let scanner = SecretScanner::new(&[]);
        let messages = vec![
            message("user", "分析系统状态 <script>alert(1)</script>"),
            call_message("c1", "bash_exec", r#"{"command":"echo <b>x</b>"}"#),
            tool_message("c1", "bash_exec", "<script>alert(2)</script>"),
            message("assistant", "结论：<img src=x onerror=alert(3)>"),
        ];
        let history = vec![entry("echo '<script>alert(4)</script>'", 0)];
        let bundle = build_report(
            &ReportInput {
                session_uuid: "11111111-2222-3333-4444-555555555555",
                model: "test-model",
                created_at: "2026-01-01 00:00:00 UTC",
                cwd: Some("/tmp/<x>"),
                messages: &messages,
                history: &history,
                transcript_failed: 0,
            },
            &scanner,
        );
        assert!(!bundle.html.contains("<script"));
        assert!(!bundle.html.contains("<img"));
        assert!(bundle.html.contains("&lt;script&gt;"));
        assert!(bundle.html.contains("&lt;img src=x onerror=alert(3)&gt;"));
        // The Markdown copy keeps session text inside code fences; only the
        // command table is raw, and `table_cell` escapes it.
        assert!(!bundle.markdown.contains("| echo '<script"));
        // The document is offline-only: no external loads, no inline script.
        assert!(bundle.html.contains("default-src 'none'"));
        assert!(!bundle.html.contains("http://"));
        assert!(!bundle.html.contains("https://"));
        assert_eq!(bundle.evidence_rows, 1);
    }

    #[test]
    fn markdown_output_is_rendered_not_escaped() {
        let findings = "**诊断结论：**\n\n| 指标 | 状态 |\n| --- | --- |\n| 内存 | 正常 |\n\n- 第一项\n- 第二项\n\n`vm.swappiness` 为 60。";
        let messages = vec![
            message("user", "诊断系统状态"),
            message("assistant", findings),
        ];
        let bundle = build_report(
            &ReportInput {
                session_uuid: "11111111-2222-3333-4444-555555555555",
                model: "test-model",
                created_at: "2026-01-01 00:00:00 UTC",
                cwd: Some("/tmp"),
                messages: &messages,
                history: &[],
                transcript_failed: 0,
            },
            &SecretScanner::new(&[]),
        );
        assert!(bundle.html.contains("<strong>诊断结论：</strong>"));
        assert!(bundle.html.contains("<table>"));
        assert!(bundle.html.contains("<li>第一项</li>"));
        assert!(bundle.html.contains("<code>vm.swappiness</code>"));
        // The conversation transcript stays verbatim; only the rendered
        // conclusion must be free of raw Markdown syntax.
        let findings_start = bundle
            .html
            .find("id=\"findings\"")
            .expect("findings section");
        let evidence_start = bundle
            .html
            .find("id=\"evidence\"")
            .expect("evidence section");
        let findings = &bundle.html[findings_start..evidence_start];
        assert!(!findings.contains("**诊断结论"));
        assert!(!findings.contains("| 指标 |"));
    }

    #[test]
    fn markdown_raw_html_is_neutralised() {
        let messages = vec![
            message("user", "look"),
            message(
                "assistant",
                "<script>alert(1)</script> and <img src=x onerror=alert(2)>",
            ),
        ];
        let bundle = build_report(
            &ReportInput {
                session_uuid: "11111111-2222-3333-4444-555555555555",
                model: "test-model",
                created_at: "2026-01-01 00:00:00 UTC",
                cwd: None,
                messages: &messages,
                history: &[],
                transcript_failed: 0,
            },
            &SecretScanner::new(&[]),
        );
        assert!(!bundle.html.contains("<script"));
        assert!(!bundle.html.contains("<img"));
        // Downgraded to text, so it stays readable instead of vanishing.
        assert!(bundle
            .html
            .contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn empty_command_table_explains_tool_only_sessions() {
        // Issue #608 follow-up: a session diagnosed purely through tool calls
        // has no shell history, so `Commands (0)` must not read as data loss.
        let messages = vec![message("user", "诊断一下内存")];
        let bundle = build_report(
            &ReportInput {
                session_uuid: "11111111-2222-3333-4444-555555555555",
                model: "test-model",
                created_at: "2026-01-01 00:00:00 UTC",
                cwd: None,
                messages: &messages,
                history: &[],
                transcript_failed: 0,
            },
            &SecretScanner::new(&[]),
        );
        let start = bundle
            .html
            .find("id=\"commands\"")
            .expect("commands section");
        let end = bundle.html.find("id=\"gaps\"").expect("gaps section");
        assert!(bundle.html[start..end]
            .contains(&escape_html(&t("shell.report.html.commands_empty_ai"))));
    }

    #[test]
    fn report_redacts_secrets_in_every_section() {
        let scanner = SecretScanner::new(&[]);
        let secret = "sk-abcdef0123456789abcdef0123456789";
        let messages = vec![
            message("user", &format!("use token {secret}")),
            message("assistant", &format!("export TOKEN={secret}")),
        ];
        let history = vec![entry(
            &format!("curl -H 'Authorization: Bearer {secret}' x"),
            1,
        )];
        let bundle = build_report(
            &ReportInput {
                session_uuid: "s1",
                model: "m",
                created_at: "now",
                cwd: None,
                messages: &messages,
                history: &history,
                transcript_failed: 0,
            },
            &scanner,
        );
        assert!(!bundle.html.contains(secret));
        assert!(!bundle.markdown.contains(secret));
        assert!(bundle.html.contains("[REDACTED:"));
        assert!(bundle.redaction_hits >= 3);
    }

    #[test]
    fn gaps_report_failures_truncation_and_missing_results() {
        let scanner = SecretScanner::new(&[]);
        let messages = vec![
            message("user", "diagnose"),
            call_message("c1", "bash_exec", "{}"),
            tool_message("c1", "bash_exec", "partial...(truncated)"),
            call_message("c2", "read_file", "{}"),
        ];
        let history = vec![entry("false", 1), entry("true", 0)];
        let bundle = build_report(
            &ReportInput {
                session_uuid: "s1",
                model: "m",
                created_at: "now",
                cwd: None,
                messages: &messages,
                history: &history,
                transcript_failed: 2,
            },
            &scanner,
        );
        assert_eq!(bundle.evidence_rows, 2);
        // Failed command, truncated output, missing result, failed appends,
        // missing conclusions, plus the two always-on disclaimers.
        assert_eq!(bundle.gaps, 7);
        assert!(bundle.html.contains("(truncated)"));
        assert!(bundle.html.contains("class=\"failed\""));
    }

    #[test]
    fn markdown_fence_outlives_embedded_backticks() {
        let rendered = fenced("```\ninner\n```");
        assert!(rendered.starts_with("````text\n"));
        assert!(rendered.ends_with("\n````\n"));
    }

    #[test]
    fn write_private_file_is_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/report.html");
        write_private_file(&path, b"<html>ok</html>").expect("write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "<html>ok</html>"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            let dir_mode = std::fs::metadata(path.parent().expect("parent"))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
    }
}
