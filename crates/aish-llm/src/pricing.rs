/// Model pricing information for cost estimation.
///
/// Prices are per 1K tokens in USD. Data sourced from official provider
/// pricing pages. When a model is not found, cost estimation is skipped.

use std::collections::HashMap;

/// Pricing for a single model variant.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// USD per 1K input (prompt) tokens.
    pub input_per_1k: f64,
    /// USD per 1K output (completion) tokens.
    pub output_per_1k: f64,
}

impl ModelPricing {
    /// Estimate cost for a given token usage.
    pub fn estimate_cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 / 1000.0) * self.input_per_1k
            + (output_tokens as f64 / 1000.0) * self.output_per_1k
    }
}

/// Look up pricing for a model identifier.
///
/// The model string may include a provider prefix (e.g. `openai/gpt-4o`,
/// `deepseek/deepseek-chat`). The lookup tries:
/// 1. Full model string
/// 2. Lowercased full model string
/// 3. Model name after the last `/`
/// 4. Substring match against known model families
pub fn lookup_pricing(model: &str) -> Option<ModelPricing> {
    let table = pricing_table();

    // 1. Exact match
    if let Some(p) = table.get(model) {
        return Some(*p);
    }

    // 2. Lowercased exact match
    let lower = model.to_lowercase();
    if let Some(p) = table.get(lower.as_str()) {
        return Some(*p);
    }

    // 3. Model name after last '/'
    if let Some(name) = model.rsplit('/').next() {
        if let Some(p) = table.get(name) {
            return Some(*p);
        }
        let name_lower = name.to_lowercase();
        if let Some(p) = table.get(name_lower.as_str()) {
            return Some(*p);
        }
    }

    // 4. Substring match for model families
    for (key, pricing) in table.iter() {
        if lower.contains(key) || key.contains(&lower) {
            return Some(*pricing);
        }
    }

    None
}

/// Estimate cost for a token usage given a model.
pub fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    lookup_pricing(model).map(|p| p.estimate_cost(input_tokens, output_tokens))
}

