use std::cmp::Reverse;
use std::collections::HashMap;

use crate::secret::patterns::SecretMatch;

/// Internal entry tracking a single secret's placeholder and original value.
#[derive(Debug)]
struct SecretEntry {
    placeholder: String,
    original: String,
}

/// In-memory vault that maps detected secrets to stable placeholders.
///
/// Typical usage:
/// 1. Call [`SecretVault::redact`] with scanner results to replace secret
///    values with `$SECRET_…` placeholders.
/// 2. Pass the redacted text to an external service.
/// 3. Call [`SecretVault::restore`] on the response to put original values back.
#[derive(Debug, Default)]
pub struct SecretVault {
    /// Maps original secret value → entry metadata.
    entries: HashMap<String, SecretEntry>,
    /// Tracks how many distinct values have been seen per pattern name,
    /// so that collisions receive `_2`, `_3`, … suffixes.
    value_counter: HashMap<String, usize>,
}

/// Convert a human-readable pattern name into a placeholder token.
///
/// Rules:
/// - Convert to uppercase
/// - Replace non-alphanumeric characters with `_`
/// - Collapse consecutive `_` into one
/// - Strip leading/trailing `_`
/// - Prepend `$SECRET_`
///
/// Examples:
/// - `"OpenAI API Key"` → `"$SECRET_OPENAI_API_KEY"`
/// - `"JWT"` → `"$SECRET_JWT"`
/// - `"URL Embedded Password"` → `"$SECRET_URL_EMBEDDED_PASSWORD"`
pub fn pattern_name_to_placeholder(name: &str) -> String {
    let upper = name.to_uppercase();
    let mut normalized = String::with_capacity(upper.len());
    let mut prev_underscore = false;

    for ch in upper.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            prev_underscore = false;
        } else {
            if !prev_underscore && !normalized.is_empty() {
                normalized.push('_');
                prev_underscore = true;
            }
        }
    }

    // Strip trailing underscore
    if normalized.ends_with('_') {
        normalized.pop();
    }

    format!("$SECRET_{normalized}")
}

impl SecretVault {
    /// Create a new, empty vault.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of secrets currently stored in the vault.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the vault contains any secrets.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return (or create) a unique placeholder for the given pattern name and value.
    ///
    /// - If the exact value was already stored, return its existing placeholder.
    /// - If the pattern name was seen before with a *different* value, append
    ///   `_2`, `_3`, … to distinguish.
    /// - Otherwise create a new placeholder from the pattern name.
    fn find_unique_placeholder(&mut self, pattern_name: &str, value: &str) -> String {
        // Same value → reuse existing placeholder
        if let Some(entry) = self.entries.get(value) {
            return entry.placeholder.clone();
        }

        let base = pattern_name_to_placeholder(pattern_name);
        let count = self
            .value_counter
            .entry(pattern_name.to_string())
            .or_insert(0);
        *count += 1;

        let placeholder = if *count == 1 {
            base
        } else {
            format!("{base}_{}", *count)
        };

        placeholder
    }

    /// Replace detected secrets in `input` with placeholder tokens.
    ///
    /// Matches are processed right-to-left so that byte offsets remain valid
    /// as earlier portions of the string are untouched until their turn.
    ///
    /// Returns the redacted string with all secrets replaced by `$SECRET_…` tokens.
    pub fn redact(&mut self, secrets: &[SecretMatch], input: &str) -> String {
        if secrets.is_empty() {
            return input.to_string();
        }

        // Collect (placeholder, start, end) tuples first, using original offsets.
        let mut replacements: Vec<(String, usize, usize)> = Vec::with_capacity(secrets.len());

        for sm in secrets {
            let value = &input[sm.start..sm.end];
            let placeholder = self.find_unique_placeholder(&sm.pattern_name, value);

            // Store the entry if this is a new value
            if !self.entries.contains_key(value) {
                self.entries.insert(
                    value.to_string(),
                    SecretEntry {
                        placeholder: placeholder.clone(),
                        original: value.to_string(),
                    },
                );
            }

            replacements.push((placeholder, sm.start, sm.end));
        }

        // Sort by start position descending (right-to-left) so that replacing
        // later spans first does not shift earlier byte offsets.
        replacements.sort_by_key(|entry| Reverse(entry.1));

        let mut result = input.to_string();
        for (placeholder, start, end) in &replacements {
            result.replace_range(*start..*end, placeholder);
        }

        result
    }

    /// Restore placeholders in `text` back to their original secret values.
    ///
    /// Returns `(restored_text, count)` where `count` is the number of
    /// placeholders that were successfully replaced.
    pub fn restore(&self, text: &str) -> (String, usize) {
        let mut result = text.to_string();
        let mut count = 0;

        // Sort by placeholder length descending so longer (more specific)
        // placeholders are replaced first, avoiding prefix collisions
        // (e.g. $SECRET_KEY_2 before $SECRET_KEY).
        let mut entries: Vec<&SecretEntry> = self.entries.values().collect();
        entries.sort_by_key(|entry| Reverse(entry.placeholder.len()));

        for entry in entries {
            let occurrences = result.matches(&entry.placeholder).count();
            if occurrences > 0 {
                result = result.replace(&entry.placeholder, &entry.original);
                count += occurrences;
            }
        }

        (result, count)
    }

