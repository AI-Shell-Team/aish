/// A single changelog entry extracted from CHANGELOG.md.
#[derive(Clone)]
pub struct ChangelogEntry {
    // Keep-a-Changelog categories: "Added", "Changed", "Fixed", "Removed",
    // "Deprecated", "Security". Stored as a free-form String because the
    // parser accepts whatever `### <word>` heading appears in the file.
    pub category: String,
    pub text: String, // description text (without leading "- ")
}

/// Embed the workspace root CHANGELOG.md at compile time.
const CHANGELOG_CONTENT: &str = include_str!("../../../CHANGELOG.md");

/// Parse the current version's changelog section from the embedded CHANGELOG.md.
///
/// Finds the `## [{version}]` heading and extracts list items under every
/// `### <Category>` subsection (Added, Changed, Fixed, Removed, Deprecated,
/// Security — the standard Keep-a-Changelog set). Categories the parser does
/// not recognise are still emitted; the caller decides how to render them.
pub fn parse_current_changelog(version: &str) -> Vec<ChangelogEntry> {
    let section = match extract_version_section(CHANGELOG_CONTENT, version) {
        Some(s) => s,
        None => return Vec::new(),
    };
    parse_section_entries(&section)
}

/// Extract the text between `## [{version}]` and the next `## [` heading.
fn extract_version_section(content: &str, version: &str) -> Option<String> {
    let heading = format!("## [{}]", version);
    let start = content.find(&heading)?;
    let after_heading = &content[start + heading.len()..];

    let end = after_heading
        .find("\n## [")
        .map(|i| start + heading.len() + i);
    let section_text = match end {
        Some(e) => &content[start..e],
        None => &content[start..],
    };

    Some(section_text.to_string())
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
"#;

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

    // Helper for testing: parse from a given string instead of the embedded file.
    fn parse_changelog_from(content: &str, version: &str) -> Vec<ChangelogEntry> {
        let section = extract_version_section(content, version);
        if section.is_none() {
            return Vec::new();
        }
        let section = section.unwrap();
        parse_section_entries(&section)
    }
}