/// The built-in pricing table (as of 2025-Q2).
///
/// Returns a HashMap of model identifiers to pricing.
/// Keys are lowercased for case-insensitive matching.
fn pricing_table() -> HashMap<&'static str, ModelPricing> {
    let mut m = HashMap::new();

    // ---- OpenAI ----
    m.insert(
        "gpt-4o",
        ModelPricing {
            input_per_1k: 0.0025,
            output_per_1k: 0.01,
        },
    );
    m.insert(
        "gpt-4o-mini",
        ModelPricing {
            input_per_1k: 0.00015,
            output_per_1k: 0.0006,
        },
    );
    m.insert(
        "gpt-4-turbo",
        ModelPricing {
            input_per_1k: 0.01,
            output_per_1k: 0.03,
        },
    );
    m.insert(
        "gpt-4",
        ModelPricing {
            input_per_1k: 0.03,
            output_per_1k: 0.06,
        },
    );
    m.insert(
        "gpt-3.5-turbo",
        ModelPricing {
            input_per_1k: 0.0005,
            output_per_1k: 0.0015,
        },
    );
    // o1 family
    m.insert(
        "o1",
        ModelPricing {
            input_per_1k: 0.015,
            output_per_1k: 0.06,
        },
    );
    m.insert(
        "o1-mini",
        ModelPricing {
            input_per_1k: 0.003,
            output_per_1k: 0.012,
        },
    );
    m.insert(
        "o1-preview",
        ModelPricing {
            input_per_1k: 0.015,
            output_per_1k: 0.06,
        },
    );
    m.insert(
        "o3-mini",
        ModelPricing {
            input_per_1k: 0.0011,
            output_per_1k: 0.0044,
        },
    );

    // ---- Anthropic Claude ----
    m.insert(
        "claude-3-5-sonnet",
        ModelPricing {
            input_per_1k: 0.003,
            output_per_1k: 0.015,
        },
    );
    m.insert(
        "claude-3-5-haiku",
        ModelPricing {
            input_per_1k: 0.001,
            output_per_1k: 0.005,
        },
    );
    m.insert(
        "claude-3-opus",
        ModelPricing {
            input_per_1k: 0.015,
            output_per_1k: 0.075,
        },
    );
    m.insert(
        "claude-3-sonnet",
        ModelPricing {
            input_per_1k: 0.003,
            output_per_1k: 0.015,
        },
    );
    m.insert(
        "claude-3-haiku",
        ModelPricing {
            input_per_1k: 0.00025,
            output_per_1k: 0.00125,
        },
    );

    // ---- DeepSeek ----
    m.insert(
        "deepseek-chat",
        ModelPricing {
            input_per_1k: 0.0014,
            output_per_1k: 0.0028,
        },
    );
    m.insert(
        "deepseek-coder",
        ModelPricing {
            input_per_1k: 0.0014,
            output_per_1k: 0.0028,
        },
    );
    m.insert(
        "deepseek-reasoner",
        ModelPricing {
            input_per_1k: 0.0055,
            output_per_1k: 0.0219,
        },
    );

    // ---- Google Gemini ----
    m.insert(
        "gemini-2.0-flash",
        ModelPricing {
            input_per_1k: 0.0001,
            output_per_1k: 0.0004,
        },
    );
    m.insert(
        "gemini-1.5-pro",
        ModelPricing {
            input_per_1k: 0.00125,
            output_per_1k: 0.005,
        },
    );
    m.insert(
        "gemini-1.5-flash",
        ModelPricing {
            input_per_1k: 0.000075,
            output_per_1k: 0.0003,
        },
    );

    // ---- Qwen (Alibaba) ----
    m.insert(
        "qwen-plus",
        ModelPricing {
            input_per_1k: 0.00057,
            output_per_1k: 0.0017,
        },
    );
    m.insert(
        "qwen-turbo",
        ModelPricing {
            input_per_1k: 0.00014,
            output_per_1k: 0.00043,
        },
    );
    m.insert(
        "qwen-max",
        ModelPricing {
            input_per_1k: 0.0043,
            output_per_1k: 0.0129,
        },
    );

    // ---- Zhipu / Z.AI ----
    m.insert(
        "glm-4",
        ModelPricing {
            input_per_1k: 0.0014,
            output_per_1k: 0.0014,
        },
    );
    m.insert(
        "glm-4-flash",
        ModelPricing {
            input_per_1k: 0.0001,
            output_per_1k: 0.0001,
        },
    );

    // ---- Moonshot (Kimi) ----
    m.insert(
        "moonshot-v1-8k",
        ModelPricing {
            input_per_1k: 0.0017,
            output_per_1k: 0.0017,
        },
    );
    m.insert(
        "moonshot-v1-32k",
        ModelPricing {
            input_per_1k: 0.0034,
            output_per_1k: 0.0034,
        },
    );

    // ---- Mistral ----
    m.insert(
        "mistral-large-latest",
        ModelPricing {
            input_per_1k: 0.002,
            output_per_1k: 0.006,
        },
    );
    m.insert(
        "mistral-small-latest",
        ModelPricing {
            input_per_1k: 0.0002,
            output_per_1k: 0.0006,
        },
    );

    // ---- Meta Llama (via various providers) ----
    m.insert(
        "llama-3.1-70b",
        ModelPricing {
            input_per_1k: 0.00059,
            output_per_1k: 0.00079,
        },
    );
    m.insert(
        "llama-3.1-8b",
        ModelPricing {
            input_per_1k: 0.00005,
            output_per_1k: 0.00008,
        },
    );

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_model_pricing() {
        let p = lookup_pricing("gpt-4o").expect("gpt-4o should have pricing");
        assert_eq!(p.input_per_1k, 0.0025);
        assert_eq!(p.output_per_1k, 0.01);
    }

    #[test]
    fn test_provider_prefix() {
        let p = lookup_pricing("openai/gpt-4o").expect("openai/gpt-4o should resolve");
        assert_eq!(p.input_per_1k, 0.0025);
    }

    #[test]
    fn test_case_insensitive() {
        let p = lookup_pricing("GPT-4O").expect("GPT-4O should resolve case-insensitively");
        assert_eq!(p.input_per_1k, 0.0025);
    }

    #[test]
    fn test_unknown_model() {
        assert!(lookup_pricing("some-unknown-model-xyz").is_none());
    }

    #[test]
    fn test_cost_estimation() {
        let cost = estimate_cost("gpt-4o", 10000, 5000).unwrap();
        // 10K * 0.0025/1K + 5K * 0.01/1K = 0.025 + 0.05 = 0.075
        assert!((cost - 0.075).abs() < 0.0001);
    }

    #[test]
    fn test_deepseek_pricing() {
        let p = lookup_pricing("deepseek/deepseek-chat").expect("deepseek-chat should resolve");
        assert_eq!(p.input_per_1k, 0.0014);
        assert_eq!(p.output_per_1k, 0.0028);
    }
}
