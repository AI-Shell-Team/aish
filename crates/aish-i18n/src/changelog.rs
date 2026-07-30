/// A single changelog entry extracted from CHANGELOG.md.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogEntry {
    // Keep-a-Changelog categories: "Added", "Changed", "Fixed", "Removed",
    // "Deprecated", "Security". Stored as a free-form String because the
    // parser accepts whatever `### <word>` heading appears in the file.
    pub category: String,
    pub text: String, // description text (without leading "- ")
}

/// One Keep-a-Changelog version section (`## [x.y.z]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogVersionSection {
    pub version: String,
    pub entries: Vec<ChangelogEntry>,
}

/// Embed the workspace root CHANGELOG.md at compile time.
const CHANGELOG_CONTENT: &str = include_str!("../../../CHANGELOG.md");

/// Workspace root CHANGELOG.md embedded at compile time.
pub fn embedded_changelog() -> &'static str {
    CHANGELOG_CONTENT
}

/// Parse every Keep-a-Changelog section from the embedded file.
pub fn parse_embedded_changelog_sections() -> Vec<ChangelogVersionSection> {
    parse_changelog_sections(CHANGELOG_CONTENT)
}

/// Parse the current version's changelog section from the embedded CHANGELOG.md.
///
/// Finds the `## [{version}]` heading and extracts list items under every
/// `### <Category>` subsection (Added, Changed, Fixed, Removed, Deprecated,
/// Security — the standard Keep-a-Changelog set). Categories the parser does
/// not recognise are still emitted; the caller decides how to render them.
pub fn parse_current_changelog(version: &str) -> Vec<ChangelogEntry> {
    parse_changelog_version(CHANGELOG_CONTENT, version)
}

/// Parse list items for a single version section from arbitrary changelog text.
pub fn parse_changelog_version(content: &str, version: &str) -> Vec<ChangelogEntry> {
    let section = match extract_version_section(content, version) {
        Some(s) => s,
        None => return Vec::new(),
    };
    parse_section_entries(&section)
}

