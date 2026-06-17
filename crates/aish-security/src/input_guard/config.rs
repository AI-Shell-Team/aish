use regex::Regex;
use serde::{Deserialize, Serialize};

use super::{InputRule, RuleCategory};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputGuardConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    #[serde(default)]
    pub custom_block_rules: Vec<CustomRule>,

    #[serde(default)]
    pub custom_confirm_rules: Vec<CustomRule>,

    /// Override the default 4096-byte unanalyzable threshold.
    #[serde(default)]
    pub max_analyzable_bytes: Option<usize>,
}

impl Default for InputGuardConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            custom_block_rules: Vec::new(),
            custom_confirm_rules: Vec::new(),
            max_analyzable_bytes: None,
        }
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomRule {
    pub name: String,
    pub pattern: String,
    pub message: String,
}

impl CustomRule {
    pub fn to_input_rule(&self, category: RuleCategory) -> Option<InputRule> {
        Regex::new(&format!("(?i){}", self.pattern))
            .ok()
            .map(|regex| InputRule {
                regex,
                name: self.name.clone(),
                message: self.message.clone(),
                category,
                target_group: super::TargetGroup::None,
                safer_alternative: None,
            })
    }
}

pub fn merge_custom_rules(
    block_rules: &mut Vec<InputRule>,
    confirm_rules: &mut Vec<InputRule>,
    config: &InputGuardConfig,
) {
    for custom in &config.custom_block_rules {
        // Empty pattern usually means a YAML indent mistake: the user
        // wrote `pattern:` at the same column as `- name:` (or shallower),
        // so serde parsed it as a sibling key and silently dropped it.
        // Empty regex would compile and match the empty string only —
        // not what the user wants. Skip + warn so they see the rule is
        // inert instead of believing they're protected.
        if custom.pattern.trim().is_empty() {
            tracing::warn!(
                rule = %custom.name,
                message = %custom.message,
                "skipping custom block rule with empty pattern; \
                 this usually means the YAML indent is wrong — `pattern:` \
                 and `message:` must be aligned with `name:` inside the \
                 list item (more indented than the leading `-`)"
            );
            continue;
        }
        // N6: surface invalid regex explicitly instead of silently dropping.
        // A typo in a security rule would otherwise disable that rule with
        // no signal to the user — they'd believe they're protected when
        // they're not. Log at warn so it shows up in normal log levels.
        let rule = match Regex::new(&format!("(?i){}", custom.pattern)) {
            Ok(regex) => InputRule {
                regex,
                name: custom.name.clone(),
                message: custom.message.clone(),
                category: RuleCategory::DestructiveCommand,
                target_group: super::TargetGroup::None,
                safer_alternative: None,
            },
            Err(e) => {
                tracing::warn!(
                    rule = %custom.name,
                    pattern = %custom.pattern,
                    error = %e,
                    "skipping invalid custom block rule: regex failed to compile; \
                     this rule will NOT be enforced"
                );
                continue;
            }
        };
        if let Some(pos) = block_rules.iter().position(|r| r.name == rule.name) {
            block_rules[pos] = rule;
        } else {
            block_rules.push(rule);
        }
    }

    for custom in &config.custom_confirm_rules {
        if custom.pattern.trim().is_empty() {
            tracing::warn!(
                rule = %custom.name,
                message = %custom.message,
                "skipping custom confirm rule with empty pattern; \
                 check YAML indent — `pattern:` and `message:` must be \
                 aligned with `name:` inside the list item"
            );
            continue;
        }
        let rule = match Regex::new(&format!("(?i){}", custom.pattern)) {
            Ok(regex) => InputRule {
                regex,
                name: custom.name.clone(),
                message: custom.message.clone(),
                category: RuleCategory::CodeInjection,
                target_group: super::TargetGroup::None,
                safer_alternative: None,
            },
            Err(e) => {
                tracing::warn!(
                    rule = %custom.name,
                    pattern = %custom.pattern,
                    error = %e,
                    "skipping invalid custom confirm rule: regex failed to compile; \
                     this rule will NOT be enforced"
                );
                continue;
            }
        };
        if let Some(pos) = confirm_rules.iter().position(|r| r.name == rule.name) {
            confirm_rules[pos] = rule;
        } else {
            confirm_rules.push(rule);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_enabled_with_no_custom_rules() {
        let config = InputGuardConfig::default();
        assert!(config.enabled);
        assert!(config.custom_block_rules.is_empty());
        assert!(config.custom_confirm_rules.is_empty());
    }

    #[test]
    fn custom_rule_compiles_to_input_rule() {
        let custom = CustomRule {
            name: "test_rule".into(),
            pattern: r"dangerous_tool\s+--destroy".into(),
            message: "Test rule".into(),
        };
        let rule = custom
            .to_input_rule(RuleCategory::DestructiveCommand)
            .unwrap();
        assert!(rule.regex.is_match("dangerous_tool --destroy"));
    }

    #[test]
    fn invalid_regex_returns_none() {
        let custom = CustomRule {
            name: "bad".into(),
            pattern: "(".into(),
            message: "Bad regex".into(),
        };
        assert!(custom
            .to_input_rule(RuleCategory::DestructiveCommand)
            .is_none());
    }

    #[test]
    fn merge_appends_new_rules() {
        let mut block_rules = Vec::new();
        let mut confirm_rules = Vec::new();
        let config = InputGuardConfig {
            enabled: true,
            custom_block_rules: vec![CustomRule {
                name: "my_block".into(),
                pattern: "my_dangerous_cmd".into(),
                message: "Custom block".into(),
            }],
            custom_confirm_rules: vec![],
            max_analyzable_bytes: None,
        };

        merge_custom_rules(&mut block_rules, &mut confirm_rules, &config);

        assert_eq!(block_rules.len(), 1);
        assert_eq!(block_rules[0].name, "my_block");
    }

    #[test]
    fn merge_overrides_same_name() {
        let mut block_rules = vec![InputRule {
            regex: Regex::new(r"old_pattern").unwrap(),
            name: "my_rule".into(),
            message: "Old message".into(),
            category: RuleCategory::DestructiveCommand,
            target_group: crate::input_guard::TargetGroup::None,
            safer_alternative: None,
        }];
        let mut confirm_rules = Vec::new();
        let config = InputGuardConfig {
            enabled: true,
            custom_block_rules: vec![CustomRule {
                name: "my_rule".into(),
                pattern: "new_pattern".into(),
                message: "New message".into(),
            }],
            custom_confirm_rules: vec![],
            max_analyzable_bytes: None,
        };

        merge_custom_rules(&mut block_rules, &mut confirm_rules, &config);

        assert_eq!(block_rules.len(), 1);
        assert_eq!(block_rules[0].message, "New message");
    }

    // N6 regression: invalid regex in a custom rule must be skipped
    // (not added) and must not panic. The corresponding warn! log line
    // is exercised at runtime; here we just assert the rules vec stays
    // empty and the valid sibling rule still lands.
    #[test]
    fn merge_invalid_custom_block_rule_is_skipped() {
        let mut block_rules = Vec::new();
        let mut confirm_rules = Vec::new();
        let config = InputGuardConfig {
            enabled: true,
            custom_block_rules: vec![
                CustomRule {
                    name: "broken".into(),
                    pattern: "(".into(), // unbalanced paren — invalid regex
                    message: "Should be dropped".into(),
                },
                CustomRule {
                    name: "good".into(),
                    pattern: "definitely_malicious_tool".into(),
                    message: "Should land".into(),
                },
            ],
            custom_confirm_rules: vec![],
            max_analyzable_bytes: None,
        };

        merge_custom_rules(&mut block_rules, &mut confirm_rules, &config);

        assert_eq!(block_rules.len(), 1, "invalid rule must be skipped");
        assert_eq!(block_rules[0].name, "good");
        assert!(block_rules[0].regex.is_match("definitely_malicious_tool"));
    }

    #[test]
    fn merge_invalid_custom_confirm_rule_is_skipped() {
        let mut block_rules = Vec::new();
        let mut confirm_rules = Vec::new();
        let config = InputGuardConfig {
            enabled: true,
            custom_block_rules: vec![],
            custom_confirm_rules: vec![
                CustomRule {
                    name: "broken".into(),
                    pattern: ")(".into(), // invalid regex
                    message: "Should be dropped".into(),
                },
                CustomRule {
                    name: "good".into(),
                    pattern: "dangerous_eval".into(),
                    message: "Should land".into(),
                },
            ],
            max_analyzable_bytes: None,
        };

        merge_custom_rules(&mut block_rules, &mut confirm_rules, &config);

        assert_eq!(confirm_rules.len(), 1, "invalid rule must be skipped");
        assert_eq!(confirm_rules[0].name, "good");
    }

    // Regression: a CustomRule whose `pattern` is empty (usually a YAML
    // indent mistake where `pattern:` landed outside the list item) must
    // be skipped with a warn — NOT compiled as an empty regex that
    // matches nothing useful. Otherwise the user believes they're
    // protected while their custom rule is inert.
    #[test]
    fn merge_empty_pattern_custom_rule_is_skipped() {
        let mut block_rules = Vec::new();
        let mut confirm_rules = Vec::new();
        let config = InputGuardConfig {
            enabled: true,
            custom_block_rules: vec![
                CustomRule {
                    name: "missing_pattern".into(),
                    pattern: "".into(),
                    message: "Should be dropped".into(),
                },
                CustomRule {
                    name: "whitespace_only".into(),
                    pattern: "   ".into(),
                    message: "Should also be dropped".into(),
                },
                CustomRule {
                    name: "good".into(),
                    pattern: "pwd".into(),
                    message: "Should land".into(),
                },
            ],
            custom_confirm_rules: vec![],
            max_analyzable_bytes: None,
        };

        merge_custom_rules(&mut block_rules, &mut confirm_rules, &config);

        assert_eq!(
            block_rules.len(),
            1,
            "empty/whitespace pattern rules must be skipped"
        );
        assert_eq!(block_rules[0].name, "good");
        assert!(block_rules[0].regex.is_match("pwd"));
    }
}
