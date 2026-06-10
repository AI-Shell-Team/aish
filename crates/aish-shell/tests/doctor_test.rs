#[cfg(test)]
mod tests {
    use aish_shell::doctor::{
        ApiKeyChecker, Checker, ConfigChecker, DirsChecker, ExternalToolsChecker,
    };

    #[test]
    fn test_dirs_checker_creates_expected_items() {
        let checker = DirsChecker::new();
        let results = checker.check();
        assert!(!results.is_empty());
        let result = &results[0];
        assert_eq!(result.checker, "Directory Structure");
        assert!(!result.items.is_empty());
    }

    #[test]
    fn test_apikey_checker_checks_env_vars() {
        let checker = ApiKeyChecker::new();
        let results = checker.check();
        assert!(!results.is_empty());
        let result = &results[0];
        assert_eq!(result.checker, "API Keys");
    }

    #[test]
    fn test_config_checker_returns_result() {
        let checker = ConfigChecker::new();
        let results = checker.check();
        assert!(!results.is_empty());
        let result = &results[0];
        assert_eq!(result.checker, "Configuration");
    }

    #[test]
    fn test_external_tools_checker_returns_result() {
        let checker = ExternalToolsChecker::new();
        let results = checker.check();
        assert!(!results.is_empty());
        let result = &results[0];
        assert_eq!(result.checker, "External Tools");
        assert!(!result.items.is_empty());
    }

    #[test]
    fn test_pass_items_are_not_fixable() {
        let checker = DirsChecker::new();
        let results = checker.check();
        for result in results {
            for item in result.items {
                if item.status == aish_shell::doctor::CheckStatus::Pass {
                    assert!(
                        !item.fixable,
                        "Pass items should not be fixable: {}",
                        item.message
                    );
                }
            }
        }
    }
}
