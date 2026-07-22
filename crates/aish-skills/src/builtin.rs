//! Compile-time embedded packaged skills.
//!
//! The repo `skills/` tree is baked into the binary via [`include_dir`]. At
//! runtime the tree is materialized once into a versioned cache directory so
//! skill scripts remain executable on a real filesystem path. User skills in
//! `~/.config/aish/skills` still shadow these by name.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use include_dir::{include_dir, Dir};

/// Distinguishes concurrent `ensure_materialized_at` calls in one process
/// (threads share `process::id()`).
static STAGING_SEQ: AtomicU64 = AtomicU64::new(1);

/// Packaged skills from the repository `skills/` directory.
static BUILTIN_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../skills");

/// Top-level directory names in the embedded `skills/` tree.
pub fn embedded_skill_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = HashSet::new();
        for entry in BUILTIN_SKILLS.entries() {
            let Some(dir) = entry.as_dir() else {
                continue;
            };
            // Paths on nested Dir entries are relative to the embed root; look up
            // SKILL.md via the root Dir (child Dir::get_file is unreliable here).
            let skill_md = dir.path().join("SKILL.md");
            if BUILTIN_SKILLS.get_file(&skill_md).is_none() {
                continue;
            }
            if let Some(name) = dir.path().file_name() {
                names.insert(name.to_string_lossy().into_owned());
            }
        }
        names
    })
}

/// Serialize tests that mutate process-global skill-related env vars.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Versioned cache root for materialized builtin skills.
///
/// Override with `AISH_BUILTIN_SKILLS_CACHE` (tests / special installs).
pub fn cache_root() -> PathBuf {
    if let Ok(override_dir) = std::env::var("AISH_BUILTIN_SKILLS_CACHE") {
        if !override_dir.is_empty() {
            return PathBuf::from(override_dir);
        }
    }
    let version = env!("CARGO_PKG_VERSION");
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("aish").join("builtin-skills").join(version)
}

/// Ensure embedded skills exist on disk under [`cache_root`].
pub fn ensure_materialized() -> aish_core::Result<PathBuf> {
    ensure_materialized_at(&cache_root())
}

/// Ensure embedded skills exist on disk under `root`.
///
/// Idempotent: if a completion marker is present, returns the existing root.
/// Uses a per-process staging directory so concurrent launches do not clobber
/// each other's extract; the completion marker is written into staging before
/// `rename`, so a successful publish is atomic. On `rename` loss, this process
/// drops its staging and treats an existing marker as success.
pub fn ensure_materialized_at(root: &Path) -> aish_core::Result<PathBuf> {
    let marker = root.join(".complete");
    if marker.is_file() {
        return Ok(root.to_path_buf());
    }

    // Leftover incomplete tree from a crashed older attempt (no marker). Best
    // effort only — if another process wins the publish race, rename handling
    // below recovers via the marker.
    if root.exists() && !marker.is_file() {
        let _ = fs::remove_dir_all(root);
    }

    // Unique staging per attempt (pid alone is shared by threads in one process).
    // Avoid Path::with_extension — version dirs like "0.3.8" become "0.3.staging".
    let staging_suffix = format!(
        "staging.{}.{}",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let staging = root
        .parent()
        .map(|parent| {
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skills".to_string());
            parent.join(format!("{name}.{staging_suffix}"))
        })
        .unwrap_or_else(|| PathBuf::from(format!("{}.{staging_suffix}", root.display())));

    fs::create_dir_all(&staging).map_err(|e| {
        aish_core::AishError::Skill(format!(
            "Failed to create builtin skills staging {}: {}",
            staging.display(),
            e
        ))
    })?;

    BUILTIN_SKILLS.extract(&staging).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        aish_core::AishError::Skill(format!(
            "Failed to extract embedded skills to {}: {}",
            staging.display(),
            e
        ))
    })?;

    ensure_scripts_executable(&staging);

    // Marker inside staging → rename publishes tree + completion atomically.
    let staging_marker = staging.join(".complete");
    if let Err(e) = fs::write(&staging_marker, env!("CARGO_PKG_VERSION").as_bytes()) {
        let _ = fs::remove_dir_all(&staging);
        return Err(aish_core::AishError::Skill(format!(
            "Failed to write builtin skills marker {}: {}",
            staging_marker.display(),
            e
        )));
    }

    if let Err(e) = fs::rename(&staging, root) {
        let _ = fs::remove_dir_all(&staging);
        // Another process already published (or is finishing) — prefer their tree.
        if marker.is_file() {
            return Ok(root.to_path_buf());
        }
        return Err(aish_core::AishError::Skill(format!(
            "Failed to publish builtin skills cache {} -> {}: {}",
            staging.display(),
            root.display(),
            e
        )));
    }

    tracing::debug!(
        path = %root.display(),
        "Materialized embedded builtin skills"
    );
    Ok(root.to_path_buf())
}

fn ensure_scripts_executable(root: &Path) {
    let Ok(skills) = fs::read_dir(root) else {
        return;
    };
    for entry in skills.flatten() {
        let scripts = entry.path().join("scripts");
        if !scripts.is_dir() {
            continue;
        }
        chmod_tree_executable(&scripts);
    }
}

fn chmod_tree_executable(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            chmod_tree_executable(&path);
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o111);
                let _ = fs::set_permissions(&path, perms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skills_contain_skill_md() {
        assert!(
            BUILTIN_SKILLS
                .get_file("diagnose_system_lag/SKILL.md")
                .is_some(),
            "embedded skills tree must include diagnose_system_lag/SKILL.md"
        );
        assert!(
            embedded_skill_names().contains("diagnose_system_lag"),
            "diagnose_system_lag must appear in embedded_skill_names()"
        );
    }

    #[test]
    fn materialize_writes_cache_with_diagnose_skill() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Use a subdirectory so we never remove_dir_all the TempDir root itself.
        let cache = dir.path().join("cache");
        let root = ensure_materialized_at(&cache).expect("materialize builtin skills");
        let skill = root.join("diagnose_system_lag").join("SKILL.md");
        assert!(
            skill.is_file(),
            "expected {} after materialize",
            skill.display()
        );
        assert!(root.join(".complete").is_file());
        let root2 = ensure_materialized_at(&cache).expect("rematerialize");
        assert_eq!(root, root2);
    }

    #[test]
    fn concurrent_materialize_shares_one_complete_cache() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = dir.path().join("cache");
        let cache1 = cache.clone();
        let cache2 = cache.clone();
        let t1 = std::thread::spawn(move || ensure_materialized_at(&cache1));
        let t2 = std::thread::spawn(move || ensure_materialized_at(&cache2));
        let r1 = t1.join().expect("thread1").expect("materialize1");
        let r2 = t2.join().expect("thread2").expect("materialize2");
        assert_eq!(r1, r2);
        assert!(r1.join(".complete").is_file());
        assert!(r1.join("diagnose_system_lag").join("SKILL.md").is_file());
        // Only the published cache dir should remain — no leaked shared staging.
        let parent = cache.parent().expect("parent");
        let leftovers: Vec<_> = fs::read_dir(parent)
            .expect("read parent")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("staging"))
            .collect();
        assert!(leftovers.is_empty(), "leaked staging dirs: {leftovers:?}");
    }
}