/// Parse every `## [version]` section in document order (typically newest-first).
///
/// Skips `[Unreleased]` and any heading that is not wrapped in `[...]`.
/// Empty version bodies are kept so callers can still enumerate the release range.
pub fn parse_changelog_sections(content: &str) -> Vec<ChangelogVersionSection> {
    let mut sections = Vec::new();
    let mut current_version: Option<String> = None;
    let mut current_body = String::new();

    let flush = |version: &Option<String>, body: &str, out: &mut Vec<ChangelogVersionSection>| {
        let Some(version) = version else {
            return;
        };
        if version.eq_ignore_ascii_case("Unreleased") {
            return;
        }
        // Keep empty version sections so update summaries can still name every
        // release in the current→latest range, even when a section has no bullets.
        let entries = parse_section_entries(body);
        out.push(ChangelogVersionSection {
            version: version.clone(),
            entries,
        });
    };

    for line in content.lines() {
        if let Some(version) = parse_version_heading(line) {
            flush(&current_version, &current_body, &mut sections);
            current_version = Some(version);
            current_body.clear();
            continue;
        }
        if current_version.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    flush(&current_version, &current_body, &mut sections);
    sections
}

/// Keep sections where `after < version <= through`, preserving document order.
pub fn select_changelog_range(
    sections: &[ChangelogVersionSection],
    after: &str,
    through: &str,
    mut cmp: impl FnMut(&str, &str) -> std::cmp::Ordering,
) -> Vec<ChangelogVersionSection> {
    let after = after.strip_prefix('v').unwrap_or(after);
    let through = through.strip_prefix('v').unwrap_or(through);

    sections
        .iter()
        .filter(|section| {
            let version = section.version.as_str();
            cmp(version, after) == std::cmp::Ordering::Greater
                && cmp(version, through) != std::cmp::Ordering::Greater
        })
        .cloned()
        .collect()
}

/// Extract the text between `## [{version}]` and the next `## [` heading.
///
/// Uses [`parse_version_heading`] so version matching is exact (avoids treating
/// `0.3.1` as a prefix of `0.3.10`).
fn extract_version_section(content: &str, version: &str) -> Option<String> {
    let mut collecting = false;
    let mut out = String::new();

    for line in content.lines() {
        if let Some(heading_version) = parse_version_heading(line) {
            if collecting {
                break;
            }
            if heading_version == version {
                collecting = true;
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        if collecting {
            out.push_str(line);
            out.push('\n');
        }
    }

    collecting.then_some(out)
}

fn parse_version_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("## [")?;
    let version = rest.split(']').next()?.trim();
    if version.is_empty() {
        return None;
    }
    Some(version.to_string())
}

/// Parse `### Category` subsections into entries.
fn parse_section_entries(section: &str) -> Vec<ChangelogEntry> {
    let mut entries = Vec::new();
    let mut current_category = String::new();

    for line in section.lines() {
        let trimmed = line.trim();

        if let Some(cat) = trimmed.strip_prefix("### ") {
            current_category = cat.trim().to_string();
            continue;
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            if !current_category.is_empty() {
                entries.push(ChangelogEntry {
                    category: current_category.clone(),
                    text: item.to_string(),
                });
            }
        }
    }

    entries
}

/// Compare version strings with SemVer prerelease ordering.
pub fn compare_changelog_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse_semver = |v: &str| semver::Version::parse(v.strip_prefix('v').unwrap_or(v));
    if let (Ok(a_version), Ok(b_version)) = (parse_semver(a), parse_semver(b)) {
        return a_version.cmp(&b_version);
    }

    let parse_parts = |v: &str| -> Vec<u64> {
        v.strip_prefix('v')
            .unwrap_or(v)
            .split('.')
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let a_parts = parse_parts(a);
    let b_parts = parse_parts(b);
    for i in 0..a_parts.len().max(b_parts.len()) {
        let a_val = a_parts.get(i).unwrap_or(&0);
        let b_val = b_parts.get(i).unwrap_or(&0);
        match a_val.cmp(b_val) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

const STARTUP_CHANGELOG_MAX_CHARS: usize = 8_000;
const STARTUP_CHANGELOG_MAX_ENTRIES_PER_VERSION: usize = 8;

fn category_badge(category: &str) -> &'static str {
    match category {
        "Added" => "\x1b[32m[+]\x1b[0m",
        "Changed" => "\x1b[33m[*]\x1b[0m",
        "Fixed" => "\x1b[31m[!]\x1b[0m",
        _ => "\x1b[2m[-]\x1b[0m",
    }
}

fn render_changelog_version_section(section: &ChangelogVersionSection) -> String {
    use crate::t_with_args;
    use std::collections::HashMap;

    let mut chunk = String::new();
    let version_label = {
        let mut args = HashMap::new();
        args.insert("version".to_string(), section.version.clone());
        t_with_args("shell.welcome2.changelog_summary_version", &args)
    };
    chunk.push('\n');
    chunk.push_str("\x1b[1m");
    chunk.push_str(&version_label);
    chunk.push_str("\x1b[0m\n");

    let show = section
        .entries
        .len()
        .min(STARTUP_CHANGELOG_MAX_ENTRIES_PER_VERSION);
    for entry in &section.entries[..show] {
        chunk.push_str("  ");
        chunk.push_str(category_badge(&entry.category));
        chunk.push(' ');
        chunk.push_str(&entry.text);
        chunk.push('\n');
    }
    if section.entries.len() > show {
        let mut args = HashMap::new();
        args.insert(
            "count".to_string(),
            (section.entries.len() - show).to_string(),
        );
        chunk.push_str("  \x1b[2m");
        chunk.push_str(&t_with_args("shell.welcome2.changelog_summary_more", &args));
        chunk.push_str("\x1b[0m\n");
    }
    chunk
}

/// Keep as many complete version chunks as fit in `budget`, preferring newer
/// versions. Returns the kept chunks in display order (oldest → newest) and
/// whether anything was omitted.
fn select_sections_preferring_latest(
    rendered_oldest_first: &[String],
    budget: usize,
) -> (Vec<&str>, bool) {
    if rendered_oldest_first.is_empty() {
        return (Vec::new(), false);
    }

    // Always try to keep the newest section, even if it alone exceeds the budget.
    let mut keep_from = rendered_oldest_first.len() - 1;
    let mut used = rendered_oldest_first[keep_from].len();
    while keep_from > 0 {
        let prev = keep_from - 1;
        let next_used = used + rendered_oldest_first[prev].len();
        if next_used > budget {
            break;
        }
        used = next_used;
        keep_from = prev;
    }

    let truncated = keep_from > 0;
    let kept = rendered_oldest_first[keep_from..]
        .iter()
        .map(String::as_str)
        .collect();
    (kept, truncated)
}

/// Format Keep-a-Changelog notes for `after → through` (exclusive of `after`,
/// inclusive of `through`) from the embedded CHANGELOG.md.
///
/// Display order is oldest → newest. When the char budget is exceeded, older
/// versions are dropped first so the newest (`through`) section is kept.
pub fn format_changelog_range_summary(after: &str, through: &str) -> Option<String> {
    use crate::{t, t_with_args};
    use std::collections::HashMap;

    let sections = parse_embedded_changelog_sections();
    let selected = select_changelog_range(&sections, after, through, compare_changelog_versions);
    if selected.is_empty() {
        return None;
    }

    let ordered: Vec<_> = selected.into_iter().rev().collect();
    let rendered_sections: Vec<String> = ordered
        .iter()
        .map(render_changelog_version_section)
        .collect();

    let mut out = String::new();
    out.push_str("\x1b[1;36m");
    out.push_str(&{
        let mut args = HashMap::new();
        args.insert(
            "current".to_string(),
            after.strip_prefix('v').unwrap_or(after).to_string(),
        );
        args.insert(
            "latest".to_string(),
            through.strip_prefix('v').unwrap_or(through).to_string(),
        );
        t_with_args("shell.welcome2.changelog_summary_title", &args)
    });
    out.push_str("\x1b[0m\n");

    let title_len = out.len();
    let budget = STARTUP_CHANGELOG_MAX_CHARS.saturating_sub(title_len);
    let (kept, truncated) = select_sections_preferring_latest(&rendered_sections, budget);

    for chunk in kept {
        out.push_str(chunk);
    }

    let overflowed = out.len() > STARTUP_CHANGELOG_MAX_CHARS;
    if overflowed {
        // Cut on a line boundary so a partial ANSI escape is never emitted.
        let end = out[..STARTUP_CHANGELOG_MAX_CHARS.min(out.len())]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        out.truncate(end);
    }
    if truncated || overflowed {
        out.push_str("\n\x1b[2m");
        out.push_str(&t("shell.welcome2.changelog_summary_truncated"));
        out.push_str("\x1b[0m\n");
    }

    Some(out)
}

/// Plain-text Ctrl+O body for a startup range summary (no ANSI).
pub fn format_changelog_range_plain(after: &str, through: &str) -> Option<String> {
    use crate::t_with_args;
    use std::collections::HashMap;

    let sections = parse_embedded_changelog_sections();
    let selected = select_changelog_range(&sections, after, through, compare_changelog_versions);
    if selected.is_empty() {
        return None;
    }
    let ordered: Vec<_> = selected.into_iter().rev().collect();
    let mut out = String::new();
    out.push_str(&{
        let mut args = HashMap::new();
        args.insert(
            "current".to_string(),
            after.strip_prefix('v').unwrap_or(after).to_string(),
        );
        args.insert(
            "latest".to_string(),
            through.strip_prefix('v').unwrap_or(through).to_string(),
        );
        t_with_args("shell.welcome2.changelog_summary_title", &args)
    });
    out.push('\n');
    for section in ordered {
        let mut args = HashMap::new();
        args.insert("version".to_string(), section.version.clone());
        out.push('\n');
        out.push_str(&t_with_args(
            "shell.welcome2.changelog_summary_version",
            &args,
        ));
        out.push('\n');
        let show = section
            .entries
            .len()
            .min(STARTUP_CHANGELOG_MAX_ENTRIES_PER_VERSION);
        for entry in &section.entries[..show] {
            out.push_str("- [");
            out.push_str(&entry.category);
            out.push_str("] ");
            out.push_str(&entry.text);
            out.push('\n');
        }
        if section.entries.len() > show {
            let mut more = HashMap::new();
            more.insert(
                "count".to_string(),
                (section.entries.len() - show).to_string(),
            );
            out.push_str(&t_with_args("shell.welcome2.changelog_summary_more", &more));
            out.push('\n');
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CHANGELOG: &str = r#"## [0.3.4] - 2026-06-08

### Added

- Added multimodal image support.
- Added interactive /feedback command.

### Changed

- Changed the feedback issue template.

### Fixed

- Fixed confirmation panel rendering.

## [0.3.3] - 2026-06-03

### Added

- Added a slash-command suggestion popup.

### Fixed

- Fixed PTY cleanup on shell shutdown.

## [0.3.2] - 2026-05-01

### Fixed

- Fixed an older bug.
"#;

    fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
        compare_changelog_versions(a, b)
    }

    #[test]
    fn test_parse_returns_current_version_entries() {
        let entries = parse_changelog_from(SAMPLE_CHANGELOG, "0.3.4");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].category, "Added");
        assert_eq!(entries[0].text, "Added multimodal image support.");
        assert_eq!(entries[1].category, "Added");
        assert_eq!(entries[2].category, "Changed");
        assert_eq!(entries[3].category, "Fixed");
    }

    #[test]
    fn test_parse_skips_other_versions() {
        let entries = parse_changelog_from(SAMPLE_CHANGELOG, "0.3.3");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].category, "Added");
        assert_eq!(entries[1].category, "Fixed");
    }

    #[test]
    fn test_parse_strips_leading_dash() {
        let entries = parse_changelog_from(SAMPLE_CHANGELOG, "0.3.4");
        assert!(!entries[0].text.starts_with('-'));
    }

    #[test]
    fn test_parse_unknown_version_returns_empty() {
        let entries = parse_changelog_from(SAMPLE_CHANGELOG, "99.0.0");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_empty_section_skipped() {
        let changelog = r#"## [0.1.0] - 2025-01-01

### Added

- No unreleased changes yet.

## [0.0.9] - 2024-01-01

### Added

- Something.
"#;
        let entries = parse_changelog_from(changelog, "0.1.0");
        // "No unreleased changes yet." is still a valid entry, just informational
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "No unreleased changes yet.");
    }

    #[test]
    fn test_real_changelog_current_version_has_entries() {
        // Guards against silent breakage when CHANGELOG.md format drifts or
        // when the embedded version no longer matches any section. If this
        // fires, either CHANGELOG.md needs a section for the current version
        // or the parser needs to be updated for the new format.
        let v = env!("CARGO_PKG_VERSION");
        let entries = parse_current_changelog(v);
        assert!(
            !entries.is_empty(),
            "current version {} has no changelog entries — check CHANGELOG.md format",
            v,
        );
        for entry in &entries {
            assert!(
                matches!(
                    entry.category.as_str(),
                    "Added" | "Changed" | "Fixed" | "Removed" | "Deprecated" | "Security"
                ),
                "unexpected category {:?} for version {}",
                entry.category,
                v,
            );
            assert!(
                !entry.text.is_empty(),
                "empty changelog text for version {}",
                v
            );
        }
    }

    #[test]
    fn test_parse_unreleased_section() {
        // "Unreleased" is a common Keep-a-Changelog section header that
        // must round-trip through the parser just like a numbered version.
        let changelog = r#"## [Unreleased]

### Added

- Sneak preview feature.

## [0.1.0] - 2025-01-01

### Added

- Released.
"#;
        let entries = parse_changelog_from(changelog, "Unreleased");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, "Added");
        assert_eq!(entries[0].text, "Sneak preview feature.");
    }

    #[test]
    fn test_extract_version_section_does_not_prefix_match() {
        let changelog = r#"## [0.3.10] - 2026-07-01

### Added

- Ten.

## [0.3.1] - 2026-06-01

### Added

- One.
"#;
        let entries = parse_changelog_version(changelog, "0.3.1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "One.");
        let ten = parse_changelog_version(changelog, "0.3.10");
        assert_eq!(ten.len(), 1);
        assert_eq!(ten[0].text, "Ten.");
    }

    #[test]
    fn test_parse_changelog_sections_keeps_empty_version() {
        let changelog = r#"## [0.2.0] - 2026-01-02

## [0.1.0] - 2026-01-01

### Fixed

- Older fix.
"#;
        let sections = parse_changelog_sections(changelog);
        assert_eq!(
            sections
                .iter()
                .map(|s| (s.version.as_str(), s.entries.len()))
                .collect::<Vec<_>>(),
            vec![("0.2.0", 0), ("0.1.0", 1)]
        );
    }

    #[test]
    fn test_parse_changelog_sections_skips_unreleased() {
        let changelog = r#"## [Unreleased]

### Added

- Not shipped yet.

## [0.2.0] - 2026-01-02

### Added

- Newer feature.

## [0.1.0] - 2026-01-01

### Fixed

- Older fix.
"#;
        let sections = parse_changelog_sections(changelog);
        assert_eq!(
            sections
                .iter()
                .map(|s| s.version.as_str())
                .collect::<Vec<_>>(),
            vec!["0.2.0", "0.1.0"]
        );
    }

    #[test]
    fn test_select_changelog_range_exclusive_after_inclusive_through() {
        let sections = parse_changelog_sections(SAMPLE_CHANGELOG);
        let selected = select_changelog_range(&sections, "0.3.2", "0.3.4", cmp_versions);
        assert_eq!(
            selected
                .iter()
                .map(|s| s.version.as_str())
                .collect::<Vec<_>>(),
            vec!["0.3.4", "0.3.3"]
        );
        assert!(selected.iter().all(|s| !s.entries.is_empty()));
    }

    #[test]
    fn test_format_changelog_range_summary_real_range() {
        let sections = parse_embedded_changelog_sections();
        assert!(
            sections.len() >= 3,
            "embedded CHANGELOG.md needs at least three released sections"
        );
        let latest = sections[0].version.as_str();
        let intermediate = sections[1].version.as_str();
        let current = sections[2].version.as_str();
        let summary = format_changelog_range_summary(current, latest)
            .expect("real changelog range should render");
        let heading = |v: &str| {
            let mut args = std::collections::HashMap::new();
            args.insert("version".to_string(), v.to_string());
            crate::t_with_args("shell.welcome2.changelog_summary_version", &args)
        };
        assert!(summary.contains(&heading(intermediate)));
        assert!(summary.contains(&heading(latest)));
        assert!(!summary.contains(&heading(current)));
    }

    #[test]
    fn test_select_sections_preferring_latest_drops_older_first() {
        let newest = "NEWEST_CHUNK".to_string();
        let older = "O".repeat(100);
        let rendered = vec![older, newest];
        let (kept, truncated) = select_sections_preferring_latest(&rendered, 20);
        assert!(truncated);
        assert_eq!(kept, vec!["NEWEST_CHUNK"]);

        let (kept_all, truncated_all) = select_sections_preferring_latest(&rendered, usize::MAX);
        assert!(!truncated_all);
        assert_eq!(kept_all.len(), 2);
        assert_eq!(kept_all[1], "NEWEST_CHUNK");
    }

    #[test]
    fn test_select_changelog_range_supports_v_prefix() {
        let sections = parse_changelog_sections(SAMPLE_CHANGELOG);
        let selected = select_changelog_range(&sections, "v0.3.3", "v0.3.4", cmp_versions);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].version, "0.3.4");
    }

    // Helper for testing: parse from a given string instead of the embedded file.
    fn parse_changelog_from(content: &str, version: &str) -> Vec<ChangelogEntry> {
        parse_changelog_version(content, version)
    }
}
