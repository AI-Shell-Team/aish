<!-- markdownlint-disable MD024 -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.12] - 2026-09-03

### Added

- `/doctor` Shell Compatibility checker: identity, startup files, startup speed, environment inheritance (proxy values redacted), secret detection (set/unset only), PTY bash compatibility probe, and alias/function/completion counts. `aish doctor --json` and `/doctor --json` emit machine-readable results.
- Live-session resource monitoring: `/live_sessions` shows CPU% and RSS per session; configurable alerts when a detached session exceeds CPU/RSS thresholds (`pty_resource_check_interval_secs` / `_cpu_percent` / `_rss_mb`). Killing a session now terminates the full worker process tree with pidfd-pinned SIGTERM then SIGKILL so recycled PIDs are not hit.
- Long-term memory: confirm before store/forget, provenance, expiry/TTL, `/memory` (list / verify / forget), and a category-aware TTL policy with an explicit `permanent` flag.
- `/audit export`: portable audit package (`audit.md` + `events.jsonl` + `manifest.json`) with `--until` and `--session` filters, SHA-256 hashes, and owner-only permissions.
- System-level security policy: `/etc/aish/security_policy.yaml` takes full precedence over the user-level `~/.config/aish/security_policy.yaml`.

### Changed

- `glob` tool uses pruned DFS (skips `target` / `node_modules` / … at traversal time), a 60s wall-clock budget, Ctrl+C cancellation, and `spawn_blocking`. Cancellation returns partial results so the agent does not retry into another cancelled walk. Absolute patterns start at the longest wildcard-free prefix instead of scanning from `/`.
- Session database (`sessions.db`) and its WAL/SHM siblings are created or tightened to owner-only `0600` on open.

### Fixed

- `web_fetch` returns a structured failure (`ok=false`, `http_status`) on HTTP 4xx/5xx instead of treating error pages as success, caching them, and sending them to the secondary LLM.
- `/doctor` no longer hangs forever in the REPL: probe subprocesses are detached from the controlling TTY so interactive bash cannot SIGTTOU-stop itself. Probe stderr noise is a warning rather than a fatal baseline error, and API connectivity treats 401/403/404 as reachable (only 5xx and network errors fail that check).
- Nested-confirm buffer truncation no longer panics on a UTF-8 character boundary (for example a large CJK MOTD inside nested SSH), and the kept tail is hard-capped.
- Sub-agent fatal outcomes preserve already-completed tool results and a structured `error_category`, so the parent can continue without redoing finished work.
- Main session now persists AI tool-call history, so a follow-up question can see what the AI just executed instead of misattributing those commands to the user.
- Thinking-animation frames no longer swallow tracing log lines, so LLM retry warnings stay visible.
- Ctrl+C now stops the sub-agent thinking animation thread; tool-result status lines no longer overlap leftover frames.

## [0.3.11] - 2026-08-18

### Added

- Startup update check in the interactive shell: after entering the REPL, if `cdn.aishell.ai` has a newer release, show a one-time omp-style tip (`check_update_on_startup`, default `true`; failures stay silent).
- Setup model picker shows the built-in local catalog immediately instead of blocking on `GET /models`; only Ollama, vLLM, and endpoints without a local catalog discover models online, and failed discovery no longer silently substitutes a built-in list.
- Refreshed the built-in local model catalog so known providers show current model IDs in setup.

### Changed

- `aish update` output is denser and less debug-like, progress placeholders are fixed, and the `y/N` confirm is dropped so at most one sudo prompt remains.

### Fixed

- WebFetch "remember this session" now keys on Host (not bash command memory), shows the full URL in the confirm panel, and treats `web_fetch` as an alias of `WebFetch` for skills and display.
- Chat Completions requests send `max_completion_tokens` so newer OpenAI-compatible gateways that reject `max_tokens` or treat a missing budget as `0` no longer break tool probes and live tool calls.
- Setup live model discovery strips ANSI/control sequences from untrusted HTTP error bodies before they reach the terminal.

## [0.3.10] - 2026-08-10

### Added

