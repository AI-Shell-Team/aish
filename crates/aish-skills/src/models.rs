use aish_core::SkillSource;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillExecutionContext {
    #[default]
    Inline,
    #[serde(rename = "subagent", alias = "fork", alias = "sub_agent")]
    SubAgent,
}

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
    #[serde(default, alias = "allowed-tools")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub context: SkillExecutionContext,
    #[serde(default)]
    pub agent: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_context_defaults_to_inline() {
        let metadata: SkillMetadata =
            serde_yaml::from_str("name: legacy\ndescription: Legacy skill\n")
                .expect("legacy metadata should parse");

        assert_eq!(metadata.context, SkillExecutionContext::Inline);
        assert_eq!(metadata.agent, None);
    }

    #[test]
    fn skill_context_parses_subagent_and_allowed_tools_alias() {
        let metadata: SkillMetadata = serde_yaml::from_str(
            "name: diagnose\ndescription: Diagnose host\ncontext: subagent\nagent: troubleshoot\nallowed-tools:\n  - bash\n  - read_file\n",
        )
        .expect("sub-agent metadata should parse");

        assert_eq!(metadata.context, SkillExecutionContext::SubAgent);
        assert_eq!(metadata.agent.as_deref(), Some("troubleshoot"));
        assert_eq!(
            metadata.allowed_tools,
            Some(vec!["bash".to_string(), "read_file".to_string()])
        );
    }

    #[test]
    fn claude_fork_context_maps_to_subagent() {
        let metadata: SkillMetadata = serde_yaml::from_str(
            "name: imported\ndescription: Imported skill\ncontext: fork\nagent: explore\n",
        )
        .expect("Claude-compatible fork metadata should parse");

        assert_eq!(metadata.context, SkillExecutionContext::SubAgent);
    }
}
