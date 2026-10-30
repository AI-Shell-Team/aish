//! Final-answer HTML export (`/report`, issue #608).
//!
//! Deliberately small: only `/report` calls in, and it writes the final answer
//! of the last turn to `<data_dir>/aish/reports/<session>/report.html`, rendered
//! from the Markdown the model produced. Nothing is written automatically, so
//! no configuration decides whether a page appears. The page is self-contained —
//! inline CSS, no scripts, no external resources, CSP `default-src 'none'` — so
//! it opens with no network.
//!
//! Raw HTML in the answer is downgraded to escaped text: `push_html` forwards
//! `Event::Html` verbatim, so a log line containing `<script>` would otherwise
//! become live markup. Links are restricted to `http`/`https`/`mailto` and
//! fragments for the same reason.
//!
//! Besides the answer, the page lists the tool calls the turn made, joined to
//! their results through the recorded `tool_call_id`. Every body goes through
//! [`escape_html`] into `<pre>`, so tool output stays text: it is never parsed
//! as Markdown and cannot become markup. A call whose result is missing, cut at
//! the 4 KB tool cap, or only an offload preview is labelled as such instead of
//! being shown as if it were complete.
//!
//! Nothing here touches the session or re-runs a command: the caller hands over
//! the messages it already read.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use aish_i18n::t;

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
fn is_remote_session_from(has_connection: bool, has_tty: bool) -> bool {
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
fn has_graphical_session_from(native_gui: bool, display: bool, wayland: bool) -> bool {
    native_gui || display || wayland
}

// ---------------------------------------------------------------------------
// Output paths
// ---------------------------------------------------------------------------

/// Directory holding this session's report file.
///
/// `$XDG_DATA_HOME/aish/reports/<session>` (`~/.local/share/...` on Linux).
/// There is no override: a report is an on-demand artifact, so its location is
/// fixed and always printed by `/report`.
pub fn reports_dir(session_uuid: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aish")
        .join("reports")
        .join(sanitize_component(session_uuid))
}

/// Keep the session id from escaping the report directory. Session ids are
/// generated UUIDs, but `/fork` and imported sessions are not guaranteed to be,
/// and a path component must never carry separators or `..`.
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
/// Files are created 0600 and the containing directory 0700 from the start: the
/// page holds the model's answer, which may quote logs.
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
// Rendering
// ---------------------------------------------------------------------------

/// Escape text for element content and double-quoted attributes.
fn escape_html(text: &str) -> String {
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

/// Render Markdown to HTML, dropping anything that could execute.
///
/// Raw HTML becomes text (visible, inert). A link or image whose destination is
/// not `http`/`https`/`mailto`/fragment is dropped: `push_html` writes
/// `href`/`src` verbatim, so `[x](javascript:...)` would otherwise be a live
/// link in a page that claims to be inert.
fn markdown_to_html(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // Counters are per kind because an image may sit inside a link; one shared
    // counter would drop the wrong closing event.
    let mut skipped_links = 0usize;
    let mut skipped_images = 0usize;
    let events = Parser::new_ext(text, options).filter_map(move |event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
        Event::Start(Tag::Link { ref dest_url, .. }) if !is_safe_destination(dest_url) => {
            // Keep the label, drop the navigation.
            skipped_links += 1;
            None
        }
        Event::End(TagEnd::Link) if skipped_links > 0 => {
            skipped_links -= 1;
            None
        }
        Event::Start(Tag::Image { ref dest_url, .. }) if !is_safe_destination(dest_url) => {
            skipped_images += 1;
            None
        }
        Event::End(TagEnd::Image) if skipped_images > 0 => {
            skipped_images -= 1;
            None
        }
        other => Some(other),
    });
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

/// Whether a link/image destination may survive into the page.
fn is_safe_destination(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.starts_with('#') {
        return true;
    }
    let Some((scheme, _)) = trimmed.split_once(':') else {
        // No scheme: a relative reference. `//host` is protocol-relative and
        // would silently load a remote resource, so it is rejected.
        return !trimmed.starts_with("//");
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return false;
    }
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

// ---------------------------------------------------------------------------
// Tool evidence
// ---------------------------------------------------------------------------

/// Marker `aish_llm::cap_tool_output` appends when a tool result was cut at
/// [`aish_llm::MAX_TOOL_RESULT_BYTES`] before entering the transcript.
const TRUNCATION_MARKER: &str = "...(truncated)";

/// How complete a recorded tool result is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceStatus {
    Ok,
    /// Cut at the 4 KB tool cap, so only the head is on the page.
    Truncated,
    /// Only an offload envelope is in the transcript; full output is on disk.
    Offloaded,
    /// The call has no tool message at all.
    Missing,
}

impl EvidenceStatus {
    fn label(self) -> String {
        t(match self {
            Self::Ok => "shell.report.html.status_ok",
            Self::Truncated => "shell.report.html.status_truncated",
            Self::Offloaded => "shell.report.html.status_offloaded",
            Self::Missing => "shell.report.html.status_missing",
        })
    }

    fn css(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Truncated | Self::Offloaded => "warn",
            Self::Missing => "bad",
        }
    }
}

/// One tool call of the reported turn, paired with its recorded result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    /// Tool name the model requested; empty when the call carried none.
    pub tool: String,
    /// Raw JSON arguments string, as recorded.
    pub arguments: String,
    /// Recorded result body, or `None` when no tool message exists.
    pub output: Option<String>,
    pub status: EvidenceStatus,
}

