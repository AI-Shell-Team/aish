use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use aish_core::SkillSource;

use crate::models::*;

/// Regex to extract YAML frontmatter from markdown files.
const FRONTMATTER_REGEX: &str = r"(?s)^---\s*\n(.*?)\n---\s*\n";

/// Discovers, loads, and manages skill plugins from filesystem directories.
pub struct SkillManager {
    skills: HashMap<String, Skill>,
    skill_lists: Vec<SkillList>,
    skills_version: u64,
    /// Set when this process ran the one-shot legacy seed migration and moved
    /// at least one skill. Consumed by the interactive shell for a tip.
    seed_migration_notice: Option<crate::migrate_seeded::SeedMigrationNotice>,
}

impl Default for SkillManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillManager {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            skill_lists: Vec::new(),
            skills_version: 0,
            seed_migration_notice: None,
        }
    }

    /// Take the one-shot seed-migration notice, if any, for display to the user.
    pub fn take_seed_migration_notice(
        &mut self,
    ) -> Option<crate::migrate_seeded::SeedMigrationNotice> {
        self.seed_migration_notice.take()
    }

    /// Resolve the user skills directory, mirroring the config loader's
    /// config-dir resolution so installed skills land where the loader reads
    /// them. Checked in order: `$AISH_CONFIG_DIR/skills`, then
    /// `$XDG_CONFIG_HOME/aish/skills` (via `dirs::config_dir()`), then
    /// `~/.config/aish/skills`. Shared by the loader (`scan_skill_roots`) and
    /// the install paths (shell `/skill`, AI `skill_install`, `aish skill`
    /// CLI) so they can never disagree.
    pub fn user_skills_root() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("AISH_CONFIG_DIR") {
            return Some(PathBuf::from(dir).join("skills"));
        }
        if let Some(config_dir) = dirs::config_dir() {
            return Some(config_dir.join("aish").join("skills"));
        }
        dirs::home_dir().map(|h| h.join(".config").join("aish").join("skills"))
    }

    /// Scan and return skill root directories in priority order:
    /// USER > CLAUDE > Builtin (embedded packaged skills, materialized to cache).
    pub fn scan_skill_roots(&self) -> Vec<(SkillSource, PathBuf)> {
        let mut roots = Vec::new();

        // 1. USER: resolved via user_skills_root() so installs land exactly
        // where the loader reads them (respects $XDG_CONFIG_HOME, matching
        // aish_config::ConfigLoader::default_config_path). Previously this
        // hardcoded ~/.config and diverged from the install paths.
        if let Some(user_root) = Self::user_skills_root() {
            roots.push((SkillSource::User, user_root));
        }

        // 2. CLAUDE: $HOME/.claude/skills
        if let Some(home) = dirs::home_dir() {
            roots.push((SkillSource::Claude, home.join(".claude").join("skills")));
        }

        // 3. Builtin: versioned cache of compile-time embedded skills/
        let builtin_root = crate::builtin::cache_root();
        if builtin_root.is_dir() {
            roots.push((SkillSource::Builtin, builtin_root));
        }

        roots.into_iter().filter(|(_, p)| p.is_dir()).collect()
    }

    /// Load all skills from all sources with priority deduplication.
    ///
    /// Skills from higher-priority sources (listed first) shadow skills with
    /// the same name from lower-priority sources.
    pub fn load_all_skills(&mut self) -> aish_core::Result<()> {
        // Deprecated transitional path — remove with `migrate_seeded` (see
        // CHANGELOG [Unreleased] Deprecated / Notes for releasers).
        #[allow(deprecated)]
        {
            self.seed_migration_notice = crate::migrate_seeded::migrate_legacy_seeded_skills();
        }

        if let Err(err) = crate::builtin::ensure_materialized() {
            tracing::warn!("Failed to materialize embedded builtin skills: {}", err);
        }

        let mut loaded_skills: HashMap<String, Skill> = HashMap::new();
        let mut skill_lists: Vec<SkillList> = Vec::new();

        for (source, root_path) in self.scan_skill_roots() {
            let skill_list = self.load_skills(source, &root_path)?;
            skill_lists.push(skill_list);

            for skill in &skill_lists.last().unwrap().skills {
                let name = skill.metadata.name.clone();
                loaded_skills.entry(name).or_insert_with(|| skill.clone());
            }
        }

        self.skills = loaded_skills;
        self.skill_lists = skill_lists;
        self.skills_version += 1;
        Ok(())
    }

    /// Load all skills from a specific directory.
    fn load_skills(&self, source: SkillSource, skill_root: &Path) -> aish_core::Result<SkillList> {
        let mut skills = Vec::new();

        if !skill_root.is_dir() {
            return Ok(SkillList {
                source,
                skills,
                root_path: skill_root.to_string_lossy().to_string(),
            });
        }

        // Find all SKILL.md files recursively
        for entry in walk_dir(skill_root) {
            if entry
                .file_name()
                .map(|n| n.to_string_lossy().to_uppercase() == "SKILL.MD")
                .unwrap_or(false)
            {
                match self.parse_skill_file(source.clone(), &entry) {
                    Ok(skill) => skills.push(skill),
                    Err(e) => {
                        tracing::warn!("Failed to load skill from {:?}: {}", entry, e);
                    }
                }
            }
        }

        Ok(SkillList {
            source,
            skills,
            root_path: skill_root.to_string_lossy().to_string(),
        })
    }

    /// Parse a single SKILL.md file into a [`Skill`].
    ///
    /// The file must start with a YAML frontmatter block delimited by `---`.
    fn parse_skill_file(&self, source: SkillSource, skill_path: &Path) -> aish_core::Result<Skill> {
        let content = std::fs::read_to_string(skill_path)?;
        let (metadata, body) = parse_skill_metadata(&content)?;
        let skill_content = body.trim();

        let base_dir = skill_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .to_string();

        // Skills imported from the Claude ecosystem often hardcode `~/.claude/skills/<name>`;
        // aish seeds them to `~/.config/aish/skills/<name>` instead. Rewrite both forms to
        // the actual base_dir so script paths resolve correctly regardless of where the
        // skill was loaded from.
        let skill_content = rewrite_skill_paths(skill_content, &metadata.name, &base_dir);

        // Registry-installed skills carry a `.untrusted` sentinel at the
        // skill's top-level dir until reviewed. `load_skills` walks the tree
        // recursively, so a skill may ship nested SKILL.md files in
        // subdirectories; those inherit the top-level quarantine. Walk up the
        // ancestors so a nested `my-skill/sub/SKILL.md` is also quarantined
        // when `my-skill/.untrusted` exists (otherwise it would load as
        // trusted and bypass the review gate). Markers can only ever exist
        // inside a skill dir (validate_install_slug rejects empty/traversal
        // slugs), so this never over-quarantines unrelated skills.
        let quarantined = Self::is_quarantined_under(skill_path);

        Ok(Skill {
            metadata,
            content: skill_content,
            source,
            file_path: skill_path.to_string_lossy().to_string(),
            base_dir,
            quarantined,
        })
    }

    /// True if the skill at `skill_path` lives under a directory carrying the
    /// `.untrusted` quarantine marker. Walks up from the file's parent so
    /// nested SKILL.md files (under an untrusted skill's top-level dir) are
    /// caught too.
    fn is_quarantined_under(skill_path: &Path) -> bool {
        let mut dir = match skill_path.parent() {
            Some(d) => d,
            None => return false,
        };
        loop {
            if dir.join(UNTRUSTED_MARKER).exists() {
                return true;
            }
            dir = match dir.parent() {
                Some(p) => p,
                None => return false,
            };
        }
    }

    /// Look up a skill by name.
    pub fn get_skill(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Return references to all loaded skills.
    pub fn list_skills(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Return the current version counter (bumped on each reload).
    pub fn skills_version(&self) -> u64 {
        self.skills_version
    }

    /// Return the list of skill root directories that should be watched.
    pub fn get_skill_dirs(&self) -> Vec<PathBuf> {
        self.scan_skill_roots()
            .into_iter()
            .filter(|(_, p)| p.is_dir())
            .map(|(_, p)| p)
            .collect()
    }

    /// Find a skill by its file path.
    pub fn get_skill_by_path(&self, path: &Path) -> Option<&Skill> {
        let path_str = path.to_string_lossy();
        self.skills.values().find(|s| s.file_path == path_str)
    }

    /// Find a skill name by its file path.
    pub fn find_skill_name_by_path(&self, path: &Path) -> Option<String> {
        let path_str = path.to_string_lossy();
        self.skills
            .iter()
            .find(|(_, s)| s.file_path == path_str)
            .map(|(name, _)| name.clone())
    }

    /// Reload a single skill from its file path.
    ///
    /// If the file can be parsed successfully, the skill is inserted (or
    /// replaced) in the cache.  On failure the old entry is kept and the
    /// error is returned.
    pub fn reload_skill(&mut self, path: &Path) -> aish_core::Result<()> {
        // Determine which source owns this path.
        let source = self.source_for_path(path);

        let skill = self.parse_skill_file(source, path)?;
        let name = skill.metadata.name.clone();
        tracing::info!("Reloaded skill '{}' from {:?}", name, path);
        self.skills.insert(name.clone(), skill);
        self.skills_version += 1;
        Ok(())
    }

    /// Remove a skill from the cache by name.
    ///
    /// Returns `true` if the skill was present and removed.
    pub fn remove_skill(&mut self, name: &str) -> bool {
        if self.skills.remove(name).is_some() {
            tracing::info!("Removed skill '{}' from cache", name);
            self.skills_version += 1;
            true
        } else {
            false
        }
    }

    /// Try to determine which [`SkillSource`] owns the given path.
    fn source_for_path(&self, path: &Path) -> SkillSource {
        let roots = self.scan_skill_roots();
        for (source, root) in &roots {
            if path.starts_with(root) {
                return source.clone();
            }
        }
        // Default to User if we cannot determine the source.
        SkillSource::User
    }
}

/// Parse a SKILL.md document: extract the YAML frontmatter, deserialize it
/// into [`SkillMetadata`], and enforce the loader's invariants. Returns the
/// parsed metadata and the document body (the content after the frontmatter
/// block, not yet trimmed).
///
/// This is the single source of truth for "would the loader accept this file":
/// [`SkillManager::parse_skill_file`], the registry installer, and the verifier
/// all route through it, so install/verify-time validation can never drift from
/// load-time validation. A skill that fails this (e.g. one that declares
/// `context: subagent`/`fork` without an `agent`) is rejected here rather than
/// landing on disk only to be rejected on every hot-reload.
pub fn parse_skill_metadata(content: &str) -> aish_core::Result<(SkillMetadata, &str)> {
    let re = regex::Regex::new(FRONTMATTER_REGEX)
        .map_err(|e| aish_core::AishError::Skill(format!("Invalid frontmatter regex: {}", e)))?;
    let caps = re.captures(content).ok_or_else(|| {
        aish_core::AishError::Skill(
            "Invalid skill file format: must start with YAML frontmatter".into(),
        )
    })?;
    let frontmatter_yaml = caps.get(1).unwrap().as_str();
    let body = &content[caps.get(0).unwrap().end()..];
    let metadata: SkillMetadata = serde_yaml::from_str(frontmatter_yaml)
        .map_err(|e| aish_core::AishError::Skill(format!("Invalid YAML frontmatter: {}", e)))?;
    if metadata.context == crate::SkillExecutionContext::SubAgent
        && metadata.agent.as_deref().is_none_or(str::is_empty)
    {
        return Err(aish_core::AishError::Skill(format!(
            "Skill '{}' uses context=subagent but does not declare an agent",
            metadata.name
        )));
    }
    Ok((metadata, body))
}

/// Walk a directory recursively, following symlinks while detecting cycles.
fn walk_dir(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut visited: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();

    fn walk(
        dir: &Path,
        files: &mut Vec<PathBuf>,
        visited: &mut std::collections::HashSet<(u64, u64)>,
    ) {
        // Follow symlinks and guard against cycles
        if let Ok(metadata) = std::fs::symlink_metadata(dir) {
            if metadata.is_symlink() {
                if let Ok(real) = std::fs::canonicalize(dir) {
                    if let Ok(stat) = std::fs::metadata(&real) {
                        let key = (stat.dev(), stat.ino());
                        if visited.contains(&key) {
                            return;
                        }
                        visited.insert(key);
                    }
                }
            }
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    // Skip .git directories
                    if path.file_name().map(|n| n == ".git").unwrap_or(false) {
                        continue;
                    }
                    walk(&path, files, visited);
                } else if file_type.is_file() {
                    files.push(path);
                }
            }
        }
    }

    walk(dir, &mut files, &mut visited);
    files.sort();
    files
}

