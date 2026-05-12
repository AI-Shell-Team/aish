# NL Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add lightweight natural language heuristic detection to aish, so inputs like `"list all files"` or `"列出所有文件"` are identified and offered to AI after user confirmation.

**Architecture:** New `nl_detect.rs` module with embedded keyword list (~200 words), CJK character detection, and PATH-based command check. Integration point is the `Command` branch in `app.rs` REPL loop — before PTY execution, run NL detection, and if triggered show an `inquire::Confirm` prompt.

**Tech Stack:** Rust, `which` crate (already in deps), `inquire` crate (already in deps), `regex` (already in deps). Zero new dependencies.

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/aish-shell/src/nl_detect.rs` | Create | NL detection algorithm: keywords, CJK, scoring, PATH check |
| `crates/aish-shell/src/lib.rs` | Modify | Add `pub mod nl_detect;` |
| `crates/aish-shell/src/app.rs` | Modify | Insert NL check + confirm in Command branch (lines ~1275-1280) |
| `crates/aish-i18n/locales/en-US.yaml` | Modify | Add `nl_detection.confirm_ask_ai` key |
| `crates/aish-i18n/locales/zh-CN.yaml` | Modify | Add `nl_detection.confirm_ask_ai` key |

---

### Task 1: Create `nl_detect.rs` with types and keyword constants

**Files:**
- Create: `crates/aish-shell/src/nl_detect.rs`
- Test: `crates/aish-shell/src/nl_detect.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the test for types and keyword lookup**

```rust
// In nl_detect.rs, bottom of file:

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nl_language_variants() {
        let v = NlVerdict::english(3);
        assert!(v.is_natural_language);
        assert_eq!(v.score, 3);
        assert_eq!(v.language, NlLanguage::English);

        let v = NlVerdict::not_nl();
        assert!(!v.is_natural_language);
        assert_eq!(v.score, 0);
        assert_eq!(v.language, NlLanguage::None);
    }

    #[test]
    fn test_keywords_are_lowercase() {
        for kw in NL_KEYWORDS {
            assert_eq!(*kw, kw.to_lowercase(), "keyword not lowercase: {}", kw);
        }
    }

    #[test]
    fn test_keywords_contains_core_words() {
        assert!(NL_KEYWORDS_SET.contains("what"));
        assert!(NL_KEYWORDS_SET.contains("how"));
        assert!(NL_KEYWORDS_SET.contains("list"));
        assert!(NL_KEYWORDS_SET.contains("all"));
        assert!(NL_KEYWORDS_SET.contains("files"));
    }

    #[test]
    fn test_is_nl_keyword() {
        assert!(is_nl_keyword("what"));
        assert!(is_nl_keyword("files"));
        assert!(!is_nl_keyword("grep"));
        assert!(!is_nl_keyword("awk"));
    }

    #[test]
    fn test_simplify_word() {
        assert_eq!(simplify_word("running"), "runn");
        assert_eq!(simplify_word("listed"), "list");
        assert_eq!(simplify_word("quickly"), "quick");
        assert_eq!(simplify_word("list"), "list");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aish-shell nl_detect -- --test-threads=1 2>&1 | head -30`
Expected: compilation error (module does not exist yet)

- [ ] **Step 3: Write the implementation — types, keywords, helper functions**