/// Classify a recorded result body.
///
/// Markers are matched where the writer puts them, not anywhere in the text:
/// `aish_tools::format_tagged_result` appends the offload block as the last
/// section, and `cap_tool_output` appends the truncation marker at the very
/// end. A plain `contains` reads a log line that happens to print
/// `<offload>` — or a command whose output quotes the marker — as metadata,
/// and would label complete output as incomplete.
///
/// `Truncated` wins over `Offloaded`: a preview that was itself cut tells the
/// reader more (both the cap and the offload apply) than the offload marker
/// alone, and the ordering keeps the status stable for the same input.
fn classify_output(body: Option<&str>) -> EvidenceStatus {
    let Some(text) = body else {
        return EvidenceStatus::Missing;
    };
    if text.trim_end().ends_with(TRUNCATION_MARKER) {
        return EvidenceStatus::Truncated;
    }
    // The offload envelope is the final block; nothing follows `</offload>`.
    if text
        .trim_end()
        .split_once("</offload>")
        .is_some_and(|(_, rest)| rest.trim().is_empty())
    {
        return EvidenceStatus::Offloaded;
    }
    EvidenceStatus::Ok
}

/// Pair every assistant `tool_calls` entry of the last turn with the tool
/// message that answers it, in call order.
///
/// Both the arguments and the result body are redacted through `scanner`: a
/// tool argument is a provider-supplied string that reaches the transcript
/// verbatim, so `{"command": "export API_KEY=sk-..."}` would otherwise be
/// written to a file the user may hand to someone else. The answer goes
/// through the same scanner, so terminal, answer and evidence agree.
///
/// Only the turn being reported is scanned. A session keeps every turn in one
/// transcript, so walking all of it would list the calls of earlier questions
/// as evidence for this answer — a fifty-turn session produces a report whose
/// evidence section has nothing to do with its conclusion. The current turn
/// starts at the last user prompt (`role == "user"`, `MemoryType::Llm`); shell
/// command records are also `role == "user"` but carry `MemoryType::Shell`, so
/// they are skipped.
///
/// Pairing is by `tool_call_id`, not by position: a turn may interleave calls
/// and results, and a provider that dropped a result must leave that call
/// visibly unpaired rather than shifting every later row.
pub fn collect_evidence(
    messages: &[aish_session::SessionContextMessage],
    scanner: &aish_security::secret::SecretScanner,
) -> Vec<EvidenceRow> {
    use std::collections::HashMap;

    let redact = |text: &str| aish_security::secret::redact_secrets(text, scanner);

    let turn_start = messages
        .iter()
        .rposition(|m| m.role == "user" && m.memory_type == aish_core::MemoryType::Llm)
        .unwrap_or(0);
    let turn = &messages[turn_start..];

    let results: HashMap<&str, &str> = turn
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.as_deref().map(|id| (id, m.content.as_str())))
        .collect();

    let mut rows = Vec::new();
    for message in turn.iter().filter(|m| m.role == "assistant") {
        let Some(calls) = message.tool_calls.as_ref() else {
            continue;
        };
        for call in calls {
            // Classify the borrowed body, then take the one owned copy.
            let body = results.get(call.id.as_str()).copied();
            rows.push(EvidenceRow {
                tool: call.name.clone(),
                arguments: redact(&call.arguments),
                status: classify_output(body),
                output: body.map(redact),
            });
        }
    }
    rows
}

