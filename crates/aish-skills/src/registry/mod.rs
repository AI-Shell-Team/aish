//! Multi-source skill registry: search, install, verify.
//!
//! Each registry type implements [`RegistryAdapter`]. The [`RegistryManager`]
//! aggregates enabled adapters and searches them in parallel.

mod installer;
pub mod skillhub;
pub mod skills_sh;
mod verifier;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use aish_core::Result;

/// A cancel flag that is never set. Used by call sites that do not need
/// cooperative cancellation (e.g. the plain `install` trait path).
pub(crate) static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Lightweight registry descriptor — mirrors `aish_config::RegistrySource`
/// without creating a cross-crate dependency on `aish-config`.
#[derive(Clone)]
pub struct RegistryConfig {
    pub name: String,
    pub registry_type: String,
    pub enabled: bool,
    pub url: String,
}

pub use installer::InstallResult;
pub use verifier::VerifyReport;

/// Minimal percent-encoding for query parameters.
pub(crate) fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Shared blocking HTTP client for registry adapters.
///
/// Adds a connect+read timeout so a hung registry cannot stall a search
/// indefinitely — `reqwest::blocking::get` has no timeout by default.
pub(crate) fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("aish-skill-registry")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// Normalized skill representation returned by any registry search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegistrySkill {
    /// Globally-unique id: `"{registry_name}/{slug}"`.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Short description (localized if available).
    pub description: String,
    /// Name of the registry that produced this result.
    pub registry: String,
    /// Original source string understood by the adapter
    /// (e.g. `"owner/repo"` for skills.sh, `"@handle/slug"` for skillhub).
    pub source: String,
    /// Install identifier — the value passed to [`RegistryAdapter::install`].
    pub slug: String,
    /// Install count for popularity ranking (0 if unknown).
    pub installs: u64,
    /// Optional detail-page URL for the user.
    pub homepage: Option<String>,
}

/// A pluggable skill registry adapter.
///
/// Built-in adapters: [`skills_sh::SkillsShAdapter`], [`skillhub::SkillHubAdapter`].
/// Custom adapters can be registered with [`RegistryManager::register`].
pub trait RegistryAdapter: Send + Sync {
    /// Registry display name (matches config `name` field).
    fn name(&self) -> &str;

    /// Search the registry for skills matching `query`.
    fn search(&self, query: &str, limit: usize) -> Result<Vec<RegistrySkill>>;

    /// Download and install `skill` into `target_dir`.
    /// Returns the path where the skill was installed and its metadata.
    fn install(&self, skill: &RegistrySkill, target_dir: &Path) -> Result<InstallResult>;

    /// Like [`RegistryAdapter::install`], but cooperatively aborts between file
    /// downloads when `cancel` is set. The default delegates to `install`
    /// (ignoring the flag); adapters that download many files override this.
    fn install_with_cancel(
        &self,
        skill: &RegistrySkill,
        target_dir: &Path,
        cancel: &AtomicBool,
    ) -> Result<InstallResult> {
        let _ = cancel;
        self.install(skill, target_dir)
    }

    /// Test whether the registry is reachable and responding.
    /// Returns `Ok((skill_count, latency_ms))` on success, or `Err` with
    /// a human-readable diagnostic.
    fn test_connection(&self) -> Result<(usize, u128)> {
        // Default: do a minimal search and report result count + timing.
        let start = std::time::Instant::now();
        let results = self.search("test", 1)?;
        Ok((results.len(), start.elapsed().as_millis()))
    }
}

/// Aggregates multiple registry adapters.
///
/// Searches all enabled adapters and merges results by install count.
pub struct RegistryManager {
    adapters: Vec<Box<dyn RegistryAdapter>>,
}

/// One registry's failure during a multi-registry search.
#[derive(Debug, Clone)]
pub struct RegistrySearchError {
    /// Registry (adapter) name that failed.
    pub registry: String,
    /// Human-readable error message.
    pub error: String,
}

/// Aggregated search outcome across all enabled registries: merged results
/// plus any per-registry errors. Exposing errors (instead of swallowing them)
/// lets callers tell "no matches" apart from "a registry was unreachable /
/// rate-limited", so an empty result is diagnosable.
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    /// Merged results, sorted by install count, truncated to `limit`.
    pub results: Vec<RegistrySkill>,
    /// Per-registry failures (e.g. timeout, rate limit, DNS).
    pub errors: Vec<RegistrySearchError>,
}

