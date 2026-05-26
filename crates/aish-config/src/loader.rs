use std::path::{Path, PathBuf};

use aish_core::{AishError, Result};
use tracing::debug;

use crate::model::ConfigModel;

/// Configuration loader with environment variable override support.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load config from the given file, falling back to the default path.
    ///
    /// If the file does not exist a default config is returned (with env
    /// overrides applied).
    pub fn load(config_path: Option<&Path>) -> Result<ConfigModel> {
        let path = match config_path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path(),
        };

        let mut config = if path.exists() {
            debug!(path = %path.display(), "loading config file");
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                AishError::Config(format!("failed to read {}: {e}", path.display()))
            })?;
            serde_yaml::from_str(&raw).map_err(|e| {
                AishError::Config(format!("failed to parse {}: {e}", path.display()))
            })?
        } else {
            debug!("config file not found, using defaults");
            ConfigModel::default()
        };

        Self::apply_env_overrides(&mut config);
        Ok(config)
    }

    /// Return the default config path following XDG conventions.
    ///
    /// Priority:
    /// 1. `$AISH_CONFIG_DIR/config.yaml`
    /// 2. `$XDG_CONFIG_HOME/aish/config.yaml`
    /// 3. `~/.config/aish/config.yaml`
    pub fn default_config_path() -> PathBuf {
        if let Ok(dir) = std::env::var("AISH_CONFIG_DIR") {
            return PathBuf::from(dir).join("config.yaml");
        }

        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aish")
            .join("config.yaml")
    }

    /// Apply environment variable overrides on top of the loaded config.
    pub fn apply_env_overrides(config: &mut ConfigModel) {
        if let Ok(v) = std::env::var("AISH_MODEL") {
            debug!("override model from env");
            config.model = v;
        }
        if let Ok(v) = std::env::var("AISH_API_KEY") {
            debug!("override api_key from env");
            config.api_key = v;
        }
        if let Ok(v) = std::env::var("AISH_API_BASE") {
            debug!("override api_base from env");
            config.api_base = v;
        }
        if let Ok(v) = std::env::var("AISH_CODEX_AUTH_PATH") {
            debug!("override codex_auth_path from env");
            config.codex_auth_path = Some(v);
        }
    }

    /// Persist config to a YAML file.
    pub fn save(config: &ConfigModel, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AishError::Config(format!(
                    "failed to create config directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let yaml = serde_yaml::to_string(config)
            .map_err(|e| AishError::Config(format!("failed to serialize config: {e}")))?;

        std::fs::write(path, yaml)
            .map_err(|e| AishError::Config(format!("failed to write {}: {e}", path.display())))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_env_overrides_codex_auth_path() {
        // Save previous value to restore later (env vars are process-global).
        let prev = std::env::var("AISH_CODEX_AUTH_PATH").ok();

        // Test 1: env var set → overrides config
        std::env::set_var("AISH_CODEX_AUTH_PATH", "/tmp/test_auth.json");
        let mut config = ConfigModel::default();
        ConfigLoader::apply_env_overrides(&mut config);
        assert_eq!(
            config.codex_auth_path.as_deref(),
            Some("/tmp/test_auth.json")
        );

        // Test 2: env var unset → no change
        std::env::remove_var("AISH_CODEX_AUTH_PATH");
        let mut config2 = ConfigModel::default();
        let before = config2.codex_auth_path.clone();
        ConfigLoader::apply_env_overrides(&mut config2);
        assert_eq!(config2.codex_auth_path, before);

        // Restore original value
        match &prev {
            Some(v) => std::env::set_var("AISH_CODEX_AUTH_PATH", v),
            None => std::env::remove_var("AISH_CODEX_AUTH_PATH"),
        }
    }
}
