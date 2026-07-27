//! Skill installer: downloads individual skill files from GitHub via the
//! Contents API (avoids downloading multi-MB repo tarballs), with a tarball
//! fallback for repos that don't expose the standard `skills/` layout.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aish_core::{AishError, Result};

/// Maximum bytes accepted from a single registry download. Bounds memory so a
/// hostile registry cannot OOM the install process by streaming a huge body.
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Read a response body into a Vec, rejecting it if it exceeds
/// [`MAX_DOWNLOAD_BYTES`]. Checks Content-Length first when available, then
/// caps the actual read so a chunked stream with no length cannot OOM us.
fn read_bounded(response: reqwest::blocking::Response) -> Result<Vec<u8>> {
    if let Some(len) = response.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(AishError::Skill(format!(
                "Download too large: server reports {} bytes (limit {})",
                len, MAX_DOWNLOAD_BYTES
            )));
        }
    }
    use std::io::Read;
    let mut buf = Vec::new();
    // Read at most limit+1 bytes so we can detect an over-limit body even
    // when Content-Length is absent.
    response
        .take(MAX_DOWNLOAD_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| AishError::Skill(format!("Failed to read download body: {}", e)))?;
    if buf.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(AishError::Skill(format!(
            "Download too large: exceeded {} bytes limit",
            MAX_DOWNLOAD_BYTES
        )));
    }
    Ok(buf)
}

/// Result of a successful install.
#[derive(Debug, Clone)]
pub struct InstallResult {
    /// Directory where the skill was installed.
    pub dir: PathBuf,
    /// Name of the skill (from SKILL.md metadata or directory name).
    pub skill_name: String,
}

/// HTTP client used for all GitHub requests.
fn github_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent("aish-skill-installer")
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| AishError::Skill(format!("HTTP client build failed: {}", e)))
}

/// Build an authenticated request, adding the GitHub token header if
/// `GITHUB_TOKEN` is set in the environment.
macro_rules! gh_header {
    ($req:expr) => {{
        let mut r = $req.header("Accept", "application/vnd.github+json");
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            if !token.is_empty() {
                r = r.header("Authorization", format!("Bearer {}", token));
            }
        }
        r
    }};
}

/// Reject a slug that could escape the install directory.
///
/// Slugs originate from untrusted registry responses or user input and are
/// used directly as `target_dir.join(slug)`. Without this guard a value like
/// `..` or `a/../../etc` writes outside the skills directory (zip/tar-slip).
/// Rejects path separators, NUL, and the `.`/`..` components outright.
pub(crate) fn validate_install_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.contains('/')
        || slug.contains('\\')
        || slug.contains('\0')
        || slug == "."
        || slug == ".."
    {
        return Err(AishError::Skill(format!(
            "Unsafe skill slug rejected (path traversal): {:?}",
            slug
        )));
    }
    Ok(())
}

/// True if a relative path extracted from an untrusted source could escape
/// the install directory: an absolute path, a backslash separator, or a `..`
/// traversal component. Used by both the GitHub contents-API and the tarball
/// extraction paths so they apply an identical rule.
fn is_unsafe_rel_path(rel: &str) -> bool {
    rel.starts_with('/') || rel.contains('\\') || rel.contains("..")
}

/// Abort cleanly when the caller requested cancellation. Checked between file
/// downloads so an in-flight request still finishes, but no new one starts —
/// the granularity reqwest::blocking allows without runtime surgery.
fn check_cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(AishError::Skill("skill install cancelled".into()));
    }
    Ok(())
}

