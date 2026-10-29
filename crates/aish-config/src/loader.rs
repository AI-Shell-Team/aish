use std::io::Write;
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
    /// (e.g. `enable_token_estimation`) show up in existing users' yaml
    /// without manual editing. User-set values and field ordering are
    /// preserved; only missing keys are appended. Legacy security keys
    /// that now live in `security_policy.yaml` are stripped on load.
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
            // Clean the output cap up before `merge_missing` backfills the
            // schema, so the value is judged as the user wrote it.
            let mut changed = false;
            if let Some(map) = value.as_mapping_mut() {
                let (cleaned, dropped_cap) = Self::clean_max_tokens(map);
                changed |= cleaned;
                if let Some(cap) = dropped_cap {
                    eprintln!(
                        "warning: removed `max_tokens: {cap}` from {} — a cap that small truncates answers: a reasoning model spends the whole budget thinking and the turn ends as \"incomplete response\". The request now carries no cap (Anthropic endpoints use their 64k fallback). To cap output deliberately, use {} or more.",
                        path.display(),
                        Self::MIN_MAX_TOKENS
                    );
                }
            }
            if Self::merge_missing(&mut value, default_value) {
                changed = true;
            }
            let stripped = Self::strip_legacy_security_keys(&mut value);
            if !stripped.is_empty() {
                changed = true;
                eprintln!(
                    "warning: removed legacy security keys from {}: [{}]. These settings now live in ~/.config/aish/security_policy.yaml — use /setting or edit that file.",
                    path.display(),
                    stripped.join(", ")
                );
            }
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
                //
                // The same crash-safe writer as every other config write is
                // used here on purpose: it carries the target's file mode onto
                // the replacement, so migrating a config the user chmod'ed to
                // 0600 cannot hand back a world-readable file with the API key
                // in it.
                let migrated = serde_yaml::to_string(&value).map_err(|e| {
                    AishError::Config(format!("failed to serialize migrated config: {e}"))
                })?;
                Self::write_atomic(&path, &migrated)?;
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

    /// Smallest output cap that is safe to keep in `config.yaml`.
    ///
    /// A smaller cap is removed, which makes the request omit `max_tokens` and
    /// lets the provider apply the model's own limit. Two reasons: older
    /// releases wrote their own default into the file (the Python shell's
    /// `1000`, the `v0.3.0`–`v0.3.6` wizard's `4096`), and any handwritten cap
    /// that small truncates a reasoning turn into an `IncompleteResponse`.
    ///
    /// The value is the lowest output ceiling among models in current use
    /// (GPT-4o: 16,384); the models seen most often are far above it (GLM-5.3
    /// 65,536, DeepSeek V4.1 64K, Claude 5.5 and GPT-5.1-Codex-Max 128K), so a
    /// cap at or above it is a deliberate, workable choice.
    pub const MIN_MAX_TOKENS: u32 = 16_384;

    /// Remove a top-level `max_tokens` that would truncate answers.
    ///
    /// Any value below [`Self::MIN_MAX_TOKENS`] is dropped, as are the "no
    /// cap" spellings `0` and `null` (leftover keys that never changed
    /// behaviour). A value at or above the threshold is a user choice and
    /// stays untouched.
    ///
    /// Returns whether the document changed and, when a cap that actually
    /// limited answers was dropped, its value so the caller can warn. A
    /// dropped `0`/`null` never capped anything and is reported as `None`.
    ///
    /// Only the top-level key is touched: `inline_completion.max_tokens` (512)
    /// and `context_auto_compact.summary_max_tokens` are separate budgets.
    fn clean_max_tokens(map: &mut serde_yaml::Mapping) -> (bool, Option<u32>) {
        let key = serde_yaml::Value::String("max_tokens".to_string());
        let Some(current) = map.get(&key).cloned() else {
            return (false, None);
        };
        // `0` and `null` already mean "no cap"; dropping the key only keeps
        // the file from carrying a dead setting.
        if current.is_null() || current.as_u64() == Some(0) {
            map.remove(&key);
            return (true, None);
        }
        let Some(cap) = current
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n < Self::MIN_MAX_TOKENS)
        else {
            return (false, None);
        };
        map.remove(&key);
        debug!(max_tokens = cap, "removed a max_tokens below the minimum");
        (true, Some(cap))
    }

    /// Remove security fields that used to live in `config.yaml` but are now
    /// owned exclusively by `security_policy.yaml` (issue #407).
    ///
    /// Returns the names of keys that were actually present and removed so
    /// callers can warn the user once.
    fn strip_legacy_security_keys(value: &mut serde_yaml::Value) -> Vec<String> {
        const LEGACY_KEYS: &[&str] = &[
            "enable_sandbox",
            "sandbox_off_action",
            "sandbox_timeout_seconds",
            "default_risk_level",
            "input_guard_enabled",
        ];
        let Some(map) = value.as_mapping_mut() else {
            return Vec::new();
        };
        let mut removed = Vec::new();
        for key in LEGACY_KEYS {
            if map
                .remove(serde_yaml::Value::String((*key).to_string()))
                .is_some()
            {
                removed.push((*key).to_string());
            }
        }
        removed
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
    ///
    /// Delegates to [`Self::save_atomic`], so the write is crash-safe (temp
    /// file + rename) rather than a truncating in-place write.
    pub fn save(config: &ConfigModel, path: &Path) -> Result<()> {
        Self::save_atomic(config, path)
    }

    /// Serialize-then-rename, so a crash never leaves a truncated config behind.
    ///
    /// The serialized YAML is written to a sibling temp file in the same
    /// directory (same filesystem, so `rename` is atomic), synced, and then
    /// renamed over `path`, whose permission bits are carried over first (unix;
    /// see [`Self::write_atomic`]). Missing parent directories are created
    /// first, and the temp file is removed if either the write or the rename
    /// fails.
    pub fn save_atomic(config: &ConfigModel, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(config)
            .map_err(|e| AishError::Config(format!("failed to serialize config: {e}")))?;
        Self::write_atomic(path, &yaml)
    }

    /// Edit the raw YAML document in place: parse the file (or an empty document
    /// when it does not exist), hand the mutable document to `edit`, then write it
    /// back atomically. Unknown keys and any structure the typed `ConfigModel`
    /// does not model are preserved — only the subtree `edit` touches changes.
    ///
    /// The file is only written when `edit` returns `Ok`; if `edit` fails the
    /// original file is left untouched byte-for-byte. An empty or non-existent
    /// file starts as an empty `Mapping` so the result is always valid YAML.
    ///
    /// The final write goes through [`Self::write_atomic`]: crash-safe (temp
    /// file + `rename` + `fsync`) and mode-preserving.
    ///
    /// Limitation: this is an *unlocked* read-modify-write. Two processes that
    /// load and edit the same file concurrently can lose one of the two
    /// updates (last writer wins); there is deliberately no lockfile, so
    /// nothing breaks when a writer is killed.
    pub fn update_raw<F>(path: &Path, edit: F) -> Result<()>
    where
        F: FnOnce(&mut serde_yaml::Value) -> Result<()>,
    {
        let mut document = if path.exists() {
            let raw = std::fs::read_to_string(path).map_err(|e| {
                AishError::Config(format!("failed to read {}: {e}", path.display()))
            })?;
            // An empty file (or one containing only `null`) parses to
            // `Value::Null`; normalize it to an empty mapping so edits can
            // insert keys without a special case.
            match serde_yaml::from_str::<serde_yaml::Value>(&raw).map_err(|e| {
                AishError::Config(format!("failed to parse {}: {e}", path.display()))
            })? {
                serde_yaml::Value::Null => serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                other => other,
            }
        } else {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        };

        edit(&mut document)?;

        let yaml = serde_yaml::to_string(&document).map_err(|e| {
            AishError::Config(format!("failed to serialize {}: {e}", path.display()))
        })?;
        Self::write_atomic(path, &yaml)
    }

    /// Write `contents` to `path` via a same-directory temp file + `rename`.
    ///
    /// Crash-safe: the temp file is flushed to stable storage (`sync_all`)
    /// before the `rename`, and the parent directory is synced after it, so a
    /// crash leaves either the complete old file or the complete new one.
    ///
    /// When `path` already exists its permissions are copied onto the temp file
    /// *before* the content is written, so a hand-`chmod 600` config (api_key,
    /// tokens) does not come back world-readable after an edit. This is unix
    /// mode-copying and best effort — a target whose metadata cannot be read
    /// keeps the default mode the fresh temp file got. A brand-new file keeps
    /// the usual `0666 & !umask`.
    ///
    /// Creates missing parent directories. The temp file is removed if the
    /// write or the rename fails, so a failed call never leaves debris behind.
    ///
    /// Limitation: the replacement itself is atomic, but this does not serialize
    /// concurrent writers — see [`Self::update_raw`] for the read-modify-write
    /// window that callers inherit.
    fn write_atomic(path: &Path, contents: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AishError::Config(format!(
                        "failed to create config directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }

        let tmp_path = Self::temp_path(path);
        let write = std::fs::File::create(&tmp_path).and_then(|mut file| {
            // The mode must be in place before any byte of content lands,
            // otherwise the temp file would sit world-readable with credentials
            // in it. An existing file keeps its own mode; a new one is created
            // owner-only, because a config carries the API key and header
            // values and must not depend on the process umask.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path)
                    .map(|metadata| metadata.permissions())
                    .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o600));
                // A failure here is fatal on purpose: without the mode in place
                // the temp file may be readable by other users, so no
                // credential is ever written into it.
                file.set_permissions(mode)?;
            }
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        });
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(AishError::Config(format!(
                "failed to write temp {}: {e}",
                tmp_path.display()
            )));
        }
        if let Err(e) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(AishError::Config(format!(
                "failed to atomically replace {}: {e}",
                path.display()
            )));
        }
        Self::sync_parent_directory(path);
        Ok(())
    }

    /// Best-effort `fsync` of `path`'s parent directory, so the `rename` that
    /// replaced `path` also survives a crash (the file's own data is already
    /// synced by [`Self::write_atomic`]).
    ///
    /// Failure is ignored on purpose: not every platform/filesystem allows a
    /// directory to be opened or synced, and durability is a bonus — it must
    /// not turn a completed write into an error.
    fn sync_parent_directory(path: &Path) {
        let Some(parent) = path.parent() else {
            return;
        };
        let directory = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if let Ok(handle) = std::fs::File::open(directory) {
            let _ = handle.sync_all();
        }
    }

    /// Build the sibling temp path used by [`Self::write_atomic`].
    ///
    /// Shape: `<file-name>.tmp-<pid>` (e.g. `config.yaml.tmp-4242`). Keeping it
    /// in the same directory as the target guarantees the final `rename` stays
    /// on one filesystem; the pid keeps concurrent processes from colliding.
    fn temp_path(path: &Path) -> PathBuf {
        let mut name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("config.yaml"));
        name.push(format!(".tmp-{}", std::process::id()));
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
            _ => PathBuf::from(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the on-disk file and return the top-level key's value.
    ///
    /// Nested budgets (`inline_completion.max_tokens`,
    /// `context_auto_compact.summary_max_tokens`) share the name, so a plain
    /// substring check would not prove anything about the top-level cap.
    fn top_level(path: &Path, key: &str) -> Option<serde_yaml::Value> {
        let raw = std::fs::read_to_string(path).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
        value
            .as_mapping()
            .and_then(|map| map.get(serde_yaml::Value::String(key.to_string())).cloned())
    }

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
        // enable_token_estimation wasn't in the yaml; default true should
        // have been both applied AND persisted.
        assert!(config.enable_token_estimation);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("model: glm-4.7"));
        assert!(on_disk.contains("api_key: secret"));
        assert!(on_disk.contains("enable_token_estimation: true"));
    }

    #[test]
    fn load_drops_a_max_tokens_below_the_minimum() {
        // Both values a released version ever wrote for the user are below the
        // minimum: the Python shell's pydantic 1000 and the v0.3.0-v0.3.6
        // wizard's 4096. (The 40960 floor of #619 never shipped — the commit
        // postdates v0.3.15 — so no released build produced it.) The check is
        // on the number, not on a schema version, so a file a previous build
        // already rewrote or one restored from a backup is cleaned too.
        for small in [1u32, 1000, 4096, 16_383] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.yaml");
            std::fs::write(&path, format!("model: glm-4.7\nmax_tokens: {small}\n")).unwrap();

            let config = ConfigLoader::load(Some(&path)).unwrap();
            assert_eq!(
                config.max_tokens, None,
                "max_tokens {small} is below the minimum and must become null"
            );
            assert_eq!(
                top_level(&path, "max_tokens"),
                None,
                "max_tokens {small} must be removed from disk"
            );
        }
    }

    #[test]
    fn load_rejects_a_negative_max_tokens_instead_of_dropping_it() {
        // A negative value does not fit `Option<u32>`, so it fails to
        // deserialize. The cleanup must not swallow it and must not rewrite
        // the file: a bad value stays visible instead of vanishing silently.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let original = "model: glm-4.7\nmax_tokens: -1\n";
        std::fs::write(&path, original).unwrap();

        assert!(
            ConfigLoader::load(Some(&path)).is_err(),
            "a negative max_tokens must be rejected"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "a rejected file must not be rewritten"
        );
    }

    #[test]
    fn load_keeps_a_max_tokens_at_or_above_the_minimum() {
        // 16,384 is the smallest output ceiling among models in use (GPT-4o),
        // so a cap there is workable and deliberate: it must survive together
        // with everything above it, up to the largest ceilings in use.
        for cap in [16_384u32, 20_000, 65_536, 131_072, 393_216] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.yaml");
            std::fs::write(&path, format!("model: glm-4.7\nmax_tokens: {cap}\n")).unwrap();

            let config = ConfigLoader::load(Some(&path)).unwrap();
            assert_eq!(
                config.max_tokens,
                Some(cap),
                "max_tokens {cap} is at or above the minimum and must survive"
            );
            assert_eq!(
                top_level(&path, "max_tokens"),
                Some(serde_yaml::Value::Number(serde_yaml::Number::from(
                    u64::from(cap)
                ))),
                "max_tokens {cap} must stay on disk"
            );
        }
    }

    #[test]
    fn load_never_touches_the_nested_token_budgets() {
        // `inline_completion.max_tokens` (512) and
        // `context_auto_compact.summary_max_tokens` are separate budgets that
        // are supposed to be small; the top-level minimum must not reach them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "model: glm-4.7\ninline_completion:\n  max_tokens: 512\ncontext_auto_compact:\n  summary_max_tokens: 1200\n",
        )
        .unwrap();

        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(config.inline_completion.max_tokens, 512);
        assert_eq!(config.context_auto_compact.summary_max_tokens, 1200);
    }

    #[test]
    fn load_leaves_an_unset_max_tokens_out_of_the_file() {
        // `None` means "no cap"; writing `max_tokens: null` back would only
        // re-introduce the key on every migration. A legacy file that spelled
        // the unset budget as an explicit null drops the key too, and so does
        // a leftover `0` — both were already "no cap", so neither warns.
        for yaml in [
            "model: glm-4.7\n",
            "model: glm-4.7\nmax_tokens: null\n",
            "model: glm-4.7\nmax_tokens: 0\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.yaml");
            std::fs::write(&path, yaml).unwrap();

            let config = ConfigLoader::load(Some(&path)).unwrap();
            assert_eq!(config.max_tokens, None);

            assert_eq!(
                top_level(&path, "max_tokens"),
                None,
                "an unset budget must not be materialized; got:\n{yaml}"
            );
        }
    }

    #[test]
    fn load_cleans_a_legacy_file_only_once() {
        // The cleanup is idempotent: after the first load removed the cap and
        // rewrote the file, a second load neither finds it again nor rewrites.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\nmax_tokens: 4096\n").unwrap();

        let first = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(first.max_tokens, None);
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(top_level(&path, "max_tokens"), None);

        let second = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(second.max_tokens, None);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            migrated,
            "a cleaned file must not be rewritten again"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_migration_keeps_the_file_mode() {
        // The migration rewrite replaces the file, so it must go through the
        // same mode-preserving writer as every other config write: a 0600
        // config holds the API key and must not come back world-readable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\napi_key: secret\n").unwrap();
        chmod(&path, 0o600);

        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(config.model, "glm-4.7");
        assert_eq!(mode_of(&path), 0o600, "migration must keep mode 600");
        assert!(leftover_tmp_files(dir.path()).is_empty());
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
        assert!(on_disk.contains("enable_token_estimation:"));
    }

    #[test]
    fn load_preserves_user_overrides_during_migration() {
        // User explicitly set auto_suggest: true (default is false).
        // Migration must NOT overwrite it with the default.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\nauto_suggest: true\n").unwrap();

        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert!(config.auto_suggest);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        // Should still be true, not overwritten with false.
        assert!(on_disk.contains("auto_suggest: true"));
        // Other missing fields should have been added.
        assert!(on_disk.contains("api_key:"));
    }

    #[test]
    fn load_strips_legacy_security_keys() {
        // Security settings moved to security_policy.yaml (#407). Old keys
        // left in config.yaml must be removed on load so they stop confusing
        // users / docs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "model: glm-4.7\nenable_sandbox: true\ninput_guard_enabled: false\nsandbox_off_action: block\n",
        )
        .unwrap();

        let _config = ConfigLoader::load(Some(&path)).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("model: glm-4.7"));
        assert!(
            !on_disk.contains("enable_sandbox:"),
            "legacy enable_sandbox must be stripped; got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("input_guard_enabled:"),
            "legacy input_guard_enabled must be stripped; got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("sandbox_off_action:"),
            "legacy sandbox_off_action must be stripped; got:\n{on_disk}"
        );
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
        assert!(config.enable_token_estimation);
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
    #[test]
    fn save_creates_missing_parent_directory() {
        // Regression for first-run setup wizard: when ~/.config/aish/ does
        // not yet exist, ConfigLoader::save must create it rather than fail
        // with ENOENT/EACCES. The setup wizard previously called
        // std::fs::write directly and hit this bug.
        let dir = tempfile::tempdir().unwrap();
        // Parent dir does not exist yet — the nested aish/ folder.
        let path = dir.path().join("aish").join("config.yaml");
        assert!(!path.parent().unwrap().exists());

        let config = ConfigModel::default();
        ConfigLoader::save(&config, &path).unwrap();

        assert!(path.exists(), "config file should have been created");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("model:"));
    }

    /// Names of leftover temp files (`*.tmp*`) in `dir`, if any.
    fn leftover_tmp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                if name.contains(".tmp") {
                    Some(name)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn update_raw_only_touches_edited_subtree() {
        // A user config with an unknown extension key and a nested object.
        // Editing the `mcp` subtree must leave everything else byte-identical
        // in value terms — unknown keys are the whole reason this API exists.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "model: glm-4.7\n\
             api_key: secret\n\
             my_custom_extension: 7\n\
             nested_thing:\n\
             \x20 inner: keep\n\
             \x20 other: [a, b]\n\
             mcp:\n\
             \x20 enabled: true\n\
             \x20 servers:\n\
             \x20 - name: old\n\
             \x20   transport: stdio\n\
             \x20   command: old-cmd\n",
        )
        .unwrap();

        ConfigLoader::update_raw(&path, |doc| {
            let root = doc.as_mapping_mut().expect("root mapping");
            let mut server = serde_yaml::Mapping::new();
            server.insert("name".into(), serde_yaml::Value::String("new".into()));
            server.insert(
                "transport".into(),
                serde_yaml::Value::String("stdio".into()),
            );
            server.insert(
                "command".into(),
                serde_yaml::Value::String("new-cmd".into()),
            );
            let mut mcp = serde_yaml::Mapping::new();
            mcp.insert("enabled".into(), serde_yaml::Value::Bool(true));
            mcp.insert(
                "servers".into(),
                serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(server)]),
            );
            root.insert("mcp".into(), serde_yaml::Value::Mapping(mcp));
            Ok(())
        })
        .unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("model: glm-4.7"), "got:\n{on_disk}");
        assert!(on_disk.contains("api_key: secret"), "got:\n{on_disk}");
        assert!(
            on_disk.contains("my_custom_extension: 7"),
            "unknown key must survive update_raw; got:\n{on_disk}"
        );
        assert!(
            on_disk.contains("nested_thing:"),
            "unrelated subtree must survive; got:\n{on_disk}"
        );
        assert!(on_disk.contains("inner: keep"), "got:\n{on_disk}");
        assert!(
            on_disk.contains("new-cmd"),
            "mcp subtree must be new; got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("old-cmd"),
            "old mcp subtree must be gone; got:\n{on_disk}"
        );

        // And the resulting document still loads as a typed ConfigModel.
        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(config.model, "glm-4.7");
        assert!(config.mcp.enabled);
        assert_eq!(config.mcp.servers.len(), 1);
        assert_eq!(config.mcp.servers[0].name, "new");
    }

    #[test]
    fn update_raw_leaves_file_untouched_when_edit_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let original = "model: glm-4.7\nmy_custom_extension: 7\n";
        std::fs::write(&path, original).unwrap();

        let err =
            ConfigLoader::update_raw(&path, |_doc| Err(AishError::Config("edit refused".into())))
                .unwrap_err();
        assert!(err.to_string().contains("edit refused"), "got: {err}");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(leftover_tmp_files(dir.path()).is_empty());
    }

    #[test]
    fn save_atomic_round_trips_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        // Nested, not-yet-existing parent: save_atomic must create it.
        let path = dir.path().join("aish").join("config.yaml");

        let mut config = ConfigModel::default();
        config.model = "glm-4.7".into();
        config.api_key = "secret".into();
        ConfigLoader::save_atomic(&config, &path).unwrap();

        let loaded = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(loaded.model, "glm-4.7");
        assert_eq!(loaded.api_key, "secret");

        assert!(
            leftover_tmp_files(&path.parent().unwrap()).is_empty(),
            "no *.tmp* files may remain: {:?}",
            leftover_tmp_files(&path.parent().unwrap())
        );
    }

    #[test]
    fn update_raw_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aish").join("config.yaml");
        assert!(!path.exists());

        ConfigLoader::update_raw(&path, |doc| {
            let root = doc.as_mapping_mut().expect("root mapping");
            root.insert("model".into(), serde_yaml::Value::String("glm-4.7".into()));
            Ok(())
        })
        .unwrap();

        assert!(path.exists());
        let config = ConfigLoader::load(Some(&path)).unwrap();
        assert_eq!(config.model, "glm-4.7");
        assert!(leftover_tmp_files(&path.parent().unwrap()).is_empty());
    }

    /// The permission bits of `path` (unix).
    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn update_raw_preserves_the_file_mode() {
        // config.yaml holds api_key / MCP tokens, so a user who ran
        // `chmod 600 config.yaml` must not get it back as world-readable after
        // any `/mcp` (or other) raw edit: the temp file has to inherit the
        // target's mode before the rename.
        //
        // 0o600 and 0o640 are both checked because they cover every umask: a
        // freshly created temp file (`0666 & !umask`) can equal at most one of
        // them, so this cannot pass by accident.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        for mode in [0o600, 0o640] {
            std::fs::write(&path, "model: glm-4.7\napi_key: secret\n").unwrap();
            chmod(&path, mode);

            ConfigLoader::update_raw(&path, |document| {
                document
                    .as_mapping_mut()
                    .expect("root mapping")
                    .insert("theme".into(), serde_yaml::Value::String("dark".into()));
                Ok(())
            })
            .unwrap();

            assert_eq!(mode_of(&path), mode, "update_raw must keep mode {mode:o}");
            assert!(std::fs::read_to_string(&path)
                .unwrap()
                .contains("theme: dark"));
            assert!(leftover_tmp_files(dir.path()).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn save_atomic_preserves_the_file_mode() {
        // Same guarantee through the other public entry point (`save` ->
        // `save_atomic` -> `write_atomic`).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: glm-4.7\napi_key: secret\n").unwrap();
        chmod(&path, 0o600);

        let mut config = ConfigModel::default();
        config.model = "glm-4.7".into();
        ConfigLoader::save_atomic(&config, &path).unwrap();

        assert_eq!(mode_of(&path), 0o600, "save_atomic must keep the mode");
        assert!(leftover_tmp_files(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_config_file_is_owner_only() {
        // A config carries the API key and header values, so a file created by
        // aish must not depend on the process umask for its mode.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        let mut config = ConfigModel::default();
        config.model = "glm-4.7".into();
        ConfigLoader::save_atomic(&config, &path).unwrap();

        assert_eq!(
            mode_of(&path),
            0o600,
            "a new config must be readable by its owner only"
        );
        assert!(leftover_tmp_files(dir.path()).is_empty());
    }
}