impl SearchOutcome {
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

impl RegistryManager {
    /// Build a manager from the user's registry config list.
    pub fn from_config(registries: &[RegistryConfig]) -> Self {
        let mut adapters: Vec<Box<dyn RegistryAdapter>> = Vec::new();
        for src in registries {
            if !src.enabled {
                continue;
            }
            match src.registry_type.as_str() {
                "skills_sh" => adapters.push(Box::new(skills_sh::SkillsShAdapter::new(
                    &src.name, &src.url,
                ))),
                "skillhub" => adapters.push(Box::new(skillhub::SkillHubAdapter::new(
                    &src.name, &src.url,
                ))),
                "clawhub" => {
                    // clawhub.ai has a Convex-based API that is not implemented
                    // natively. We reuse the skillhub adapter, which speaks the
                    // skillhub protocol (`/api/skills`, file listing API). A real
                    // clawhub.ai URL will fail search/install — log a warning so
                    // the mismatch is visible rather than silently broken.
                    tracing::warn!(
                        registry = src.name,
                        "clawhub type is not natively supported; using the skillhub-protocol \
                         adapter (only works against skillhub-API-compatible servers)"
                    );
                    adapters.push(Box::new(skillhub::SkillHubAdapter::new(
                        &src.name, &src.url,
                    )));
                }
                other => {
                    tracing::warn!(
                        registry = src.name,
                        r#type = other,
                        "Unknown registry type, skipping. Valid types: skills_sh, skillhub, clawhub"
                    );
                }
            }
        }
        Self { adapters }
    }

    /// Register a custom adapter at runtime.
    pub fn register(&mut self, adapter: Box<dyn RegistryAdapter>) {
        self.adapters.push(adapter);
    }

    /// Iterate over enabled adapter names.
    pub fn adapter_names(&self) -> Vec<&str> {
        self.adapters.iter().map(|a| a.name()).collect()
    }

    /// Search all enabled registries and merge by install count.
    ///
    /// Errors from individual adapters are logged and skipped so one
    /// unreachable registry does not break the entire search.
    pub fn search_all(&self, query: &str, limit: usize) -> Vec<RegistrySkill> {
        self.search_all_with_errors(query, limit).results
    }