```rust
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Detected language category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NlLanguage {
    English,
    Chinese,
    Mixed,
    None,
}

/// Result of natural language detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlVerdict {
    pub is_natural_language: bool,
    pub score: usize,
    pub language: NlLanguage,
}

impl NlVerdict {
    fn english(score: usize) -> Self {
        Self {
            is_natural_language: score >= 2,
            score,
            language: NlLanguage::English,
        }
    }

    fn chinese() -> Self {
        Self {
            is_natural_language: true,
            score: 0,
            language: NlLanguage::Chinese,
        }
    }

    fn not_nl() -> Self {
        Self {
            is_natural_language: false,
            score: 0,
            language: NlLanguage::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Keyword list
// ---------------------------------------------------------------------------

/// High-frequency natural language words commonly appearing in shell queries.
/// Covers question words, common verbs, filler words, and tech terms.
const NL_KEYWORDS: &[&str] = &[
    // Question /指示 words
    "what", "who", "where", "when", "why", "how", "which", "whose", "whom",
    // Common verbs
    "list", "show", "find", "create", "delete", "remove", "install", "configure",
    "tell", "explain", "describe", "help", "get", "set", "make", "run", "start",
    "stop", "restart", "update", "upgrade", "check", "test", "build", "compile",
    "download", "upload", "copy", "move", "rename", "search", "replace", "sort",
    "count", "compare", "merge", "split", "convert", "extract", "filter", "display",
    "print", "read", "write", "open", "close", "enable", "disable", "add",
    "clear", "reset", "restore", "backup", "recover", "fix", "resolve", "debug",
    // Common nouns / adjectives
    "all", "files", "file", "directory", "directories", "folder", "folders",
    "process", "processes", "service", "services", "user", "users", "group",
    "groups", "port", "ports", "network", "connection", "connections",
    "package", "packages", "dependency", "dependencies", "version", "versions",
    "log", "logs", "error", "errors", "warning", "warnings", "output",
    "environment", "variable", "variables", "config", "configuration",
    "permission", "permissions", "owner", "mode", "path", "paths",
    "system", "server", "client", "database", "table", "record", "records",
    "current", "local", "remote", "global", "active", "available", "running",
    "stopped", "hidden", "empty", "large", "small", "new", "old", "latest",
    "previous", "recent", "total", "free", "used", "size", "name", "type",
    "status", "state", "info", "information", "details", "summary", "list",
    "content", "contents", "text", "line", "lines", "word", "words",
    "number", "numbers", "string", "strings", "value", "values",
    // Filler / grammar
    "the", "is", "are", "was", "were", "am", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "can", "may", "might", "must", "shall",
    "to", "of", "in", "for", "on", "with", "at", "by", "from", "into",
    "about", "between", "through", "during", "before", "after", "above",
    "below", "under", "over", "out", "up", "down", "off",
    "and", "or", "but", "not", "no", "nor",
    "this", "that", "these", "those", "my", "your", "his", "her", "its",
    "our", "their", "me", "him", "us", "them",
    "i", "you", "he", "she", "it", "we", "they",
    "there", "here", "every", "each", "any", "some", "many", "much",
    "more", "most", "other", "another", "such",
    // Tech-specific terms
    "git", "docker", "container", "image", "volume", "compose",
    "ssh", "scp", "rsync", "curl", "wget",
    "cpu", "memory", "disk", "ram", "swap",
    "hostname", "ip", "address", "domain", "url", "endpoint",
];

static NL_KEYWORDS_SET: LazyLock<std::collections::HashSet<&'static str>> =
    LazyLock::new(|| NL_KEYWORDS.iter().copied().collect());

/// Check if a word is in the NL keyword set.
fn is_nl_keyword(word: &str) -> bool {
    NL_KEYWORDS_SET.contains(word)
}

/// Simple suffix stripping to handle common English inflections.
fn simplify_word(word: &str) -> &str {
    word.strip_suffix("ing")
        .or_else(|| word.strip_suffix("ed"))
        .or_else(|| word.strip_suffix("ly"))
        .unwrap_or(word)
}
```

- [ ] **Step 4: Register the module in `lib.rs`**

In `crates/aish-shell/src/lib.rs`, add after `pub mod input;`:

```rust
pub mod nl_detect;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p aish-shell nl_detect -- --test-threads=1 2>&1 | tail -20`
Expected: all 5 tests PASS

- [ ] **Step 6: Commit**

```bash
git add crates/aish-shell/src/nl_detect.rs crates/aish-shell/src/lib.rs
git commit -m "feat(nl-detect): add types, keyword list, and helper functions"
```

