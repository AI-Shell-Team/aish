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
