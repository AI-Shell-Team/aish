## Summary

Describe the problem and fix in 2–5 bullets:

- Problem:
- Why it matters:
- What changed:
- What did NOT change (scope boundary):

## Change Type (select all)

- [ ] Bug fix
- [ ] Feature
- [ ] Refactor
- [ ] Docs
- [ ] Security hardening
- [ ] Chore/infra

## Scope (select all touched areas)

- [ ] Core shell / PTY (`src/aish/shell*.py`, `src/aish/shell_enhanced/`)
- [ ] AI agent / LLM (`src/aish/agents.py`, `src/aish/llm.py`, `src/aish/prompts.py`)
- [ ] Skills / Tools (`src/aish/skills/`, `src/aish/tools/`, `debian/skills/`)
- [ ] Security / Risk assessment (`src/aish/security/`)
- [ ] Sandbox (`src/aish/sandboxd.py`, `src/aish/security/sandbox*.py`)
- [ ] Configuration / Wizard (`src/aish/config.py`, `src/aish/wizard/`)
- [ ] CLI / Interface (`src/aish/cli.py`, `src/aish/help_manager.py`)
- [ ] I18N (`src/aish/i18n/`)
- [ ] Observability (`src/aish/logging_utils.py`, `src/aish/session_store.py`)
- [ ] Packaging / Installation (`aish.spec`, `build.sh`, `debian/`)
- [ ] CI/CD / Infra (`.github/`)
- [ ] Documentation (`docs/`, `*.md`)

## Linked Issue/PR

- Closes #
- Related #

## User-visible / Behavior Changes

List user-visible changes (including defaults/config).
If none, write `None`.

## Security Impact (required)

- New security risk levels or policy changes? (`Yes/No`)
- Secrets/tokens handling changed? (`Yes/No`)
- New/changed network calls (LLM providers)? (`Yes/No`)
- Command execution surface changed? (`Yes/No`)
- Sandbox isolation changed? (`Yes/No`)
- Data access scope changed? (`Yes/No`)
- If any `Yes`, explain risk + mitigation:

## Repro + Verification

### Environment

- OS:
- Python version:
- Model/provider tested:
- Install method (deb/built from source/uv run):

### Steps

1.
2.
3.

### Expected

-

### Actual

-

## Evidence

Attach at least one:

- [ ] Failing test/log before + passing after
- [ ] Test output (pytest)
- [ ] Log snippets (~/.config/aish/logs/aish.log)
- [ ] Screenshot/recording

## Human Verification (required)

What you personally verified (not just CI), and how:

- Verified scenarios:
- Edge cases checked:
- What you did **not** verify:

## Compatibility / Migration

- Backward compatible? (`Yes/No`)
- Config changes? (`Yes/No` - if yes, describe migration)
- Security policy changes? (`Yes/No` - if yes, describe impact)
- If migration needed, exact steps:

## Failure Recovery (if this breaks)

- How to disable/revert this change quickly:
- Files/config to restore:
- Known bad symptoms reviewers should watch for:

## Risks and Mitigations

List only real risks for this PR. Add/remove entries as needed. If none, write `None`.

- Risk:
  - Mitigation:
