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

/// Result of natural language detection (without PATH check).
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

    pub fn not_nl() -> Self {
        Self {
            is_natural_language: false,
            score: 0,
            language: NlLanguage::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const NL_SCORE_THRESHOLD: usize = 2;
const CJK_RATIO_THRESHOLD: f64 = 0.5;

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
    "status", "state", "info", "information", "details", "summary",
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

// ---------------------------------------------------------------------------
// Shell syntax detection
// ---------------------------------------------------------------------------

const SHELL_SYNTAX_CHARS: &[char] = &[
    '$', '=', '{', '}', '[', ']', '>', '<', '*', '~', '&', '(', ')', '|', '/', '-',
];

fn has_shell_syntax(word: &str) -> bool {
    !word.contains(' ') && word.contains(SHELL_SYNTAX_CHARS)
}

fn wrapped_in_quotes(word: &str) -> bool {
    (word.starts_with('"') && word.ends_with('"'))
        || (word.starts_with('\'') && word.ends_with('\''))
}

// ---------------------------------------------------------------------------
// CJK detection
// ---------------------------------------------------------------------------

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
    )
}

fn cjk_ratio(input: &str) -> f64 {
    let chars: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return 0.0;
    }
    let cjk_count = chars.iter().filter(|c| is_cjk(**c)).count();
    cjk_count as f64 / chars.len() as f64
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn is_nl_keyword(word: &str) -> bool {
    NL_KEYWORDS_SET.contains(word)
}

fn simplify_word(word: &str) -> &str {
    word.strip_suffix("ing")
        .or_else(|| word.strip_suffix("ed"))
        .or_else(|| word.strip_suffix("ly"))
        .unwrap_or(word)
}

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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Quick check if a single line (from SSH session shadow buffer) looks like NL.
/// Returns true if the line should be offered to AI instead of forwarded to PTY.
/// Does NOT check PATH — the caller should skip known commands before calling this.
pub fn looks_like_natural_language(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    // Single token: check CJK (Chinese has no spaces)
    if tokens.len() < 2 {
        return cjk_ratio(trimmed) >= CJK_RATIO_THRESHOLD;
    }

    // CJK ratio check
    if cjk_ratio(trimmed) >= CJK_RATIO_THRESHOLD {
        return true;
    }

    // English NL keyword scoring
    let score = english_nl_score(&tokens);
    score >= NL_SCORE_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_like_nl_english() {
        assert!(looks_like_natural_language("list all files"));
        assert!(looks_like_natural_language("what time is it"));
        assert!(looks_like_natural_language("how to check disk usage"));
    }

    #[test]
    fn test_looks_like_nl_chinese() {
        assert!(looks_like_natural_language("列出所有文件"));
        assert!(looks_like_natural_language("怎么查看磁盘使用情况"));
    }

    #[test]
    fn test_not_nl_shell_commands() {
        assert!(!looks_like_natural_language("ls -la"));
        // "git status" matches NL keywords (git + status), so the core scorer
        // classifies it as NL. The caller is expected to do a PATH check first
        // and skip known commands before calling looks_like_natural_language().
        assert!(!looks_like_natural_language("whoami"));
        assert!(!looks_like_natural_language("chmod +x script.sh"));
    }

    #[test]
    fn test_not_nl_empty() {
        assert!(!looks_like_natural_language(""));
        assert!(!looks_like_natural_language("   "));
    }

    #[test]
    fn test_shell_syntax_penalty() {
        // Heavy shell syntax should not look like NL
        assert!(!looks_like_natural_language("FOO=bar | grep something"));
    }

    #[test]
    fn test_cjk_ratio() {
        assert!(cjk_ratio("列出所有文件") >= 0.9);
        assert_eq!(cjk_ratio("list all files"), 0.0);
    }

    #[test]
    fn test_simplify_word() {
        assert_eq!(simplify_word("running"), "runn");
        assert_eq!(simplify_word("listed"), "list");
        assert_eq!(simplify_word("list"), "list");
    }
}
