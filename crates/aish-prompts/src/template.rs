use std::collections::HashMap;

/// Render a template string by replacing `{{key}}` placeholders with values.
///
/// Unknown placeholders are left as-is (not removed). This allows templates
/// to contain literal `{{` / `}}` sequences that are not variable references.
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' && chars.peek() == Some(&'{') {
            chars.next(); // consume second {
            let mut key = String::new();
            let mut found_close = false;
            while let Some(kch) = chars.next() {
                if kch == '}' && chars.peek() == Some(&'}') {
                    chars.next(); // consume second }
                    found_close = true;
                    break;
                }
                key.push(kch);
            }
            if found_close {
                let key = key.trim();
                if let Some(val) = vars.get(key) {
                    result.push_str(val);
                } else {
                    result.push_str("{{");
                    result.push_str(key);
                    result.push_str("}}");
                }
            } else {
                result.push_str("{{");
                result.push_str(&key);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_substitution() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "world".to_string());
        assert_eq!(render_template("hello {{name}}!", &vars), "hello world!");
    }

    #[test]
    fn test_multiple_vars() {
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), "1".to_string());
        vars.insert("b".to_string(), "2".to_string());
        assert_eq!(render_template("{{a}}+{{b}}", &vars), "1+2");
    }

    #[test]
    fn test_unknown_placeholder_preserved() {
        let vars = HashMap::new();
        assert_eq!(
            render_template("hello {{unknown}}!", &vars),
            "hello {{unknown}}!"
        );
    }

    #[test]
    fn test_no_placeholders() {
        let vars = HashMap::new();
        assert_eq!(render_template("plain text", &vars), "plain text");
    }

    #[test]
    fn test_whitespace_in_key() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "x".to_string());
        assert_eq!(render_template("{{ name }}", &vars), "x");
    }
}
