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
        }
    }

    /// Scan and return skill root directories in priority order: USER > CLAUDE.
    pub fn scan_skill_roots(&self) -> Vec<(SkillSource, PathBuf)> {
        let mut roots = Vec::new();

        // 1. USER: $AISH_CONFIG_DIR/skills or ~/.config/aish/skills
        if let Ok(config_dir) = std::env::var("AISH_CONFIG_DIR") {
            roots.push((SkillSource::User, PathBuf::from(config_dir).join("skills")));
        } else if let Some(home) = dirs::home_dir() {
            roots.push((
                SkillSource::User,
                home.join(".config").join("aish").join("skills"),
            ));
        }

        // 2. CLAUDE: $HOME/.claude/skills
        if let Some(home) = dirs::home_dir() {
            roots.push((SkillSource::Claude, home.join(".claude").join("skills")));
        }

        roots.into_iter().filter(|(_, p)| p.is_dir()).collect()
    }

    /// Load all skills from all sources with priority deduplication.
    ///
    /// Skills from higher-priority sources (listed first) shadow skills with
    /// the same name from lower-priority sources.
    pub fn load_all_skills(&mut self) -> aish_core::Result<()> {
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
        let re = regex::Regex::new(FRONTMATTER_REGEX).map_err(|e| {
            aish_core::AishError::Skill(format!("Invalid frontmatter regex: {}", e))
        })?;

        let caps = re.captures(&content).ok_or_else(|| {
            aish_core::AishError::Skill(
                "Invalid skill file format: must start with YAML frontmatter".into(),
            )
        })?;

        let frontmatter_yaml = caps.get(1).unwrap().as_str();
        let skill_content = &content[caps.get(0).unwrap().end()..];
        let skill_content = skill_content.trim();

        let metadata: SkillMetadata = serde_yaml::from_str(frontmatter_yaml)
            .map_err(|e| aish_core::AishError::Skill(format!("Invalid YAML frontmatter: {}", e)))?;

        Ok(Skill {
            metadata,
            content: skill_content.to_string(),
            source,
            file_path: skill_path.to_string_lossy().to_string(),
            base_dir: skill_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_string_lossy()
                .to_string(),
        })
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
