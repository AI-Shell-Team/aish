//! Multi-account credential rotation and model fallback chains.
//!
//! Inspired by oh-my-pi's `retry.fallbackChains` + round-robin credentials.
//! Provides two-layer recovery on top of the existing per-request HTTP retry
//! (which lives in `client.rs` / `api::mod.rs`):
//!
//! 1. **Account rotation** — one provider, multiple API keys. On a rate-limit /
//!    usage-limit the current account is cooled down and the next available
//!    one is tried for the rest of the turn.
//! 2. **Model fallback** — when every account is exhausted (or a model-specific
//!    hard error fires), switch to a configured fallback model. The primary
//!    model is suppressed for a cooldown window and may be restored on success
//!    (`revert_on_cooldown`).
//!
//! The state is kept in a [`RotationState`] behind a `Mutex` on `LlmSession`.
//! Each request reads the current credential via [`RotationState::current`] and,
//! on failure, advances via [`RotationState::advance_on_error`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aish_core::AishError;

/// Default cooldown applied to an account after a rate-limit / usage-limit hit.
pub const DEFAULT_ACCOUNT_COOLDOWN: Duration = Duration::from_secs(60);
/// Default cooldown applied to the primary model after falling back.
pub const DEFAULT_MODEL_COOLDOWN: Duration = Duration::from_secs(120);

/// A single API credential used for quota rotation. Multiple accounts under the
/// same provider let aish keep working when one key burns its quota.
#[derive(Debug, Clone)]
pub struct ApiAccount {
    /// Human-readable label, e.g. `"work-key"` / `"team-b"`.
    pub name: String,
    pub api_key: String,
    /// Optional override of the provider base URL for this account.
    pub api_base: Option<String>,
    /// Optional per-account model override. Falls back to the active model
    /// (primary or fallback chain) when `None`.
    pub model: Option<String>,
    /// Relative selection weight (higher = preferred). Currently advisory.
    pub weight: u32,
    /// When true the account is skipped entirely.
    pub disabled: bool,
}

impl ApiAccount {
    pub fn new(name: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            api_key: api_key.into(),
            api_base: None,
            model: None,
            weight: 1,
            disabled: false,
        }
    }
}

/// Retry / fallback policy driving [`RotationState`].
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub account_cooldown: Duration,
    pub model_cooldown: Duration,
    /// When true, restore the primary model after its cooldown expires.
    pub revert_on_cooldown: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            account_cooldown: DEFAULT_ACCOUNT_COOLDOWN,
            model_cooldown: DEFAULT_MODEL_COOLDOWN,
            revert_on_cooldown: true,
        }
    }
}

/// Classification of a recoverable failure, used to decide the rotation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// HTTP 429 / explicit rate-limit wording.
    RateLimit,
    /// Quota / usage / billing exhausted (parsed from the response body).
    UsageLimit,
    /// 5xx server error after the inner HTTP retry budget is spent.
    ServerError,
    /// Connection / timeout / network.
    Network,
    /// Model-specific hard error (not found, unsupported) — skip the model.
    ModelError,
}

impl FailureKind {
    /// Heuristic classification from an [`AishError`], mirroring oh-my-pi's
    /// regex text classification: provider errors are not typed here, they are
    /// string-classified against the user-facing message. Returns `None` for
    /// errors rotation cannot fix (auth, parse, cancellation).
    pub fn from_error(err: &AishError) -> Option<Self> {
        let msg = match err {
            AishError::Llm(s) => s.as_str(),
            AishError::Timeout => return Some(Self::Network),
            _ => return None,
        };
        Self::from_message(msg)
    }

