//! Remote access authorization (#586/#587 hardening): action-level RBAC
//! declared in the installed policy's `remote_access` section.
//!
//! Policy YAML extension (parsed leniently — absent section = allow all
//! authenticated operators, preserving single-machine behavior):
//!
//! ```yaml
//! remote_access:
//!   actions:            # absent actions map = allow all actions
//!     session.attach:
//!       operators: ["3fa8…", "89f5…"]   # operator public key hex prefixes
//!     policy.install:
//!       operators: ["89f5…"]            # only the admin key may push policy
//! ```
//!
//! An operator key matches an entry when the configured hex string is a
//! prefix of the presented key (so admins can pin full keys or shorthand
//! prefixes). Authorization runs AFTER identity authentication: first you
//! prove who you are, then we check what you may do.

/// Check whether `operator_key` may perform `action` given the installed
/// policy YAML. Order of evaluation:
///   1. policy absent (empty string = no remote policy installed) → allow
///   2. YAML present but **unparsable → deny (fail closed)**: the caller is
///      in enforcement mode, so a corrupted policy must not silently drop
///      RBAC restrictions
///   3. no `remote_access` section, or the action is not listed → allow
///   4. action listed → operator key must prefix-match one entry;
///      an empty/missing `operators` list denies everyone
pub fn authorize_action(policy_yaml: &str, action: &str, operator_key: &str) -> bool {
    // No remote policy installed: RBAC has never been configured here.
    if policy_yaml.trim().is_empty() {
        return true;
    }
    // Fail closed on parse errors: a hand-edited policy with a YAML mistake
    // must not widen access.
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(policy_yaml) else {
        return false;
    };
    let Some(ra) = v.get("remote_access") else {
        return true;
    };
    let Some(actions) = ra.get("actions") else {
        return true;
    };
    let Some(entry) = actions.get(action) else {
        return true;
    };
    let Some(operators) = entry.get("operators").and_then(|o| o.as_sequence()) else {
        // Action listed without operators = deny by default (explicit
        // declaration without a grant means nobody is authorized).
        return false;
    };
    operators.iter().any(|o| {
        o.as_str()
            .map(|prefix| operator_key.starts_with(prefix))
            .unwrap_or(false)
    })
}

/// Load the installed policy YAML from the standard path.
/// Absent file → Ok(empty string) = no remote policy installed.
/// Present but unreadable (permissions/IO error) → Err: the caller must
/// deny, because a policy that exists but cannot be read must not be
/// treated as "no policy installed" (review fix: fail closed on read errors,
/// not just on parse errors).
pub fn installed_policy_yaml() -> Result<String, String> {
    let Some(path) = aish_security::writable_security_policy_path() else {
        return Ok(String::new());
    };
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read installed policy {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = "global:\n  default_risk_level: LOW\nremote_access:\n  actions:\n    session.attach:\n      operators: [\"3fa8\"]\n    policy.install:\n      operators: [\"89f5118d\"]\n";

    #[test]
    fn absent_section_allows_everything() {
        assert!(authorize_action(
            "global:\n  default_risk_level: LOW\n",
            "session.attach",
            "abc"
        ));
        assert!(authorize_action("", "policy.install", "xyz"));
    }

    #[test]
    fn action_prefix_matching() {
        assert!(authorize_action(POLICY, "session.attach", "3fa81234abcd"));
        assert!(!authorize_action(POLICY, "session.attach", "89f5118d"));
        assert!(authorize_action(POLICY, "policy.install", "89f5118dfe08"));
        assert!(!authorize_action(POLICY, "policy.install", "3fa8"));
    }

    #[test]
    fn unlisted_action_allows() {
        assert!(authorize_action(POLICY, "diagnose.run", "anything"));
    }

    #[test]
    fn action_without_operators_denies() {
        let p = "remote_access:\n  actions:\n    session.kill: {}\n";
        assert!(!authorize_action(p, "session.kill", "3fa8"));
    }

    #[test]
    fn unparsable_policy_fails_closed() {
        // A corrupted policy must NOT drop RBAC restrictions (review fix):
        // deny instead of fail-open.
        assert!(!authorize_action(
            "\t\tgarbage\n  - [",
            "session.attach",
            "k"
        ));
    }
}