/// Argument keys that name what a tool acted on, most useful first.
///
/// Used only for the one-line hint in a collapsed row: `prompt` comes last
/// because it holds multi-paragraph prose, while `description` is its short
/// label. An unknown tool still gets a preview from its first string value.
const PREVIEW_KEYS: &[&str] = &[
    "command",
    "path",
    "file_path",
    "pattern",
    "query",
    "url",
    "description",
    "action",
    "tool",
    "prompt",
];

/// One-line summary of a call's arguments for a collapsed row.
///
/// The collapsed list is what a reader scans, so a row that only shows a tool
/// name cannot be told apart from the twenty others. Newlines are flattened so
/// a multi-line command stays on one line.
fn preview_of_arguments(raw: &str, limit: usize) -> String {
    let text = match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(map)) => PREVIEW_KEYS
            .iter()
            .find_map(|key| map.get(*key).and_then(|v| v.as_str()))
            .or_else(|| map.values().find_map(|v| v.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| raw.to_string()),
        Ok(serde_json::Value::String(inner)) => inner,
        _ => raw.to_string(),
    };
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut clipped: String = flat.chars().take(limit).collect();
    clipped.push('…');
    clipped
}

/// Every status, in the order the totals are shown.
const STATUSES: [EvidenceStatus; 4] = [
    EvidenceStatus::Ok,
    EvidenceStatus::Truncated,
    EvidenceStatus::Offloaded,
    EvidenceStatus::Missing,
];

/// Totals per status, aligned with [`STATUSES`].
fn status_counts(rows: &[EvidenceRow]) -> [usize; 4] {
    let mut counts = [0usize; 4];
    for row in rows {
        let slot = STATUSES
            .iter()
            .position(|s| *s == row.status)
            .expect("row.status is one of STATUSES");
        counts[slot] += 1;
    }
    counts
}

/// Render the collapsible evidence section.
///
/// `<details>` is native, so collapsing needs no script and the page keeps its
/// `default-src 'none'` policy. Bodies are escaped into `<pre>`: tool output is
/// arbitrary bytes, not Markdown.
///
/// Every row starts collapsed. The one-line argument preview plus the status
/// colour stripe make the list scannable, so no example has to be forced open.
fn render_evidence(rows: &[EvidenceRow]) -> String {
    let mut html = String::new();
    let _ = write!(
        html,
        "<section class=\"evidence\">\n<h2>{}\n",
        escape_html(&t("shell.report.html.evidence"))
    );
    if rows.is_empty() {
        let _ = writeln!(
            html,
            "</h2>\n<p class=\"muted\">{}</p>\n</section>",
            escape_html(&t("shell.report.html.evidence_empty"))
        );
        return html;
    }

    // Totals sit next to the heading: how much of the evidence is complete is
    // the first thing a reader wants from a long list.
    let counts = status_counts(rows);
    let mut parts = Vec::new();
    for (status, count) in STATUSES.into_iter().zip(counts) {
        if count > 0 {
            parts.push(format!("{count} {}", escape_html(&status.label())));
        }
    }
    let _ = writeln!(
        html,
        "<span class=\"counts\">{}</span></h2>",
        parts.join(" · ")
    );

    let hint = t("shell.report.html.evidence_hint").replace("{count}", &rows.len().to_string());
    let _ = writeln!(html, "<p class=\"muted\">{}</p>", escape_html(&hint));

    for (index, row) in rows.iter().enumerate() {
        let tool = if row.tool.trim().is_empty() {
            escape_html(&t("shell.report.html.tool_unknown"))
        } else {
            escape_html(&row.tool)
        };
        let css = row.status.css();
        let _ = write!(
            html,
            "<details class=\"evidence-row {css}\">\n<summary>
<span class=\"idx\">{index}</span>
<code class=\"tool\">{tool}</code>
<span class=\"status {css}\">{label}</span>
<span class=\"preview\">{preview}</span>
</summary>
<div class=\"row-body\">
<div class=\"field\"><span class=\"field-label\">{args_label}</span><pre class=\"args\"><code>{args}</code></pre></div>
<div class=\"field\"><span class=\"field-label\">{output_label}</span><pre class=\"out\"><code>{output}</code></pre></div>
</div>
</details>
",
            index = index + 1,
            label = escape_html(&row.status.label()),
            preview = escape_html(&preview_of_arguments(&row.arguments, 120)),
            args_label = escape_html(&t("shell.report.html.args_label")),
            args = escape_html(&pretty_json(&row.arguments)),
            output_label = escape_html(&t("shell.report.html.output_label")),
            output = match &row.output {
                Some(body) => escape_html(body),
                None => escape_html(&t("shell.report.html.output_missing")),
            },
        );
    }
    html.push_str("</section>\n");
    html
}

