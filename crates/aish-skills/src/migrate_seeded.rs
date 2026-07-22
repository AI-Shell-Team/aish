//! One-shot migration of pre-embed install-seeded skills.
//!
//! # Deprecated (temporary)
//!
//! Older installers copied packaged skills into `~/.config/aish/skills/<name>/`,
//! which now shadows compile-time embedded builtins. This module moves those
//! same-named trees aside once, then leaves a marker so it never runs again.
//!
//! **Remove this entire module** (and the call in `SkillManager::load_all_skills`)
//! in a future release when leftover seeds are uncommon. Reminder lives in
//! `CHANGELOG.md` under `[Unreleased]` → Deprecated / Notes for releasers.
//! Removal version is intentionally not pinned.

use std::fs;
use std::path::{Path, PathBuf};

/// Written under the aish config dir after a successful migration attempt.
pub const MIGRATION_MARKER: &str = ".skills-seed-migrated-v1";

/// Backup location relative to the aish config dir (sibling of `skills/`).
pub const BACKUP_DIR_NAME: &str = "migrated-seeded-skills";

/// User-visible summary when at least one legacy seeded skill was moved.
#[derive(Debug, Clone)]
pub struct SeedMigrationNotice {
    pub moved: Vec<String>,
    pub backup_dir: PathBuf,
}

impl SeedMigrationNotice {
    /// One-line (or short) message for the terminal on the session that migrated.
    pub fn user_message(&self) -> String {
        let names = self.moved.join(", ");
        format!(
            "aish: moved {} legacy install-seeded skill(s) ({names}) to {} so embedded builtins can load. Restore from that backup if you customized them.",
            self.moved.len(),
            self.backup_dir.display()
        )
    }
}

fn config_aish_dir() -> Option<PathBuf> {
    if let Ok(config_dir) = std::env::var("AISH_CONFIG_DIR") {
        if !config_dir.is_empty() {
            return Some(PathBuf::from(config_dir));
        }
    }
    dirs::home_dir().map(|home| home.join(".config").join("aish"))
}

/// Move leftover install-seeded packaged skills out of the user skills dir.
///
/// Idempotent via [`MIGRATION_MARKER`]. Failures are logged and do not abort
/// skill loading; the marker is only written when the pass finishes without
/// move errors (so a partial failure can retry).
///
/// Returns [`SeedMigrationNotice`] when one or more skills were moved, so the
/// interactive shell can print a one-shot user-visible tip.
#[deprecated(
    note = "temporary pre-embed seed cleanup; remove migrate_seeded.rs in a future release (see CHANGELOG Deprecated / Unreleased releaser notes)"
)]
pub fn migrate_legacy_seeded_skills() -> Option<SeedMigrationNotice> {
    let config_dir = config_aish_dir()?;
    let marker = config_dir.join(MIGRATION_MARKER);
    if marker.is_file() {
        return None;
    }

    let skills_dir = config_dir.join("skills");
    if !skills_dir.is_dir() {
        // Nothing to migrate; still mark so we don't re-check every launch.
        let _ = write_marker(&marker);
        return None;
    }

    let packaged = crate::builtin::embedded_skill_names();
    if packaged.is_empty() {
        let _ = write_marker(&marker);
        return None;
    }

    let backup_root = config_dir.join(BACKUP_DIR_NAME);
    let mut moved: Vec<String> = Vec::new();
    let mut failed = false;

    let Ok(entries) = fs::read_dir(&skills_dir) else {
        return None;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // Skip hidden dirs under skills/.
        if name.starts_with('.') {
            continue;
        }
        if !packaged.contains(&name) {
            continue;
        }

        if let Err(err) = fs::create_dir_all(&backup_root) {
            tracing::warn!(
                path = %backup_root.display(),
                error = %err,
                "Failed to create backup dir for legacy seeded skills; will retry later"
            );
            return None;
        }

        let dest = unique_backup_path(&backup_root, &name);
        match fs::rename(&path, &dest) {
            Ok(()) => {
                tracing::info!(
                    skill = %name,
                    from = %path.display(),
                    to = %dest.display(),
                    "Moved legacy install-seeded skill aside so embedded builtin can load; restore from backup if you customized it"
                );
                moved.push(name);
            }
            Err(err) => {
                tracing::warn!(
                    skill = %name,
                    from = %path.display(),
                    to = %dest.display(),
                    error = %err,
                    "Failed to move legacy seeded skill; will retry on next launch"
                );
                failed = true;
            }
        }
    }

    if failed {
        return None;
    }

    if let Err(err) = write_marker(&marker) {
        tracing::warn!(
            path = %marker.display(),
            error = %err,
            "Failed to write skills seed migration marker"
        );
        return None;
    }

    if moved.is_empty() {
        return None;
    }

    tracing::info!(
        count = moved.len(),
        backup = %backup_root.display(),
        skills = %moved.join(", "),
        "Completed one-shot migration of legacy install-seeded skills (deprecated transitional path; will be removed in a future aish release)"
    );

    Some(SeedMigrationNotice {
        moved,
        backup_dir: backup_root,
    })
}

fn write_marker(marker: &Path) -> std::io::Result<()> {
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(marker, b"v1\n")
}

fn unique_backup_path(backup_root: &Path, name: &str) -> PathBuf {
    let dest = backup_root.join(name);
    if !dest.exists() {
        return dest;
    }
    for i in 1..1000 {
        let candidate = backup_root.join(format!("{name}.{i}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    backup_root.join(format!("{name}.{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::test_env_lock;

    #[test]
    fn migrate_moves_packaged_names_once() {
        let _guard = test_env_lock();
        let dir = tempfile::tempdir().expect("temp dir");
        let config = dir.path().join("config");
        let skills = config.join("skills");
        let packaged = skills.join("diagnose_system_lag");
        let custom = skills.join("my-custom-skill");
        fs::create_dir_all(&packaged).unwrap();
        fs::create_dir_all(&custom).unwrap();
        fs::write(
            packaged.join("SKILL.md"),
            "---\nname: diagnose_system_lag\ndescription: old\n---\nOLD\n",
        )
        .unwrap();
        fs::write(
            custom.join("SKILL.md"),
            "---\nname: my-custom-skill\ndescription: keep\n---\nKEEP\n",
        )
        .unwrap();

        std::env::set_var("AISH_CONFIG_DIR", &config);
        #[allow(deprecated)]
        let notice = migrate_legacy_seeded_skills();
        assert!(notice.is_some(), "should report a user-visible notice");
        let notice = notice.unwrap();
        assert!(notice.user_message().contains("diagnose_system_lag"));
        assert!(notice.user_message().contains(BACKUP_DIR_NAME));

        assert!(
            !packaged.exists(),
            "packaged seed should be moved out of skills/"
        );
        assert!(custom.exists(), "user-authored skill must stay");
        assert!(
            config
                .join(BACKUP_DIR_NAME)
                .join("diagnose_system_lag")
                .is_dir(),
            "backup should contain diagnose_system_lag"
        );
        assert!(config.join(MIGRATION_MARKER).is_file());

        // Second run is a no-op even if we put the seed back.
        fs::create_dir_all(&packaged).unwrap();
        fs::write(
            packaged.join("SKILL.md"),
            "---\nname: diagnose_system_lag\ndescription: again\n---\nAGAIN\n",
        )
        .unwrap();
        #[allow(deprecated)]
        let notice2 = migrate_legacy_seeded_skills();
        assert!(notice2.is_none(), "after marker, do not migrate again");
        assert!(packaged.exists(), "after marker, do not migrate again");

        std::env::remove_var("AISH_CONFIG_DIR");
    }
}
