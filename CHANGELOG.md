<!-- markdownlint-disable MD024 -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Fixed packaged skills so they are compile-time embedded in the `aish` binary and loaded for every user (including bare-root installs), instead of seeding `~/.config/aish/skills` at install time (which skipped root and could leave root-owned config dirs).
- Hardened the packaging smoke test to assert the installer/bundle no longer ship or invoke skill seeding.
- Show a one-line terminal tip on the interactive session that performs the temporary legacy seed migration (only when skills were actually moved).

### Removed

- Removed the `seed-skills.sh` installer helper; packaged skills no longer need a post-install copy into user config.

### Deprecated

- The one-shot migration of pre-embed install-seeded skills (`migrate_seeded`, backup dir `~/.config/aish/migrated-seeded-skills/`, marker `.skills-seed-migrated-v1`) is temporary and will be removed in a future release once leftover seeds are uncommon.

### Notes for releasers

- Review whether to remove `crates/aish-skills/src/migrate_seeded.rs` (one-shot legacy install-seed migration). If keeping it another cycle, mention the deprecation again in that release's notes; if removing, list it under Removed and delete the module + call site in `SkillManager::load_all_skills`.

## [0.3.8] - 2026-07-22

### Added

- Added `@path` file-mention popup in AI mode with fuzzy search, Tab longest-common-prefix completion, and directory drill-down.
- Added a CJK-aware Markdown rendering pipeline (`aish-md-table` + `md_render`) for width-aware tables, inline styles, nested lists, and terminal wrapping.
- Added response-footer compaction markers (`⟳compacted` / `⟳micro`) when automatic context compaction runs during a turn.

### Changed

- Renamed the response-footer token label from `ctx` to `req` to reflect per-request window usage rather than cumulative conversation history.
- Added shared HTTP retry with exponential backoff for transient 429/5xx and network errors across OpenAI, Anthropic, and Codex providers.

### Fixed

- Fixed the inline completion spinner so CJK terminals no longer shift the `aish` mode icon.
- Fixed PTY detach after session exit to skip the Detach write and avoid Broken-pipe WARN noise (Ctrl+Q still sends Detach).
- Fixed packaged skills seeding to write into `~/.config/aish/skills` as the target user (no `/usr/local` install + leaf-only chown), including a one-shot repair for leftover root-owned config trees from older installers.
- Hardened the packaging ownership smoke test to require seed/install scripts and assert privilege drop instead of fakeroot ownership checks.

## [0.3.7] - 2026-07-20

### Added

- Added the Agent tool and built-in sub-agents (`explore`, `plan`, `general-purpose`, `troubleshoot`), including spawn progress in the shell TUI and isolated skill execution through spawn.
- Added an interactive `/setting` panel for config editing, with a flat single-panel layout, choice picker, category memory, and slash-command prefill.
- Added a centralized theme system with display polish and session management improvements.
- Added audit logging for shell and agent activity.
- Added PTY daemon attach architecture for session persistence across reconnects.
- Added inline AI completion with ghost text in the interactive shell.
- Added `PromptAssembly` to unify MainChat and sub-agent system prompt and tool spec assembly.

### Changed

- Moved per-tool routing guidance into tool descriptions; tool usage remains in tool prompts. Oracle delegates tool choice to descriptions (no duplicated routing tables). If you customize `~/.config/aish/prompts/oracle.md`, remove duplicated tool-selection sections that mirror tool descriptions.
- Added explicit routing between `Agent(subagent_type=plan)` and `enter_plan_mode`; `enter_plan_mode` keeps routing in its description and usage-only text in its prompt appendix.
- Expanded built-in sub-agent system prompts (explore/plan/general-purpose) with read-only rules and efficient search strategy; Agent `prompt` schema now requires scope and thoroughness.
- Converted embedded LLM prompt templates (`oracle`, `cmd_error`, `failure_diagnose`) to English-only; SSH error-correction context injection is English as well.
- Migrated `/diagnose` to the troubleshoot sub-agent spawn path.

### Fixed

- Fixed `web_fetch` HTML entity decoding to a single pass to prevent double-decoding.
- Fixed PTY Ctrl+C handling to kill the foreground process group so pagers unblock reliably.
- Fixed PTY bash rcfile handling to use a unique temp file created with mode `0600`.
- Fixed a PTY file-descriptor leak and sub-shell echo behavior.
- Fixed sub-agent tool inheritance from the parent and abort-on-Ctrl+C cancellation.
- Fixed the sub-agent UI flag reset when the Agent tool ends.
- Fixed packaged skills seeding so install overwrites bundled skill files as intended.
- Fixed the setup wizard so the config directory is created before the first-run save.
- Fixed diagnose confirm-execute to use `has_alternatives` when presenting choices.
- Fixed Release Preparation preflight so `resolve-release-pr` preserves `--pr-number` after YAML parsing.

### Removed

- Removed unused embedded prompt templates (`error_detect`, `system_diagnose`, `guess_command`) and stale copies under `crates/aish-shell/prompts/`.
- Removed legacy `LlmSession` prompt filtering APIs in favor of `PromptAssembly`.
- Removed `docs/skills-guide.md`.

## [0.3.6] - 2026-07-03

### Added

- Added `/diagnose` for read-only failure diagnosis when shell commands are blocked.
- Added read-only mode support for the bash tool, with hardened parsing for sudo segments and escapes.
- Added bash-style multi-column tab completion with a custom pager.
- Added environment-aware remote PS1 with danger escalation, including SSH host and git branch display.
- Added a welcome changelog panel on startup.
- Added a cliclack-based setup wizard with Codex OAuth login and OpenClaw-aligned auth flows.
- Added bundled ops skills that seed to the user directory on install.
- Added a multi API dialect registry for Anthropic and Codex, with streaming via an OpenAI SSE bridge.

