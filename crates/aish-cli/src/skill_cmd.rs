//! `aish skill` subcommand — search, install, verify, list, remove, registry.
//!
//! Every handler returns `Result<(), String>`; `main` exits non-zero on `Err`
//! so scripts can gate on `$?` (matching the setup/doctor exit contract).

use std::path::PathBuf;

use aish_config::{ConfigLoader, ConfigModel, RegistrySource};
use aish_skills::registry::{RegistryConfig, RegistryManager, RegistrySkill};

/// Resolve the user skill directory (`~/.config/aish/skills`).
fn user_skills_dir() -> PathBuf {
    if let Ok(config_dir) = std::env::var("AISH_CONFIG_DIR") {
        PathBuf::from(config_dir).join("skills")
    } else {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aish")
            .join("skills")
    }
}

/// Build a RegistryManager from the config's registry list.
fn build_manager(config: &ConfigModel) -> RegistryManager {
    let configs: Vec<RegistryConfig> = config
        .skills
        .registries
        .iter()
        .map(|r| RegistryConfig {
            name: r.name.clone(),
            registry_type: r.registry_type.clone(),
            enabled: r.enabled,
            url: r.url.clone(),
        })
        .collect();
    RegistryManager::from_config(&configs)
}

// ----- Search -----

pub fn cmd_search(query: &str, limit: usize) -> Result<(), String> {
    let config = ConfigLoader::load(None).unwrap_or_default();
    let manager = build_manager(&config);

    if manager.adapter_names().is_empty() {
        return Err(
            "No enabled skill registries. Use `aish skill registry add` to add one.".into(),
        );
    }

    println!("Searching for: \"{}\" (limit {}) ...\n", query, limit);
    let outcome = manager.search_all_with_errors(query, limit);

    if outcome.results.is_empty() {
        println!("No skills found.");
    } else {
        for (i, skill) in outcome.results.iter().enumerate() {
            let installs = if skill.installs > 0 {
                format!(" ({} installs)", skill.installs)
            } else {
                String::new()
            };
            println!(
                "  {}. [{}] {}{}",
                i + 1,
                skill.registry,
                skill.name,
                installs
            );
            if !skill.description.is_empty() {
                // Truncate long descriptions for terminal display.
                let desc: String = skill.description.chars().take(120).collect();
                let suffix = if skill.description.chars().count() > 120 {
                    "..."
                } else {
                    ""
                };
                println!("     {}{}", desc, suffix);
            }
            println!("     ID: {}", skill.id);
            if let Some(ref home) = skill.homepage {
                println!("     URL: {}", home);
            }
            println!();
        }

        println!("Install with: aish skill install <ID>");
    }

    if !outcome.errors.is_empty() {
        eprintln!("\nRegistry errors (results may be incomplete):");
        for e in &outcome.errors {
            eprintln!("  - {}: {}", e.registry, e.error);
        }
    }

    Ok(())
}

// ----- Install -----

pub fn cmd_install(skill_id: &str) -> Result<(), String> {
    let config = ConfigLoader::load(None).unwrap_or_default();
    let manager = build_manager(&config);
    let adapter_names = manager.adapter_names();
    let target_dir = user_skills_dir();

    // Determine registry prefix and rest.
    let (reg_filter, rest) = {
        if let Some((first, r)) = skill_id.split_once('/') {
            if adapter_names.contains(&first) {
                (Some(first), r)
            } else {
                (None, skill_id)
            }
        } else {
            (None, skill_id)
        }
    };

    // Strategy 1: Direct parse (owner/repo/skill-name).
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 3 {
        let source = format!("{}/{}", parts[0], parts[1]);
        let slug = parts.last().unwrap().to_string();
        // Reject unsafe slugs (empty from trailing slash, or traversal like
        // "..") before constructing a RegistrySkill. Other skill subcommands
        // already gate on is_unsafe_skill_name; install must too.
        if is_unsafe_skill_name(&slug) {
            return Err(format!(
                "Invalid skill ID '{}': unsafe or empty skill name.",
                skill_id
            ));
        }
        let reg = reg_filter.unwrap_or(&config.skills.default_registry);
        let skill = RegistrySkill {
            id: skill_id.to_string(),
            name: slug.clone(),
            description: String::new(),
            registry: reg.to_string(),
            source,
            slug,
            installs: 0,
            homepage: None,
        };
        println!(
            "Resolving skill '{}' from '{}'...",
            skill.slug, skill.registry
        );
        println!("Installing '{}' from {}...", skill.name, skill.registry);
        std::fs::create_dir_all(&target_dir).ok();
        match manager.install(&skill, &target_dir) {
            Ok(result) => {
                print_install_success(&result);
                return Ok(());
            }
            Err(e) => {
                // Not fatal: fall through to the search strategy.
                eprintln!("Direct install failed: {}. Trying search...", e);
            }
        }
    }

    // Strategy 2: Search + match.
    let search_query = rest.rsplit('/').next().unwrap_or(rest);
    println!(
        "Searching for '{}' across {}...",
        search_query,
        adapter_names.join(", ")
    );
    let results = manager.search_all(search_query, 20);
    let last = search_query;

    let skill = results.iter().find(|s| {
        if let Some(reg) = reg_filter {
            if s.registry != reg {
                return false;
            }
        }
        s.id == skill_id
            || s.slug == rest
            || s.slug == last
            || s.name.to_lowercase() == last.to_lowercase()
    });

    let skill = match skill {
        Some(s) => s.clone(),
        None => {
            return Err(format!(
                "Skill '{}' not found. Search returned {} results.\n\
                 Use `aish skill search {}` to find the correct ID.",
                skill_id,
                results.len(),
                search_query
            ));
        }
    };

    println!("Installing '{}' from {}...", skill.name, skill.registry);
    std::fs::create_dir_all(&target_dir).ok();
    match manager.install(&skill, &target_dir) {
        Ok(result) => {
            print_install_success(&result);
            Ok(())
        }
        Err(e) => Err(format!("Install failed: {}", e)),
    }
}

