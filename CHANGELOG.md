<!-- markdownlint-disable MD024 -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-04-15

### Added

- Added an interactive plan mode for non-trivial tasks, including persisted plan artifacts, review and approval flow, and an explicit transition back into execution. Enter plan mode with `/plan` or `F2`, then leave it with `/plan exit` or `F2` when you want to return to normal shell execution.
- Added persistent long-term memory backed by Markdown storage, with memory recall and store tooling that can carry forward user preferences and project context across sessions.
- Added `aish update` and `aish uninstall` commands so archive, pip, and system-package installs have a built-in path for upgrade and removal.

### Changed

- Changed the terminal UI to show a visible thinking timer while the model is working, making longer requests easier to track.
- Changed startup and session wiring so plan mode, memory, and the new CLI management flows are initialized more consistently from the current package layout.

### Fixed

- Fixed the interactive shell so `quit` works again as an exit alias.
- Fixed compact prompt theme spacing and related prompt rendering regressions in the shell UI.
- Fixed release automation paths so preparation and publishing workflows target the current repository layout.

## [0.2.0] - 2026-04-03

### Added

- Added `aish models usage` so the CLI can show the current model, resolved provider, credential source or auth state, and provider dashboard entry.
- Added `prompt_theme` configuration for reusable shell prompt styles on top of the existing prompt scripting support.
- Added opt-in live smoke coverage for real provider credentials and installed bundle verification before release.

### Changed

- Changed the shell architecture from the old `shell.py` plus `shell_enhanced` and `tui` helpers into dedicated `shell/runtime`, `shell/ui`, `shell/pty`, shared `pty`, and `interaction` modules.
- Changed the interactive shell flow to use explicit backend control events and editing phases, improving multiline input, completions, confirmation panels, ask_user dialogs, and recovery after long-running terminal sessions.
- Changed model auth entry so `aish models auth` is the primary command path, while the old `login` path remains as a compatibility alias.

### Removed

- Removed the unfinished plan, research, think, and old TUI-oriented code paths from the active shell implementation.

### Fixed

- Fixed Ctrl+C handling for AI operations and interactive PTY sessions so control returns to the shell more predictably after interruptions.
- Fixed false error hints for normal SIGPIPE-based pager exits such as quitting `less`.
- Fixed packaged bundle startup by including the bash wrapper assets required by the PTY shell.

### Security

- Fixed a history command injection vulnerability in the shell execution path.

## [0.1.3] - 2026-03-19

### Added

- Added shell prompt scripting support with built-in templates, examples, and hot reload so prompts can be customized without modifying core code.
- Added full localized interface coverage for German, Spanish, French, Japanese, and Chinese alongside the existing English experience.

### Changed

- Changed the setup wizard to better guide provider configuration with clearer loading feedback during key assignment and verification.
- Changed assistant response rendering to use a more compact message box layout for long replies in the terminal UI.

### Fixed

- Fixed transient OpenAI Codex request failures by retrying temporary upstream errors during provider requests.
- Fixed sandbox startup and IPC routing so sandboxed execution remains reliable in both normal and frozen binary environments.
- Fixed missing localized labels for sandbox approval actions in non-English interfaces.

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