    /// Redact secret values from command output before sending to AI.
    ///
    /// Scans `text` for all stored original values and replaces them with
    /// their corresponding placeholders. Returns `(redacted_text, count)`.
    ///
    /// Sort by original value length descending to avoid short secrets
    /// matching substrings of longer ones.
    pub fn redact_output(&self, text: &str) -> (String, usize) {
        let mut result = text.to_string();
        let mut count = 0;

        let mut entries: Vec<&SecretEntry> = self.entries.values().collect();
        entries.sort_by_key(|entry| Reverse(entry.original.len()));

        for entry in entries {
            let occurrences = result.matches(&entry.original).count();
            if occurrences > 0 {
                result = result.replace(&entry.original, &entry.placeholder);
                count += occurrences;
            }
        }

        (result, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::patterns::SecretType;

    /// Helper to build a SecretMatch quickly.
    fn make_match(name: &str, start: usize, end: usize, st: SecretType) -> SecretMatch {
        SecretMatch {
            pattern_name: name.to_string(),
            start,
            end,
            secret_type: st,
        }
    }

    // ── pattern_name_to_placeholder ──────────────────────────────────────

    #[test]
    fn placeholder_converts_openai_api_key() {
        assert_eq!(
            pattern_name_to_placeholder("OpenAI API Key"),
            "$SECRET_OPENAI_API_KEY"
        );
    }

    #[test]
    fn placeholder_converts_single_word() {
        assert_eq!(pattern_name_to_placeholder("JWT"), "$SECRET_JWT");
    }

    #[test]
    fn placeholder_collapses_multiple_spaces() {
        assert_eq!(
            pattern_name_to_placeholder("  foo   bar  "),
            "$SECRET_FOO_BAR"
        );
    }

    #[test]
    fn placeholder_strips_leading_trailing_non_alnum() {
        assert_eq!(
            pattern_name_to_placeholder("--hello-world--"),
            "$SECRET_HELLO_WORLD"
        );
    }

    #[test]
    fn placeholder_handles_special_chars() {
        assert_eq!(
            pattern_name_to_placeholder("key/v2 (beta)"),
            "$SECRET_KEY_V2_BETA"
        );
    }

    // ── redact ───────────────────────────────────────────────────────────

    #[test]
    fn redact_single_secret() {
        let mut vault = SecretVault::new();
        let input = "my key is sk-abc123xyz789 use it wisely";
        // "sk-abc123xyz789" starts at 10, ends at 25
        let secrets = vec![make_match("OpenAI API Key", 10, 25, SecretType::ApiKey)];

        let redacted = vault.redact(&secrets, input);
        assert_eq!(redacted, "my key is $SECRET_OPENAI_API_KEY use it wisely");
    }

    #[test]
    fn redact_multiple_different_secrets() {
        let mut vault = SecretVault::new();
        let input = "key=sk-aaaa token=sk-bbbb";
        // "sk-aaaa" is at 4..11, "sk-bbbb" is at 18..25
        let secrets = vec![
            make_match("OpenAI API Key", 4, 11, SecretType::ApiKey),
            make_match("Generic SK API Key", 18, 25, SecretType::ApiKey),
        ];

        let redacted = vault.redact(&secrets, input);
        assert_eq!(
            redacted,
            "key=$SECRET_OPENAI_API_KEY token=$SECRET_GENERIC_SK_API_KEY"
        );
    }

    #[test]
    fn redact_same_value_reuses_placeholder() {
        let mut vault = SecretVault::new();
        let input = "first=sk-aaaa second=sk-aaaa";
        // Both "sk-aaaa" occurrences: 6..13 and 21..28
        let secrets = vec![
            make_match("OpenAI API Key", 6, 13, SecretType::ApiKey),
            make_match("OpenAI API Key", 21, 28, SecretType::ApiKey),
        ];

        let redacted = vault.redact(&secrets, input);
        assert_eq!(
            redacted,
            "first=$SECRET_OPENAI_API_KEY second=$SECRET_OPENAI_API_KEY"
        );
    }

    #[test]
    fn redact_same_type_different_value_gets_suffix() {
        let mut vault = SecretVault::new();
        let input = "first=sk-aaaa second=sk-bbbb";
        // "sk-aaaa" at 6..13, "sk-bbbb" at 21..28
        let secrets = vec![
            make_match("OpenAI API Key", 6, 13, SecretType::ApiKey),
            make_match("OpenAI API Key", 21, 28, SecretType::ApiKey),
        ];

        let redacted = vault.redact(&secrets, input);
        assert_eq!(
            redacted,
            "first=$SECRET_OPENAI_API_KEY second=$SECRET_OPENAI_API_KEY_2"
        );
    }

    #[test]
    fn redact_no_secrets_returns_input_unchanged() {
        let mut vault = SecretVault::new();
        let input = "nothing to see here";
        let redacted = vault.redact(&[], input);
        assert_eq!(redacted, input);
    }

    // ── restore ──────────────────────────────────────────────────────────

    #[test]
    fn restore_unknown_placeholder_passes_through() {
        let vault = SecretVault::new();
        let text = "hello $SECRET_UNKNOWN world";
        let (restored, count) = vault.restore(text);
        assert_eq!(restored, text);
        assert_eq!(count, 0);
    }

    #[test]
    fn roundtrip_redact_restore_preserves_original() {
        let mut vault = SecretVault::new();
        let input = "key=sk-abc123xyz789 done";
        let secrets = vec![make_match("OpenAI API Key", 4, 19, SecretType::ApiKey)];

        let redacted = vault.redact(&secrets, input);
        assert_ne!(redacted, input);

        let (restored, count) = vault.restore(&redacted);
        assert_eq!(restored, input);
        assert_eq!(count, 1);
    }

    #[test]
    fn roundtrip_multiple_secrets() {
        let mut vault = SecretVault::new();
        let input = "a=sk-aaaa b=sk-bbbb c=sk-aaaa";
        // "sk-aaaa" at 2..9, "sk-bbbb" at 12..19, "sk-aaaa" at 22..29
        let secrets = vec![
            make_match("OpenAI API Key", 2, 9, SecretType::ApiKey),
            make_match("OpenAI API Key", 12, 19, SecretType::ApiKey),
            make_match("OpenAI API Key", 22, 29, SecretType::ApiKey),
        ];

        let redacted = vault.redact(&secrets, input);
        // Two distinct values: first occurrence gets base, second gets _2,
        // third reuses the same placeholder as the first (same value).
        assert!(redacted.contains("$SECRET_OPENAI_API_KEY"));
        assert!(redacted.contains("$SECRET_OPENAI_API_KEY_2"));

        let (restored, count) = vault.restore(&redacted);
        assert_eq!(restored, input);
        assert_eq!(count, 3);
    }

    // ── redact_output ────────────────────────────────────────────────────

    #[test]
    fn redact_output_removes_secret_from_command_output() {
        let mut vault = SecretVault::new();
        let input = "key=sk-abc123xyz789 done";
        let secrets = vec![make_match("OpenAI API Key", 4, 19, SecretType::ApiKey)];

        let _redacted = vault.redact(&secrets, input);

        // Simulate command output containing the real secret.
        let output = "sk-abc123xyz789";
        let (result, count) = vault.redact_output(output);
        assert_eq!(result, "$SECRET_OPENAI_API_KEY");
        assert_eq!(count, 1);
    }

    #[test]
    fn redact_output_preserves_non_secret_text() {
        let vault = SecretVault::new();
        let (result, count) = vault.redact_output("hello world");
        assert_eq!(result, "hello world");
        assert_eq!(count, 0);
    }

    #[test]
    fn redact_output_handles_multiple_secrets() {
        let mut vault = SecretVault::new();
        let input = "a=sk-aaaa b=sk-bbbb";
        let secrets = vec![
            make_match("OpenAI API Key", 2, 9, SecretType::ApiKey),
            make_match("OpenAI API Key", 12, 19, SecretType::ApiKey),
        ];
        let _redacted = vault.redact(&secrets, input);

        let output = "results: sk-aaaa and sk-bbbb";
        let (result, count) = vault.redact_output(output);
        assert!(result.contains("$SECRET_OPENAI_API_KEY"));
        assert!(!result.contains("sk-aaaa"));
        assert!(!result.contains("sk-bbbb"));
        assert_eq!(count, 2);
    }

    #[test]
    fn full_roundtrip_redact_restore_redact_output() {
        let mut vault = SecretVault::new();
        let input = "my key is sk-abc123xyz789";
        let secrets = vec![make_match("OpenAI API Key", 10, 25, SecretType::ApiKey)];

        // 1. Redact input
        let redacted = vault.redact(&secrets, input);
        assert_eq!(redacted, "my key is $SECRET_OPENAI_API_KEY");

        // 2. AI generates command, restore for execution
        let cmd = "echo $SECRET_OPENAI_API_KEY";
        let (restored_cmd, _) = vault.restore(cmd);
        assert_eq!(restored_cmd, "echo sk-abc123xyz789");

        // 3. Simulate command output, redact before sending to AI
        let output = "sk-abc123xyz789";
        let (redacted_output, count) = vault.redact_output(output);
        assert_eq!(redacted_output, "$SECRET_OPENAI_API_KEY");
        assert_eq!(count, 1);
    }
}