fn print_install_success(result: &aish_skills::registry::InstallResult) {
    if result.dir.join(aish_skills::UNTRUSTED_MARKER).exists() {
        println!("\nInstalled successfully (QUARANTINED — untrusted, not loaded yet):");
        println!("  Name: {}", result.skill_name);
        println!("  Path: {}", result.dir.display());
        println!(
            "Review it: start aish and run  /skill vet {}",
            result.skill_name
        );
        println!(
            "Trust it after review:  aish skill trust {}",
            result.skill_name
        );
        return;
    }
    println!("\nInstalled successfully!");
    println!("  Name: {}", result.skill_name);
    println!("  Path: {}", result.dir.display());

    let report = RegistryManager::verify(&result.dir);
    if report.valid {
        println!("  Verify: PASSED");
    } else {
        println!("  Verify: WARNINGS");
        for check in &report.checks {
            if !check.passed {
                println!("    - {}: {}", check.label, check.detail);
            }
        }
    }
}

// ----- Verify -----

pub fn cmd_verify(name: &str) -> Result<(), String> {
    if is_unsafe_skill_name(name) {
        return Err(format!("Unsafe skill name: {:?}", name));
    }
    let dir = user_skills_dir().join(name);
    if !dir.exists() {
        return Err(format!(
            "Skill '{}' not found in {}.",
            name,
            user_skills_dir().display()
        ));
    }

    let report = RegistryManager::verify(&dir);
    println!("Skill: {}", report.skill_name);
    println!("Valid: {}", if report.valid { "YES" } else { "NO" });
    println!();
    for check in &report.checks {
        let status = if check.passed { "PASS" } else { "FAIL" };
        println!("  [{}] {} — {}", status, check.label, check.detail);
    }
    Ok(())
}

// ----- Trust & vet (quarantine) -----

/// Reject names that could escape the skills directory: empty, path
/// separators, NUL, or the special `.` / `..` entries.
fn is_unsafe_skill_name(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
}

/// `aish skill trust <name>`: remove the `.untrusted` marker so the skill
/// loads on the next aish start. CLI companion to the interactive
/// `/skill vet` flow for users who reviewed a skill out of band.
pub fn cmd_trust(name: &str) -> Result<(), String> {
    if is_unsafe_skill_name(name) {
        return Err(format!("Invalid skill name '{}'.", name));
    }
    let dir = user_skills_dir().join(name);
    if !dir.is_dir() {
        return Err(format!("Skill '{}' is not installed.", name));
    }
    if !dir.join(aish_skills::UNTRUSTED_MARKER).exists() {
        println!("Skill '{}' is already trusted.", name);
        return Ok(());
    }
    match aish_skills::set_skill_trusted(&dir, true) {
        Ok(_) => {
            println!("Trusted skill '{}'. It will load on next aish start.", name);
            Ok(())
        }
        Err(e) => Err(format!("Failed to trust skill '{}': {}", name, e)),
    }
}

