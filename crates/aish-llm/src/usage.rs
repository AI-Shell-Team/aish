/// Token usage from a single LLM API response.
#[derive(Debug, Default, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl TokenUsage {
    /// Extract token usage from an OpenAI-compatible API response JSON.
    ///
    /// Looks for `usage.prompt_tokens` and `usage.completion_tokens`.
    /// Returns default (zeroed) if the fields are missing.
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
        }
    }
}

/// Cumulative token statistics for an LLM session.
#[derive(Debug, Default, Clone)]
pub struct TokenStats {
    pub total_input: u64,
    pub total_output: u64,
    pub request_count: u64,
}

impl TokenStats {
    /// Record a single API call's token usage.
    pub fn record(&mut self, usage: TokenUsage) {
        self.total_input += usage.prompt_tokens;
        self.total_output += usage.completion_tokens;
        self.request_count += 1;
    }

    /// Total tokens consumed (input + output).
    pub fn total_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        });
        stats.record(TokenUsage {
            prompt_tokens: 200,
            completion_tokens: 80,
        });
        assert_eq!(stats.total_input, 300);
        assert_eq!(stats.total_output, 130);
        assert_eq!(stats.request_count, 2);
        assert_eq!(stats.total_tokens(), 430);
    }
}
