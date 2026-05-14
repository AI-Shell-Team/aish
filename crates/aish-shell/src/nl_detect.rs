// NL detection for the local REPL: delegates core scoring to aish-pty and
// adds a PATH-based command check that requires the `which` crate.

// Re-export types from aish-pty so consumers don't need to import from both crates.
pub use aish_pty::nl_detect::NlLanguage;
pub use aish_pty::nl_detect::NlVerdict;

/// Check whether input looks like natural language rather than a shell command.
/// Wraps `aish_pty::nl_detect::looks_like_natural_language` with an additional
/// PATH check: if the first token resolves to an executable, it is never NL.
pub fn detect(input: &str) -> NlVerdict {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return NlVerdict::not_nl();
    }

    // If the first token is found in PATH, treat as command (not NL).
    let first_token = trimmed.split_whitespace().next().unwrap_or("");
    if which::which(first_token).is_ok() {
        return NlVerdict::not_nl();
    }

    // Delegate core NL detection to aish-pty.
    if aish_pty::nl_detect::looks_like_natural_language(input) {
        // Determine language for the verdict.
        let is_cjk = input
            .chars()
            .filter(|c| !c.is_whitespace())
            .any(|c| matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}'));
        NlVerdict {
            is_natural_language: true,
            score: 0,
            language: if is_cjk { NlLanguage::Chinese } else { NlLanguage::English },
        }
    } else {
        NlVerdict::not_nl()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_single_token_not_nl() {
        let v = detect("whoami");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_real_command_not_nl() {
        let v = detect("ls -la");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_english_sentence() {
        let v = detect("list all files");
        if which::which("list").is_err() {
            assert!(v.is_natural_language);
            assert_eq!(v.language, NlLanguage::English);
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
        let v = detect("ls 列出文件");
        assert!(!v.is_natural_language);
    }

    #[test]
    fn test_detect_empty() {
        assert!(!detect("").is_natural_language);
        assert!(!detect("   ").is_natural_language);
    }

    #[test]
    fn test_detect_git_command_not_nl() {
        let v = detect("git status");
        assert!(!v.is_natural_language);
    }
}
