//! Skill verification — validates SKILL.md structure and file integrity.

use std::path::Path;

/// Result of verifying a skill directory.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub valid: bool,
    pub skill_name: String,
    pub checks: Vec<VerifyCheck>,
}

/// A single check result.
#[derive(Debug, Clone)]
pub struct VerifyCheck {
    pub label: String,
    pub passed: bool,
    pub detail: String,
}

/// Verify a skill directory.
///
/// Checks:
/// 1. SKILL.md exists and is readable.
/// 2. YAML frontmatter parses and contains `name` + `description`.
/// 3. Referenced scripts (if any) exist.
/// 4. Scripts are executable (on Unix).
pub fn verify_skill_dir(dir: &Path) -> VerifyReport {
    let mut checks = Vec::new();
    let mut skill_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // 1. SKILL.md exists.
    let skill_md = dir.join("SKILL.md");
    let content = match std::fs::read_to_string(&skill_md) {
        Ok(c) => {
            checks.push(VerifyCheck {
                label: "SKILL.md exists".into(),
                passed: true,
                detail: skill_md.display().to_string(),
            });
            c
        }
        Err(e) => {
            checks.push(VerifyCheck {
                label: "SKILL.md exists".into(),
                passed: false,
                detail: format!("{}: {}", skill_md.display(), e),
            });
            return VerifyReport {
                valid: false,
                skill_name,
                checks,
            };
        }
    };

    // 2. Frontmatter parses and satisfies the loader's invariants. Reuses the
    //    exact parse the loader + installer use, so `verify` cannot report a
    //    skill as valid that the loader would reject (e.g. context=fork with
    //    no agent).
    let frontmatter_ok = match crate::manager::parse_skill_metadata(&content) {
        Ok((metadata, _)) => {
            skill_name = metadata.name.clone();
            checks.push(VerifyCheck {
                label: "Frontmatter valid".into(),
                passed: true,
                detail: format!("name=\"{}\"", skill_name),
            });
            true
        }
        Err(e) => {
            checks.push(VerifyCheck {
                label: "Frontmatter valid".into(),
                passed: false,
                detail: e.to_string(),
            });
            false
        }
    };

    // 3. Scripts directory — check referenced scripts exist and are executable.
    let scripts_dir = dir.join("scripts");
    if scripts_dir.is_dir() {
        check_scripts_dir(&scripts_dir, &mut checks);
    }

    let all_passed = checks.iter().all(|c| c.passed);
    let _ = frontmatter_ok; // Already reflected in checks.

    VerifyReport {
        valid: all_passed,
        skill_name,
        checks,
    }
}

/// Check that scripts in a directory exist and have executable permissions.
fn check_scripts_dir(scripts_dir: &Path, checks: &mut Vec<VerifyCheck>) {
    let entries = match std::fs::read_dir(scripts_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let is_exec = meta.permissions().mode() & 0o111 != 0;
                checks.push(VerifyCheck {
                    label: format!("scripts/{} executable", name),
                    passed: is_exec,
                    detail: if is_exec {
                        "OK".into()
                    } else {
                        "Not executable (chmod +x may be needed)".into()
                    },
                });
            }
        }

        #[cfg(not(unix))]
        {
            let _ = &path;
            checks.push(VerifyCheck {
                label: format!("scripts/{} exists", name),
                passed: true,
                detail: "OK".into(),
            });
        }
    }
}
