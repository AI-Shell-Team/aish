# Contributing to AISH

Welcome to make Shell smarter!

## Quick Links

- **GitHub:** https://github.com/AI-Shell-Team/aish
- **Website:** https://aishell.ai
- **Documentation:** [CONFIGURATION.md](CONFIGURATION.md) · [QUICKSTART.md](QUICKSTART.md)
- **Issue Tracker:** https://github.com/AI-Shell-Team/aish/issues
- **Discussions:** https://github.com/AI-Shell-Team/aish/discussions

## Maintainers

- **AISH TEAM** - Project Lead
  - GitHub: [@AI-Shell-Team](https://github.com/AI-Shell-Team) · Email: team@aishell.ai 

<!-- TODO: Add more maintainers with their areas of responsibility
- **Name** - Area of responsibility
  - GitHub: [@username] · Contact info
-->

## How to Contribute

1. **Bugs & small fixes** → Open a PR!
2. **New features / architecture** → Start a [GitHub Discussion](https://github.com/AI-Shell-Team/aish/discussions) first
3. **Questions** → GitHub [Discussions](https://github.com/AI-Shell-Team/aish/discussions) or [Issues](https://github.com/AI-Shell-Team/aish/issues)

## Before You PR

- Test locally with your AISH instance: `uv sync && uv run aish`
- Run tests: `uv run pytest tests/ -v`
- Run formatting: `uv run black src/ tests/ && uv run isort src/ tests/`
- Check formatting: `uv run black --check src/ tests/`
- Run type checking: `uv run mypy src/`
- Ensure CI checks pass (if configured)
- Keep PRs focused (one thing per PR; do not mix unrelated concerns)
- Describe what & why in your PR description
- **Include screenshots** — one showing the problem/before, one showing the fix/after (for UI or visual changes)

## CI and Release Workflows

- Code PRs run lint, tests, and cross-platform smoke checks.
- Packaging-related PRs additionally run Linux bundle build, install smoke checks, and installed-binary runtime smoke checks.
- Release-candidate PRs are treated as publishable candidates: by convention they use a head branch like `release/vX.Y.Z-prep`, or they are explicitly marked with the `release-candidate` label. These PRs must pass bundle build, bundle install, and installed-binary runtime validation before merge.
- `Auto response` is the repository's community bot for Issues and PRs. Reply text lives in `.github/auto-response-config.json`, and runtime logic lives in `.github/scripts/auto-response.cjs`.
- `Release Metadata` is the shared release action that normalizes stable version inputs, validates repository version state, and uploads both markdown and JSON metadata artifacts.
- `make prepare-release-files VERSION=X.Y.Z [DATE=YYYY-MM-DD]` updates `pyproject.toml`, `src/aish/__init__.py`, `uv.lock`, and inserts a dated release section at the top of `CHANGELOG.md`.
- Prepare release files locally in a dedicated release PR. Use the standard release branch naming (`release/vX.Y.Z-prep`) when possible; if you need a different branch name, add the `release-candidate` label so CI still applies the release gate.
- Run `Release Preparation` against the release PR ref before merge. It validates release metadata, rebuilds dry-run bundles, reruns install and installed-binary smoke checks, and runs artifact-based live smoke with real provider credentials.
- Merge the release PR only after the release candidate gate and `Release Preparation` both pass.
- After merge, run `Release Final Check` as the lightweight final confirmation on `main`, then push the stable tag `vX.Y.Z` to trigger `Release`.
- `Release` is triggered by pushing a stable tag `vX.Y.Z`. It validates the tag against repository metadata, verifies that the tagged commit is on `main`, waits on the protected `release` environment approval gate, creates the GitHub Release entry with generated notes, and uploads bundle assets.
- Configure the GitHub Environment named `release` with required reviewers if you want manual approval before production publishing.

## Python Code Style

The project uses **Black** (formatting) and **isort** (import sorting). When writing code:

```python
# Type hints are required for function signatures
def process_command(command: str, timeout: int = 30) -> CommandResult:
    """Process a shell command with PTY execution."""
    ...

# Use Pydantic models for configuration
class ConfigModel(BaseModel):
    model: str
    api_base: str | None = None
    api_key: str | None = None

# Async functions should use anyio for structured concurrency
async def execute_with_pty(cmd: str) -> str:
    async with await_asyncio(openpty) as (master, slave):
        ...
```

The `pyproject.toml` is configured with Black and isort settings. Avoid changing these unless updating the tooling.

**Code comments:** Use English for all code comments (see CLAUDE.md).

**Dependencies:**
- Prefer `uv` over `pip` for package management
- Keep dependencies minimal and well-justified
- Vendor dependencies (like litellm) require explicit approval

## AI/Vibe-Coded PRs Welcome! 🤖

Built with Claude, Copilot, or other AI tools? **Awesome - just mark it!**

Please include in your PR:

- [ ] Mark as AI-assisted in the PR title or description
- [ ] Note the degree of testing (untested / lightly tested / fully tested)
- [ ] Include prompts or session logs if possible (super helpful!)
- [ ] Confirm you understand what the code does

AI PRs are first-class citizens here. We just want transparency so reviewers know what to look for.

## Current Focus & Roadmap 🗺

We are currently prioritizing:

- **Stability**: Fixing edge cases in PTY command execution and interactive program handling.
- **UX**: Improving the setup wizard (provider/endpoint selection) and error messages.
- **Provider Support**: Expanding multi-endpoint provider support (Z.AI, MiniMax, Moonshot, etc.).
- **Security**: Enhancing sandbox and risk assessment features.
- **Skills**: Expanding the skills/plugin ecosystem.

Check the [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) for "good first issue" labels!

## Maintainers

We're selectively expanding the maintainer team.
If you're an experienced contributor who wants to help shape AISH's direction — whether through code, docs, or community — we'd like to hear from you.

Being a maintainer is a responsibility, not an honorary title. We expect active, consistent involvement — triaging issues, reviewing PRs, and helping move the project forward.

Still interested? Email yinshuiboy@gmail.com with:

- Links to your PRs on AISH (if you don't have any, start there first)
- Links to open source projects you maintain or actively contribute to
- Your GitHub and other contact handles
- A brief intro: background, experience, and areas of interest
- Languages you speak and where you're based
- How much time you can realistically commit

We welcome people across all skill sets — engineering, documentation, community management, and more.
We review every application carefully and add maintainers slowly and deliberately.
Please allow a few weeks for a response.

## Report a Vulnerability

We take security reports seriously. Report vulnerabilities directly to the repository:

- **Core CLI** — [AI-Shell-Team/aish](https://github.com/AI-Shell-Team/aish)
- **Sandbox subsystem** — [AI-Shell-Team/aish](https://github.com/AI-Shell-Team/aish) (src/aish/security/sandbox)
- **Skills system** — [AI-Shell-Team/aish](https://github.com/AI-Shell-Team/aish) (src/aish/skills)

For security issues, email **team@aishell.ai** and we'll route it.

### Required in Reports

1. **Title**
2. **Severity Assessment**
3. **Impact**
4. **Affected Component**
5. **Technical Reproduction**
6. **Demonstrated Impact**
7. **Environment**
8. **Remediation Advice**

Reports without reproduction steps, demonstrated impact, and remediation advice will be deprioritized. Given the volume of AI-generated scanner findings, we must ensure we're receiving vetted reports from researchers who understand the issues.