### Changed

- Improved setup wizard UX, provider copy, and localized verification error messages.
- Normalized model IDs for setup/runtime and adjusted default `max_tokens`.

### Fixed

- Fixed read-only bash enforcement gaps in the parser and session.
- Fixed remote path detection for `host:/path` syntax and stdin draining before `--More--` prompts.
- Fixed `send_command_interactive` blocking after sudo/PAM authentication.
- Fixed PTY `PRINTF_ERASE` chunk-boundary leak.
- Fixed Codex auth, SSE streaming, and Responses adapter `temperature` / `max_tokens` passthrough.

## [0.3.5] - 2026-06-24

### Added

- Added `/record start|stop` for asciinema v2 session recording, including real-time PTY output and error-correction flow capture.
- Added native bash Tab completion via control-pipe JSON, with readline forwarding and Bash 4.2 compatibility.
- Added InputGuard pre-execution screening for shell commands and AI prompts (Allow / Confirm / Block), including SSH/PTY sessions.
- Added `/status` to display system environment (hostname, OS, CPU, memory, network, services, errors) in local and remote SSH modes.
- Added `/doctor` for parallel environment diagnostics (config, API key, dirs, session, tools, skills, memory, connectivity).

### Changed

- Improved slash-command popup Enter/Tab UX, viewport stability, and arrow navigation.
- Improved SSH error correction with exit codes, remote host context, and broader shell error-prefix support.
- Added release profile size optimizations (LTO, strip).

### Fixed

- Fixed cast replay by preserving standalone `\r` and recording Ctrl+C events.
- Fixed Tab completion timeout for slow native completions (e.g. systemctl).
- Fixed Tab completion on Bash 4.2 (CentOS 7).

### Removed

- Removed deprecated dead code from the Rust runtime (LiteLLMClient, legacy diagnose agent, state_capture).

## [0.3.4] - 2026-06-08

### Added

- Added multimodal image support so user messages can attach local image files and send them as structured content blocks to compatible models.
- Added an interactive `/feedback` command that collects system context, redacts sensitive log lines, and opens a pre-filled GitHub issue in the browser.
- Added `/help` with topic-based help pages and markdown rendering, plus `/quit` as an exit alias for the interactive shell.
- Added a `web_fetch` tool so the agent can retrieve and summarize web page content during a session.
- Added ESC handling in readline to cancel the current input line without leaving the shell.

### Changed

- Changed the feedback issue template to include bug-report sections such as steps to reproduce, expected behavior, and actual behavior.
- Changed the project license metadata from MIT to Apache-2.0 so packaging files match the shipped `LICENSE`.

### Fixed

- Fixed confirmation panel rendering so the right border draws correctly in the terminal UI.
- Fixed feedback log attachment so oversized bodies keep as many recent log lines as possible instead of dropping logs entirely when the GitHub issue URL would exceed the length limit.

## [0.3.3] - 2026-06-03

### Added

- Added a slash-command suggestion popup and a built-in `/feedback` command so interactive command discovery is faster inside the shell.
- Added ESC interrupt handling, richer keyboard events, and improved inline dialogs and panels for interactive shell flows.

### Changed

- Changed `ask_user` interactions to use the new dialog flow with clearer validation, cancel handling, and localized prompt copy.

### Fixed

- Fixed PTY cleanup on shell shutdown so background terminal resources are released more reliably.
- Fixed prompt cwd refresh after `ai bash` changes directory so the shell prompt stays in sync.
- Fixed `ask_user` default matching and validation reporting so trimmed defaults and structured errors behave consistently.
- Fixed slash popup anchoring so the menu stays attached to the active prompt line.


## [0.3.2] - 2026-05-29

### Added

- Added official PyPI packaging for stable Linux amd64 and arm64 releases, including a packaged `aish` launcher that installs via `pip`.
- Added release workflow steps to build, smoke test, and publish PyPI artifacts alongside the existing bundle release assets.

### Changed

- Changed self-update and uninstall flows to detect pip-based installations and preserve the original install channel details when upgrading.

### Fixed

- Fixed release CI follow-up issues around formatting, lint gates, and the sandbox worker test so the new PyPI release path can pass the full validation pipeline reliably.

## [0.3.1] - 2026-05-29

### Fixed

- Fixed competing stdin readers around `ask_user` and related interactive flows so prompts no longer fight with shell input watchers.
- Fixed UTF-8 slicing bugs that could panic on non-ASCII paths or truncated error bodies during setup and prompt rendering.

## [0.3.0] - 2026-05-26

### Added

- Added nested SSH session detection with stronger interrupt handling so remote interactive sessions can be identified and interrupted more reliably.
- Added a host dossier pipeline and the `host_note` AI tool so per-host notes and profile data can persist across sessions.

### Changed

- Changed the Rust PTY and secure-bash flow to better support nested remote session execution and follow-up tool work.

### Fixed

- Fixed `host_note` persistence so profile-save failures are surfaced instead of being reported as success.
- Fixed Rust CI and clippy regressions introduced by the SSH and host-dossier changes.

## [0.3.0-beta.3] - 2026-05-13

### Added

- Added nested SSH session detection with stronger interrupt handling so remote interactive sessions can be identified and interrupted more reliably.
- Added a host dossier pipeline and the `host_note` AI tool so per-host notes and profile data can persist across sessions.

### Changed

- Changed the Rust PTY and secure-bash flow to better support nested remote session execution and follow-up tool work.

### Fixed

- Fixed `host_note` persistence so profile-save failures are surfaced instead of being reported as success.
- Fixed Rust CI and clippy regressions introduced by the SSH and host-dossier changes.

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