/// Install a skill from a GitHub repo by downloading only the skill directory
/// via the Contents API (fast, avoids large tarball downloads).
///
/// `owner_repo` is `"owner/repo"`.
/// `skill_slug` is the directory name under `skills/` in the repo.
/// Falls back to tarball extraction if the Contents API doesn't find the skill.
pub fn install_from_github(
    owner_repo: &str,
    skill_slug: &str,
    target_dir: &Path,
    cancel: &AtomicBool,
) -> Result<InstallResult> {
    validate_install_slug(skill_slug)?;
    // `owner/repo` must be a single path segment pair; reject traversal here too.
    if owner_repo.is_empty()
        || owner_repo.starts_with('/')
        || owner_repo.contains("..")
        || owner_repo.contains('\0')
        || owner_repo.matches('/').count() != 1
    {
        return Err(AishError::Skill(format!(
            "Invalid owner/repo specifier: {:?}",
            owner_repo
        )));
    }
    // Try the Contents API first — it downloads only the skill files.
    match install_via_contents_api(owner_repo, skill_slug, target_dir, cancel) {
        Ok(result) => return Ok(result),
        Err(e) => {
            // Always fall back to the tarball. The Contents API downloads each
            // file from raw.githubusercontent.com, but the tarball is served by
            // api.github.com — different hosts. A failure on one (a network
            // block on raw, a rate limit, or a not-found) does not predict the
            // other, and the tarball is the only path that works when raw is
            // unreachable. Trying it is cheap when it also fails.
            tracing::warn!(error = %e, "Contents API install failed; trying tarball fallback");
        }
    }

    // Fallback: download the full repo tarball and extract.
    install_via_tarball(owner_repo, skill_slug, target_dir, cancel)
}

/// Fetch a repo's default branch via the GitHub API.
fn get_default_branch(client: &reqwest::blocking::Client, owner_repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{}", owner_repo);
    let resp = gh_header!(client.get(&url))
        .send()
        .map_err(|e| AishError::Skill(format!("GitHub API request failed: {}", e)))?;

    if !resp.status().is_success() {
        return Err(AishError::Skill(format!(
            "GitHub API returned {} for {}",
            resp.status(),
            url
        )));
    }

    let json: serde_json::Value = resp
        .json()
        .map_err(|e| AishError::Skill(format!("Failed to parse repo info: {}", e)))?;

    json.get("default_branch")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AishError::Skill("Could not determine default branch".into()))
}

/// Download a single skill directory via the GitHub Git Trees API + raw URLs.
///
/// Uses 2 API calls (repo info + tree) plus N small file downloads, avoiding
/// the multi-MB tarball download entirely.
fn install_via_contents_api(
    owner_repo: &str,
    skill_slug: &str,
    target_dir: &Path,
    cancel: &AtomicBool,
) -> Result<InstallResult> {
    let client = github_client()?;

    let branch = get_default_branch(&client, owner_repo)?;

    // Fetch the recursive tree to find all files under skills/<slug>/.
    let tree_url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        owner_repo, branch
    );
    let resp = gh_header!(client.get(&tree_url))
        .send()
        .map_err(|e| AishError::Skill(format!("Git tree request failed: {}", e)))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(AishError::Skill(format!(
            "GitHub tree API returned {} for {}",
            status, tree_url
        )));
    }

    let tree_json: serde_json::Value = resp
        .json()
        .map_err(|e| AishError::Skill(format!("Failed to parse tree response: {}", e)))?;

    // GitHub truncates recursive trees above ~100k entries / 7MB. A truncated
    // tree may silently miss the skill or some of its files; fall back to the
    // complete tarball instead of risking a partial install.
    if tree_json
        .get("truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(AishError::Skill(format!(
            "Repository {} has a truncated git tree",
            owner_repo
        )));
    }

    // Collect all file paths matching skills/<slug>/...
    let prefix = format!("skills/{}/", skill_slug);
    let files: Vec<String> = tree_json
        .get("tree")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let path = entry.get("path").and_then(|p| p.as_str())?;
                    let entry_type = entry.get("type").and_then(|t| t.as_str())?;
                    if entry_type == "blob" && path.starts_with(&prefix) {
                        Some(path.to_string())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    if files.is_empty() {
        return Err(AishError::Skill(format!(
            "No files found under '{}' in repository {}",
            prefix, owner_repo
        )));
    }

    // Download each file via raw.githubusercontent.com.
    let dest_dir = target_dir.join(skill_slug);
    std::fs::create_dir_all(&dest_dir)?;
    let mut written = 0usize;

    for file_path in &files {
        check_cancelled(cancel)?;
        let rel = &file_path[prefix.len()..];

        // Security: reject path traversal in relative path.
        if is_unsafe_rel_path(rel) {
            tracing::warn!(path = file_path, "Skipping suspicious path");
            continue;
        }

        let dest = dest_dir.join(rel);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            owner_repo, branch, file_path
        );

        let resp = client
            .get(&raw_url)
            .send()
            .map_err(|e| AishError::Skill(format!("Failed to download {}: {}", file_path, e)))?;

        if !resp.status().is_success() {
            tracing::warn!(
                path = file_path,
                status = resp.status().as_u16(),
                "Skipping file"
            );
            continue;
        }

        let bytes = read_bounded(resp)?;

        std::fs::write(&dest, &bytes)?;
        written += 1;
    }

    if written == 0 {
        let _ = std::fs::remove_dir_all(&dest_dir);
        return Err(AishError::Skill(format!(
            "No files could be downloaded for skill '{}' (all downloads failed or were skipped)",
            skill_slug
        )));
    }

    if !dest_dir.join("SKILL.md").exists() {
        let _ = std::fs::remove_dir_all(&dest_dir);
        return Err(AishError::Skill(format!(
            "Skill '{}' installed but SKILL.md is missing (incomplete)",
            skill_slug
        )));
    }

    tracing::info!(
        slug = skill_slug,
        files = written,
        "Installed skill via Contents API"
    );

    Ok(InstallResult {
        dir: dest_dir,
        skill_name: skill_slug.to_string(),
    })
}

