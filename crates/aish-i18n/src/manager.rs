use std::collections::HashMap;

use aish_core::Result;
use tracing::debug;

// Embed all locale files at compile time so the binary works standalone.
const EN_US_YAML: &str = include_str!("../locales/en-US.yaml");
const ZH_CN_YAML: &str = include_str!("../locales/zh-CN.yaml");
const JA_JP_YAML: &str = include_str!("../locales/ja-JP.yaml");
const DE_DE_YAML: &str = include_str!("../locales/de-DE.yaml");
const ES_ES_YAML: &str = include_str!("../locales/es-ES.yaml");
const FR_FR_YAML: &str = include_str!("../locales/fr-FR.yaml");

/// Compile-time embedded locales: locale tag → YAML content.
static EMBEDDED_LOCALES: &[(&str, &str)] = &[
    ("en-US", EN_US_YAML),
    ("zh-CN", ZH_CN_YAML),
    ("ja-JP", JA_JP_YAML),
    ("de-DE", DE_DE_YAML),
    ("es-ES", ES_ES_YAML),
    ("fr-FR", FR_FR_YAML),
];

/// Manages locale detection, YAML loading and key lookup.
pub struct I18nManager {
    translations: HashMap<String, String>,
    #[allow(dead_code)] // kept for future locale-switching support
    locale: String,
}

impl I18nManager {
    /// Create a new manager, auto-detecting the locale from `LANG` / `LC_ALL`.
    /// Falls back to `"en-US"` when the environment variables are absent or
    /// don't match a known locale file.
    pub fn new() -> Self {
        let locale = Self::detect_locale();
        Self::new_with_locale(&locale)
    }

    /// Create a manager for the given locale, falling back to `"en-US"` when
    /// the requested locale file cannot be found.
    pub fn new_with_locale(locale: &str) -> Self {
        let translations = match Self::load_translations(locale) {
            Some(t) => t,
            None => if locale != "en-US" {
                debug!(
                    target: "aish_i18n",
                    "locale {:?} not found, falling back to en-US",
                    locale
                );
                Self::load_translations("en-US")
            } else {
                None
            }
            .unwrap_or_else(|| {
                debug!(
                    target: "aish_i18n",
                    "en-US also missing, using embedded en-US as last resort"
                );
                parse_yaml(EN_US_YAML)
            }),
        };

        Self {
            translations,
            locale: locale.to_string(),
        }
    }

    /// Load translations from embedded YAML files.
    /// Equivalent to [`new()`] but returns a `Result` for explicit error handling.
    pub fn load() -> Result<Self> {
        Ok(Self::new())
    }

    /// Look up a dot-separated key.  Returns the value string, or the key
    /// itself when the key is missing (so callers always get usable text).
    pub fn t(&self, key: &str) -> String {
        match self.translations.get(key) {
            Some(val) => val.clone(),
            None => {
                debug!(target: "aish_i18n", "missing i18n key: {:?}", key);
                key.to_string()
            }
        }
    }

    /// Look up a key and substitute `{variable}` placeholders from `args`.
    ///
    /// Placeholders that have no matching entry in `args` are left as-is.
    pub fn t_with_args(&self, key: &str, args: &HashMap<String, String>) -> String {
        let template = self.t(key);
        substitute_placeholders(&template, args)
    }

    // -- internal helpers ---------------------------------------------------

    fn detect_locale() -> String {
        // LC_ALL takes precedence over LANG.
        for var in &["LC_ALL", "LANG"] {
            if let Ok(val) = std::env::var(var) {
                // Common formats: "en_US.UTF-8", "en_US", "en-US"
                let normalized = val.split('.').next().unwrap_or("").replace('_', "-");
                // Only return if it looks plausible (contains at least a language code).
                if !normalized.is_empty() && normalized != "C" {
                    return normalized;
                }
            }
        }
        "en-US".to_string()
    }

    /// Try to load and parse a locale YAML.
    ///
    /// Search order:
    /// 1. Filesystem override (user config → system → development path)
    /// 2. Embedded compile-time locale
    fn load_translations(locale: &str) -> Option<HashMap<String, String>> {
        // Try filesystem first (allows user overrides)
        if let Some(content) = Self::load_locale_yaml_from_disk(locale) {
            return Some(parse_yaml(&content));
        }

        // Fall back to embedded locale
        for &(tag, yaml) in EMBEDDED_LOCALES {
            if tag == locale {
                debug!(
                    target: "aish_i18n",
                    "using embedded locale {:?}",
                    locale
                );
                return Some(parse_yaml(yaml));
            }
        }

        None
    }

