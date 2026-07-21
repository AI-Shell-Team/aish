#!/usr/bin/env bash
# Regression: sudo-style seed must leave ~/.config/aish owned by the target
# user so config.yaml can be created (not only skill leaves).
# Also assert install-bundle no longer installs skills under /usr/local.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SEED_SCRIPT="$ROOT_DIR/packaging/scripts/seed-skills.sh"
INSTALL_SCRIPT="$ROOT_DIR/packaging/scripts/install-bundle.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

# --- static: no host /usr/local skills install path ---
if grep -E 'install_tree.*(/usr/local/share/aish/skills)|seed-skills\.sh".*(/usr/local/share/aish/skills)' \
  "$INSTALL_SCRIPT"; then
  echo "FAIL: install-bundle still installs or seeds via /usr/local/share/aish/skills" >&2
  exit 1
fi

if grep -qE 'seed-skills\.sh".*/usr/local' "$INSTALL_SCRIPT"; then
  echo "FAIL: install-bundle still seeds from /usr/local" >&2
  exit 1
fi

# --- ownership under fakeroot (simulates sudo install) ---
if ! command -v fakeroot >/dev/null 2>&1; then
  echo "seed-skills ownership smoke test passed (static checks only; fakeroot missing)."
  exit 0
fi

mkdir -p "$TMP_DIR/source/demo-skill"
cat >"$TMP_DIR/source/demo-skill/SKILL.md" <<'EOF'
---
name: demo-skill
---
demo
EOF

FAKE_USER="$(id -un)"
FAKE_HOME="$TMP_DIR/home/$FAKE_USER"
mkdir -p "$FAKE_HOME"

fakeroot env HOME="$FAKE_HOME" SUDO_USER= "$SEED_SCRIPT" "$TMP_DIR/source"

AISH_DIR="$FAKE_HOME/.config/aish"
owner="$(stat -c '%u' "$AISH_DIR")"
skills_owner="$(stat -c '%u' "$AISH_DIR/skills")"
# TARGET_USER is the real user (id -un); after chown, dirs must not be uid 0.
if [[ "$owner" == "0" ]] || [[ "$skills_owner" == "0" ]]; then
  echo "FAIL: ~/.config/aish still root-owned (aish=$owner skills=$skills_owner)" >&2
  find "$FAKE_HOME/.config" -printf '%u:%g %p\n' >&2 || true
  exit 1
fi

# Must be writable for config.yaml (the original user symptom).
touch "$AISH_DIR/config.yaml"

echo "seed-skills ownership smoke test passed."
