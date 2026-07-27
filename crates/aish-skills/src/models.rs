use aish_core::SkillSource;
use serde::{Deserialize, Serialize};

/// Deserialize `allowed_tools` from either a YAML sequence or a plain
/// string (space- or comma-separated).  Skills from external registries
/// (skills.sh, skillhub.cn) often use `allowed-tools: Read Write Bash`
/// instead of the list form.
fn deserialize_allowed_tools<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<serde_yaml::Value> = Option::deserialize(d)?;
    match opt {
        None => Ok(None),
        Some(serde_yaml::Value::String(s)) => {
            let items: Vec<String> = s
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            if items.is_empty() {
                Ok(None)
            } else {
                Ok(Some(items))
            }
        }
        Some(serde_yaml::Value::Sequence(seq)) => {
            let items: Vec<String> = seq
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Ok(Some(items))
        }
        Some(_) => Ok(None),
    }
}

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
    #[serde(
        default,
        alias = "allowed-tools",
        deserialize_with = "deserialize_allowed_tools"
    )]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub context: SkillExecutionContext,
    #[serde(default)]
    pub agent: Option<String>,
}

/// Sentinel file written into a registry-installed skill directory to mark it
/// untrusted (pending security review). Its presence makes the loader set
/// [`Skill::quarantined`] so the skill's content is NOT injected into AI
/// context until reviewed and trusted.
pub const UNTRUSTED_MARKER: &str = ".untrusted";

/// A fully loaded skill with its content and provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub metadata: SkillMetadata,
    pub content: String,
    pub source: SkillSource,
    pub file_path: String,
    pub base_dir: String,
    /// True when this skill was installed from an external registry and has
    /// not yet passed security review. Quarantined skills are excluded from
    /// AI context injection (their SKILL.md is untrusted instruction text)
    /// until a review trusts them (removes the `.untrusted` marker).
    #[serde(default)]
    pub quarantined: bool,
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
    fn allowed_tools_accepts_string_form() {
        // External registries often use a plain string instead of a YAML list.
        let metadata: SkillMetadata = serde_yaml::from_str(
            "name: docker-ops\ndescription: Docker ops\nallowed-tools: Read Write Bash\n",
        )
        .expect("string-form allowed-tools should parse");

        assert_eq!(
            metadata.allowed_tools,
            Some(vec![
                "Read".to_string(),
                "Write".to_string(),
                "Bash".to_string()
            ])
        );
    }

    #[test]
    fn allowed_tools_accepts_comma_string_form() {
        let metadata: SkillMetadata = serde_yaml::from_str(
            "name: multi\ndescription: Multi\nallowed-tools: bash, read_file, web_fetch\n",
        )
        .expect("comma-separated allowed-tools should parse");

        assert_eq!(
            metadata.allowed_tools,
            Some(vec![
                "bash".to_string(),
                "read_file".to_string(),
                "web_fetch".to_string()
            ])
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
