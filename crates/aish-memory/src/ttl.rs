//! TTL policy for long-term memories.
//!
//! Issue #472 asked for expiry specifically for *volatile* facts (temporary
//! ports, test-host identities), not for everything. PR #478 shipped a flat
//! 7-day TTL on all auto-retained entries, which silently ages out stable
//! user preferences. This module restores the intent with a hybrid policy:
//!
//! - The **model** judges volatility per fact (`ttl_seconds` + `reason`).
//! - The **policy** anchors defaults per category/scope and caps the maximum,
//!   so a missing or over-long model value can never make a volatile host
//!   fact permanent.

use aish_core::{MemoryCategory, MemoryScope};

/// Default TTL (seconds) when the caller does not supply one.
///
/// `None` = no expiry. The model has already judged volatility per the
/// tool prompt (short TTL for temporary ports/test hosts, silence for
/// durable facts), so silence means durable EXCEPT for the categories
/// #472 targets: Environment facts (endpoints/ports/credentials) and
/// host-scoped facts rot even when the model stays quiet. Imposing a
/// default expiry on Other/Solution would silently age out durable facts
/// the user never asked to expire.
pub fn default_ttl(category: &MemoryCategory, scope: &MemoryScope) -> Option<u64> {
    // Host-scoped facts are inherently volatile (machines get rebuilt, IPs
    // change) and must never be permanent by default, regardless of category.
    if matches!(scope, MemoryScope::Host) {
        return Some(HOST_SCOPE_CAP_SECS);
    }
    match category {
        MemoryCategory::Environment => Some(7 * 24 * 3600),
        MemoryCategory::Preference
        | MemoryCategory::Pattern
        | MemoryCategory::Solution
        | MemoryCategory::Other => None,
    }
}

/// Hard cap (seconds) on any TTL for host-scoped entries. Even when the
/// model explicitly proposes a long TTL, host facts cannot outlive this
/// without a user verify/renew (`/memory verify`).
pub const HOST_SCOPE_CAP_SECS: u64 = 30 * 24 * 3600;

/// Hard cap (seconds) on any TTL for Environment entries in user scope:
/// endpoints/ports/credentials rot, so a model-proposed "permanent" or
/// multi-year value is clamped.
pub const ENVIRONMENT_CAP_SECS: u64 = 90 * 24 * 3600;

/// Minimum TTL (seconds). Proposals below this are treated as model noise
/// and raised to one minute — an entry that expires instantly serves no
/// recall purpose and pollutes the store.
pub const MIN_TTL_SECS: u64 = 60;

/// Resolve the effective TTL for a new entry.
///
/// - `proposed`: the model-supplied `ttl_seconds` (if any).
/// - Returns the proposed value when it is within policy, the category
///   default when absent, and clamps to the cap when the proposal exceeds it.
///   `None` proposals for capped categories resolve to the default (not
///   "permanent"), because "model stayed silent" must not mean "forever".
///   Proposals below [`MIN_TTL_SECS`] are raised to it: a 1-second TTL is
///   model noise, not a meaningful volatility signal.
pub fn resolve_ttl(
    proposed: Option<u64>,
    category: &MemoryCategory,
    scope: &MemoryScope,
) -> Option<u64> {
    let cap = if matches!(scope, MemoryScope::Host) {
        Some(HOST_SCOPE_CAP_SECS)
    } else if matches!(category, MemoryCategory::Environment) {
        Some(ENVIRONMENT_CAP_SECS)
    } else {
        None
    };

    match (proposed, cap) {
        // No proposal: fall back to the category/scope default. For stable
        // categories that is None (permanent); capped categories get a
        // bounded default.
        (None, _) => default_ttl(category, scope),
        (Some(secs), None) => Some(secs.max(MIN_TTL_SECS)),
        (Some(secs), Some(cap)) => Some(secs.clamp(MIN_TTL_SECS, cap)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_categories_never_expire_by_default() {
        assert_eq!(
            default_ttl(&MemoryCategory::Preference, &MemoryScope::User),
            None
        );
        assert_eq!(
            default_ttl(&MemoryCategory::Pattern, &MemoryScope::User),
            None
        );
    }

    #[test]
    fn environment_defaults_to_7_days() {
        assert_eq!(
            default_ttl(&MemoryCategory::Environment, &MemoryScope::User),
            Some(7 * 24 * 3600)
        );
    }

    #[test]
    fn solution_and_other_silent_model_never_expire() {
        // The model judged volatility already (prompt instructs short TTL
        // for temp facts); silence on Other/Solution means durable.
        // Defaulting these to an expiry would silently age out durable
        // facts the user never asked to expire.
        assert_eq!(
            default_ttl(&MemoryCategory::Solution, &MemoryScope::User),
            None
        );
        assert_eq!(
            default_ttl(&MemoryCategory::Other, &MemoryScope::User),
            None
        );
    }

    #[test]
    fn host_scope_always_capped() {
        // Host facts are volatile even for stable categories.
        assert_eq!(
            default_ttl(&MemoryCategory::Preference, &MemoryScope::Host),
            Some(HOST_SCOPE_CAP_SECS)
        );
        // Model proposal beyond the cap is clamped.
        assert_eq!(
            resolve_ttl(
                Some(10 * 365 * 24 * 3600),
                &MemoryCategory::Environment,
                &MemoryScope::Host
            ),
            Some(HOST_SCOPE_CAP_SECS)
        );
        // Silent model still gets a bounded TTL, never permanent.
        assert_eq!(
            resolve_ttl(None, &MemoryCategory::Other, &MemoryScope::Host),
            Some(HOST_SCOPE_CAP_SECS)
        );
    }

    #[test]
    fn environment_cap_clamps_user_scope() {
        assert_eq!(
            resolve_ttl(
                Some(10 * 365 * 24 * 3600),
                &MemoryCategory::Environment,
                &MemoryScope::User
            ),
            Some(ENVIRONMENT_CAP_SECS)
        );
        assert_eq!(
            resolve_ttl(None, &MemoryCategory::Environment, &MemoryScope::User),
            Some(7 * 24 * 3600)
        );
    }

    #[test]
    fn sub_minute_proposals_raised_to_floor() {
        // 1-second TTL is model noise, not a volatility signal.
        assert_eq!(
            resolve_ttl(Some(1), &MemoryCategory::Environment, &MemoryScope::User),
            Some(MIN_TTL_SECS)
        );
        assert_eq!(
            resolve_ttl(Some(0), &MemoryCategory::Preference, &MemoryScope::User),
            Some(MIN_TTL_SECS)
        );
    }

    #[test]
    fn explicit_proposal_respected_within_policy() {
        // Short proposal for a volatile fact is honored as-is.
        assert_eq!(
            resolve_ttl(Some(3600), &MemoryCategory::Environment, &MemoryScope::User),
            Some(3600)
        );
        // No proposal for a stable preference stays permanent.
        assert_eq!(
            resolve_ttl(None, &MemoryCategory::Preference, &MemoryScope::User),
            None
        );
        // Explicit proposal for a stable category is not clamped.
        assert_eq!(
            resolve_ttl(
                Some(365 * 24 * 3600),
                &MemoryCategory::Preference,
                &MemoryScope::User
            ),
            Some(365 * 24 * 3600)
        );
    }
}