    /// Pure classifier over a message string — extracted for unit testing.
    pub fn from_message(msg: &str) -> Option<Self> {
        let lower = msg.to_lowercase();
        // Auth / permission: never recoverable by rotation.
        if lower.contains("401")
            || lower.contains("403")
            || lower.contains("authentication failed")
            || lower.contains("unauthorized")
            || lower.contains("forbidden")
            || lower.contains("invalid api key")
        {
            return None;
        }
        // Context-length / prompt-too-long: not recoverable by rotation — the
        // identical over-long prompt fails on every key and model. Must run
        // before the usage-limit check, whose "exceeded" keyword would otherwise
        // misclassify e.g. "context_length_exceeded" as a UsageLimit and burn
        // account cooldowns on an unrecoverable prompt-size error.
        if lower.contains("context length")
            || lower.contains("context_length")
            || lower.contains("maximum context")
            || lower.contains("context window")
        {
            return None;
        }
        // Model-not-found / unsupported → switch model.
        if lower.contains("404")
            || lower.contains("model not found")
            || lower.contains("does not exist")
            || lower.contains("does not support")
            || lower.contains("not supported")
            || lower.contains("unsupported")
        {
            return Some(Self::ModelError);
        }
        if lower.contains("429")
            || lower.contains("rate limit")
            || lower.contains("rate-limit")
            || lower.contains("too many requests")
        {
            return Some(Self::RateLimit);
        }
        if lower.contains("quota")
            || lower.contains("usage limit")
            || lower.contains("billing")
            || lower.contains("exceeded")
            || lower.contains("insufficient")
            || lower.contains("credit")
        {
            return Some(Self::UsageLimit);
        }
        if lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("connection")
            || lower.contains("connect")
            || lower.contains("reset")
            || lower.contains("unreachable")
            || lower.contains("broken pipe")
        {
            return Some(Self::Network);
        }
        if lower.contains("500")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("504")
            || lower.contains("server error")
            || lower.contains("overloaded")
            || lower.contains("service unavailable")
            || lower.contains("bad gateway")
            || lower.contains("internal error")
        {
            return Some(Self::ServerError);
        }
        None
    }

    /// Whether this failure should trigger account rotation (vs model fallback).
    pub fn is_account_recoverable(self) -> bool {
        // Network failures are environment-wide (not account-specific), so
        // rotating keys only burns otherwise-good credentials. They fall
        // through to model fallback instead of exhausting every account.
        matches!(self, Self::RateLimit | Self::UsageLimit | Self::ServerError)
    }
}

/// A resolved credential + model to use for the next request.
#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    pub api_key: String,
    pub api_base: String,
    pub model: String,
    /// Human-readable label for logging / UI, e.g. `"team-b @ gpt-4o"`.
    pub label: String,
}

/// Immutable snapshot of rotation state for UI display (`/token`).
#[derive(Debug, Clone, Default)]
pub struct RotationSnapshot {
    pub current_account: Option<String>,
    pub available_accounts: usize,
    pub cooled_accounts: Vec<String>,
    pub account_names: Vec<String>,
    pub primary_model: String,
    pub fallback_models: Vec<String>,
    pub active_model: String,
    pub on_fallback: bool,
    pub total_rotations: u64,
}

/// State machine for credential rotation + model fallback.
///
/// Owned by `LlmSession` behind a `Mutex`. Not `Clone`: rotation history is
/// session-scoped and must not be silently duplicated.
pub struct RotationState {
    accounts: Vec<ApiAccount>,
    current_account: usize,
    fallback_models: Vec<String>,
    primary_model: String,
    /// `0` = primary, `n` = `fallback_models[n-1]`.
    current_model_index: usize,
    account_cooldowns: HashMap<usize, Instant>,
    model_cooldown_until: Option<Instant>,
    policy: RetryPolicy,
    total_rotations: u64,
}

impl RotationState {
    pub fn new(
        primary_model: String,
        accounts: Vec<ApiAccount>,
        fallback_models: Vec<String>,
        policy: RetryPolicy,
    ) -> Self {
        Self {
            accounts,
            current_account: 0,
            fallback_models,
            primary_model,
            current_model_index: 0,
            account_cooldowns: HashMap::new(),
            model_cooldown_until: None,
            policy,
            total_rotations: 0,
        }
    }

    /// True when rotation can actually do something useful (more than one
    /// enabled account, or at least one fallback model).
    pub fn is_active(&self) -> bool {
        self.enabled_accounts().count() > 1 || !self.fallback_models.is_empty()
    }

