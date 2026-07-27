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

    // 2. Frontmatter parses.
    let frontmatter_ok = match parse_frontmatter(&content) {
        Ok(fm) => {
            if let Some(name) = fm.get("name").and_then(|v| v.as_str()) {
                skill_name = name.to_string();
            }
            let has_name = fm.get("name").and_then(|v| v.as_str()).is_some();
            let has_desc = fm.get("description").and_then(|v| v.as_str()).is_some();
            checks.push(VerifyCheck {
                label: "Frontmatter valid".into(),
                passed: has_name && has_desc,
                detail: if has_name && has_desc {
                    format!("name=\"{}\", has description", skill_name)
                } else {
                    let mut missing = Vec::new();
                    if !has_name {
                        missing.push("name");
                    }
                    if !has_desc {
                        missing.push("description");
                    }
                    format!("Missing fields: {}", missing.join(", "))
                },
            });
            has_name && has_desc
        }
        Err(e) => {
            checks.push(VerifyCheck {
                label: "Frontmatter valid".into(),
                passed: false,
                detail: e,
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

/// Parse YAML frontmatter from SKILL.md content.
/// Returns a map of key → JSON value.
fn parse_frontmatter(content: &str) -> Result<serde_json::Value, String> {
    let re = regex::Regex::new(r"(?s)^---\s*\n(.*?)\n---\s*\n")
        .map_err(|e| format!("Regex error: {}", e))?;

    let caps = re
        .captures(content)
        .ok_or_else(|| "Missing YAML frontmatter (--- delimiters)".to_string())?;

    let yaml_str = caps.get(1).unwrap().as_str();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(yaml_str).map_err(|e| format!("YAML parse error: {}", e))?;

    serde_json::to_value(&yaml).map_err(|e| format!("Conversion error: {}", e))
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