/// Fallback: download the full repo tarball and extract the skill directory.
/// Used when the Contents API doesn't find the skill (non-standard layout).
fn install_via_tarball(
    owner_repo: &str,
    skill_slug: &str,
    target_dir: &Path,
    cancel: &AtomicBool,
) -> Result<InstallResult> {
    let url = format!("https://api.github.com/repos/{}/tarball", owner_repo);

    tracing::info!(url = %url, slug = skill_slug, "Downloading GitHub tarball (fallback)");

    let client = github_client()?;
    let response = gh_header!(client.get(&url))
        .send()
        .map_err(|e| AishError::Skill(format!("GitHub tarball download failed: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
        let hint = if status.as_u16() == 403 || status.as_u16() == 429 {
            "GitHub API rate limit exceeded (60 requests/hour for unauthenticated users). \
             Wait a few minutes or set GITHUB_TOKEN to increase the limit."
        } else if status.as_u16() == 404 {
            "Repository not found. Check the skill ID and source."
        } else {
            ""
        };
        return Err(AishError::Skill(format!(
            "GitHub API returned {} for {}\n{}",
            status, url, hint
        )));
    }

    let bytes = read_bounded(response)?;

    extract_skill_from_tarball(&bytes, skill_slug, target_dir, cancel)
}

/// Extract a single skill directory from a GitHub tarball.
///
/// GitHub tarballs have a top-level directory like `owner-repo-abc123/`,
/// with skills under `owner-repo-abc123/skills/<skill-name>/`.
fn extract_skill_from_tarball(
    gzip_bytes: &[u8],
    skill_slug: &str,
    target_dir: &Path,
    cancel: &AtomicBool,
) -> Result<InstallResult> {
    let decoder = flate2::read::GzDecoder::new(gzip_bytes);
    let mut archive = tar::Archive::new(decoder);

    // Collect entries that match `*/skills/<skill_slug>/...`
    let prefix = format!("skills/{}/", skill_slug);

    std::fs::create_dir_all(target_dir.join(skill_slug))?;

    let mut found_any = false;
    for entry in archive
        .entries()
        .map_err(|e| AishError::Skill(format!("Tar archive entries error: {}", e)))?
    {
        check_cancelled(cancel)?;
        let mut entry =
            entry.map_err(|e| AishError::Skill(format!("Tar entry read error: {}", e)))?;

        let path = entry
            .path()
            .map_err(|e| AishError::Skill(format!("Tar entry path error: {}", e)))?;
        let path_str = path.to_string_lossy();

        // Anchor `skills/<slug>/` at a path-component boundary. A bare
        // substring `find` would match inside `xskills/<slug>/` and extract
        // files from an unrelated sibling directory. GitHub tarballs always
        // prefix paths with `owner-repo-sha/`, so the real segment is
        // preceded by `/` (also allow the path to start with it).
        let anchored_prefix = format!("/{}", prefix);
        let rel_idx = if path_str.starts_with(&prefix) {
            0
        } else {
            match path_str.find(&anchored_prefix) {
                Some(idx) => idx + 1,
                None => continue,
            }
        };
        {
            // The relative path within the skill directory.
            let rel = &path_str[rel_idx + prefix.len()..];

            if rel.is_empty() {
                continue; // The directory entry itself.
            }

            // Security: reject path traversal. `entry.unpack` writes to the
            // caller-computed `dest` verbatim (no canonicalization), so a `..`
            // component would escape the install dir.
            if is_unsafe_rel_path(rel) {
                tracing::warn!(path = %path_str, "Skipping suspicious tarball entry");
                continue;
            }

            // Security: reject non-regular entries. `entry.unpack` follows a
            // symlink/hardlink to its raw target with no bounds check (the tar
            // crate only confines writes inside `unpack_in`, never `unpack`),
            // so a symlink pointing outside the install dir would let a later
            // file entry escape via the link — arbitrary file write -> code
            // execution. Skills ship only regular files and directories.
            let entry_type = entry.header().entry_type();
            if entry_type.is_symlink() || entry_type.is_hard_link() {
                tracing::warn!(path = %path_str, "Skipping non-regular tarball entry");
                continue;
            }

            let dest = target_dir.join(skill_slug).join(rel);
            if entry_type.is_dir() {
                std::fs::create_dir_all(&dest)?;
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                entry.unpack(&dest).map_err(|e| {
                    AishError::Skill(format!("Failed to extract {}: {}", dest.display(), e))
                })?;
            }

            // Only count the entry once it has actually landed on disk.
            // Setting this before the safety checks would let a tarball of
            // all-skipped entries (every path unsafe or non-regular) report
            // success. A directory entry with a non-empty rel counts too.
            found_any = true;
        }
    }

    if !found_any {
        // Clean up the empty directory we created.
        let _ = std::fs::remove_dir_all(target_dir.join(skill_slug));
        return Err(AishError::Skill(format!(
            "Skill '{}' not found in repository tarball (looked for '{}' prefix)",
            skill_slug, prefix
        )));
    }

    Ok(InstallResult {
        dir: target_dir.join(skill_slug),
        skill_name: skill_slug.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Native skillhub.cn installer (no npx/clawhub CLI needed)
// ---------------------------------------------------------------------------

/// skillhub `/api/v1/skills/<slug>/files` response.
#[derive(serde::Deserialize)]
struct SkillHubFilesResponse {
    files: Vec<SkillHubFileEntry>,
}

#[derive(serde::Deserialize)]
struct SkillHubFileEntry {
    path: String,
}

/// Install a skill from skillhub.cn by downloading individual files via the
/// native API. No `npx clawhub` dependency required.
///
/// API flow:
/// 1. `GET <base>/api/v1/skills/<slug>/files` → file listing
/// 2. `GET <base>/api/v1/skills/<slug>/file?path=<path>` → file content (302 redirect)
pub fn install_via_skillhub_api(
    slug: &str,
    base_url: &str,
    target_dir: &Path,
    cancel: &AtomicBool,
) -> Result<InstallResult> {
    validate_install_slug(slug)?;
    let client = reqwest::blocking::Client::builder()
        .user_agent("aish-skill-installer")
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AishError::Skill(format!("HTTP client build failed: {}", e)))?;

    // 1. Fetch file listing.
    let files_url = format!(
        "{}/api/v1/skills/{}/files",
        base_url,
        super::url_encode(slug)
    );
    tracing::info!(url = %files_url, slug = slug, "Fetching skillhub file listing");

    let resp = client
        .get(&files_url)
        .send()
        .map_err(|e| AishError::Skill(format!("skillhub files request failed: {}", e)))?;

    if !resp.status().is_success() {
        return Err(AishError::Skill(format!(
            "skillhub files API returned {} for {}",
            resp.status(),
            files_url
        )));
    }

    let files_resp: SkillHubFilesResponse = resp
        .json()
        .map_err(|e| AishError::Skill(format!("Failed to parse file listing: {}", e)))?;

    if files_resp.files.is_empty() {
        return Err(AishError::Skill(format!(
            "Skill '{}' has no files on skillhub",
            slug
        )));
    }

    // 2. Download each file.
    let dest_dir = target_dir.join(slug);
    std::fs::create_dir_all(&dest_dir)?;
    let mut written = 0usize;

    for entry in &files_resp.files {
        check_cancelled(cancel)?;
        let file_path = &entry.path;

        // Security: reject path traversal from the untrusted API. Reuse the
        // shared helper (also catches backslash) so all install paths apply an
        // identical rule, as the helper's doc comment promises.
        if is_unsafe_rel_path(file_path) {
            tracing::warn!(path = file_path, "Skipping suspicious path");
            continue;
        }

        let dest = dest_dir.join(file_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let dl_url = format!(
            "{}/api/v1/skills/{}/file?path={}",
            base_url,
            super::url_encode(slug),
            super::url_encode(file_path)
        );

        tracing::debug!(path = file_path, url = %dl_url, "Downloading skillhub file");

        let resp = client
            .get(&dl_url)
            .send()
            .map_err(|e| AishError::Skill(format!("Failed to download {}: {}", file_path, e)))?;

        if !resp.status().is_success() {
            tracing::warn!(
                path = file_path,
                status = resp.status().as_u16(),
                "Skipping file"
            );
            continue;
        }

        let bytes = read_bounded(resp)?;

        std::fs::write(&dest, &bytes)?;
        written += 1;
    }

    if written == 0 {
        let _ = std::fs::remove_dir_all(&dest_dir);
        return Err(AishError::Skill(format!(
            "No files could be downloaded for skill '{}' (all downloads failed or were skipped)",
            slug
        )));
    }

    if !dest_dir.join("SKILL.md").exists() {
        let _ = std::fs::remove_dir_all(&dest_dir);
        return Err(AishError::Skill(format!(
            "Skill '{}' installed but SKILL.md is missing (incomplete)",
            slug
        )));
    }

    tracing::info!(
        slug = slug,
        files = written,
        "Installed skill via skillhub API"
    );

    Ok(InstallResult {
        dir: dest_dir,
        skill_name: slug.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_skill_from_tarball_finds_prefix() {
        // Build a minimal gzip tar with owner-repo-abc/skills/my-skill/SKILL.md.
        let mut buf: Vec<u8> = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let data = b"---\nname: my-skill\ndescription: test\n---\nHello";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_size(data.len() as u64);
            file_header.set_mode(0o644);
            builder
                .append_data(
                    &mut file_header,
                    "owner-repo-abc/skills/my-skill/SKILL.md",
                    &data[..],
                )
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let result =
            extract_skill_from_tarball(&buf, "my-skill", tmp.path(), &AtomicBool::new(false));
        assert!(result.is_ok(), "{:?}", result.err());
        let installed = result.unwrap();
        let skill_md = installed.dir.join("SKILL.md");
        assert!(skill_md.exists(), "SKILL.md should exist");
        let content = std::fs::read_to_string(&skill_md).unwrap();
        assert!(content.contains("name: my-skill"));
    }

    #[test]
    fn validate_install_slug_rejects_traversal() {
        // The slug is joined onto the user's skills directory verbatim, so
        // traversal payloads must be rejected outright.
        assert!(validate_install_slug("..").is_err());
        assert!(validate_install_slug(".").is_err());
        assert!(validate_install_slug("").is_err());
        assert!(validate_install_slug("a/b").is_err());
        assert!(validate_install_slug("a\\b").is_err());
        assert!(validate_install_slug("a\0b").is_err());
        // Ordinary slugs are accepted (including dots inside the name).
        assert!(validate_install_slug("my-skill").is_ok());
        assert!(validate_install_slug("under_score.v2").is_ok());
    }

    #[test]
    fn is_unsafe_rel_path_detects_traversal() {
        // Ordinary relative paths are safe (including dots inside names).
        assert!(!is_unsafe_rel_path("SKILL.md"));
        assert!(!is_unsafe_rel_path("scripts/run.sh"));
        assert!(!is_unsafe_rel_path("docs/a.b.md"));
        // Traversal, absolute, and backslash paths must be flagged.
        assert!(is_unsafe_rel_path("../escape"));
        assert!(is_unsafe_rel_path("a/../../etc"));
        assert!(is_unsafe_rel_path("/etc/passwd"));
        assert!(is_unsafe_rel_path("a\\b"));
        assert!(is_unsafe_rel_path("sub/.."));
    }

    #[test]
    fn check_cancelled_reflects_flag() {
        let off = AtomicBool::new(false);
        assert!(check_cancelled(&off).is_ok());
        let on = AtomicBool::new(true);
        let err = check_cancelled(&on).unwrap_err().to_string();
        assert!(err.contains("cancelled"), "got: {err}");
    }

    #[test]
    fn extract_skill_from_tarball_aborts_when_cancelled() {
        // A pre-set cancel flag must abort extraction before writing files.
        let mut buf: Vec<u8> = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let data = b"x";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            builder
                .append_data(&mut h, "repo/skills/sk/SKILL.md", &data[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let result = extract_skill_from_tarball(&buf, "sk", tmp.path(), &cancel);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cancelled"), "got: {err}");
        assert!(!tmp.path().join("sk").join("SKILL.md").exists());
    }

    #[test]
    fn extract_skill_from_tarball_rejects_symlink_escape() {
        // A malicious tarball bundles a symlink entry pointing outside the
        // skill dir, then a regular file written *through* that link. The
        // extractor must skip the symlink so the file cannot escape.
        let escape = tempfile::tempdir().unwrap();
        let escape_target = escape.path().join("stolen");
        std::fs::create_dir_all(&escape_target).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);

            // Hand-craft a symlink entry (bypassing Builder::append_link's own
            // target checks) to model a hostile archive: the entry path is a
            // benign "lnk", but its link target points outside the install dir.
            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::symlink());
            link_header.set_path("repo/skills/sk/lnk").unwrap();
            link_header.set_link_name(&escape_target).unwrap();
            link_header.set_size(0);
            link_header.set_mode(0o777);
            link_header.set_cksum();
            let mut empty = std::io::empty();
            builder.append(&link_header, &mut empty).unwrap();

            // Regular file written *through* the link.
            let data = b"escaped";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_size(data.len() as u64);
            file_header.set_mode(0o644);
            builder
                .append_data(&mut file_header, "repo/skills/sk/lnk/SKILL.md", &data[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let result = extract_skill_from_tarball(&buf, "sk", tmp.path(), &AtomicBool::new(false));
        assert!(result.is_ok(), "{:?}", result.err());

        // The symlink was skipped, so `lnk` is at most a plain directory and
        // nothing was written through it to the escape target.
        let lnk = tmp.path().join("sk").join("lnk");
        assert!(!lnk.is_symlink(), "a symlink leaked into the install dir");
        assert!(
            !escape_target.join("SKILL.md").exists(),
            "file escaped the install dir via the symlink"
        );
    }
}
