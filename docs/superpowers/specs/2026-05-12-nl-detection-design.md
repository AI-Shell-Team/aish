# Natural Language Heuristic Detection for aish

## Problem

Users must prefix questions with `;` or `；` to route input to AI. Without the prefix,
everything goes to shell execution. This creates friction for natural language input
like `"list all files"` or `"列出所有文件"`.

## Decision

Add a lightweight NL detection module that identifies natural language input at the
`Command` routing stage. When detected, prompt the user for confirmation before
routing to AI.

## Requirements

- Support English and Chinese detection
- Prompt user every time (no remembered preferences)
- Single-token input never triggers NL detection (avoids `whoami` false positive)
- First token found in PATH never triggers NL detection (command takes priority)
- Zero new crate dependencies

## Architecture

### New module: `nl_detect.rs`

Public interface:

```rust
pub struct NlVerdict {
    pub is_natural_language: bool,
    pub score: usize,
    pub language: NlLanguage,
}

pub enum NlLanguage { English, Chinese, Mixed, None }

pub fn detect(input: &str) -> NlVerdict
```

### Detection algorithm

**Order of checks:**

1. `tokens.len() < 2` → not NL (single token like `whoami` is never ambiguous)
2. First token found via `which::which()` → not NL (command priority)
3. CJK character ratio >= 0.5 → NL (Chinese input)
4. English NL keyword score >= 2 → NL

**English scoring:**

- Embedded `NL_KEYWORDS` constant (~200 words): question words, common verbs,
  filler words
- Each token matching keyword → score +1
- Token with shell syntax chars (`$ = { } | > < * &`) without quote wrapping → score -1
- Simple suffix stripping: `strip_suffix("ing")`, `strip_suffix("ed")`,
  `strip_suffix("ly")` — no external stemmer dependency

**Chinese detection:**

- CJK Unicode ranges: U+4E00–U+9FFF, U+3400–U+4DBF, U+F900–U+FAFF
- CJK ratio = CJK chars / total non-whitespace chars
- Ratio >= 0.5 → Chinese NL

### Integration in `app.rs`

In the REPL `Command` branch, before PTY execution:

1. Call `nl_detect::detect(input)`
2. If `is_natural_language == true`, show `inquire::Confirm` prompt
3. User confirms → route to AI flow (same as `;` prefix)
4. User declines → proceed with normal command execution

### Files changed

| File | Change |
|------|--------|
| `nl_detect.rs` | New, ~200 lines, detection algorithm |
| `lib.rs` | Add `pub mod nl_detect;` |
| `app.rs` | Add ~15 lines NL check + confirm in Command branch |

`input.rs`, `types.rs`, `readline.rs` remain unchanged.

### Edge cases

- `whoami` → single token → not NL → executed as command
- `who am i` → "who" not in PATH, English score >= 2 → NL
- `list all files` on BSD (where `list` exists) → PATH found → not NL → executed
- `list all files` on Linux (no `list` command) → PATH not found → NL
- `git status how to undo` → "git" in PATH → not NL → command fails → error correction fallback
- `列出所有文件` → CJK ratio >= 0.5 → NL

## Future considerations

- If suffix stripping proves insufficient for English, consider adding `rust_stemmers`
  as optional enhancement
- Could add `NlLanguage::Japanese` / `NlLanguage::Korean` by extending CJK detection
  with Hiragana/Katakana/Hangul ranges