---

### Task 2: Implement CJK detection and shell syntax check

**Files:**
- Modify: `crates/aish-shell/src/nl_detect.rs`

- [ ] **Step 1: Write tests for CJK detection and shell syntax check**

Append to the `tests` module in `nl_detect.rs`:

```rust
    #[test]
    fn test_is_cjk() {
        assert!(is_cjk('中'));
        assert!(is_cjk('文'));
        assert!(is_cjk('列'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('1'));
        assert!(!is_cjk(' '));
    }

    #[test]
    fn test_cjk_ratio_pure_chinese() {
        assert!(cjk_ratio("列出所有文件") >= 0.9);
    }

    #[test]
    fn test_cjk_ratio_mixed() {
        // "ls 列出文件" → 4 CJK / 7 non-ws chars ≈ 0.57
        let r = cjk_ratio("ls 列出文件");
        assert!(r >= 0.5 && r < 0.8, "ratio was {}", r);
    }

    #[test]
    fn test_cjk_ratio_english_only() {
        assert_eq!(cjk_ratio("list all files"), 0.0);
    }

    #[test]
    fn test_cjk_ratio_empty() {
        assert_eq!(cjk_ratio(""), 0.0);
        assert_eq!(cjk_ratio("   "), 0.0);
    }

    #[test]
    fn test_has_shell_syntax() {
        assert!(has_shell_syntax("foo=bar"));
        assert!(has_shell_syntax("$HOME"));
        assert!(has_shell_syntax("*.txt"));
        assert!(has_shell_syntax("a|b"));
        assert!(!has_shell_syntax("hello"));
        assert!(!has_shell_syntax("files"));
    }

    #[test]
    fn test_wrapped_in_quotes() {
        assert!(wrapped_in_quotes("\"hello\""));
        assert!(wrapped_in_quotes("'hello'"));
        assert!(!wrapped_in_quotes("hello"));
        assert!(!wrapped_in_quotes("\"hello"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aish-shell nl_detect -- --test-threads=1 2>&1 | tail -20`
Expected: compilation errors for undefined functions

- [ ] **Step 3: Implement CJK detection and shell syntax functions**

Add before the `#[cfg(test)]` block in `nl_detect.rs`:

```rust
// ---------------------------------------------------------------------------
// CJK detection
// ---------------------------------------------------------------------------

/// Check if a character falls in CJK Unicode ranges.
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}'   // CJK Extension A
        | '\u{F900}'..='\u{FAFF}'   // CJK Compatibility Ideographs
    )
}

/// Calculate the ratio of CJK characters in the input.
fn cjk_ratio(input: &str) -> f64 {
    let chars: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return 0.0;
    }
    let cjk_count = chars.iter().filter(|c| is_cjk(**c)).count();
    cjk_count as f64 / chars.len() as f64
}

// ---------------------------------------------------------------------------
// Shell syntax detection
// ---------------------------------------------------------------------------

const SHELL_SYNTAX_CHARS: &[char] = &[
    '$', '=', '{', '}', '[', ']', '>', '<', '*', '~', '&', '(', ')', '|', '/', '-',
];

/// Check if a token contains shell syntax characters (not in quotes).
fn has_shell_syntax(word: &str) -> bool {
    !word.contains(' ') && word.contains(SHELL_SYNTAX_CHARS)
}

/// Check if a token is wrapped in single or double quotes.
fn wrapped_in_quotes(word: &str) -> bool {
    (word.starts_with('"') && word.ends_with('"'))
        || (word.starts_with('\'') && word.ends_with('\''))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aish-shell nl_detect -- --test-threads=1 2>&1 | tail -20`