- Multi-source skill registry with search, install, verify, trust/quarantine, and an AI-callable `skill_install` tool (`/skill`, `aish skill`), including built-in adapters for `skills.sh` and `skillhub.cn`.
- Interactive `/skill` manager panel (Installed / Browse / Registries tabs) for search, install, trust, verify, remove, and registry enable/disable; complete-arg subcommands still run directly.
- `/model` multi-account rotation and interactive model picker: recoverable failures (429 / usage limits) rotate across configured API accounts.
- Session workflow commands: `/export [md]` (session → Markdown), `/fork` (branch a session while preserving the original), and `/sessions` (browse the session tree and switch).
- File-change snapshot store with `/undo [path]` and interactive `/rollback` to restore prior checkpoints in the current session; `write_file` / `edit_file` results report when a write is not undoable.
- Parallel execution for batches of independent `Agent` (sub-agent) tool calls.
- After upgrading, the next interactive `aish` launch shows a Keep-a-Changelog summary for every version between the previously seen release and the newly installed one (oldest → newest), sourced from the `CHANGELOG.md` embedded in the binary. A `~/.config/aish/last-changelog-version` marker tracks what has already been shown.
- Built-in local ops diagnostic skills: rewritten symptom-first `diagnose_system_lag`, plus `network-path-diagnose`, `dns-diagnose`, `nfs-cifs-mount-diagnose`, and `ssl-cert-toolkit`.

### Changed

- `security_policy.yaml` is the sole source of truth for security settings: `/setting` reads/writes the live `SecurityManager`, security fields are stripped from `config.yaml` on load (with a warning), and installs no longer write `/etc/aish` security config.
- Hardened the read-only bash classifier so harmless inspection redirects/globs are allowed while real write redirects are not misclassified as read-only.
- File-edit diffs render as centered hunks with bounded context so edits near the end of large files stay visible.

### Fixed

- Restored rich sandbox policy reasons (rule id / paths / human-facing `reason`) instead of generic HIGH-level block text; wired the security panel UI with a closed confirm layout and independent Paths / degraded-sandbox Note rows.
- Fixed PTY handling so terminal device-query responses (CPR/DA/DSR) no longer leak into stdin as garbage keystrokes (e.g. `0;115;0c`) under ssh/tmux/pager scenarios.
- CI always reports the Lint and test required check for docs-only PRs, and treats `Makefile` / `rust-toolchain.toml` as code changes.

## [0.3.9] - 2026-07-27

### Added

- Added session-scoped command approval memory when the sandbox is enabled: choosing "allow and don't ask again" remembers the same host + command for the rest of the session (`[a]`), with `/forget-approvals` to clear. Path-bearing commands hide the remember option; denials are never persisted, and `sudo` is not stripped during normalization so a non-root approval cannot auto-authorize the root variant.

### Changed

- Dropped unused crate dependencies, centralized local version pins under `workspace.dependencies`, and set MSRV to `1.89`.
- `aish update` now relies on CDN `/latest` instead of hard-depending on the GitHub Releases API after the CDN check, so rate-limit `403` responses no longer fail updates.

### Fixed

- Fixed packaged skills so they are compile-time embedded in the `aish` binary and loaded for every user (including bare-root installs), instead of seeding `~/.config/aish/skills` at install time (which skipped root and could leave root-owned config dirs).
- Made builtin skill materialization race-safe with unique staging directories and a `.complete` marker before rename.
- Hardened packaging smoke tests to assert the installer/bundle no longer ship or invoke skill seeding, and to fail closed when `build_bundle.sh` is missing.
- Show a one-line terminal tip on the interactive session that performs the temporary legacy seed migration (only when skills were actually moved).
- Fixed `/setting` sandbox and security toggles so they take effect immediately and sync to `security_policy.yaml` (atomic write with fallback when the policy directory is not writable).
- Fixed welcome panel label formatting (no duplicated colon) and right-border alignment under CJK locales / WeTTY.
- Restored PTY echo for cooked-mode user prompts so typed input is visible for confirms such as `aish update [y/N]`.
- Reject prerelease tags on the stable CDN channel so a mis-tagged stable `/latest` cannot offer beta builds when `include_pre_release` is false.
- Improved `aish uninstall` sudo authentication UX: authenticate before printing progress, suppress cancel-only sudo noise while surfacing real failures, avoid blank-line side effects on Ctrl+C, and localize the cancelled message (including French `Annulé`).

### Removed

- Removed the `seed-skills.sh` installer helper; packaged skills no longer need a post-install copy into user config.

### Deprecated

- The one-shot migration of pre-embed install-seeded skills (`migrate_seeded`, backup dir `~/.config/aish/migrated-seeded-skills/`, marker `.skills-seed-migrated-v1`) is temporary and will be removed in a future release once leftover seeds are uncommon.

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