/// `aish skill vet <name>`: manual review aid for a quarantined skill. Prints
/// the install path, the full `SKILL.md` contents, and the other files in the
/// directory. Full automated vetting (with an AI session) runs via the
/// interactive `/skill vet <name>` command inside aish.
pub fn cmd_vet(name: &str) -> Result<(), String> {
    if is_unsafe_skill_name(name) {
        return Err(format!("Invalid skill name '{}'.", name));
    }
    let dir = user_skills_dir().join(name);
    if !dir.is_dir() {
        return Err(format!("Skill '{}' is not installed.", name));
    }

    println!("Path: {}", dir.display());

    let skill_md = dir.join("SKILL.md");
    match std::fs::read_to_string(&skill_md) {
        Ok(content) => {
            println!("\n--- SKILL.md ---");
            println!("{}", content);
        }
        Err(e) => println!("\nCould not read SKILL.md: {}", e),
    }

    println!("\nOther files:");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut files: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|f| f != "SKILL.md")
                .collect();
            files.sort();
            if files.is_empty() {
                println!("  (none)");
            } else {
                for f in &files {
                    println!("  {}", f);
                }
            }
        }
        Err(e) => eprintln!("Could not list directory: {}", e),
    }

    println!(
        "\nFull automated vetting runs in interactive aish: /skill vet {}",
        name
    );
    Ok(())
}

// ----- List -----

pub fn cmd_list() -> Result<(), String> {
    let dir = user_skills_dir();
    if !dir.exists() {
        println!("No skills installed.");
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();

    entries.sort_by_key(|e| e.file_name());

    let count = entries.iter().filter(|e| e.path().is_dir()).count();

    if count == 0 {
        println!("No skills installed in {}.", dir.display());
        return Ok(());
    }

    println!("Installed skills ({}):\n", count);
    for entry in &entries {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let skill_md = entry.path().join("SKILL.md");
        if skill_md.exists() {
            println!("  {} ✓", name);
        } else {
            println!("  {} (no SKILL.md)", name);
        }
    }
    Ok(())
}

// ----- Remove -----

pub fn cmd_remove(name: &str) -> Result<(), String> {
    if is_unsafe_skill_name(name) {
        return Err(format!("Unsafe skill name: {:?}", name));
    }
    let dir = user_skills_dir().join(name);
    if !dir.exists() {
        return Err(format!("Skill '{}' not found.", name));
    }

    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            println!("Removed skill '{}' from {}.", name, dir.display());
            Ok(())
        }
        Err(e) => Err(format!("Failed to remove '{}': {}", name, e)),
    }
}

// ----- Registry management -----

pub fn cmd_registry_list() -> Result<(), String> {
    let config = ConfigLoader::load(None).unwrap_or_default();
    println!("Configured skill registries:\n");
    for r in &config.skills.registries {
        let status = if r.enabled { "enabled" } else { "disabled" };
        let default_marker = if r.name == config.skills.default_registry {
            " (default)"
        } else {
            ""
        };
        println!(
            "  {} [{}] {} — {}{}",
            r.name, r.registry_type, r.url, status, default_marker
        );
    }
    Ok(())
}

pub fn cmd_registry_add(name: &str, registry_type: &str, url: &str) -> Result<(), String> {
    let mut config = ConfigLoader::load(None).map_err(|e| {
        format!(
            "Failed to load config (refusing to overwrite with defaults): {}",
            e
        )
    })?;

    // Validate registry_type: unknown types are silently skipped at search
    // time (RegistryManager::from_config), so a typo would look like success
    // but never query. Reject up front with the known set.
    const KNOWN_REGISTRY_TYPES: &[&str] = &["skills_sh", "skillhub", "clawhub"];
    if !KNOWN_REGISTRY_TYPES.contains(&registry_type) {
        return Err(format!(
            "Unknown registry type '{}'. Valid types: {}",
            registry_type,
            KNOWN_REGISTRY_TYPES.join(", ")
        ));
    }

    // Check for duplicate name.
    if config.skills.registries.iter().any(|r| r.name == name) {
        return Err(format!("Registry '{}' already exists.", name));
    }

    config.skills.registries.push(RegistrySource {
        name: name.to_string(),
        registry_type: registry_type.to_string(),
        enabled: true,
        url: url.to_string(),
    });

    match save_config(&config) {
        Ok(()) => {
            println!(
                "Added registry '{}' (type: {}, url: {}).",
                name, registry_type, url
            );
            Ok(())
        }
        Err(e) => Err(format!("Failed to save config: {}", e)),
    }
}

pub fn cmd_registry_remove(name: &str) -> Result<(), String> {
    let mut config = ConfigLoader::load(None).map_err(|e| {
        format!(
            "Failed to load config (refusing to overwrite with defaults): {}",
            e
        )
    })?;
    let before = config.skills.registries.len();
    config.skills.registries.retain(|r| r.name != name);

    if config.skills.registries.len() == before {
        return Err(format!("Registry '{}' not found.", name));
    }

    match save_config(&config) {
        Ok(()) => {
            println!("Removed registry '{}'.", name);
            Ok(())
        }
        Err(e) => Err(format!("Failed to save config: {}", e)),
    }
}

/// Save config back to disk.
fn save_config(config: &ConfigModel) -> Result<(), String> {
    let path = aish_config::ConfigLoader::default_config_path();
    aish_config::ConfigLoader::save(config, &path).map_err(|e| e.to_string())
}