    /// Search all enabled registries, returning merged results AND any
    /// per-registry errors so callers can distinguish "no matches" from
    /// "registry unreachable / rate-limited". Results are merged, sorted by
    /// install count, and truncated to `limit`.
    pub fn search_all_with_errors(&self, query: &str, limit: usize) -> SearchOutcome {
        let mut results = Vec::new();
        let mut errors = Vec::new();
        for adapter in &self.adapters {
            match adapter.search(query, limit) {
                Ok(r) => results.extend(r),
                Err(e) => {
                    tracing::warn!(
                        registry = adapter.name(),
                        error = %e,
                        "Registry search failed"
                    );
                    errors.push(RegistrySearchError {
                        registry: adapter.name().to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }
        results.sort_by_key(|s| std::cmp::Reverse(s.installs));
        results.truncate(limit);
        SearchOutcome { results, errors }
    }

    /// Find a specific adapter by name.
    pub fn find_adapter(&self, name: &str) -> Option<&dyn RegistryAdapter> {
        self.adapters
            .iter()
            .find(|a| a.name() == name)
            .map(|a| a.as_ref())
    }

    /// Install a skill using the adapter that produced it.
    pub fn install(&self, skill: &RegistrySkill, target_dir: &Path) -> Result<InstallResult> {
        Self::check_reserved(&skill.slug)?;
        let adapter = self.find_adapter(&skill.registry).ok_or_else(|| {
            aish_core::AishError::Skill(format!("No adapter for registry '{}'", skill.registry))
        })?;
        Self::install_transactional(adapter, skill, target_dir, None)
    }

    /// Install a skill, respecting a cooperative cancel flag checked between
    /// file downloads. Used by interactive/AI install paths that can be
    /// cancelled (Ctrl+C) or timed out.
    pub fn install_with_cancel(
        &self,
        skill: &RegistrySkill,
        target_dir: &Path,
        cancel: &AtomicBool,
    ) -> Result<InstallResult> {
        Self::check_reserved(&skill.slug)?;
        let adapter = self.find_adapter(&skill.registry).ok_or_else(|| {
            aish_core::AishError::Skill(format!("No adapter for registry '{}'", skill.registry))
        })?;
        Self::install_transactional(adapter, skill, target_dir, Some(cancel))
    }

    /// Transactional install core shared by [`install`](Self::install) and
    /// [`install_with_cancel`](Self::install_with_cancel).
    ///
    /// For a reinstall the existing skill dir is moved aside first (same
    /// filesystem, so the rename is atomic), then the replacement is downloaded
    /// into a fresh dir and validated. On failure the failed replacement is
    /// removed and the original restored in place — a bad replacement can never
    /// overwrite/quarantine a previously trusted or locally modified skill.
    fn install_transactional(
        adapter: &dyn RegistryAdapter,
        skill: &RegistrySkill,
        target_dir: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<InstallResult> {
        // Hold the per-slug lock for the whole transaction: two concurrent
        // installs of the same slug would race the live→stash rename / swap.
        let install_lock = install_lock_for(&skill.slug);
        let _install_guard = install_lock.lock().unwrap();
        // Reject path-escaping slugs BEFORE constructing the stash path —
        // reinstall_stash_path joins the slug into target_dir, so an unsafe
        // slug must not reach it. (pre_quarantine re-validates downstream.)
        installer::validate_install_slug(&skill.slug)?;
        let live = target_dir.join(&skill.slug);
        let stash = if live.exists() {
            let stash = reinstall_stash_path(target_dir, &skill.slug);
            if stash.exists() {
                let _ = std::fs::remove_dir_all(&stash);
            }
            std::fs::rename(&live, &stash).map_err(|e| {
                aish_core::AishError::Skill(format!(
                    "failed to stash existing skill for reinstall: {e}"
                ))
            })?;
            Some(stash)
        } else {
            None
        };
        let result = pre_quarantine(target_dir, &skill.slug)
            .and_then(|_| match cancel {
                None => adapter.install(skill, target_dir),
                Some(flag) => adapter.install_with_cancel(skill, target_dir, flag),
            })
            .and_then(|r| validate_installed_skill(&r.dir).map(|_| r));
        match result {
            Ok(result) => {
                // Success: the validated replacement supersedes the original.
                if let Some(s) = stash {
                    let _ = std::fs::remove_dir_all(s);
                }
                Ok(result)
            }
            Err(e) => {
                // Failure: drop the failed replacement and restore the original.
                let _ = std::fs::remove_dir_all(&live);
                if let Some(s) = stash {
                    let _ = std::fs::rename(s, &live);
                }
                Err(e)
            }
        }
    }

    /// Test connection to a specific registry adapter by name.
    pub fn test_connection(&self, name: &str) -> Result<(usize, u128)> {
        let adapter = self.find_adapter(name).ok_or_else(|| {
            aish_core::AishError::Skill(format!("No adapter for registry '{}'", name))
        })?;
        adapter.test_connection()
    }

    /// Verify a skill directory has a valid SKILL.md and referenced files.
    pub fn verify(skill_dir: &Path) -> VerifyReport {
        verifier::verify_skill_dir(skill_dir)
    }
}

/// Reserved skill names that must not be installed from a registry. `skill-vetter`
/// is the built-in security reviewer invoked after every install; a same-named
/// registry skill would shadow it and could neutralize vetting for all later
/// installs. Kept as a slice so the check lives in one place.
const RESERVED_SKILL_SLUGS: &[&str] = &["skill-vetter"];

impl RegistryManager {
    fn check_reserved(slug: &str) -> Result<()> {
        if RESERVED_SKILL_SLUGS.contains(&slug) {
            return Err(aish_core::AishError::Skill(format!(
                "Skill name '{}' is reserved for a built-in skill and cannot be \
                 installed from a registry.",
                slug
            )));
        }
        Ok(())
    }
}

/// True if a skill name or slug could escape the skills directory when used as
/// a path component: empty, a path separator (`/` or `\`), NUL, or the `.`
/// / `..` special entries. This is the single source of truth for the
/// predicate — the registry installer, the shell, the CLI, and the AI install
/// tool all route through it so the rule cannot drift between entry points.
pub fn is_unsafe_skill_name(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
}

/// Parse a bare `owner/repo/skill-name` skill ID (3+ `/`-separated segments)
/// into `(source = "owner/repo", slug = "skill-name")`. Returns `None` when
/// there are fewer than 3 segments or the trailing slug is unsafe. Shared by
/// the shell, CLI, and AI install tool so the parse + slug check cannot drift.
/// The `source` half is re-validated by the installer (exactly one `/`, no
/// traversal/NUL); the slug is the install directory name.
pub fn parse_skill_id_rest(rest: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 3 {
        let source = format!("{}/{}", parts[0], parts[1]);
        let slug = parts.last()?.to_string();
        if is_unsafe_skill_name(&slug) {
            return None;
        }
        Some((source, slug))
    } else {
        None
    }
}

/// Build a unique sibling path (same `target_dir` filesystem) for stashing an
/// existing skill dir during a transactional reinstall. A same-filesystem
/// rename is atomic; stashing under `/tmp` could cross mounts and fail with
/// EXDEV. The nanos+pid suffix avoids collisions between concurrent installs.
fn reinstall_stash_path(target_dir: &Path, slug: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    target_dir.join(format!(".{slug}.reinstall-{nanos}-{}", std::process::id()))
}

/// Per-slug install lock. Concurrent installs of the SAME slug would race the
/// stash/restore swap (each renames the live dir aside and writes a fresh
/// replacement), so serialize per-slug — different slugs still install in
/// parallel. Entries live for the process, bounded by the finite slug set.
static INSTALL_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn install_lock_for(slug: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    let mut map = INSTALL_LOCKS.lock().unwrap();
    map.entry(slug.to_string())
        .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
        .clone()
}

/// Confirm an installed skill's SKILL.md is loadable by running the exact
/// parse the loader uses. A skill the loader rejects is unusable — it lands on
/// disk only to be rejected on every hot-reload — so this turns a silent
/// post-install failure (warning spam) into an actionable install-time error.
fn validate_installed_skill(dir: &Path) -> Result<()> {
    let content = std::fs::read_to_string(dir.join("SKILL.md")).map_err(|e| {
        aish_core::AishError::Skill(format!("Installed skill has no readable SKILL.md: {}", e))
    })?;
    crate::manager::parse_skill_metadata(&content).map(|_| ())
}

/// Pre-create the skill directory with its `.untrusted` quarantine marker
/// BEFORE the adapter downloads any files. Writing the marker first (a) closes
/// the race where a hot-reload observes files without the marker and (b) makes
/// a marker-write failure fail-closed — the install aborts instead of leaving
/// an untrusted skill that the loader would treat as trusted.
fn pre_quarantine(target_dir: &Path, slug: &str) -> Result<PathBuf> {
    // Defense-in-depth: reject any slug that could escape the install dir
    // BEFORE `target_dir.join(slug)` is ever formed. Slugs flow in from three
    // untrusted entry points (interactive `/skill`, the AI `skill_install`
    // tool, and the `aish skill` CLI); validating here is the single
    // chokepoint that closes all of them. Without it, an empty or `..` slug
    // makes the failure-cleanup `remove_dir_all(target_dir.join(slug))`
    // delete the skills directory or its parent (~/.config/aish).
    installer::validate_install_slug(slug)?;
    let dir = target_dir.join(slug);
    std::fs::create_dir_all(&dir)?;
    if let Err(e) = std::fs::write(
        dir.join(crate::models::UNTRUSTED_MARKER),
        b"untrusted: pending security review\n",
    ) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(aish_core::AishError::Skill(format!(
            "Failed to quarantine skill (marker write): {}",
            e
        )));
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_skill_name_rejected_before_install() {
        // `skill-vetter` is reserved (built-in security reviewer); installing
        // a registry skill under that name would shadow it and neutralize
        // vetting, so it must be rejected. Other names are allowed.
        assert!(RegistryManager::check_reserved("skill-vetter").is_err());
        assert!(RegistryManager::check_reserved("my-skill").is_ok());
    }

    #[test]
    fn pre_quarantine_writes_marker_and_fails_closed() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Normal case: marker written, dir returned.
        let skill_dir = pre_quarantine(dir.path(), "demo").expect("pre-quarantine should succeed");
        assert!(skill_dir.join(crate::models::UNTRUSTED_MARKER).exists());
    }

    #[test]
    fn pre_quarantine_rejects_unsafe_slug_without_touching_parent() {
        // Regression: an empty slug (from a trailing slash in a skill ID) or a
        // `..` slug used to make pre_quarantine join the parent dir and the
        // failure cleanup remove_dir_all it. They must now be rejected before
        // any path is joined, and must NOT create the quarantine marker in the
        // parent (which would batch-quarantine every installed skill).
        let dir = tempfile::tempdir().expect("temp dir");
        let parent = dir.path();
        // Seed a sibling file to prove the parent is left untouched.
        std::fs::write(parent.join("sibling"), b"x").unwrap();

        for bad in ["", ".", ".."] {
            assert!(
                pre_quarantine(parent, bad).is_err(),
                "slug {:?} must be rejected",
                bad
            );
        }

        // No marker leaked into the parent, and the sibling survives.
        assert!(
            !parent.join(crate::models::UNTRUSTED_MARKER).exists(),
            "quarantine marker must not be written to the parent directory"
        );
        assert!(
            parent.join("sibling").exists(),
            "parent contents must be intact"
        );
    }
    /// Test adapter that writes a fixed SKILL.md into the install dir, so we
    /// can drive `RegistryManager::install` end-to-end without any HTTP.
    struct FakeAdapter {
        name: &'static str,
        skill_md: &'static str,
    }

    impl RegistryAdapter for FakeAdapter {
        fn name(&self) -> &str {
            self.name
        }
        fn search(&self, _query: &str, _limit: usize) -> Result<Vec<RegistrySkill>> {
            Ok(vec![])
        }
        fn install(&self, skill: &RegistrySkill, target_dir: &Path) -> Result<InstallResult> {
            let dir = target_dir.join(&skill.slug);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), self.skill_md).unwrap();
            Ok(InstallResult {
                dir,
                skill_name: skill.slug.clone(),
            })
        }
    }

    fn fake_registry_skill(slug: &str, registry: &str) -> RegistrySkill {
        RegistrySkill {
            id: format!("{registry}/{slug}"),
            name: slug.to_string(),
            description: "test".into(),
            registry: registry.to_string(),
            source: "owner/repo".into(),
            slug: slug.to_string(),
            installs: 0,
            homepage: None,
        }
    }

    #[test]
    fn install_rejects_unloadable_skill_and_rolls_back_fresh_install() {
        // Regression for "installed skill spams hot-reload warnings forever":
        // a skill the loader rejects (context=fork, no agent) must be refused
        // at install time and the fresh install dir removed.
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut manager = RegistryManager { adapters: vec![] };
        manager.register(Box::new(FakeAdapter {
            name: "fake",
            skill_md: "---\nname: bad\ndescription: d\ncontext: fork\n---\nbody\n",
        }));
        let skill = fake_registry_skill("bad-skill", "fake");

        let err = manager
            .install(&skill, tmp.path())
            .expect_err("unloadable skill must be rejected at install time");
        assert!(err.to_string().contains("agent"), "got: {err}");
        assert!(
            !tmp.path().join("bad-skill").exists(),
            "fresh install must be rolled back"
        );
    }

    #[test]
    fn install_accepts_loadable_skill() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut manager = RegistryManager { adapters: vec![] };
        manager.register(Box::new(FakeAdapter {
            name: "fake",
            skill_md: "---\nname: good\ndescription: d\n---\nbody\n",
        }));
        let skill = fake_registry_skill("good-skill", "fake");

        let result = manager
            .install(&skill, tmp.path())
            .expect("loadable skill installs");
        assert!(result.dir.join("SKILL.md").exists());
    }

    #[test]
    fn install_failed_reinstall_keeps_previous_skill_dir() {
        // A failed RE-install must not delete the previously installed skill
        // dir, even when the newly written SKILL.md fails validation.
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("exists");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: old\ndescription: d\n---\nold body\n",
        )
        .unwrap();

        let mut manager = RegistryManager { adapters: vec![] };
        manager.register(Box::new(FakeAdapter {
            name: "fake",
            skill_md: "---\nname: bad\ndescription: d\ncontext: fork\n---\nbody\n",
        }));
        let skill = fake_registry_skill("exists", "fake");

        let err = manager
            .install(&skill, tmp.path())
            .expect_err("unloadable reinstall must still fail");
        assert!(err.to_string().contains("agent"));
        assert!(
            dir.exists(),
            "previous skill dir must survive a failed reinstall"
        );
        // The original content must be byte-for-byte intact (not overwritten by
        // the failed replacement) and the original trusted state preserved.
        let preserved = std::fs::read_to_string(dir.join("SKILL.md"))
            .expect("original SKILL.md must survive a failed reinstall");
        assert_eq!(
            preserved, "---\nname: old\ndescription: d\n---\nold body\n",
            "failed reinstall must not overwrite the previous skill content"
        );
        assert!(
            !dir.join(crate::models::UNTRUSTED_MARKER).exists(),
            "original trust state must be restored (no quarantine marker)"
        );
    }
}