/// Rewrite hardcoded skill paths in SKILL.md content to the actual base_dir.
///
/// Many skills imported from the Claude ecosystem reference their own scripts as
/// `~/.claude/skills/<name>/scripts/...`. aish loads the same skill from a different
/// location (`~/.config/aish/skills/<name>`),
/// so without rewriting, LLM-driven `bash` calls would hit non-existent paths. Both
/// `~/.claude/skills/<name>` and `~/.config/aish/skills/<name>` forms are replaced
/// with the absolute base_dir, preserving any sub-path that follows.
///
/// Replacement is boundary-aware: a pattern only matches when followed by `/`,
/// end-of-string, or a non-identifier character (i.e. not `[A-Za-z0-9_-]`). This
/// prevents `~/.claude/skills/deploy` from corrupting `~/.claude/skills/deploy-helper`.
fn rewrite_skill_paths(content: &str, skill_name: &str, base_dir: &str) -> String {
    let patterns = [
        format!("~/.claude/skills/{}", skill_name),
        format!("~/.config/aish/skills/{}", skill_name),
        format!("$HOME/.claude/skills/{}", skill_name),
        format!("$HOME/.config/aish/skills/{}", skill_name),
    ];
    let mut result = content.to_string();
    for p in patterns {
        result = replace_path_prefix(&result, &p, base_dir);
    }
    result
}

