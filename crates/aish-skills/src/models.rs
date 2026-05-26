use aish_core::SkillSource;
use serde::{Deserialize, Serialize};

/// Metadata extracted from YAML frontmatter of a SKILL.md file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}

/// A fully loaded skill with its content and provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub metadata: SkillMetadata,
    pub content: String,
    pub source: SkillSource,
    pub file_path: String,
    pub base_dir: String,
}

/// A group of skills loaded from a single source directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillList {
    pub source: SkillSource,
    pub skills: Vec<Skill>,
    pub root_path: String,
}
