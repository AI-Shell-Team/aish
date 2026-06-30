//! Tokenized list filter (OpenClaw `tokenizedOptionFilter` semantics).
pub fn search_tokens(input: &str) -> Vec<String> {
    input
        .to_lowercase()
        .split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// Returns true when every token appears in `haystack` (already lowercased).
pub fn tokenized_match(haystack: &str, input: &str) -> bool {
    let tokens = search_tokens(input);
    if tokens.is_empty() {
        return true;
    }
    tokens.iter().all(|token| haystack.contains(token))
}

/// Count options that match the query.
pub fn count_matches(haystacks: &[String], input: &str) -> usize {
    if search_tokens(input).is_empty() {
        return haystacks.len();
    }
    haystacks
        .iter()
        .filter(|haystack| tokenized_match(haystack, input))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cliclack::compose_filter_haystack;

    #[test]
    fn hello_does_not_match_vllm() {
        let haystack = "vllm preset api base".to_string();
        assert!(!tokenized_match(&haystack, "hello"));
    }

    #[test]
    fn open_matches_openai() {
        let haystack = "openai".to_string();
        assert!(tokenized_match(&haystack, "open"));
    }

    #[test]
    fn multi_token_requires_all_parts() {
        let haystack = "openrouter preset api base".to_string();
        assert!(tokenized_match(&haystack, "open router"));
        assert!(!tokenized_match(&haystack, "open anthropic"));
    }

    #[test]
    fn provider_key_search_matches_haystack_used_by_theme_and_cliclack() {
        let haystack = compose_filter_haystack("Kilo Gateway", "Preset API Base", "kilocode");
        assert!(tokenized_match(&haystack, "kilocode"));
        assert_eq!(count_matches(&[haystack], "kilocode"), 1);
    }

    #[test]
    fn ll_matches_ollama_and_vllm_haystacks() {
        let haystacks = [
            compose_filter_haystack("Ollama", "", "ollama"),
            compose_filter_haystack("vLLM", "vLLM preset API base", "vllm"),
            compose_filter_haystack("Custom", "OpenAI-compatible", "custom"),
        ];
        assert!(tokenized_match(&haystacks[0], "ll"));
        assert!(tokenized_match(&haystacks[1], "ll"));
        assert!(!tokenized_match(&haystacks[2], "ll"));
        assert_eq!(count_matches(&haystacks.map(String::from), "ll"), 2);
    }
}
