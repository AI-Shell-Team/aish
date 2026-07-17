//! Product-bundled skills embedded in the binary.
//!
//! Packaged skills ship **inside the binary** via [`rust_embed`], then are
//! materialized into an app-managed cache directory for script/path access.
//! They are never owned by `~/.config/aish/skills` unless the user copies them.

use std::fs;
use std::path::PathBuf;

use rust_embed::Embed;

/// Skills tree from the aish repo (`aish/skills/`), embedded at compile time.
#[derive(Embed)]
#[folder = "../../skills/"]
#[exclude = "**/.gitignore"]
struct BundledSkills;

/// Return the Builtin skills root to scan.
///
/// Resolution order:
/// 1. `AISH_BUILTIN_SKILLS_DIR` — tests / explicit override
/// 2. App cache: `$XDG_CACHE_HOME/aish/bundled-skills/<version>/` (materialized embed)
pub fn bundled_skills_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("AISH_BUILTIN_SKILLS_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
        tracing::warn!(
            "AISH_BUILTIN_SKILLS_DIR={:?} is not a directory; falling back to embedded skills",
            path
        );
    }

    materialize_bundled_skills().ok()
}

/// Extract embedded skills into the versioned cache dir (idempotent).
fn materialize_bundled_skills() -> aish_core::Result<PathBuf> {
    let cache_root = dirs::cache_dir().ok_or_else(|| {
        aish_core::AishError::Skill("cannot resolve cache directory for bundled skills".into())
    })?;
    let dest = cache_root
        .join("aish")
        .join("bundled-skills")
        .join(env!("CARGO_PKG_VERSION"));
    let marker = dest.join(".complete");

    if marker.is_file() && dest.is_dir() {
        return Ok(dest);
    }

    // Incomplete extract from a previous crash — start clean.
    if dest.exists() {
        let _ = fs::remove_dir_all(&dest);
    }
    fs::create_dir_all(&dest).map_err(|e| {
        aish_core::AishError::Skill(format!(
            "failed to create bundled skills cache {}: {}",
            dest.display(),
            e
        ))
    })?;

    for file in BundledSkills::iter() {
        let rel = file.as_ref();
        let Some(data) = BundledSkills::get(rel) else {
            continue;
        };
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                aish_core::AishError::Skill(format!("failed to create {}: {}", parent.display(), e))
            })?;
        }
        fs::write(&path, data.data.as_ref()).map_err(|e| {
            aish_core::AishError::Skill(format!("failed to write {}: {}", path.display(), e))
        })?;
    }

    fs::write(&marker, env!("CARGO_PKG_VERSION").as_bytes()).map_err(|e| {
        aish_core::AishError::Skill(format!(
            "failed to write bundled skills marker {}: {}",
            marker.display(),
            e
        ))
    })?;

    tracing::debug!("materialized bundled skills at {}", dest.display());
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_contains_diagnose_system_lag() {
        assert!(
            BundledSkills::get("diagnose_system_lag/SKILL.md").is_some(),
            "repo skills/ must be embedded"
        );
    }

    #[test]
    fn materialize_writes_skill_md() {
        let root = materialize_bundled_skills().expect("materialize");
        assert!(root.join("diagnose_system_lag/SKILL.md").is_file());
        assert!(root.join(".complete").is_file());
        assert!(root.components().any(|c| c.as_os_str() == "bundled-skills"));
    }
}
