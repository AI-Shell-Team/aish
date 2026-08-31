/// Default conservative context window used when no explicit budget is configured.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 100_000;
const DEFAULT_RESERVED_OUTPUT_TOKENS: usize = 20_000;
const DEFAULT_AUTO_COMPACT_BUFFER_TOKENS: usize = 13_000;
const DEFAULT_WARNING_BUFFER_TOKENS: usize = 20_000;
const DEFAULT_BLOCKING_BUFFER_TOKENS: usize = 3_000;

/// Default suffix depth below which microcompact may rewrite messages;
/// deeper (cache-warm) messages are left for full compaction.
const DEFAULT_CACHE_WARM_SUFFIX_TOKENS: usize = 8_000;
const MIN_EFFECTIVE_CONTEXT_WINDOW_TOKENS: usize = 1_000;
const MIN_PROMPT_BUDGET_TOKENS: usize = 8_000;
const CONTEXT_WINDOW_WARN_BELOW_TOKENS: usize = 8_000;
const CONTEXT_WINDOW_HARD_MIN_TOKENS: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPressureLevel {
    Normal,
    Warning,
    AutoCompact,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextWindowSource {
    AutoCompactOverride,
    ModelConfig,
    LegacyContextTokenBudget,
    Default,
}

impl ContextWindowSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoCompactOverride => "context_auto_compact.context_window_tokens",
            Self::ModelConfig => "context_auto_compact.model_context_windows",
            Self::LegacyContextTokenBudget => "context_token_budget",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextWindowResolution {
    pub tokens: usize,
    pub source: ContextWindowSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetPolicy {
    pub enabled: bool,
    pub full_compact_enabled: bool,
    pub context_window_tokens: usize,
    pub context_window_source: ContextWindowSource,
    pub reserved_output_tokens: usize,
    pub auto_compact_buffer_tokens: usize,
    pub warning_buffer_tokens: usize,
    pub blocking_buffer_tokens: usize,
    pub micro_keep_recent_messages: usize,
    pub shell_keep_recent_commands: usize,
    /// Messages whose trailing suffix exceeds this many estimated tokens
    /// sit inside the provider's warm prompt-cache prefix and are never
    /// rewritten by microcompact; full compaction reclaims them. 0
    /// disables the guard.
    pub cache_warm_suffix_tokens: usize,
    pub max_consecutive_failures: usize,
    pub summary_max_tokens: usize,
    pub enable_token_estimation: bool,
}

impl Default for ContextBudgetPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            full_compact_enabled: true,
            context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            context_window_source: ContextWindowSource::Default,
            reserved_output_tokens: DEFAULT_RESERVED_OUTPUT_TOKENS,
            auto_compact_buffer_tokens: DEFAULT_AUTO_COMPACT_BUFFER_TOKENS,
            warning_buffer_tokens: DEFAULT_WARNING_BUFFER_TOKENS,
            blocking_buffer_tokens: DEFAULT_BLOCKING_BUFFER_TOKENS,
            micro_keep_recent_messages: 6,
            shell_keep_recent_commands: 8,
            cache_warm_suffix_tokens: DEFAULT_CACHE_WARM_SUFFIX_TOKENS,
            max_consecutive_failures: 3,
            summary_max_tokens: 4_000,
            enable_token_estimation: true,
        }
    }
}

impl ContextBudgetPolicy {
    pub fn from_optional_budget(
        context_token_budget: Option<usize>,
        enable_token_estimation: bool,
    ) -> Self {
        let mut policy = Self::default();
        if let Some(budget) = context_token_budget {
            policy.context_window_tokens = budget.max(MIN_EFFECTIVE_CONTEXT_WINDOW_TOKENS);
            policy.context_window_source = ContextWindowSource::LegacyContextTokenBudget;
        }
        policy.enable_token_estimation = enable_token_estimation;
        policy
    }

    pub fn effective_context_window(&self) -> usize {
        effective_context_window(self.context_window_tokens, self.reserved_output_tokens)
    }

    pub fn thresholds(&self) -> ContextBudgetThresholds {
        calculate_thresholds(self)
    }