/// Replace `needle` with `replacement` in `haystack`, but only when the match is
/// followed by a path boundary (`/`, end-of-string, or any char that is not
/// alphanumeric, `_`, or `-`). Without this guard, `String::replace` would treat
/// `~/.claude/skills/deploy` as a prefix of `~/.claude/skills/deploy-helper`.
fn replace_path_prefix(haystack: &str, needle: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(idx) = rest.find(needle) {
        let after = &rest[idx + needle.len()..];
        let is_boundary = after
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(true);
        out.push_str(&rest[..idx]);
        if is_boundary {
            out.push_str(replacement);
        } else {
            out.push_str(needle);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Mark a skill directory trusted (`trusted = true` removes the `.untrusted`
/// sentinel so the loader injects it into AI context) or untrusted
/// (`trusted = false` writes the sentinel). Returns whether the marker state
/// actually changed. Removing/adding the sentinel is picked up by hot-reload.
pub fn set_skill_trusted(skill_dir: &Path, trusted: bool) -> std::io::Result<bool> {
    let marker = skill_dir.join(UNTRUSTED_MARKER);
    let exists = marker.exists();
    if trusted && exists {
        std::fs::remove_file(&marker)?;
        Ok(true)
    } else if !trusted && !exists {
        std::fs::write(&marker, b"untrusted: pending security review\n")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_rejects_subagent_skill_without_agent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("SKILL.md");
        std::fs::write(
            &path,
            "---\nname: broken\ndescription: Broken isolated skill\ncontext: subagent\n---\nDo work\n",
        )
        .expect("write skill");
        let mut manager = SkillManager::new();

        let error = manager
            .reload_skill(&path)
            .expect_err("subagent skill without agent must be rejected");

        assert!(error.to_string().contains("agent"));
        assert!(manager.get_skill("broken").is_none());
    }

    #[test]
    fn packaged_system_lag_skill_uses_read_only_troubleshoot_subagent() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/diagnose_system_lag/SKILL.md");
        let manager = SkillManager::new();

        let skill = manager
            .parse_skill_file(SkillSource::Builtin, &path)
            .expect("packaged system lag skill should parse");

        assert_eq!(
            skill.metadata.context,
            crate::SkillExecutionContext::SubAgent
        );
        assert_eq!(skill.metadata.agent.as_deref(), Some("troubleshoot"));
        assert_eq!(
            skill.metadata.allowed_tools,
            Some(vec![
                "bash".to_string(),
                "read_file".to_string(),
                "grep".to_string(),
                "glob".to_string(),
            ])
        );
    }

    #[test]
    fn load_all_skills_includes_embedded_builtins() {
        let _guard = crate::builtin::test_env_lock();

        let dir = tempfile::tempdir().expect("temp dir");
        let config_dir = dir.path().join("config");
        let builtin_dir = dir.path().join("builtin");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        std::env::set_var("AISH_BUILTIN_SKILLS_CACHE", &builtin_dir);
        std::env::set_var("AISH_CONFIG_DIR", &config_dir);

        let mut manager = SkillManager::new();
        manager
            .load_all_skills()
            .expect("load skills with embedded builtins");

        let skill = manager
            .get_skill("diagnose_system_lag")
            .expect("embedded diagnose_system_lag must load");
        assert_eq!(skill.source, SkillSource::Builtin);
        assert!(
            Path::new(&skill.base_dir).join("SKILL.md").is_file(),
            "builtin skill base_dir must point at materialized files"
        );

        std::env::remove_var("AISH_BUILTIN_SKILLS_CACHE");
        std::env::remove_var("AISH_CONFIG_DIR");
    }

    #[test]
    fn rewrite_replaces_claude_path() {
        let content = "Run `bash ~/.claude/skills/my-skill/scripts/x.sh`";
        let rewritten = rewrite_skill_paths(content, "my-skill", "/abs/path");
        assert_eq!(rewritten, "Run `bash /abs/path/scripts/x.sh`");
    }

    #[test]
    fn rewrite_replaces_aish_path() {
        let content = "See ~/.config/aish/skills/my-skill/README.md";
        let rewritten = rewrite_skill_paths(content, "my-skill", "/abs/path");
        assert_eq!(rewritten, "See /abs/path/README.md");
    }

    #[test]
    fn rewrite_does_not_touch_other_skills() {
        let content = "Refers to ~/.claude/skills/other-skill/x.sh";
        let rewritten = rewrite_skill_paths(content, "my-skill", "/abs/path");
        assert_eq!(rewritten, content, "other-skill path must be left alone");
    }

    #[test]
    fn rewrite_handles_home_env_form() {
        let content = "bash $HOME/.claude/skills/my-skill/scripts/y.sh";
        let rewritten = rewrite_skill_paths(content, "my-skill", "/abs/path");
        assert_eq!(rewritten, "bash /abs/path/scripts/y.sh");
    }

    #[test]
    fn rewrite_does_not_match_prefixed_skill_names() {
        // `deploy` must not be treated as a prefix of `deploy-helper`.
        let content = "bash ~/.claude/skills/deploy-helper/scripts/x.sh";
        let rewritten = rewrite_skill_paths(content, "deploy", "/abs/path");
        assert_eq!(rewritten, content, "deploy-helper must not be touched");
    }

    #[test]
    fn rewrite_matches_bare_skill_at_end_of_string() {
        let content = "Installed at ~/.claude/skills/my-skill";
        let rewritten = rewrite_skill_paths(content, "my-skill", "/abs/path");
        assert_eq!(rewritten, "Installed at /abs/path");
    }

    #[test]
    fn quarantined_skill_detected_via_untrusted_marker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\nbody",
        )
        .unwrap();
        std::fs::write(skill_dir.join(UNTRUSTED_MARKER), b"untrusted").unwrap();

        let manager = SkillManager::new();
        let skill = manager
            .parse_skill_file(SkillSource::User, &skill_dir.join("SKILL.md"))
            .expect("skill should parse");
        assert!(
            skill.quarantined,
            "skill with .untrusted marker must be quarantined"
        );

        // Trusting removes the marker -> no longer quarantined.
        assert!(set_skill_trusted(&skill_dir, true).unwrap());
        let trusted = manager
            .parse_skill_file(SkillSource::User, &skill_dir.join("SKILL.md"))
            .unwrap();
        assert!(
            !trusted.quarantined,
            "after trust, skill is not quarantined"
        );

        // Re-quarantine writes the marker back.
        assert!(set_skill_trusted(&skill_dir, false).unwrap());
        assert!(skill_dir.join(UNTRUSTED_MARKER).exists());
    }

    #[test]
    fn nested_skill_md_inherits_top_level_quarantine() {
        // A registry skill ships a nested SKILL.md in a subdir. The marker
        // lives at the skill's top-level dir, so the nested file must also be
        // treated as quarantined — otherwise it loads as trusted and bypasses
        // the review gate.
        let dir = tempfile::tempdir().expect("temp dir");
        let skill_dir = dir.path().join("my-skill");
        let nested_dir = skill_dir.join("sub");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(
            skill_dir.join(UNTRUSTED_MARKER),
            b"untrusted: pending review",
        )
        .unwrap();
        std::fs::write(
            nested_dir.join("SKILL.md"),
            "---\nname: nested\ndescription: test\n---\nbody",
        )
        .unwrap();

        let manager = SkillManager::new();
        let skill = manager
            .parse_skill_file(SkillSource::User, &nested_dir.join("SKILL.md"))
            .expect("nested skill should parse");
        assert!(
            skill.quarantined,
            "nested SKILL.md under an untrusted skill must be quarantined"
        );
    }
    #[test]
    fn parse_skill_metadata_rejects_fork_without_agent() {
        // The docker-best-practices bug: `context: fork` aliases to SubAgent,
        // which requires a named agent. The shared parse must reject it so the
        // installer and verifier reject it too.
        let content = "---\nname: docker\ndescription: d\ncontext: fork\n---\nbody\n";
        let err = parse_skill_metadata(content).expect_err("fork without agent must fail");
        assert!(err.to_string().contains("agent"), "got: {err}");
    }

    #[test]
    fn parse_skill_metadata_accepts_subagent_with_agent_and_returns_body() {
        let content = "---\nname: diag\ndescription: d\ncontext: subagent\nagent: troubleshoot\n---\n## Body\n";
        let (metadata, body) =
            parse_skill_metadata(content).expect("valid subagent skill must parse");
        assert_eq!(metadata.name, "diag");
        assert_eq!(metadata.agent.as_deref(), Some("troubleshoot"));
        assert!(body.trim_start().starts_with("## Body"));
    }

    #[test]
    fn parse_skill_metadata_rejects_missing_frontmatter() {
        let err = parse_skill_metadata("no frontmatter here at all")
            .expect_err("must require frontmatter");
        assert!(err.to_string().contains("frontmatter"), "got: {err}");
    }
}
