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
    ///
    /// If the file exists, any fields missing relative to the current
    /// `ConfigModel` schema are auto-inserted with their default values
    /// and the file is rewritten in place. This lets new config options
    /// (e.g. `input_guard_enabled`) show up in existing users' yaml
    /// without manual editing. User-set values and field ordering are
    /// preserved; only missing keys are appended.
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
            // Parse as a generic Value first so we can detect missing
            // fields and migrate the file in place before deserializing
            // into the typed model.
            let mut value: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|e| {
                AishError::Config(format!("failed to parse {}: {e}", path.display()))
            })?;
            // `ConfigModel::default()` + serialization is non-trivial; cache
            // it so repeated loads (e.g. PTY path in the same process) don't
            // rebuild the same default Value on every invocation.
            static DEFAULT_VALUE: std::sync::OnceLock<serde_yaml::Value> =
                std::sync::OnceLock::new();
            let default_value = DEFAULT_VALUE.get_or_init(|| {
                serde_yaml::to_value(ConfigModel::default())
                    .expect("ConfigModel::default() must serialize")
            });
            let changed = Self::merge_missing(&mut value, default_value);
            // Deserialize BEFORE writing — if the merged Value still
            // fails to deserialize (e.g. user has a field with an
            // invalid type that migration didn't touch), surface the
            // error without having already rewritten the user's file.
            let config: ConfigModel = serde_yaml::from_value(value.clone()).map_err(|e| {
                AishError::Config(format!("failed to parse {}: {e}", path.display()))
            })?;
            if changed {
                // Persist the merged Value (not the typed ConfigModel)
                // so unknown keys / user-extension fields / comments are
                // preserved — migration is additive, not lossy.
                let migrated = serde_yaml::to_string(&value).map_err(|e| {
                    AishError::Config(format!("failed to serialize migrated config: {e}"))
                })?;
                // Atomic replace: write to a temp file in the same
                // directory then rename, so a crash mid-write cannot
                // corrupt config.yaml.
                let tmp_path = path.with_extension("yaml.tmp");
                std::fs::write(&tmp_path, &migrated).map_err(|e| {
                    AishError::Config(format!(
                        "failed to write migrated temp {}: {e}",
                        tmp_path.display()
                    ))
                })?;
                std::fs::rename(&tmp_path, &path).map_err(|e| {
                    AishError::Config(format!(
                        "failed to atomically replace migrated {}: {e}",
                        path.display()
                    ))
                })?;
                debug!(path = %path.display(), "migrated config: added missing fields");
            }
            config
        } else {
            debug!("config file not found, using defaults");
            ConfigModel::default()
        };

        Self::apply_env_overrides(&mut config);
        Ok(config)
    }

    /// Walk two Values in parallel. For mapping nodes, any key present in
    /// `default` but missing from `target` is inserted with its default
    /// value. Returns true if any key was inserted (caller should persist).
    ///
    /// Edge case: if the user wrote `key:` with no value (parsed as Null)
    /// but the schema default is a nested mapping, replace the Null with
    /// the default mapping. Otherwise the Null would block recursion and
    /// the nested fields would never get migrated.
    fn merge_missing(target: &mut serde_yaml::Value, default: &serde_yaml::Value) -> bool {
        if target.is_null() && default.is_mapping() {
            *target = default.clone();
            return true;
        }
        let (target_map, default_map) = match (target.as_mapping_mut(), default.as_mapping()) {
            (Some(t), Some(d)) => (t, d),
            _ => return false,
        };
        let mut changed = false;
        for (key, value) in default_map {
            if !target_map.contains_key(key) {
                target_map.insert(key.clone(), value.clone());
                changed = true;
            } else if let Some(target_inner) = target_map.get_mut(key) {
                if Self::merge_missing(target_inner, value) {
                    changed = true;
                }
            }
        }
        changed
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
    fn load_migrates_missing_fields_into_existing_yaml() {
        // Minimal user yaml with only a few fields set. After load,
        // the file on disk should contain ALL ConfigModel fields
        // (default values for the missing ones), while preserving
        // the user-set values.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\napi_key: secret\ntheme: dark\n").unwrap();

        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(config.model, "glm-4.7");
        assert_eq!(config.api_key, "secret");
        // input_guard_enabled wasn't in the yaml; default true should
        // have been both applied AND persisted.
        assert!(config.input_guard_enabled);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("model: glm-4.7"));
        assert!(on_disk.contains("api_key: secret"));
        assert!(on_disk.contains("input_guard_enabled: true"));
    }

    #[test]
    fn load_preserves_unknown_keys_during_migration() {
        // User has a field the schema doesn't know about (forward-compat
        // or extension key). Migration must NOT drop it — the rewrite
        // should be additive on the merged Value, not a typed re-serialize.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\nfuture_extension_key: keep_me\n").unwrap();

        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(config.model, "glm-4.7");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("future_extension_key: keep_me"),
            "unknown user-extension keys must survive migration; got:\n{on_disk}"
        );
        // New schema key also added.
        assert!(on_disk.contains("input_guard_enabled:"));
    }

    #[test]
    fn load_preserves_user_overrides_during_migration() {
        // User explicitly set input_guard_enabled: false. Migration
        // must NOT overwrite it with the default true.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\ninput_guard_enabled: false\n").unwrap();

        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert!(!config.input_guard_enabled);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        // Should still be false, not overwritten with true.
        assert!(on_disk.contains("input_guard_enabled: false"));
        // Other missing fields should have been added.
        assert!(on_disk.contains("api_key:"));
    }

    #[test]
    fn load_skips_rewrite_when_nothing_missing() {
        // A complete yaml shouldn't be rewritten. We detect this by
        // checking the file mtime doesn't change.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let full_yaml = serde_yaml::to_string(&ConfigModel::default()).unwrap();
        std::fs::write(&path, &full_yaml).unwrap();
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

        // Touch mtime reference point by sleeping briefly.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = ConfigLoader::load(Some(&path)).unwrap();

        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "file should not be rewritten when no fields are missing"
        );
    }

    #[test]
    fn load_creates_no_file_when_missing() {
        // If config.yaml doesn't exist, load returns defaults and
        // does NOT create the file (creation is the wizard's job).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");
        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert!(config.input_guard_enabled);
        assert!(!path.exists());
    }

    #[test]
    fn load_does_not_persist_when_deserialize_fails_after_migration() {
        // Regression for round-3 review issue #1: migrate-then-write
        // ordering. If deserialize fails (e.g. user has `model: 123`
        // which is wrong type but migration didn't touch it), the
        // file must NOT be rewritten.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let original = "model: 123\napi_key: secret\n";
        std::fs::write(&path, original).unwrap();

        let result = ConfigLoader::load(Some(&path));
        assert!(result.is_err(), "expected deserialize error");

        // File on disk must be unchanged.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, original);
    }

    #[test]
    fn load_migrates_nested_null_into_default_mapping() {
        // Regression for round-3 review issue #2: user wrote
        // `tool_arg_preview:` with no value (Null). Without the
        // null-handling fix, merge_missing would skip this key and
        // the nested fields (max_lines, max_chars, max_items) would
        // never appear in the migrated yaml.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\ntool_arg_preview:\n").unwrap();

        let config = ConfigLoader::load(Some(&path)).unwrap();
        // Defaults should have been filled in via #[serde(default)].
        assert_eq!(config.tool_arg_preview.max_lines, 5);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        // The Null should have been replaced with the default mapping.
        assert!(on_disk.contains("max_lines:"));
        assert!(on_disk.contains("max_chars:"));
    }

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
