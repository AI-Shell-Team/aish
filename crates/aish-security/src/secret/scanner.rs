use regex::{Regex, RegexSet};

use crate::secret::patterns::{builtin_patterns, CustomPattern, SecretMatch, SecretType};

#[derive(Clone)]
pub struct SecretScanner {
    set: RegexSet,
    compiled: Vec<Regex>,
    pattern_names: Vec<String>,
    pattern_types: Vec<SecretType>,
}

impl SecretScanner {
    /// Build scanner from user-defined custom patterns (built-in patterns always included).
    /// Invalid custom patterns are logged and skipped.
    pub fn new(custom: &[CustomPattern]) -> Self {
        let builtins = builtin_patterns();
        let mut all_regexes: Vec<String> = Vec::with_capacity(builtins.len() + custom.len());
        let mut names: Vec<String> = Vec::with_capacity(all_regexes.len());
        let mut types: Vec<SecretType> = Vec::with_capacity(all_regexes.len());

        for p in &builtins {
            all_regexes.push(p.regex.to_string());
            names.push(p.name.to_string());
            types.push(p.secret_type);
        }

        for c in custom {
            match regex::Regex::new(&c.pattern) {
                Ok(_) => {
                    all_regexes.push(c.pattern.clone());
                    names.push(c.name.clone());
                    types.push(c.secret_type);
                }
                Err(e) => {
                    tracing::warn!("skipping invalid custom secret pattern '{}': {}", c.name, e);
                }
            }
        }

        let set = match RegexSet::new(&all_regexes) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("failed to compile secret RegexSet: {e}");
                return Self::empty();
            }
        };

        let compiled: Vec<Regex> = all_regexes
            .iter()
            .map(|p| {
                Regex::new(p)
                    .unwrap_or_else(|e| panic!("regex that passed RegexSet failed Regex::new: {e}"))
            })
            .collect();

        Self {
            set,
            compiled,
            pattern_names: names,
            pattern_types: types,
        }
    }

    /// Scanner that never matches anything, used as a safe fallback.
    fn empty() -> Self {
        let set = RegexSet::new(&["^.$_never_match_"]).unwrap();
        Self {
            set,
            compiled: Vec::new(),
            pattern_names: Vec::new(),
            pattern_types: Vec::new(),
        }
    }

    /// Scan input text for secrets. Returns all matches.
    pub fn scan(&self, input: &str) -> Vec<SecretMatch> {
        let matched_indices: Vec<_> = self.set.matches(input).into_iter().collect();
        if matched_indices.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        for idx in matched_indices {
            let Some(re) = self.compiled.get(idx) else {
                continue;
            };
            for mat in re.find_iter(input) {
                results.push(SecretMatch {
                    pattern_name: self.pattern_names[idx].clone(),
                    start: mat.start(),
                    end: mat.end(),
                    secret_type: self.pattern_types[idx],
                });
            }
        }

        results.sort_by(|a, b| {
            a.start.cmp(&b.start).then_with(|| a.end.cmp(&b.end)) // narrower span first
        });
        // Deduplicate overlapping matches: keep the first (narrowest/most-specific) match
        // for each overlapping region. Built-in patterns are ordered from most specific
        // to least specific, so earlier pattern indices produce better results.
        let mut deduped: Vec<SecretMatch> = Vec::new();
        for m in results {
            if let Some(prev) = deduped.last() {
                if m.start < prev.end {
                    continue;
                }
            }
            deduped.push(m);
        }
        deduped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::patterns::{CustomPattern, SecretType};

    fn default_scanner() -> SecretScanner {
        SecretScanner::new(&[])
    }

    #[test]
    fn scan_returns_empty_for_clean_input() {
        let scanner = default_scanner();
        assert!(scanner.scan("hello world, no secrets here").is_empty());
    }

    #[test]
    fn scan_detects_openai_key() {
        let scanner = default_scanner();
        // sk- + exactly 48 alphanumeric characters (OpenAI pattern requires exactly 48).
        let input = "please use sk-abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuv for auth";
        let matches = scanner.scan(input);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "OpenAI API Key");
        assert_eq!(matches[0].secret_type, SecretType::ApiKey);
    }

    #[test]
    fn scan_detects_url_embedded_password() {
        let scanner = default_scanner();
        let input = "connect to https://admin:s3cret@db.example.com:5432";
        let matches = scanner.scan(input);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "URL Embedded Password");
        assert_eq!(matches[0].secret_type, SecretType::Password);
    }

    #[test]
    fn scan_detects_multiple_secrets() {
        let scanner = default_scanner();
        // sk- + exactly 48 alphanumeric chars for OpenAI key.
        let sk_key = format!("sk-{}", "a".repeat(48));
        let jwt = format!("ey{}.{}.{}", "A".repeat(12), "B".repeat(12), "C".repeat(12));
        let input = format!("key={sk_key} token={jwt}");
        let matches = scanner.scan(&input);
        assert!(matches.len() >= 2);
    }

    #[test]
    fn scan_with_custom_pattern() {
        let custom = vec![CustomPattern {
            name: "Test Token".to_string(),
            pattern: r"test_[a-zA-Z0-9]{16}".to_string(),
            secret_type: SecretType::Token,
        }];
        let scanner = SecretScanner::new(&custom);
        let matches = scanner.scan("my token is test_abcdefghijklmnop");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "Test Token");
        assert_eq!(matches[0].secret_type, SecretType::Token);
    }

    #[test]
    fn scan_skips_invalid_custom_pattern() {
        let custom = vec![CustomPattern {
            name: "Bad".to_string(),
            pattern: "[invalid(".to_string(),
            secret_type: SecretType::ApiKey,
        }];
        let scanner = SecretScanner::new(&custom);
        assert!(scanner.scan("hello").is_empty());
    }

    #[test]
    fn scan_deduplicates_overlapping_matches_to_most_specific() {
        let scanner = default_scanner();
        // OpenAI key (48 chars) also matches Generic SK (10-100 chars).
        // Dedup should keep only the more specific OpenAI match.
        let input = "key=sk-abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuv";
        let matches = scanner.scan(input);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "OpenAI API Key");
    }
}
