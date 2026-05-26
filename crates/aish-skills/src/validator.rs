use std::path::Path;

/// Validation result for a skill file.
pub struct ValidationResult {
    pub is_valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn valid() -> Self {
        Self {
            is_valid: true,
            errors: vec![],
            warnings: vec![],
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            is_valid: false,
            errors: vec![msg.into()],
            warnings: vec![],
        }
    }
}

/// Validate a skill's frontmatter metadata.
pub fn validate_skill(
    name: &str,
    description: &str,
    content: &str,
    path: &Path,
) -> ValidationResult {
    let mut result = ValidationResult::valid();
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();

    // Required: name must not be empty
    if name.trim().is_empty() {
        result.is_valid = false;
        result
            .errors
            .push(format!("{}: skill name is required", file_name));
    }

    // Required: description must not be empty
    if description.trim().is_empty() {
        result.is_valid = false;
        result
            .errors
            .push(format!("{}: skill description is required", file_name));
    }

    // Required: content must not be empty
    if content.trim().is_empty() {
        result
            .warnings
            .push(format!("{}: skill body is empty", file_name));
    }

    // Name should be alphanumeric with dashes/underscores only
    if !name.is_empty()
        && !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        result.warnings.push(format!(
            "{}: skill name '{}' contains special characters (recommended: alphanumeric, dash, underscore)",
            file_name, name
        ));
    }

    // Name should not be too long
    if name.len() > 64 {
        result.warnings.push(format!(
            "{}: skill name is very long ({} chars, recommended max 64)",
            file_name,
            name.len()
        ));
    }

    // Description should not be too long
    if description.len() > 200 {
        result.warnings.push(format!(
            "{}: skill description is very long ({} chars, recommended max 200)",
            file_name,
            description.len(),
        ));
    }

    result
}

/// Validate a skill with an optional trigger pattern.
pub fn validate_skill_with_trigger(
    name: &str,
    description: &str,
    content: &str,
    trigger: &str,
    path: &Path,
) -> ValidationResult {
    let mut result = validate_skill(name, description, content, path);
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();

    // Validate trigger regex
    if !trigger.is_empty() {
        if let Err(e) = regex::Regex::new(trigger) {
            result.warnings.push(format!(
                "{}: trigger pattern '{}' is not a valid regex: {}",
                file_name, trigger, e
            ));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_path() -> PathBuf {
        PathBuf::from("/some/dir/SKILL.md")
    }

    #[test]
    fn test_valid_skill() {
        let result = validate_skill(
            "my-skill",
            "A useful skill",
            "Do something useful",
            &test_path(),
        );
        assert!(result.is_valid);
        assert!(result.errors.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_missing_name() {
        let result = validate_skill("", "A useful skill", "Do something useful", &test_path());
        assert!(!result.is_valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("skill name is required")));
    }

    #[test]
    fn test_missing_description() {
        let result = validate_skill("my-skill", "", "Do something useful", &test_path());
        assert!(!result.is_valid);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("skill description is required")));
    }

    #[test]
    fn test_empty_content_warning() {
        let result = validate_skill("my-skill", "A useful skill", "", &test_path());
        assert!(result.is_valid);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("skill body is empty")));
    }

    #[test]
    fn test_special_chars_warning() {
        let result = validate_skill(
            "my skill!",
            "A useful skill",
            "Do something useful",
            &test_path(),
        );
        assert!(result.is_valid);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("special characters")));
    }

    #[test]
    fn test_long_name_warning() {
        let long_name = "a".repeat(65);
        let result = validate_skill(
            &long_name,
            "A useful skill",
            "Do something useful",
            &test_path(),
        );
        assert!(result.is_valid);
        assert!(result.warnings.iter().any(|w| w.contains("very long")));
    }

    #[test]
    fn test_description_too_long() {
        let long_desc = "x".repeat(201);
        let result = validate_skill("my-skill", &long_desc, "content", &test_path());
        assert!(result.is_valid); // Warning, not error
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("description is very long")));
    }

    #[test]
    fn test_trigger_invalid_regex() {
        let result = validate_skill_with_trigger(
            "my-skill",
            "desc",
            "content",
            "[invalid regex",
            &test_path(),
        );
        assert!(result.is_valid); // Warning, not error
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("not a valid regex")));
    }

    #[test]
    fn test_trigger_valid_regex() {
        let result = validate_skill_with_trigger(
            "my-skill",
            "desc",
            "content",
            r"hello\s+world",
            &test_path(),
        );
        assert!(result.is_valid);
        assert!(!result.warnings.iter().any(|w| w.contains("regex")));
    }
}
