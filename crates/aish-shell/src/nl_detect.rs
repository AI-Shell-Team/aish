use std::sync::LazyLock;

/// Threshold for English NL keyword score.
const NL_SCORE_THRESHOLD: usize = 2;

/// Minimum CJK character ratio to classify as Chinese NL.
const CJK_RATIO_THRESHOLD: f64 = 0.5;

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
            is_natural_language: score >= NL_SCORE_THRESHOLD,
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

// ---------------------------------------------------------------------------
// Main detection function
// ---------------------------------------------------------------------------

/// Check whether input looks like natural language rather than a shell command.
pub fn detect(input: &str) -> NlVerdict {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return NlVerdict::not_nl();
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    // Rule 1: Single token — check CJK first (CJK text has no spaces),
    //         otherwise never NL (e.g. whoami, ls, git)
    if tokens.len() < 2 {
        let ratio = cjk_ratio(trimmed);
        if ratio >= CJK_RATIO_THRESHOLD {
            return NlVerdict::chinese();
        }
        return NlVerdict::not_nl();
    }

    let first_token = tokens[0];

    // Rule 2: First token found in PATH → command priority
    if which::which(first_token).is_ok() {
        return NlVerdict::not_nl();
    }

    // Rule 3: CJK ratio check for multi-token input
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
        // "ls 列出文件" → 4 CJK / 7 non-ws chars
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
        let v = detect("FOO=bar something");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_git_command_not_nl() {
        let v = detect("git status");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_what_time() {
        // "what" is NOT a typical command
        if which::which("what").is_err() {
            let v = detect("what time is it");
            assert!(v.is_natural_language);
        }
    }
}
