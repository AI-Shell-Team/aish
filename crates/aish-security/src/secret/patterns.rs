use serde::{Deserialize, Serialize};

/// Category of a detected secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretType {
    ApiKey,
    Token,
    Password,
    Credential,
}

impl Default for SecretType {
    fn default() -> Self {
        Self::ApiKey
    }
}

pub fn default_secret_type() -> SecretType {
    SecretType::default()
}

/// A single secret match found in input text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMatch {
    /// Human-readable name, e.g. "Anthropic API Key".
    pub pattern_name: String,
    /// Byte offset where the match starts.
    pub start: usize,
    /// Byte offset where the match ends (exclusive).
    pub end: usize,
    /// Category of the secret.
    pub secret_type: SecretType,
}

impl SecretMatch {
    pub fn format_reason(&self) -> String {
        format!(
            "{} detected at position {}..{}",
            self.pattern_name, self.start, self.end
        )
    }
}

/// Internal representation of a compiled pattern.
pub struct SecretPattern {
    pub name: &'static str,
    pub regex: &'static str,
    pub secret_type: SecretType,
}

/// User-defined pattern from config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomPattern {
    pub name: String,
    pub pattern: String,
    #[serde(default = "default_secret_type")]
    pub secret_type: SecretType,
}

pub fn builtin_patterns() -> Vec<SecretPattern> {
    vec![
        SecretPattern {
            name: "OpenAI API Key",
            regex: r"\bsk-[a-zA-Z0-9]{48}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "Anthropic API Key",
            regex: r"\bsk-ant-api\d{0,2}-[a-zA-Z0-9\-]{80,120}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "Generic SK API Key",
            regex: r"\bsk-[a-zA-Z0-9\-]{10,100}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "Google API Key",
            regex: r"\bAIza[0-9A-Za-z-_]{35}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "AWS Access Key",
            regex: r"\b(AKIA|A3T|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{12,}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "GitHub Classic PAT",
            regex: r"\bghp_[A-Za-z0-9_]{36}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "GitHub Fine-Grained PAT",
            regex: r"\bgithub_pat_[A-Za-z0-9_]{82}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "GitHub OAuth Token",
            regex: r"\bgho_[A-Za-z0-9_]{36}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "Stripe Key",
            regex: r"\b[rs]k_(test|live)_[0-9a-zA-Z]{24}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "Slack App Token",
            regex: r"\bxapp-[0-9]+-[A-Za-z0-9_]+-[0-9]+-[a-f0-9]+\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "Fireworks API Key",
            regex: r"\bfw_[a-zA-Z0-9]{24}\b",
            secret_type: SecretType::ApiKey,
        },
        SecretPattern {
            name: "JWT",
            regex: r"\bey[a-zA-Z0-9_\-=]{10,}\.[a-zA-Z0-9_\-=]{10,}\.[a-zA-Z0-9_\-=]{10,}\b",
            secret_type: SecretType::Token,
        },
        SecretPattern {
            name: "URL Embedded Password",
            regex: r"://[^/\s:]+:[^/\s@]+@",
            secret_type: SecretType::Password,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns_are_valid_regex() {
        for p in builtin_patterns() {
            assert!(
                regex::Regex::new(p.regex).is_ok(),
                "builtin pattern '{}' has invalid regex: {}",
                p.name,
                p.regex
            );
        }
    }

    #[test]
    fn builtin_patterns_are_not_empty() {
        assert!(!builtin_patterns().is_empty());
    }

    #[test]
    fn secret_type_default_is_api_key() {
        assert_eq!(default_secret_type(), SecretType::ApiKey);
    }

    #[test]
    fn stripe_pattern_matches_rk_and_sk_prefix() {
        let re = regex::Regex::new(
            builtin_patterns()
                .iter()
                .find(|p| p.name == "Stripe Key")
                .unwrap()
                .regex,
        )
        .unwrap();
        let key_24 = "abcdefghijklmnopqrstuvwx"; // exactly 24 chars
        assert!(re.is_match(&format!("sk_test_{key_24}")));
        assert!(re.is_match(&format!("rk_live_{key_24}")));
        assert!(!re.is_match(&format!("xk_test_{key_24}")));
    }
}
