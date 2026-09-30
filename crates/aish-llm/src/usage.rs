/// Token usage from a single LLM API response.
#[derive(Debug, Default, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens served from the provider's prompt cache (reused
    /// prefix, not new work). OpenAI: `prompt_tokens_details.cached_tokens`;
    /// Anthropic: `cache_read_input_tokens`.
    pub cache_read_tokens: u64,
    /// Prompt tokens written to the provider's prompt cache (billed as new
    /// work). Anthropic: `cache_creation_input_tokens`; OpenAI-compatible
    /// gateways usually fold this into `prompt_tokens` and report 0 here.
    pub cache_write_tokens: u64,
}

impl TokenUsage {
    /// Extract token usage from an OpenAI-compatible API response JSON.
    pub fn from_response_json(json: &serde_json::Value) -> Self {
        let usage = json.get("usage");
        Self {
            prompt_tokens: usage
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            completion_tokens: usage
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_tokens: usage
                .and_then(|u| u.get("prompt_tokens_details"))
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_write_tokens: 0,
        }
    }

    /// Extract token usage from an Anthropic Messages API response JSON.
    pub fn from_anthropic_json(json: &serde_json::Value) -> Self {
        let usage = json.get("usage");
        Self {
            prompt_tokens: usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            completion_tokens: usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_tokens: usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_write_tokens: usage
                .and_then(|u| u.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        }
    }
}

/// Cumulative token statistics for an LLM session.
#[derive(Debug, Default, Clone)]
pub struct TokenStats {
    pub total_input: u64,
    pub total_output: u64,
    pub request_count: u64,
    /// Cumulative prompt tokens served from the provider's prompt cache.
    /// Reused prefix, not new work — excluded from `total_tokens()` so
    /// budgets measure fresh consumption only (same accounting as omp
    /// goals: input + cacheWrite + output, cacheRead excluded).
    pub total_cache_read: u64,
    /// Cumulative prompt tokens written to the provider's prompt cache.
    /// Billed as new work — included in `total_input`.
    pub total_cache_write: u64,
    /// Prompt tokens from the most recent API call — the actual context
    /// window consumption at the current conversation depth.
    pub last_prompt_tokens: u64,
}

impl TokenStats {
    /// Record a single API call's token usage.
    pub fn record(&mut self, usage: TokenUsage) {
        self.total_input += usage.prompt_tokens;
        self.total_output += usage.completion_tokens;
        self.total_cache_read += usage.cache_read_tokens;
        self.total_cache_write += usage.cache_write_tokens;
        self.last_prompt_tokens = usage.prompt_tokens;
        self.request_count += 1;
    }

    /// Merge another stats snapshot into this one. Only cumulative totals
    /// propagate: `last_prompt_tokens` is intentionally NOT merged because a
    /// child/aux session's final prompt depth says nothing about this
    /// session's context window consumption.
    pub fn merge_totals(&mut self, other: &TokenStats) {
        self.total_input += other.total_input;
        self.total_output += other.total_output;
        self.total_cache_read += other.total_cache_read;
        self.total_cache_write += other.total_cache_write;
        self.request_count += other.request_count;
    }

    /// Total fresh tokens consumed (input + output; cached prefix reads are
    /// excluded because they are reused context, not new work).
    pub fn total_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_usage_from_anthropic_json() {
        let json = serde_json::json!({
            "usage": {
                "input_tokens": 120,
                "output_tokens": 40
            }
        });
        let usage = TokenUsage::from_anthropic_json(&json);
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 40);
    }

    #[test]
    fn test_token_usage_from_response_json() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 50
            }
        });
        let usage = TokenUsage::from_response_json(&json);
        assert_eq!(usage.prompt_tokens, 150);
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn test_token_usage_missing_fields() {
        let json = serde_json::json!({"choices": []});
        let usage = TokenUsage::from_response_json(&json);
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
    }

    #[test]
    fn test_token_stats_record() {
        let mut stats = TokenStats::default();
        stats.record(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });
        stats.record(TokenUsage {
            prompt_tokens: 200,
            completion_tokens: 80,
            cache_read_tokens: 40,
            cache_write_tokens: 10,
        });
        assert_eq!(stats.total_input, 300);
        assert_eq!(stats.total_output, 130);
        assert_eq!(stats.total_cache_read, 40);
        assert_eq!(stats.total_cache_write, 10);
        assert_eq!(stats.request_count, 2);
        assert_eq!(stats.total_tokens(), 430);
    }

    #[test]
    fn test_token_usage_openai_cached_tokens() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 50,
                "prompt_tokens_details": { "cached_tokens": 120 }
            }
        });
        let usage = TokenUsage::from_response_json(&json);
        assert_eq!(usage.prompt_tokens, 150);
        assert_eq!(usage.cache_read_tokens, 120);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn test_token_usage_anthropic_cache_fields() {
        let json = serde_json::json!({
            "usage": {
                "input_tokens": 30,
                "output_tokens": 40,
                "cache_read_input_tokens": 120,
                "cache_creation_input_tokens": 25
            }
        });
        let usage = TokenUsage::from_anthropic_json(&json);
        assert_eq!(usage.prompt_tokens, 30);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.cache_read_tokens, 120);
        assert_eq!(usage.cache_write_tokens, 25);
    }
}
