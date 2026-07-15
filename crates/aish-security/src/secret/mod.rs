mod patterns;
mod scanner;
mod vault;

pub use patterns::{CustomPattern, SecretMatch, SecretPattern, SecretType};
pub use scanner::SecretScanner;
pub use vault::SecretVault;

/// Replace all detected secrets in `text` with `[REDACTED:type]` markers.
pub fn redact_secrets(text: &str, scanner: &SecretScanner) -> String {
    let matches = scanner.scan(text);
    if matches.is_empty() {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;
    for m in &matches {
        result.push_str(&text[last_end..m.start]);
        let tag = match m.secret_type {
            SecretType::ApiKey => "api_key",
            SecretType::Token => "token",
            SecretType::Password => "password",
            SecretType::Credential => "credential",
        };
        result.push_str(&format!("[REDACTED:{tag}]"));
        last_end = m.end;
    }
    result.push_str(&text[last_end..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_replaces_openai_key() {
        let scanner = SecretScanner::new(&[]);
        let input = "export OPENAI_API_KEY=sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let redacted = redact_secrets(input, &scanner);
        assert!(redacted.contains("[REDACTED:"));
        assert!(!redacted.contains("sk-aaaa"));
    }

    #[test]
    fn redact_preserves_clean_text() {
        let scanner = SecretScanner::new(&[]);
        let input = "ls -la /tmp";
        assert_eq!(redact_secrets(input, &scanner), input);
    }
}
