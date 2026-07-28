//! skills.sh registry adapter.
//!
//! Search API: `GET https://skills.sh/api/search?q=<query>&limit=<n>`
//! Response: `{ skills: [{ id, skillId, name, installs, source }] }`
//!
//! Install: download GitHub tarball via `owner/repo`, extract skill dir.

use std::path::Path;

use aish_core::{AishError, Result};

use super::installer::{install_from_github, InstallResult};
use super::RegistryAdapter;
use super::RegistrySkill;

/// Adapter for the skills.sh registry (Vercel Labs).
pub struct SkillsShAdapter {
    name: String,
    base_url: String,
}

impl SkillsShAdapter {
    pub fn new(name: &str, base_url: &str) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

/// skills.sh `/api/search` response shape.
#[derive(serde::Deserialize)]
struct SearchResponse {
    skills: Vec<SearchSkill>,
}

#[derive(serde::Deserialize)]
struct SearchSkill {
    id: String,
    #[serde(default)]
    skill_id: Option<String>,
    name: String,
    #[serde(default)]
    installs: u64,
    source: String,
}

impl RegistryAdapter for SkillsShAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<RegistrySkill>> {
        let url = format!(
            "{}/api/search?q={}&limit={}",
            self.base_url,
            super::url_encode(query),
            limit
        );

        tracing::debug!(url = %url, registry = self.name(), "Searching skills.sh");

        let resp = super::http_client()
            .get(&url)
            .send()
            .map_err(|e| AishError::Skill(format!("skills.sh search request failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(AishError::Skill(format!(
                "skills.sh search returned HTTP {}",
                resp.status()
            )));
        }

        let parsed: SearchResponse = resp
            .json()
            .map_err(|e| AishError::Skill(format!("skills.sh response parse failed: {}", e)))?;

        Ok(parsed
            .skills
            .into_iter()
            .map(|s| RegistrySkill {
                id: format!("{}/{}", self.name, s.id),
                name: s.name,
                description: String::new(), // skills.sh API has no description in search results
                registry: self.name.clone(),
                source: s.source,
                slug: s.skill_id.unwrap_or_else(|| {
                    // Extract skill name from `id` which is "owner/repo/skill-name".
                    s.id.rsplit('/').next().unwrap_or(&s.id).to_string()
                }),
                installs: s.installs,
                homepage: Some(format!("{}/{}", self.base_url, s.id)),
            })
            .collect())
    }

    fn install(&self, skill: &RegistrySkill, target_dir: &Path) -> Result<InstallResult> {
        // `source` is "owner/repo", `slug` is the skill name within the repo.
        install_from_github(
            &skill.source,
            &skill.slug,
            target_dir,
            &super::NEVER_CANCELLED,
        )
    }

    fn install_with_cancel(
        &self,
        skill: &RegistrySkill,
        target_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<InstallResult> {
        install_from_github(&skill.source, &skill.slug, target_dir, cancel)
    }
}
