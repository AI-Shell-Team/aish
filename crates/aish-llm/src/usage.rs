/// Token usage from a single LLM API response.
///
/// `cached_tokens` is the subset of `prompt_tokens` served from a provider
/// cache (OpenAI `prompt_tokens_details.cached_tokens`, Anthropic
/// `cache_read_input_tokens`). It is NOT billed as fresh consumption, so
/// [`TokenStats::record`] subtracts it from the running input total to avoid
/// inflated `/token` numbers (#579 sister fix).
#[derive(Debug, Default, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

impl TokenUsage {
    /// Extract token usage from an OpenAI-compatible API response JSON.
    ///
    /// Looks for `usage.prompt_tokens` and `usage.completion_tokens`, plus the
    /// optional `usage.prompt_tokens_details.cached_tokens` cache-hit count.
    /// Returns default (zeroed) if the fields are missing.
    pub fn from_response_json(json: &serde_json::Value) -> Self {
        let usage = json.get("usage");
        let prompt_tokens = usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cached_tokens = usage
            .and_then(|u| u.get("prompt_tokens_details"))
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Self {
            prompt_tokens,
            completion_tokens: usage
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cached_tokens,
        }
    }

    /// Extract token usage from an Anthropic Messages API response JSON.
    ///
    /// `usage.cache_read_input_tokens` counts prompt tokens served from the
    /// Anthropic prompt cache; `cache_creation_input_tokens` are the tokens
    /// written into the cache this turn. Only cache reads avoid fresh billing,
    /// so `cached_tokens` tracks reads only.
    pub fn from_anthropic_json(json: &serde_json::Value) -> Self {
        let usage = json.get("usage");
        let prompt_tokens = usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cached_tokens = usage
            .and_then(|u| u.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Self {
            prompt_tokens,
            completion_tokens: usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cached_tokens,
        }
    }
}

/// Cumulative token statistics for an LLM session.
#[derive(Debug, Default, Clone)]
pub struct TokenStats {
    pub total_input: u64,
    pub total_output: u64,
    pub request_count: u64,
    /// Prompt tokens from the most recent API call — the actual context
    /// window consumption at the current conversation depth.
    pub last_prompt_tokens: u64,
    /// Cumulative prompt tokens served from a provider cache (OpenAI
    /// `cached_tokens` / Anthropic `cache_read_input_tokens`). Tracked
    /// separately so `/token` can show cache hits without double-counting
    /// them in `total_input` (#579 sister fix).
    pub cached_input: u64,
}

impl TokenStats {
    /// Record a single API call's token usage.
    ///
    /// `total_input` accumulates only the non-cached portion of
    /// `prompt_tokens` so `/token` reflects real billed consumption, not
    /// the full prompt size that includes cache hits. `last_prompt_tokens`
    /// still records the full prompt size (the real context-window depth).
    pub fn record(&mut self, usage: TokenUsage) {
        let billed_input = usage.prompt_tokens.saturating_sub(usage.cached_tokens);
        self.total_input += billed_input;
        self.cached_input += usage.cached_tokens;
        self.total_output += usage.completion_tokens;
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
        self.cached_input += other.cached_input;
        self.request_count += other.request_count;
    }

    /// Total tokens consumed (billed input + output, excluding cache hits).
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
                "output_tokens": 40,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 10
            }
        });
        let usage = TokenUsage::from_anthropic_json(&json);
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.cached_tokens, 80);
    }

    #[test]
    fn test_token_usage_from_response_json() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 50,
                "prompt_tokens_details": {
                    "cached_tokens": 90
                }
            }
        });
        let usage = TokenUsage::from_response_json(&json);
        assert_eq!(usage.prompt_tokens, 150);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.cached_tokens, 90);
    }

    #[test]
    fn test_token_usage_missing_fields() {
        let json = serde_json::json!({"choices": []});
        let usage = TokenUsage::from_response_json(&json);
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.cached_tokens, 0);
    }

    #[test]
    fn test_token_stats_record() {
        let mut stats = TokenStats::default();
        stats.record(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 0,
        });
        stats.record(TokenUsage {
            prompt_tokens: 200,
            completion_tokens: 80,
            cached_tokens: 60,
        });
        // total_input excludes cache hits: (100 - 0) + (200 - 60) = 240.
        assert_eq!(stats.total_input, 240);
        assert_eq!(stats.cached_input, 60);
        assert_eq!(stats.total_output, 130);
        assert_eq!(stats.request_count, 2);
        assert_eq!(stats.total_tokens(), 370);
        // last_prompt_tokens keeps the full prompt size (context depth).
        assert_eq!(stats.last_prompt_tokens, 200);
    }
}
