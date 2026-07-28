//! skillhub.cn (and clawhub-compatible) registry adapter.
//!
//! Search API: `GET <base>/api/skills?keyword=<query>`
//! Response: `{ code: 0, data: { skills: [{ name, slug, description, ... }] } }`
//!
//! Install: delegates to `npx clawhub install --registry <base>`.

use std::path::Path;

use aish_core::{AishError, Result};

use super::installer::{install_via_skillhub_api, InstallResult};
use super::RegistryAdapter;
use super::RegistrySkill;

/// Adapter for skillhub.cn and any clawhub-compatible registry.
pub struct SkillHubAdapter {
    name: String,
    base_url: String,
}

impl SkillHubAdapter {
    pub fn new(name: &str, base_url: &str) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

/// skillhub `/api/skills` response shape.
#[derive(serde::Deserialize)]
struct SkillsResponse {
    code: i32,
    data: SkillsData,
}

#[derive(serde::Deserialize)]
struct SkillsData {
    #[serde(default)]
    skills: Vec<SkillHubSkill>,
}

#[derive(serde::Deserialize)]
struct SkillHubSkill {
    name: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    description_zh: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    namespace: Option<SkillNamespace>,
}

#[derive(serde::Deserialize)]
struct SkillNamespace {
    #[serde(default)]
    public_slug: Option<String>,
}

impl RegistryAdapter for SkillHubAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<RegistrySkill>> {
        let url = format!(
            "{}/api/skills?keyword={}",
            self.base_url,
            super::url_encode(query)
        );

        tracing::debug!(url = %url, registry = self.name(), "Searching skillhub");

        let resp = super::http_client()
            .get(&url)
            .send()
            .map_err(|e| AishError::Skill(format!("skillhub search request failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(AishError::Skill(format!(
                "skillhub search returned HTTP {}",
                resp.status()
            )));
        }

        let parsed: SkillsResponse = resp
            .json()
            .map_err(|e| AishError::Skill(format!("skillhub response parse failed: {}", e)))?;

        if parsed.code != 0 {
            return Err(AishError::Skill(format!(
                "skillhub API error code: {}",
                parsed.code
            )));
        }

        Ok(parsed
            .data
            .skills
            .into_iter()
            .take(limit)
            .map(|s| {
                let slug = s
                    .slug
                    .or_else(|| s.namespace.as_ref().and_then(|n| n.public_slug.clone()))
                    .unwrap_or_else(|| s.name.clone());
                // Prefer Chinese description if available, fall back to English.
                let desc = s
                    .description_zh
                    .as_deref()
                    .filter(|d| !d.is_empty())
                    .or(s.description.as_deref())
                    .unwrap_or("")
                    .to_string();
                RegistrySkill {
                    id: format!("{}/{}", self.name, slug),
                    name: s.name,
                    description: truncate(&desc, 300),
                    registry: self.name.clone(),
                    source: slug.clone(),
                    slug,
                    installs: s.downloads,
                    homepage: s.homepage,
                }
            })
            .collect())
    }

    fn install(&self, skill: &RegistrySkill, target_dir: &Path) -> Result<InstallResult> {
        install_via_skillhub_api(
            &skill.slug,
            &self.base_url,
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
        install_via_skillhub_api(&skill.slug, &self.base_url, target_dir, cancel)
    }
}

/// Truncate a string to at most `max` chars, appending "..." if cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}...", cut)
}
