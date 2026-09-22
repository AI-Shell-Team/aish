pub(crate) const DESCRIPTION: &str = "\
Execute Python code and return the result. Use ONLY for in-memory data \
processing, formatting, and calculations — NEVER to read or write files \
(use read_file/write_file/edit_file, which snapshot changes for undo).";

pub(crate) const PROMPT: &str = r#"Use this tool for Python snippets that are better expressed as code than shell pipelines.

Usage:
- Print values that should be returned to the conversation.
- Keep snippets focused and self-contained.
- Snippets have a 120-second wall-clock budget. If the timeout hits, the
  process is terminated and partial output is returned with a timeout note —
  split the work into smaller steps or run it in the background via the
  bash tool instead (e.g. `nohup python3 script.py >log 2>&1 &`).
- Interactive programs that read stdin are not supported; stdin is closed.
- NEVER read or write files through Python. Do not use open(), pathlib
  Path.write_text/write_bytes, shutil, or os file calls (rename, remove,
  makedirs) to create or modify files. Use read_file to read, edit_file to
  modify, and write_file to create — those tools snapshot changes for undo
  and anchor edits against stale content; Python file I/O bypasses all of
  that. Use Python only for in-memory data processing and print the result."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "Python code to execute."
            }
        },
        "required": ["code"]
    })
}
