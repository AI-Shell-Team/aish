//! Side-effect model and the trust decision applied before an MCP `call`.
//!
//! Policy (issue #564): an MCP tool call is auto-approved only when BOTH hold
//!
//! - the user declared the server (or the specific tool) `trust: allow`, and
//! - the effective side effects are `read`.
//!
//! Everything else — an undeclared server (`side_effects: unknown`), a write or
//! network tool, or a tool whose server annotations contradict the declared
//! scope — asks the user. `trust: block` refuses outright. Nothing an MCP
//! server claims about itself can widen the user's declaration: annotations and
//! the declaration are combined by taking the more dangerous of the two.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::types::{SideEffects, ToolAnnotations, TrustLevel};

/// Length of the hex digest kept in an approval key.
const DIGEST_HEX_LEN: usize = 16;

/// Outcome of the pre-call trust evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// Auto-approved.
    Allow,
    /// Requires user approval.
    Confirm,
    /// Refused by configuration.
    Block,
}

/// Side effects the server itself reported through tool annotations.
///
/// Returns `None` when the server sent no hints at all, so the caller can fall
/// back to the user's declaration instead of treating silence as "read-only".
pub fn reported_side_effects(annotations: &ToolAnnotations) -> Option<SideEffects> {
    if annotations.is_empty() {
        return None;
    }
    if annotations.destructive_hint == Some(true) {
        return Some(SideEffects::Write);
    }
    if annotations.read_only_hint == Some(false) {
        return Some(SideEffects::Write);
    }
    if annotations.open_world_hint == Some(true) {
        return Some(SideEffects::Network);
    }
    if annotations.read_only_hint == Some(true) {
        return Some(SideEffects::Read);
    }
    // Hints exist but say nothing about scope (e.g. only `idempotentHint`).
    Some(SideEffects::Unknown)
}

/// Combine the user's declaration with what the server reported, keeping the
/// more dangerous level.
pub fn combine_side_effects(declared: SideEffects, reported: Option<SideEffects>) -> SideEffects {
    match reported {
        Some(reported) => declared.max(reported),
        None => declared,
    }
}

/// Apply the trust policy to one call.
pub fn evaluate_call_trust(trust: TrustLevel, effective: SideEffects) -> TrustDecision {
    match trust {
        TrustLevel::Block => TrustDecision::Block,
        TrustLevel::Allow if effective == SideEffects::Read => TrustDecision::Allow,
        TrustLevel::Allow | TrustLevel::Ask => TrustDecision::Confirm,
    }
}

/// Stable digest of one exact argument set.
///
/// Used to bind an approval to the arguments it covered: any change to a value,
/// a key or the nesting order of an object produces a different digest, so an
/// old "yes" can never be replayed for different arguments.
pub fn args_digest(args: &Value) -> String {
    let mut canonical = String::new();
    canonicalize(args, &mut canonical);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    hex[..DIGEST_HEX_LEN.min(hex.len())].to_string()
}

/// Serialize a JSON value with object keys in sorted order so equivalent
/// values produce one canonical string regardless of insertion order.
fn canonicalize(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (position, key) in keys.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                canonicalize(&map[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                canonicalize(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_trusted_read_only_calls_are_auto_approved() {
        assert_eq!(
            evaluate_call_trust(TrustLevel::Allow, SideEffects::Read),
            TrustDecision::Allow
        );
        assert_eq!(
            evaluate_call_trust(TrustLevel::Allow, SideEffects::Write),
            TrustDecision::Confirm
        );
        assert_eq!(
            evaluate_call_trust(TrustLevel::Allow, SideEffects::Network),
            TrustDecision::Confirm
        );
        assert_eq!(
            evaluate_call_trust(TrustLevel::Allow, SideEffects::Unknown),
            TrustDecision::Confirm
        );
        assert_eq!(
            evaluate_call_trust(TrustLevel::Ask, SideEffects::Read),
            TrustDecision::Confirm
        );
        assert_eq!(
            evaluate_call_trust(TrustLevel::Block, SideEffects::Read),
            TrustDecision::Block
        );
    }

    #[test]
    fn undeclared_side_effects_never_auto_approve() {
        // Declared read-only but the server reports a destructive tool.
        let annotations = ToolAnnotations {
            destructive_hint: Some(true),
            ..ToolAnnotations::default()
        };
        let effective =
            combine_side_effects(SideEffects::Read, reported_side_effects(&annotations));
        assert_eq!(effective, SideEffects::Write);
        assert_eq!(
            evaluate_call_trust(TrustLevel::Allow, effective),
            TrustDecision::Confirm
        );
    }

    #[test]
    fn silence_from_the_server_keeps_the_declaration() {
        let effective = combine_side_effects(
            SideEffects::Read,
            reported_side_effects(&ToolAnnotations::default()),
        );
        assert_eq!(effective, SideEffects::Read);
        assert_eq!(
            combine_side_effects(SideEffects::Unknown, None),
            SideEffects::Unknown
        );
    }

    #[test]
    fn reported_hints_map_to_scopes() {
        let read_only = ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        };
        assert_eq!(reported_side_effects(&read_only), Some(SideEffects::Read));

        let open_world = ToolAnnotations {
            open_world_hint: Some(true),
            ..ToolAnnotations::default()
        };
        assert_eq!(
            reported_side_effects(&open_world),
            Some(SideEffects::Network)
        );

        let idempotent_only = ToolAnnotations {
            idempotent_hint: Some(true),
            ..ToolAnnotations::default()
        };
        assert_eq!(
            reported_side_effects(&idempotent_only),
            Some(SideEffects::Unknown)
        );

        let not_read_only = ToolAnnotations {
            read_only_hint: Some(false),
            ..ToolAnnotations::default()
        };
        assert_eq!(
            reported_side_effects(&not_read_only),
            Some(SideEffects::Write)
        );
    }

    #[test]
    fn digest_is_stable_and_argument_bound() {
        assert_eq!(
            args_digest(&json!({"a": 1, "b": [1, 2]})),
            args_digest(&json!({"b": [1, 2], "a": 1}))
        );
        assert_ne!(args_digest(&json!({"a": 1})), args_digest(&json!({"a": 2})));
        assert_ne!(args_digest(&json!({"a": 1})), args_digest(&json!({"b": 1})));
        assert_ne!(
            args_digest(&json!({"a": {"b": 1}})),
            args_digest(&json!({"a": {"b": 2}}))
        );
        assert_eq!(args_digest(&json!({})).len(), DIGEST_HEX_LEN);
    }
}