    /// Try to load a locale YAML from disk (user override, system, or dev path).
    fn load_locale_yaml_from_disk(locale: &str) -> Option<String> {
        // User override in their config directory (highest priority)
        let user_path = dirs::home_dir()
            .map(|h| format!("{}/.config/aish/locales/{}.yaml", h.display(), locale));

        // Installed system-wide path
        let system_path = format!("/usr/share/aish/locales/{}.yaml", locale);

        // Development path (relative to workspace root)
        let dev_path = format!("crates/aish-i18n/locales/{}.yaml", locale);

        let paths: Vec<String> = if let Some(ref up) = user_path {
            vec![up.clone(), system_path, dev_path]
        } else {
            vec![system_path, dev_path]
        };

        for path in &paths {
            if let Ok(content) = std::fs::read_to_string(path) {
                debug!(target: "aish_i18n", "loaded locale {:?} from {:?}", locale, path);
                return Some(content);
            }
        }
        None
    }
}

impl Default for I18nManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// YAML helpers
// ---------------------------------------------------------------------------

/// Recursively flatten a YAML value tree into `"section.key" -> "value"` pairs.
fn flatten_yaml(prefix: &str, value: &serde_yaml::Value, map: &mut HashMap<String, String>) {
    match value {
        serde_yaml::Value::Mapping(m) => {
            for (k, v) in m {
                if let serde_yaml::Value::String(key) = k {
                    let new_prefix = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{}.{}", prefix, key)
                    };
                    flatten_yaml(&new_prefix, v, map);
                }
            }
        }
        serde_yaml::Value::String(s) => {
            map.insert(prefix.to_string(), s.clone());
        }
        serde_yaml::Value::Number(n) => {
            map.insert(prefix.to_string(), n.to_string());
        }
        serde_yaml::Value::Bool(b) => {
            map.insert(prefix.to_string(), b.to_string());
        }
        // Sequences and null are stored as their debug representation.
        other => {
            map.insert(prefix.to_string(), format!("{:?}", other));
        }
    }
}

/// Parse a YAML string into a flat key-value map.
fn parse_yaml(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    match serde_yaml::from_str::<serde_yaml::Value>(content) {
        Ok(root) => flatten_yaml("", &root, &mut map),
        Err(e) => {
            debug!(target: "aish_i18n", "failed to parse YAML: {}", e);
        }
    }
    map
}

/// Replace `{name}` tokens in `template` with values from `args`.
fn substitute_placeholders(template: &str, args: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in args {
        let pattern = format!("{{{}}}", key);
        result = result.replace(&pattern, value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_simple_key() {
        let yaml = r#"
cli:
  app_help: "Hello world"
"#;
        let map = parse_yaml(yaml);
        assert_eq!(map.get("cli.app_help").unwrap(), "Hello world");
    }

    #[test]
    fn flatten_nested_keys() {
        let yaml = r#"
shell:
  welcome2:
    header: ">_ AI Shell {version}"
"#;
        let map = parse_yaml(yaml);
        assert_eq!(
            map.get("shell.welcome2.header").unwrap(),
            ">_ AI Shell {version}"
        );
    }

    #[test]
    fn substitute_works() {
        let mut args = HashMap::new();
        args.insert("version".to_string(), "0.2.0".to_string());
        let result = substitute_placeholders(">_ AI Shell {version}", &args);
        assert_eq!(result, ">_ AI Shell 0.2.0");
    }

    #[test]
    fn missing_key_returns_key() {
        let mgr = I18nManager::new_with_locale("en-US");
        assert_eq!(mgr.t("nonexistent.key"), "nonexistent.key");
    }

    #[test]
    fn unknown_locale_falls_back() {
        // "xx-XX" has no YAML file, but en-US is embedded so this should still work.
        let mgr = I18nManager::new_with_locale("xx-XX");
        assert!(!mgr.t("cli.app_help").is_empty());
        assert_ne!(mgr.t("cli.app_help"), "cli.app_help");
    }

    #[test]
    fn embedded_zh_cn_works() {
        // zh-CN is embedded at compile time, should work without filesystem access.
        let mgr = I18nManager::new_with_locale("zh-CN");
        // Should return Chinese text, not the key itself
        let step_provider = mgr.t("cli.setup.step_provider");
        assert_ne!(step_provider, "cli.setup.step_provider");
        assert!(
            step_provider.contains("步骤"),
            "Expected Chinese text, got: {}",
            step_provider
        );
    }

    #[test]
    fn all_embedded_locales_loadable() {
        for &(tag, _) in EMBEDDED_LOCALES {
            let mgr = I18nManager::new_with_locale(tag);
            let val = mgr.t("cli.app_help");
            assert_ne!(val, "cli.app_help", "Locale {} failed to load", tag);
        }
    }
}