    fn enabled_accounts(&self) -> impl Iterator<Item = (usize, &ApiAccount)> {
        self.accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.disabled)
    }

    /// The model currently in use (primary, or a fallback).
    pub fn active_model_name(&self) -> String {
        if self.current_model_index == 0 {
            self.primary_model.clone()
        } else {
            self.fallback_models
                .get(self.current_model_index - 1)
                .cloned()
                .unwrap_or_else(|| self.primary_model.clone())
        }
    }

    /// The credential + model to use right now. `default_base` is used when an
    /// account has no explicit `api_base` override.
    pub fn current(&self, default_base: &str) -> ResolvedCredential {
        let acct = self
            .accounts
            .get(self.current_account)
            .filter(|a| !a.disabled)
            .or_else(|| self.enabled_accounts().next().map(|(_, a)| a))
            .cloned()
            .unwrap_or_else(|| ApiAccount {
                name: "default".into(),
                api_key: String::new(),
                api_base: None,
                model: None,
                weight: 1,
                disabled: false,
            });
        let model = acct
            .model
            .clone()
            .unwrap_or_else(|| self.active_model_name());
        ResolvedCredential {
            api_base: acct
                .api_base
                .clone()
                .unwrap_or_else(|| default_base.to_string()),
            api_key: acct.api_key.clone(),
            label: format!("{} @ {}", acct.name, model),
            model,
        }
    }

    /// Advance rotation after a failure. Returns `true` if a recovery was
    /// applied (retry with a different credential / model), `false` if every
    /// recovery path is exhausted and the error should surface to the user.
    pub fn advance_on_error(&mut self, kind: FailureKind) -> bool {
        if !self.policy.enabled {
            return false;
        }
        // Network failures are environment-wide: rotating accounts only burns
        // otherwise-good keys against the same dead endpoint, and model fallback
        // retries that same endpoint under a different model name — also futile,
        // and it would demote the primary model for the whole cooldown window.
        // The inner HTTP retry already absorbs transient blips, so surface it.
        if matches!(kind, FailureKind::Network) {
            return false;
        }
        let now = Instant::now();

        // Layer 1: account rotation for recoverable kinds.
        if kind.is_account_recoverable() {
            self.account_cooldowns
                .insert(self.current_account, now + self.policy.account_cooldown);
            if let Some((idx, _)) = self.pick_available_account(now) {
                self.current_account = idx;
                self.total_rotations += 1;
                return true;
            }
            // No other account available right now → fall through to model fallback.
        }

        // Layer 2: model fallback (also reached when every account is cooled down).
        self.advance_model(now)
    }

    /// Pick the next enabled, non-cooled account that is not the current one.
    fn pick_available_account(&self, now: Instant) -> Option<(usize, &ApiAccount)> {
        let enabled: Vec<_> = self.enabled_accounts().collect();
        if enabled.is_empty() {
            return None;
        }
        // Round-robin starting just after the current index, skipping cooled.
        let n = self.accounts.len();
        for offset in 1..=n {
            let i = (self.current_account + offset) % n;
            if let Some(a) = self.accounts.get(i) {
                if a.disabled || self.is_cooled(i, now) {
                    continue;
                }
                return Some((i, a));
            }
        }
        None
    }

    fn is_cooled(&self, idx: usize, now: Instant) -> bool {
        self.account_cooldowns
            .get(&idx)
            .is_some_and(|until| *until > now)
    }

    fn advance_model(&mut self, now: Instant) -> bool {
        // Suppress the primary model with a cooldown on the first fall.
        if self.current_model_index == 0 {
            self.model_cooldown_until = Some(now + self.policy.model_cooldown);
        }
        let next = self.current_model_index + 1;
        if (next - 1) < self.fallback_models.len() {
            self.current_model_index = next;
            self.total_rotations += 1;
            true
        } else {
            false
        }
    }

    /// Called after a successful request: if we were on a fallback and the
    /// primary model's cooldown expired (and `revert_on_cooldown` is set),
    /// restore the primary model. Also clears any stale account cooldowns.
    pub fn on_success(&mut self) {
        let now = Instant::now();
        // Drop expired account cooldowns so they become eligible again.
        self.account_cooldowns.retain(|_, until| *until > now);

        if !self.policy.revert_on_cooldown || self.current_model_index == 0 {
            return;
        }
        if self.model_cooldown_until.is_some_and(|t| t <= now) {
            self.current_model_index = 0;
            self.model_cooldown_until = None;
        }
    }

    /// Reset back to the primary model + first account (e.g. after a manual
    /// model switch via `/model`).
    pub fn reset(&mut self) {
        self.current_model_index = 0;
        self.current_account = 0;
        self.model_cooldown_until = None;
        self.account_cooldowns.clear();
    }

    /// Manually switch to the named account, resetting any model fallback so
    /// the account's own model (or the global model) is used. Returns `false`
    /// when no enabled account matches `name`. This is the `/accounts use`
    /// path; it does not touch the rotation counter (manual switch != failover).
    pub fn use_account(&mut self, name: &str) -> bool {
        if let Some(idx) = self
            .accounts
            .iter()
            .position(|a| a.name == name && !a.disabled)
        {
            self.current_account = idx;
            self.current_model_index = 0;
            self.model_cooldown_until = None;
            self.account_cooldowns.remove(&idx);
            true
        } else {
            false
        }
    }

    /// Name of the account currently in use, for restoring the selection
    /// across a rotation rebuild (e.g. after `/accounts add`).
    pub fn current_account_name(&self) -> Option<&str> {
        self.accounts
            .get(self.current_account)
            .map(|a| a.name.as_str())
    }

    pub fn is_on_fallback(&self) -> bool {
        self.current_model_index > 0
    }

    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    /// Immutable snapshot for UI display.
    pub fn snapshot(&self) -> RotationSnapshot {
        let now = Instant::now();
        let account_names: Vec<String> = self
            .enabled_accounts()
            .map(|(_, a)| a.name.clone())
            .collect();
        let cooled: Vec<String> = self
            .enabled_accounts()
            .filter(|(i, _)| self.is_cooled(*i, now))
            .map(|(_, a)| a.name.clone())
            .collect();
        let current = self
            .accounts
            .get(self.current_account)
            .filter(|a| !a.disabled)
            .map(|a| a.name.clone());
        RotationSnapshot {
            current_account: current,
            available_accounts: self
                .enabled_accounts()
                .filter(|(i, _)| !self.is_cooled(*i, now))
                .count(),
            cooled_accounts: cooled,
            account_names,
            primary_model: self.primary_model.clone(),
            fallback_models: self.fallback_models.clone(),
            active_model: self.active_model_name(),
            on_fallback: self.is_on_fallback(),
            total_rotations: self.total_rotations,
        }
    }

    /// Human-readable error shown when every model was rejected by the provider
    /// as invalid. Aggregates the primary + fallback names so the user sees the
    /// full configured set, not only the last fallback attempted.
    pub fn model_exhaustion_error(&self) -> String {
        let tried = std::iter::once(self.primary_model.as_str())
            .chain(self.fallback_models.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "all configured models were rejected by the provider as invalid \
             (tried: {tried}). Verify the `model` / `fallback_models` names in \
             the aish config — they may be misspelled or not offered by this \
             endpoint."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts(n: usize) -> Vec<ApiAccount> {
        (0..n)
            .map(|i| {
                let mut a = ApiAccount::new(format!("acct-{i}"), format!("key-{i}"));
                a.weight = 1;
                a
            })
            .collect()
    }

    #[test]
    fn classifies_auth_as_non_recoverable() {
        assert!(FailureKind::from_message("API error 401: bad key").is_none());
        assert!(FailureKind::from_message("403 Forbidden").is_none());
        assert!(FailureKind::from_message("Invalid API key").is_none());
    }

    #[test]
    fn classifies_rate_limit_and_usage() {
        assert_eq!(
            FailureKind::from_message("API error 429: Too many requests"),
            Some(FailureKind::RateLimit)
        );
        assert_eq!(
            FailureKind::from_message("quota exceeded for this period"),
            Some(FailureKind::UsageLimit)
        );
    }

    #[test]
    fn classifies_model_and_server_errors() {
        assert_eq!(
            FailureKind::from_message("Model not found: gpt-x"),
            Some(FailureKind::ModelError)
        );
        assert_eq!(
            FailureKind::from_message("API error 503: Service unavailable"),
            Some(FailureKind::ServerError)
        );
        assert_eq!(
            FailureKind::from_message("connection reset by peer"),
            Some(FailureKind::Network)
        );
    }

    #[test]
    fn classifies_context_length_as_non_recoverable() {
        // Prompt-too-long errors are not account-specific and must not trigger
        // rotation (would waste cooldowns on an unrecoverable error).
        assert!(FailureKind::from_message("context_length_exceeded").is_none());
        assert!(
            FailureKind::from_message("This model's maximum context length is 8192 tokens")
                .is_none()
        );
        assert!(FailureKind::from_message("maximum context length exceeded").is_none());
        // A plain quota error is still classified as a usage limit.
        assert_eq!(
            FailureKind::from_message("quota exceeded for this period"),
            Some(FailureKind::UsageLimit)
        );
    }

    #[test]
    fn network_failure_surfaces_without_demoting_primary_model() {
        let mut s = RotationState::new(
            "gpt-4".into(),
            accounts(2),
            vec!["fallback-x".into()],
            RetryPolicy::default(),
        );
        // A network failure must surface immediately: no rotation recovery, and
        // the primary model must stay selected (not put on fallback cooldown).
        assert!(!s.advance_on_error(FailureKind::Network));
        assert_eq!(s.current("https://x").model, "gpt-4");
    }

    #[test]
    fn exhaustion_error_lists_primary_and_fallbacks() {
        let s = RotationState::new(
            "gl".into(),
            Vec::new(),
            vec!["glm-4.7".into()],
            RetryPolicy::default(),
        );
        let msg = s.model_exhaustion_error();
        assert!(msg.contains("gl"), "primary model missing: {msg}");
        assert!(msg.contains("glm-4.7"), "fallback model missing: {msg}");
    }

    #[test]
    fn use_account_switches_and_uses_its_model() {
        let mut deepseek = ApiAccount::new("deepseek", "key-d");
        deepseek.model = Some("deepseek-chat".into());
        let primary = ApiAccount::new("primary", "key-p");
        let mut s = RotationState::new(
            "gpt-4".into(),
            vec![primary, deepseek],
            Vec::new(),
            RetryPolicy::default(),
        );
        // primary uses the global model
        assert_eq!(s.current("https://x").model, "gpt-4");
        assert!(s.use_account("deepseek"));
        // switched account uses its own model
        assert_eq!(s.current("https://x").model, "deepseek-chat");
        assert_eq!(s.current_account_name(), Some("deepseek"));
    }

    #[test]
    fn use_account_unknown_returns_false() {
        let mut s = RotationState::new(
            "gpt-4".into(),
            accounts(2),
            Vec::new(),
            RetryPolicy::default(),
        );
        assert!(!s.use_account("nonexistent"));
        assert_eq!(s.current_account_name(), Some("acct-0"));
    }

    #[test]
    fn rotates_accounts_on_rate_limit() {
        let mut s = RotationState::new("gpt-4".into(), accounts(3), vec![], RetryPolicy::default());
        assert_eq!(s.current("https://x").api_key, "key-0");
        assert!(s.advance_on_error(FailureKind::RateLimit));
        assert_eq!(s.current("https://x").api_key, "key-1");
        assert!(s.advance_on_error(FailureKind::RateLimit));
        assert_eq!(s.current("https://x").api_key, "key-2");
    }

    #[test]
    fn falls_back_to_model_when_accounts_exhausted() {
        let mut s = RotationState::new(
            "gpt-4".into(),
            accounts(1),
            vec!["gpt-4o-mini".into()],
            RetryPolicy::default(),
        );
        // Single account: rate-limit cools it, no other account → model fallback.
        assert!(s.advance_on_error(FailureKind::RateLimit));
        assert!(s.is_on_fallback());
        assert_eq!(s.current("https://x").model, "gpt-4o-mini");
    }

    #[test]
    fn returns_false_when_everything_exhausted() {
        let mut s = RotationState::new(
            "gpt-4".into(),
            accounts(1),
            vec!["gpt-4o-mini".into()],
            RetryPolicy::default(),
        );
        assert!(s.advance_on_error(FailureKind::RateLimit)); // → fallback model
                                                             // Fallback model also fails with a model error → no more fallbacks.
        assert!(!s.advance_on_error(FailureKind::ModelError));
    }

    #[test]
    fn model_error_skips_account_rotation() {
        let mut s = RotationState::new(
            "gpt-4".into(),
            accounts(3),
            vec!["gpt-4o-mini".into()],
            RetryPolicy::default(),
        );
        // Model errors go straight to model fallback, ignoring accounts.
        assert!(s.advance_on_error(FailureKind::ModelError));
        assert!(s.is_on_fallback());
        assert_eq!(s.current("https://x").api_key, "key-0");
    }

    #[test]
    fn disabled_policy_never_rotates() {
        let mut policy = RetryPolicy::default();
        policy.enabled = false;
        let mut s = RotationState::new("gpt-4".into(), accounts(3), vec![], policy);
        assert!(!s.advance_on_error(FailureKind::RateLimit));
    }

    #[test]
    fn snapshot_reports_cooled_accounts() {
        let mut s = RotationState::new("gpt-4".into(), accounts(2), vec![], RetryPolicy::default());
        s.advance_on_error(FailureKind::RateLimit); // acct-0 cooled, switch to acct-1
        let snap = s.snapshot();
        assert_eq!(snap.current_account.as_deref(), Some("acct-1"));
        assert!(snap.cooled_accounts.contains(&"acct-0".to_string()));
        assert_eq!(snap.available_accounts, 1);
        assert_eq!(snap.total_rotations, 1);
    }

    #[test]
    fn is_active_requires_multiple_accounts_or_fallbacks() {
        let single = RotationState::new("m".into(), accounts(1), vec![], RetryPolicy::default());
        assert!(!single.is_active());
        let multi = RotationState::new("m".into(), accounts(2), vec![], RetryPolicy::default());
        assert!(multi.is_active());
        let with_fb = RotationState::new(
            "m".into(),
            accounts(1),
            vec!["x".into()],
            RetryPolicy::default(),
        );
        assert!(with_fb.is_active());
    }
}