Expected: all tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/aish-shell/src/nl_detect.rs
git commit -m "feat(nl-detect): add CJK detection and shell syntax check"
```

---

### Task 3: Implement the `detect()` function

**Files:**
- Modify: `crates/aish-shell/src/nl_detect.rs`

- [ ] **Step 1: Write tests for the `detect()` function**

Append to the `tests` module:

```rust
    #[test]
    fn test_detect_single_token_not_nl() {
        let v = detect("whoami");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_real_command_not_nl() {
        // "ls" exists in PATH on all systems
        let v = detect("ls -la");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_english_sentence() {
        let v = detect("list all files");
        // On systems without "list" in PATH, this should be NL
        if which::which("list").is_err() {
            assert!(v.is_natural_language);
            assert_eq!(v.language, NlLanguage::English);
        }
    }

    #[test]
    fn test_detect_who_am_i() {
        let v = detect("who am i");
        // "who" is a real command on most systems
        // If "who" is not in PATH, it should be NL
        if which::which("who").is_err() {
            assert!(v.is_natural_language);
        }
    }

    #[test]
    fn test_detect_chinese() {
        let v = detect("列出所有文件");
        assert!(v.is_natural_language);
        assert_eq!(v.language, NlLanguage::Chinese);
    }

    #[test]
    fn test_detect_chinese_with_command_prefix() {
        // "ls 列出文件" → first token "ls" is in PATH → not NL
        let v = detect("ls 列出文件");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_empty() {
        assert!(!detect("").is_natural_language);
        assert!(!detect("   ").is_natural_language);
    }

    #[test]
    fn test_detect_shell_syntax_penalty() {
        // "FOO=bar something" → "FOO=bar" has shell syntax, penalty applied
        let v = detect("FOO=bar something");
        // First token may or may not be in PATH, but shell syntax reduces score
        // The exact result depends on whether "FOO=bar" resolves in PATH
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_git_command_not_nl() {
        let v = detect("git status");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_what_time() {
        // "what time is it" — "what" is NOT a typical command
        // If "what" not in PATH, this should be NL
        if which::which("what").is_err() {
            let v = detect("what time is it");
            assert!(v.is_natural_language);
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aish-shell nl_detect -- --test-threads=1 2>&1 | tail -20`
Expected: compilation error for undefined `detect()`

- [ ] **Step 3: Implement the `detect()` function**

Add before the `#[cfg(test)]` block:

```rust
// ---------------------------------------------------------------------------
// Main detection function
// ---------------------------------------------------------------------------

/// Threshold for English NL keyword score.
const NL_SCORE_THRESHOLD: usize = 2;

/// Minimum CJK character ratio to classify as Chinese NL.
const CJK_RATIO_THRESHOLD: f64 = 0.5;

/// Check whether input looks like natural language rather than a shell command.
pub fn detect(input: &str) -> NlVerdict {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return NlVerdict::not_nl();
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    // Rule 1: Single token is never NL (e.g. whoami, ls, git)
    if tokens.len() < 2 {
        return NlVerdict::not_nl();
    }

    let first_token = tokens[0];

    // Rule 2: First token found in PATH → command priority
    if which::which(first_token).is_ok() {
        return NlVerdict::not_nl();
    }

    // Rule 3: CJK ratio check
    let ratio = cjk_ratio(trimmed);
    if ratio >= CJK_RATIO_THRESHOLD {
        return NlVerdict::chinese();
    }

    // Rule 4: English NL keyword scoring
    let score = english_nl_score(&tokens);
    NlVerdict::english(score)
}

/// Calculate English NL keyword score for a list of tokens.
fn english_nl_score(tokens: &[&str]) -> usize {
    let mut score: usize = 0;
    for token in tokens {
        let lower = token.to_lowercase();
        let simplified = simplify_word(&lower);

        if is_nl_keyword(&lower) || is_nl_keyword(simplified) {
            score = score.saturating_add(1);
        } else if !wrapped_in_quotes(&lower) && has_shell_syntax(&lower) {
            score = score.saturating_sub(1);
        }
    }
    score
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aish-shell nl_detect -- --test-threads=1 2>&1 | tail -30`
Expected: all tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/aish-shell/src/nl_detect.rs
git commit -m "feat(nl-detect): implement detect() with English and Chinese support"
```

---

### Task 4: Add i18n keys for NL confirmation prompt

**Files:**
- Modify: `crates/aish-i18n/locales/en-US.yaml`
- Modify: `crates/aish-i18n/locales/zh-CN.yaml`

- [ ] **Step 1: Add English translation**

In `crates/aish-i18n/locales/en-US.yaml`, add after the `error_correction:` block (after line 419, same indentation level as `error_correction`):

```yaml
  nl_detection:
    confirm_ask_ai: "This looks like a natural language question. Ask AI? (Y/n)"
```

- [ ] **Step 2: Add Chinese translation**

In `crates/aish-i18n/locales/zh-CN.yaml`, add after the `error_correction:` block (after line 419, same indentation level as `error_correction`):

```yaml
  nl_detection:
    confirm_ask_ai: "这看起来像自然语言问题，是否让 AI 解答？(Y/n)"
```

- [ ] **Step 3: Verify i18n compiles**

Run: `cargo check -p aish-i18n 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add crates/aish-i18n/locales/en-US.yaml crates/aish-i18n/locales/zh-CN.yaml
git commit -m "feat(i18n): add NL detection confirmation prompt translations"
```

---

### Task 5: Integrate NL detection into `app.rs` REPL loop

**Files:**
- Modify: `crates/aish-shell/src/app.rs` (lines 1275-1280)

- [ ] **Step 1: Write the integration code**

In `crates/aish-shell/src/app.rs`, replace the `Command` branch (lines 1275-1313):

Before (current):
```rust
crate::types::InputIntent::OperatorCommand | crate::types::InputIntent::Command => {
    self.set_phase(ShellPhase::Running);
    let exit_code = self.execute_external_command(input);
    // ... rest unchanged
```

After (with NL detection):
```rust
crate::types::InputIntent::OperatorCommand | crate::types::InputIntent::Command => {
    // NL detection: check if input looks like natural language
    // and offer to route to AI instead of executing as a command.
    let nl_verdict = crate::nl_detect::detect(input);
    if nl_verdict.is_natural_language {
        let prompt_msg = t("shell.nl_detection.confirm_ask_ai");
        print!("{} ", prompt_msg);
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            // Fall through to command execution on read error
        } else {
            let ans = answer.trim().to_lowercase();
            if ans != "n" && ans != "no" {
                // Route to AI
                let question = input.trim().to_string();
                let old_sigint = self.install_ai_sigint_handler();
                let token_ptr =
                    self.ai_handler.cancellation_token() as *const CancellationToken;
                let result = runtime.block_on(async {
                    tokio::select! {
                        r = self.ai_handler.handle_question(&question) => r,
                        _ = poll_cancelled(token_ptr) => {
                            Err(aish_core::AishError::Cancelled)
                        }
                    }
                });
                Self::restore_ai_sigint_handler(old_sigint);

                let did_stream = self.streamed_content.load(Ordering::SeqCst);
                match result {
                    Ok(response) => {
                        if !did_stream && !response.is_empty() {
                            let mut sep_renderer = ShellRenderer::new();
                            sep_renderer.render_separator();
                            print_md(&response);
                            sep_renderer.render_separator();
                        }
                        self.record_history(input, 0);
                    }
                    Err(aish_core::AishError::Cancelled) => {
                        self.animation.stop();
                        println!("\x1b[33m{}\x1b[0m", t("shell.interrupted"));
                    }
                    Err(e) => {
                        if !matches!(e, aish_core::AishError::Llm(_)) {
                            let msg = t("shell.error.llm_error_message")
                                .replace("{error}", &e.to_string());
                            eprintln!("\x1b[31m{}\x1b[0m", msg);
                        }
                    }
                }
                continue;
            }
        }
    }

    self.set_phase(ShellPhase::Running);
    let exit_code = self.execute_external_command(input);
    self.set_phase(ShellPhase::Editing);
    self.record_history(input, exit_code);
    self.reset_interruption();

    // Track for error correction
    self.state.last_command = Some(input.to_string());
    self.state.last_exit_code = exit_code;
    self.state.can_correct_error = exit_code != 0 && exit_code != 130;

    let output_preview = if self.state.last_output.len() > 4096 {
        let end = {
            let mut j = 4096;
            while j > 0 && !self.state.last_output.is_char_boundary(j) {
                j -= 1;
            }
            j
        };
        &self.state.last_output[..end]
    } else {
        &self.state.last_output
    };
    let entry = format!(
        "[Shell] {}\n<returncode>{}</returncode>\n<output>{}</output>",
        input, exit_code, output_preview
    );
    self.ai_handler.add_shell_context(&entry);

    if exit_code != 0 && exit_code != 130 {
        let hint = t("shell.error_correction.press_semicolon_hint");
        eprintln!("\x1b[2m\x1b[37m<{}>\x1b[0m", hint);
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p aish-shell 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 3: Run all shell tests**

Run: `cargo test -p aish-shell 2>&1 | tail -20`
Expected: all tests PASS

- [ ] **Step 4: Commit**

```bash
git add crates/aish-shell/src/app.rs
git commit -m "feat(shell): integrate NL detection with confirmation prompt in REPL"
```

---

### Task 6: Add remaining locale translations and final integration test

**Files:**
- Modify: `crates/aish-i18n/locales/de-DE.yaml`
- Modify: `crates/aish-i18n/locales/es-ES.yaml`
- Modify: `crates/aish-i18n/locales/fr-FR.yaml`
- Modify: `crates/aish-i18n/locales/ja-JP.yaml`

- [ ] **Step 1: Add German translation**

In `crates/aish-i18n/locales/de-DE.yaml`, add at the same indentation level as `error_correction`:

```yaml
  nl_detection:
    confirm_ask_ai: "Das sieht nach einer natürlichen Sprachfrage aus. AI fragen? (Y/n)"
```

- [ ] **Step 2: Add Spanish translation**

In `crates/aish-i18n/locales/es-ES.yaml`:

```yaml
  nl_detection:
    confirm_ask_ai: "Esto parece una pregunta en lenguaje natural. ¿Preguntar a AI? (Y/n)"
```

- [ ] **Step 3: Add French translation**

In `crates/aish-i18n/locales/fr-FR.yaml`:

```yaml
  nl_detection:
    confirm_ask_ai: "Cela ressemble à une question en langage naturel. Demander à l'IA ? (Y/n)"
```

- [ ] **Step 4: Add Japanese translation**

In `crates/aish-i18n/locales/ja-JP.yaml`:

```yaml
  nl_detection:
    confirm_ask_ai: "自然言語の質問のようです。AIに聞きますか？(Y/n)"
```

- [ ] **Step 5: Final build and test**

Run: `cargo test -p aish-shell 2>&1 | tail -20`
Expected: all tests PASS

- [ ] **Step 6: Commit**

```bash
git add crates/aish-i18n/locales/de-DE.yaml crates/aish-i18n/locales/es-ES.yaml crates/aish-i18n/locales/fr-FR.yaml crates/aish-i18n/locales/ja-JP.yaml
git commit -m "feat(i18n): add NL detection translations for de, es, fr, ja locales"
```

---

## Self-Review Checklist

- **Spec coverage:**
  - Types + interface → Task 1
  - Keywords + simplify → Task 1
  - CJK detection → Task 2
  - Shell syntax check → Task 2
  - `detect()` function with all 4 rules → Task 3
  - i18n keys → Task 4, Task 6
  - app.rs integration → Task 5
  - Edge cases tested: single token, real command, English sentence, Chinese, mixed, shell syntax → Task 3 tests
- **Placeholder scan:** No TBD/TODO found. All steps have complete code.
- **Type consistency:** `NlVerdict`, `NlLanguage`, `detect()` signature consistent across all tasks.
