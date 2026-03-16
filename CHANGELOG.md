<!-- markdownlint-disable MD024 -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] - 2026-03-14

### Added

- Added a provider abstraction layer for OAuth-backed integrations, including a reusable provider registry and shared OAuth helpers.
- Added regression coverage for release metadata extraction so tagged releases read notes from the versioned changelog section.

### Changed

- Changed the release pipeline to be fully tag-driven by removing the manual Release PR workflow and creating GitHub Releases directly from stable tag pushes.
- Changed OpenAI Codex provider internals to use the shared provider/OAuth architecture for future provider expansion.

### Fixed

- Fixed LiteLLM provider tool calls so forwarded tool parameters reach the provider correctly.

## [0.1.1] - 2026-03-13

### Added

- Added built-in OpenAI Codex OAuth support.
- Added a unified Release Preparation workflow that validates the target version and dry-runs release bundles before the final release.
- Added tag-driven GitHub Release publishing for stable `vX.Y.Z` pushes.

### Changed

- Changed the release pipeline to publish Linux binary bundles for amd64 and arm64 from stable git tags, with install and smoke-test verification in CI.
- Changed release metadata handling so version normalization, versioned changelog extraction, and previous-tag discovery are generated consistently for release workflows.

### Removed

- Removed Debian package publishing from the release path; new releases should be installed from the published binary bundle instead of a .deb package.

### Fixed

- Fixed bundle install and uninstall scripts so packaged binaries, services, and install layout are handled consistently.
- Fixed Linux bundle smoke tests to match the installer layout used by release artifacts.
- Fixed startup welcome screen rendering regressions.
- Fixed official website redirection failures.
## [0.1.0] - 2025-12-29

### Added

- Initial public project structure.

### Changed

- Established the first project release and Debian packaging baseline.