/// Pretty-print recorded arguments when they are JSON, else return them as-is.
///
/// The stored form is the raw provider string; re-indenting it only affects how
/// the arguments read on the page, never what they mean.
fn pretty_json(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

const STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --fg: #1c1c1e; --muted: #6b6b70; --bg: #ffffff; --surface: #f5f5f7;
  --border: #d8d8dd; --error: #c0392b; --warn: #8a5a00; --ok: #1f7a4d;
}
@media (prefers-color-scheme: dark) {
  :root {
    --fg: #e6e6e6; --muted: #a0a0a8; --bg: #141416; --surface: #1d1d20;
    --border: #303035; --error: #ff8577; --warn: #e0b055; --ok: #62c08a;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; padding: 2.5rem 1.25rem 5rem; background: var(--bg); color: var(--fg);
  font: 16px/1.65 system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
  -webkit-text-size-adjust: 100%;
}
main, header { max-width: 54rem; margin: 0 auto; }

header { margin-bottom: 2.5rem; padding-bottom: 1.25rem; border-bottom: 1px solid var(--border); }
header h1 { margin: 0 0 .75rem; }
header dl { display: grid; grid-template-columns: max-content 1fr; gap: .2rem 1rem; margin: 0; }
header dt { color: var(--muted); font-size: .82rem; }
header dd { margin: 0; font-size: .82rem; overflow-wrap: anywhere; }
header dd code { background: none; border: 0; padding: 0; font-size: .82rem; }

h1, h2, h3, h4, h5, h6 { line-height: 1.35; margin: 1.8rem 0 .7rem; }
h1 { font-size: 1.55rem; letter-spacing: -.01em; }
h2 { font-size: 1.15rem; padding-bottom: .4rem; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.03rem; }
h4 { font-size: .95rem; color: var(--muted); }
p, li, dd, td, th { overflow-wrap: break-word; }
a { color: inherit; text-decoration: underline; text-underline-offset: .15em; }
ul, ol { padding-left: 1.4rem; }
li { margin: .3rem 0; }
li > p { margin: .3rem 0; }

code {
  font: .875em/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  background: var(--surface); border: 1px solid var(--border); border-radius: .25rem;
  padding: .05rem .3rem;
}
pre {
  margin: .6rem 0; background: var(--surface); border: 1px solid var(--border);
  border-radius: .4rem; padding: .75rem .9rem; overflow: auto;
  font: .82rem/1.55 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
pre code { background: none; border: 0; padding: 0; font-size: inherit; }

blockquote {
  margin: 1rem 0; padding: .1rem 0 .1rem 1rem;
  border-left: 3px solid var(--border); color: var(--muted);
}
table { width: 100%; border-collapse: collapse; margin: 1rem 0; font-size: .93rem; display: block; overflow-x: auto; }
th, td { text-align: left; padding: .45rem .6rem; border-bottom: 1px solid var(--border); vertical-align: top; }
th { color: var(--muted); font-weight: 600; border-bottom-width: 2px; }
hr { border: 0; border-top: 1px solid var(--border); margin: 2rem 0; }

.muted { color: var(--muted); font-size: .9rem; }

/* Evidence ------------------------------------------------------------ */
.evidence { margin-top: 3rem; padding-top: .5rem; border-top: 1px solid var(--border); }
.evidence h2 {
  display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between;
  gap: .25rem .75rem;
}
.evidence .counts { font-size: .8rem; font-weight: 400; color: var(--muted); font-variant-numeric: tabular-nums; }

details {
  margin: .4rem 0; border: 1px solid var(--border); border-left: 3px solid var(--border);
  border-radius: .4rem; background: transparent;
}
details.warn { border-left-color: var(--warn); }
details.bad { border-left-color: var(--error); }
details.ok { border-left-color: var(--ok); }
details > summary {
  cursor: pointer; list-style: none; padding: .5rem .7rem;
  display: flex; flex-wrap: wrap; align-items: baseline; gap: .1rem .55rem;
}
details > summary::-webkit-details-marker { display: none; }
details > summary::before {
  content: "▸"; color: var(--muted); font-size: .8rem; width: .9rem;
  transition: transform .12s ease;
}
details[open] > summary::before { transform: rotate(90deg); }
details[open] > summary { border-bottom: 1px solid var(--border); }
details > summary:hover { background: var(--surface); }

.idx {
  color: var(--muted); font-variant-numeric: tabular-nums; font-size: .8rem;
  min-width: 1.4rem; text-align: right;
}
summary code.tool {
  background: none; border: 0; padding: 0; font-size: .86rem; font-weight: 600;
  white-space: nowrap;
}
.status {
  font-size: .74rem; line-height: 1.5; padding: 0 .45rem; border-radius: .7rem;
  border: 1px solid currentColor; white-space: nowrap;
}
.status.ok { color: var(--ok); }
.status.warn { color: var(--warn); }
.status.bad { color: var(--error); }
.preview {
  flex: 1 1 100%; padding-left: 1.45rem; color: var(--muted); font-size: .82rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.preview:empty { display: none; }

.row-body { padding: .6rem .7rem .8rem; }
.row-body .field { margin: 0 0 .6rem; }
.row-body .field:last-child { margin-bottom: 0; }
.field-label {
  display: block; margin-bottom: .25rem;
  font-size: .74rem; letter-spacing: .04em; text-transform: uppercase; color: var(--muted);
}
.row-body pre { margin: 0; max-height: 24rem; }

@media (max-width: 40rem) {
  body { padding: 1.5rem .75rem 3rem; }
  .preview { white-space: normal; }
  header dl { grid-template-columns: 1fr; }
  header dt { margin-top: .35rem; }
}
@media print {
  body { padding: 0; }
  details { break-inside: avoid; }
  details > summary { cursor: default; }
  /* A printed copy is an archive: collapsed evidence would be lost on paper.
     Browsers without the pseudo-element keep their previous behaviour. */
  details::details-content { content-visibility: visible; display: block; }
  details > summary::before { content: ""; width: 0; }
  .row-body pre { max-height: none; }
}
"#;

/// Build the complete page for one answer.
///
/// `title` is the page/session heading; `meta` holds label/value rows shown
/// above the answer (session id, model, time); `evidence` lists the tool calls
/// behind the answer, rendered below it as collapsed blocks.
pub fn render_page(
    title: &str,
    meta: &[(&str, &str)],
    markdown: &str,
    evidence: &[EvidenceRow],
) -> String {
    let lang = aish_i18n::current_language();
    let mut html = String::with_capacity(markdown.len() * 2 + 4096);
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
<dl>
"#,
        lang = escape_html(&lang),
        title = escape_html(title),
        style = STYLE,
    );
    for (label, value) in meta {
        let _ = writeln!(
            html,
            "<dt>{}</dt><dd><code>{}</code></dd>",
            escape_html(label),
            escape_html(value)
        );
    }
    html.push_str("</dl>\n</header>\n<main>\n");
    html.push_str(&markdown_to_html(markdown));
    html.push('\n');
    html.push_str(&render_evidence(evidence));
    html.push_str("</main>\n</body>\n</html>\n");
    html
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_covers_attribute_and_text_breakers() {
        assert_eq!(
            escape_html("<script>alert('x' & \"y\")</script>"),
            "&lt;script&gt;alert(&#39;x&#39; &amp; &quot;y&quot;)&lt;/script&gt;"
        );
    }

    #[test]
    fn remote_and_graphical_detection_are_independent() {
        assert!(is_remote_session_from(true, false));
        assert!(!is_remote_session_from(false, false));
        assert!(has_graphical_session_from(false, true, false));
        assert!(has_graphical_session_from(false, false, true));
        assert!(!has_graphical_session_from(false, false, false));
    }

    #[test]
    fn reports_dir_sanitises_the_session_id() {
        let dir = reports_dir("a/b/../c");
        assert!(dir.ends_with("aish/reports/a_b____c"), "{dir:?}");
        let dir = reports_dir("///");
        assert!(dir.ends_with("aish/reports/session"), "{dir:?}");
    }

    #[test]
    fn markdown_is_rendered_not_escaped() {
        let html = render_page(
            "t",
            &[],
            "**结论**\n\n| 指标 | 状态 |\n| --- | --- |\n| 内存 | 正常 |\n\n- 第一项\n\n`vm.swappiness`",
            &[],
        );
        assert!(html.contains("<strong>结论</strong>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<li>第一项</li>"));
        assert!(html.contains("<code>vm.swappiness</code>"));
    }

    #[test]
    fn raw_html_stays_inert() {
        let html = render_page(
            "t",
            &[],
            "<script>alert(1)</script><img src=x onerror=alert(2)>",
            &[],
        );
        assert!(!html.contains("<script"));
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("default-src 'none'"));
    }

    #[test]
    fn unsafe_link_destinations_are_dropped() {
        let html = render_page("t", &[], "[click](javascript:alert(1))", &[]);
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("<a "));
        assert!(html.contains("click"));

        assert!(render_page("t", &[], "[d](https://example.com)", &[])
            .contains("href=\"https://example.com\""));
        assert!(!render_page("t", &[], "![x](data:text/html,<script>)", &[]).contains("<img"));
        assert!(!render_page("t", &[], "[x](//evil.example/x)", &[]).contains("href"));
    }

    #[test]
    fn page_escapes_meta_values() {
        let html = render_page("a <b>", &[("session", "<uuid>")], "ok", &[]);
        assert!(html.contains("a &lt;b&gt;"));
        assert!(html.contains("&lt;uuid&gt;"));
    }

    fn msg(
        role: &str,
        content: &str,
        tool_call_id: Option<&str>,
        tool_calls: Option<Vec<aish_core::ContextToolCall>>,
    ) -> aish_session::SessionContextMessage {
        aish_session::SessionContextMessage {
            role: role.to_string(),
            content: content.to_string(),
            memory_type: aish_core::MemoryType::Llm,
            name: None,
            tool_call_id: tool_call_id.map(String::from),
            tool_calls,
            reasoning_content: None,
        }
    }

    /// Scanner with the default pattern set, for the evidence tests.
    fn test_scanner() -> aish_security::secret::SecretScanner {
        aish_security::secret::SecretScanner::new(&[])
    }

    fn call(id: &str, name: &str, arguments: &str) -> aish_core::ContextToolCall {
        aish_core::ContextToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn evidence_pairs_calls_with_results_by_id_not_position() {
        // The result of the second call is recorded before the first one, as a
        // provider may emit them out of order. Pairing by position would attach
        // each body to the wrong tool.
        let messages = vec![
            msg(
                "assistant",
                "checking",
                None,
                Some(vec![
                    call("a", "bash", r#"{"command":"df -h"}"#),
                    call("b", "read_file", r#"{"path":"/etc/fstab"}"#),
                ]),
            ),
            msg("tool", "rootfs 20G", Some("b"), None),
            msg("tool", "/dev/sda1 20G", Some("a"), None),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tool, "bash");
        assert_eq!(rows[0].output.as_deref(), Some("/dev/sda1 20G"));
        assert_eq!(rows[1].tool, "read_file");
        assert_eq!(rows[1].output.as_deref(), Some("rootfs 20G"));
        assert!(rows.iter().all(|r| r.status == EvidenceStatus::Ok));
    }

    #[test]
    fn evidence_only_covers_the_last_turn() {
        // A session keeps every turn in one transcript. Walking all of it made
        // the report list earlier questions' tool calls as evidence for this
        // answer: on a 16-turn session with 0 calls in its final turn, all 38
        // historical calls appeared. The turn starts at the last LLM prompt.
        let messages = vec![
            msg("user", "first question", None, None),
            msg("assistant", "", None, Some(vec![call("old", "bash", "{}")])),
            msg("tool", "old output", Some("old"), None),
            msg("assistant", "first answer", None, None),
            msg("user", "second question", None, None),
            msg("assistant", "second answer", None, None),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        assert!(rows.is_empty(), "got {rows:?}");
    }

    #[test]
    fn evidence_skips_shell_records_when_locating_the_turn() {
        // Shell command records are `role == "user"` too; treating one as the
        // turn boundary would drop the real prompt's calls.
        let mut shell = msg("user", "[Shell] ls", None, None);
        shell.memory_type = aish_core::MemoryType::Shell;
        let messages = vec![
            shell,
            msg("user", "the real question", None, None),
            msg("assistant", "", None, Some(vec![call("a", "bash", "{}")])),
            msg("tool", "out", Some("a"), None),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].output.as_deref(), Some("out"));
    }

    #[test]
    fn evidence_ignores_a_result_from_a_previous_turn() {
        // Duplicate ids across turns must not attach an old body to this one.
        let messages = vec![
            msg("user", "first", None, None),
            msg("assistant", "", None, Some(vec![call("a", "bash", "{}")])),
            msg("tool", "old body", Some("a"), None),
            msg("user", "second", None, None),
            msg("assistant", "", None, Some(vec![call("a", "bash", "{}")])),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, EvidenceStatus::Missing);
    }

    #[test]
    fn evidence_flags_a_call_without_a_result() {
        let messages = vec![
            msg("assistant", "", None, Some(vec![call("a", "bash", "{}")])),
            msg("assistant", "done", None, None),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, EvidenceStatus::Missing);
        // Locale-independent check: the row carries the "bad" status class and
        // its body falls back to the missing-output line.
        let html = render_page("t", &[], "ok", &rows);
        assert!(html.contains("class=\"status bad\""));
        assert!(html.contains(&t("shell.report.html.output_missing")));
    }

    #[test]
    fn evidence_distinguishes_truncated_and_offload_previews() {
        // Both states mean "the page does not hold the whole body"; they must
        // stay distinguishable so the reader knows whether a full file exists.
        // Bodies follow `format_tagged_result`: stdout, then `</offload>` last.
        let messages = vec![
            msg("assistant", "", None, Some(vec![call("a", "bash", "{}")])),
            msg("tool", "head...(truncated)", Some("a"), None),
            msg("assistant", "", None, Some(vec![call("b", "bash", "{}")])),
            msg(
                "tool",
                "<stdout>\npreview\n</stdout>\n<offload>\n{\"status\":\"offloaded\"}\n</offload>",
                Some("b"),
                None,
            ),
            // A cut preview is both; the stronger warning wins.
            msg("assistant", "", None, Some(vec![call("c", "bash", "{}")])),
            msg(
                "tool",
                "<stdout>\npreview\n</stdout>\n<offload>{\"status\":\"offloaded\"}</offload>...(truncated)",
                Some("c"),
                None,
            ),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].status, EvidenceStatus::Truncated);
        assert_eq!(rows[1].status, EvidenceStatus::Offloaded);
        assert_eq!(rows[2].status, EvidenceStatus::Truncated);
    }

    #[test]
    fn a_marker_inside_ordinary_output_is_not_metadata() {
        // Output that merely prints the markers — a log line, or a command
        // that quotes them — must not be reported as cut or offloaded.
        assert_eq!(
            classify_output(Some("grep: matched <offload> tag in source")),
            EvidenceStatus::Ok
        );
        assert_eq!(
            classify_output(Some("the docs say ...(truncated) means cut")),
            EvidenceStatus::Ok
        );
        // A real envelope: `</offload>` closes the body.
        assert_eq!(
            classify_output(Some(
                "<stdout>\nx\n</stdout>\n<offload>{\"status\":\"offloaded\"}\n</offload>"
            )),
            EvidenceStatus::Offloaded
        );
        // Complete stdout after a stray tag stays `Ok`.
        assert_eq!(
            classify_output(Some("<offload>\n{\"a\":1}\n</offload>\nmore output")),
            EvidenceStatus::Ok
        );
    }

    #[test]
    fn evidence_redacts_secrets_in_arguments_and_output() {
        // A tool argument reaches the transcript verbatim, so it must go
        // through the same scanner as the answer before it lands in a file the
        // user may hand to someone else.
        let scanner = aish_security::secret::SecretScanner::new(&[]);
        let messages = vec![
            msg("user", "q", None, None),
            msg(
                "assistant",
                "",
                None,
                Some(vec![call(
                    "a",
                    "bash",
                    r#"{"command":"export API_KEY=sk-abcdef0123456789abcdef"}"#,
                )]),
            ),
            msg("tool", "leaked sk-abcdef0123456789abcdef", Some("a"), None),
        ];
        let rows = collect_evidence(&messages, &scanner);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].arguments.contains("sk-abcdef0123456789abcdef"),
            "arguments leaked: {}",
            rows[0].arguments
        );
        assert!(
            !rows[0]
                .output
                .as_deref()
                .unwrap_or("")
                .contains("sk-abcdef0123456789abcdef"),
            "output leaked"
        );
    }

    #[test]
    fn evidence_body_stays_text_even_when_it_looks_like_markup() {
        // Tool output is not Markdown and must never be parsed as such: a `#`
        // heading or a `<script>` in a log line has to stay visible text.
        let messages = vec![
            msg("assistant", "", None, Some(vec![call("a", "bash", "{}")])),
            msg(
                "tool",
                "<script>alert(1)</script>\n# not a heading\n**not bold**",
                Some("a"),
                None,
            ),
        ];
        let html = render_page(
            "t",
            &[],
            "ok",
            &collect_evidence(&messages, &test_scanner()),
        );
        assert!(!html.contains("<script"));
        // The page's own `<h1>` is the title; the log line must not add one.
        assert!(!html.contains("# not a heading</h1>"));
        assert!(html.contains("# not a heading"));
        assert!(!html.contains("<strong>"));
        assert!(html.contains("**not bold**"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn evidence_renders_arguments_pretty_printed_and_escaped() {
        let messages = vec![
            msg(
                "assistant",
                "",
                None,
                Some(vec![call("a", "bash", r#"{"command":"echo <b>"}"#)]),
            ),
            msg("tool", "ok", Some("a"), None),
        ];
        let html = render_page(
            "t",
            &[],
            "ok",
            &collect_evidence(&messages, &test_scanner()),
        );
        assert!(html.contains("&lt;b&gt;"));
        assert!(!html.contains("<b>"));
        // Re-indented, so the reader sees one argument per line. Quotes are
        // entity-escaped by the shared escaper, hence `&quot;`.
        assert!(html.contains("{\n  &quot;command&quot;"));
        assert!(html.contains("<details"));
    }

    #[test]
    fn evidence_preview_picks_the_identifying_argument() {
        // The collapsed row is the scannable part of the list, so it must show
        // what the call acted on, not a fixed key or the whole JSON blob.
        assert_eq!(
            preview_of_arguments(r#"{"command":"df -h","timeout":10}"#, 120),
            "df -h"
        );
        assert_eq!(
            preview_of_arguments(r#"{"description":"长描述","prompt":"多段正文"}"#, 120),
            "长描述"
        );
        // Unknown shapes fall back to the first string value, then to the raw
        // text, so a tool with no recognisable key still gets a preview.
        assert_eq!(
            preview_of_arguments(r#"{"weird":{"a":1},"x":"v"}"#, 120),
            "v"
        );
        assert_eq!(preview_of_arguments("not json", 120), "not json");
        // A multi-line command must stay on one line and be clipped.
        assert_eq!(
            preview_of_arguments("{\"command\":\"a\\nb\\nc\"}", 120),
            "a b c"
        );
        assert!(
            preview_of_arguments(&format!("{{\"command\":\"{}\"}}", "x".repeat(300)), 20)
                .ends_with('…')
        );
    }

    #[test]
    fn evidence_section_counts_statuses_and_starts_collapsed() {
        let messages = vec![
            msg(
                "assistant",
                "",
                None,
                Some(vec![call("a", "bash", r#"{"command":"ls"}"#)]),
            ),
            msg("tool", "ok", Some("a"), None),
            msg(
                "assistant",
                "",
                None,
                Some(vec![call("b", "bash", r#"{"command":"cat"}"#)]),
            ),
            msg("tool", "x...(truncated)", Some("b"), None),
            msg("assistant", "", None, Some(vec![call("c", "bash", "{}")])),
        ];
        let rows = collect_evidence(&messages, &test_scanner());
        let html = render_page("t", &[], "ok", &rows);

        // Totals next to the heading describe the list before it is read.
        let counts = html
            .split("class=\"counts\">")
            .nth(1)
            .and_then(|rest| rest.split("</span>").next())
            .expect("counts block");
        assert_eq!(counts.matches("1 ").count(), 3, "counts: {counts}");

        // Every row starts closed: an open first row used to imply the whole
        // list was expanded.
        assert!(!html.contains(" open>"));
        assert_eq!(html.matches("<details").count(), 3);
        // The preview rides in the summary, so it is visible while collapsed.
        assert!(html.contains("<span class=\"preview\">cat</span>"));
    }

    #[test]
    fn evidence_section_appears_even_with_no_calls() {
        let html = render_page("t", &[], "answer", &[]);
        assert!(html.contains("class=\"evidence\""));
        assert!(html.contains(&t("shell.report.html.evidence_empty")));
        assert!(!html.contains("<details"));
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