    pub fn state_for_tokens(&self, estimated_tokens: usize) -> ContextBudgetState {
        calculate_budget_state(estimated_tokens, self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudgetThresholds {
    pub effective_context_window: usize,
    pub warning_threshold: usize,
    pub auto_compact_threshold: usize,
    pub blocking_threshold: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetState {
    pub estimated_tokens: usize,
    pub effective_context_window: usize,
    pub warning_threshold: usize,
    pub auto_compact_threshold: usize,
    pub blocking_threshold: usize,
    pub percent_used: u8,
    pub percent_left: u8,
    pub pressure: ContextPressureLevel,
    pub is_above_warning_threshold: bool,
    pub is_above_auto_compact_threshold: bool,
    pub is_at_blocking_limit: bool,
}

pub fn effective_context_window(
    context_window_tokens: usize,
    reserved_output_tokens: usize,
) -> usize {
    context_window_tokens
        .saturating_sub(effective_reserved_output_tokens(
            context_window_tokens,
            reserved_output_tokens,
        ))
        .max(MIN_EFFECTIVE_CONTEXT_WINDOW_TOKENS)
}

pub fn effective_reserved_output_tokens(
    context_window_tokens: usize,
    reserved_output_tokens: usize,
) -> usize {
    let context_window_tokens = context_window_tokens.max(1);
    let min_prompt_budget = (context_window_tokens / 2).clamp(1, MIN_PROMPT_BUDGET_TOKENS);
    let max_reserve = context_window_tokens.saturating_sub(min_prompt_budget);
    reserved_output_tokens.min(max_reserve)
}

pub fn resolve_context_window_tokens(
    auto_compact_override: Option<usize>,
    model_config_tokens: Option<usize>,
    legacy_context_token_budget: Option<usize>,
) -> ContextWindowResolution {
    if let Some(tokens) = auto_compact_override {
        return ContextWindowResolution {
            tokens: tokens.max(MIN_EFFECTIVE_CONTEXT_WINDOW_TOKENS),
            source: ContextWindowSource::AutoCompactOverride,
        };
    }
    if let Some(tokens) = model_config_tokens {
        return ContextWindowResolution {
            tokens: tokens.max(MIN_EFFECTIVE_CONTEXT_WINDOW_TOKENS),
            source: ContextWindowSource::ModelConfig,
        };
    }
    if let Some(tokens) = legacy_context_token_budget {
        return ContextWindowResolution {
            tokens: tokens.max(MIN_EFFECTIVE_CONTEXT_WINDOW_TOKENS),
            source: ContextWindowSource::LegacyContextTokenBudget,
        };
    }
    ContextWindowResolution {
        tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
        source: ContextWindowSource::Default,
    }
}

pub fn context_window_warn_below_tokens() -> usize {
    CONTEXT_WINDOW_WARN_BELOW_TOKENS
}

pub fn context_window_hard_min_tokens() -> usize {
    CONTEXT_WINDOW_HARD_MIN_TOKENS
}

pub fn calculate_thresholds(policy: &ContextBudgetPolicy) -> ContextBudgetThresholds {
    let effective = policy.effective_context_window();
    let auto_buffer = policy.auto_compact_buffer_tokens.min(effective / 3);
    let warning_buffer = policy.warning_buffer_tokens.min(effective / 3);
    let blocking_buffer = policy.blocking_buffer_tokens.min(effective / 10).max(1);

    let auto_compact_threshold = effective.saturating_sub(auto_buffer).max(1);
    let warning_threshold = auto_compact_threshold
        .saturating_sub(warning_buffer)
        .max(1)
        .min(auto_compact_threshold);
    let blocking_threshold = effective
        .saturating_sub(blocking_buffer)
        .max(auto_compact_threshold)
        .min(effective);

    ContextBudgetThresholds {
        effective_context_window: effective,
        warning_threshold,
        auto_compact_threshold,
        blocking_threshold,
    }
}

pub fn calculate_budget_state(
    estimated_tokens: usize,
    policy: &ContextBudgetPolicy,
) -> ContextBudgetState {
    let thresholds = calculate_thresholds(policy);
    let percent_used = estimated_tokens
        .saturating_mul(100)
        .checked_div(thresholds.effective_context_window)
        .unwrap_or(100)
        .min(100) as u8;
    let percent_left = 100u8.saturating_sub(percent_used);

    let is_at_blocking_limit = estimated_tokens >= thresholds.blocking_threshold;
    let is_above_auto_compact_threshold =
        policy.enabled && estimated_tokens >= thresholds.auto_compact_threshold;
    let is_above_warning_threshold =
        policy.enabled && estimated_tokens >= thresholds.warning_threshold;

    let pressure = if is_at_blocking_limit {
        ContextPressureLevel::Blocking
    } else if is_above_auto_compact_threshold {
        ContextPressureLevel::AutoCompact
    } else if is_above_warning_threshold {
        ContextPressureLevel::Warning
    } else {
        ContextPressureLevel::Normal
    };

    ContextBudgetState {
        estimated_tokens,
        effective_context_window: thresholds.effective_context_window,
        warning_threshold: thresholds.warning_threshold,
        auto_compact_threshold: thresholds.auto_compact_threshold,
        blocking_threshold: thresholds.blocking_threshold,
        percent_used,
        percent_left,
        pressure,
        is_above_warning_threshold,
        is_above_auto_compact_threshold,
        is_at_blocking_limit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_ordered_for_default_policy() {
        let policy = ContextBudgetPolicy::default();
        let thresholds = policy.thresholds();
        assert!(thresholds.warning_threshold <= thresholds.auto_compact_threshold);
        assert!(thresholds.auto_compact_threshold <= thresholds.blocking_threshold);
        assert!(thresholds.blocking_threshold <= thresholds.effective_context_window);
    }

    #[test]
    fn small_windows_are_clamped_and_ordered() {
        let policy = ContextBudgetPolicy {
            context_window_tokens: 2_000,
            reserved_output_tokens: 20_000,
            auto_compact_buffer_tokens: 13_000,
            warning_buffer_tokens: 20_000,
            blocking_buffer_tokens: 3_000,
            ..ContextBudgetPolicy::default()
        };
        let thresholds = policy.thresholds();
        assert_eq!(thresholds.effective_context_window, 1_000);
        assert!(thresholds.warning_threshold <= thresholds.auto_compact_threshold);
        assert!(thresholds.auto_compact_threshold <= thresholds.blocking_threshold);
    }

    #[test]
    fn resolve_context_window_tokens_reports_source() {
        assert_eq!(
            resolve_context_window_tokens(Some(32_000), Some(64_000), Some(8_000)).source,
            ContextWindowSource::AutoCompactOverride
        );
        assert_eq!(
            resolve_context_window_tokens(None, Some(64_000), Some(8_000)).source,
            ContextWindowSource::ModelConfig
        );
        assert_eq!(
            resolve_context_window_tokens(None, None, Some(8_000)).source,
            ContextWindowSource::LegacyContextTokenBudget
        );
        assert_eq!(
            resolve_context_window_tokens(None, None, None).tokens,
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
    }

    #[test]
    fn reserve_tokens_keep_prompt_budget_for_small_windows() {
        assert_eq!(effective_reserved_output_tokens(2_000, 20_000), 1_000);
        assert_eq!(effective_context_window(2_000, 20_000), 1_000);
        assert_eq!(effective_reserved_output_tokens(16_000, 20_000), 8_000);
        assert_eq!(effective_context_window(100_000, 20_000), 80_000);
    }

    #[test]
    fn state_reports_warning_auto_and_blocking() {
        let policy = ContextBudgetPolicy {
            context_window_tokens: 10_000,
            reserved_output_tokens: 1_000,
            auto_compact_buffer_tokens: 1_000,
            warning_buffer_tokens: 1_000,
            blocking_buffer_tokens: 500,
            ..ContextBudgetPolicy::default()
        };
        let thresholds = policy.thresholds();
        assert_eq!(
            policy
                .state_for_tokens(thresholds.warning_threshold)
                .pressure,
            ContextPressureLevel::Warning
        );
        assert_eq!(
            policy
                .state_for_tokens(thresholds.auto_compact_threshold)
                .pressure,
            ContextPressureLevel::AutoCompact
        );
        assert_eq!(
            policy
                .state_for_tokens(thresholds.blocking_threshold)
                .pressure,
            ContextPressureLevel::Blocking
        );
    }
}
