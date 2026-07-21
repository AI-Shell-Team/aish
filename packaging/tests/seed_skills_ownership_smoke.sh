#!/usr/bin/env bash
# Regression coverage for skill seeding:
#   1) install-bundle must not install/seed via /usr/local/share/aish/skills
#   2) seed-skills must drop privileges for home-dir writes when root
#   3) seeding into a user HOME must create skills and allow config.yaml
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

# --- static: drop privileges for home-dir writes ---
if ! grep -q 'run_as_target mkdir' "$SEED_SCRIPT"; then
  echo "FAIL: seed-skills.sh must mkdir as TARGET_USER via run_as_target" >&2
  exit 1
fi
if ! grep -q 'run_as_target cp' "$SEED_SCRIPT"; then
  echo "FAIL: seed-skills.sh must cp as TARGET_USER via run_as_target" >&2
  exit 1
fi
if ! grep -qE 'runuser -u|sudo -u' "$SEED_SCRIPT"; then
  echo "FAIL: seed-skills.sh does not drop to TARGET_USER via runuser/sudo -u" >&2
  exit 1
fi
# Reject the old pattern: create as root then always chown -R the tree.
if grep -qE '^\s*mkdir -p "\$USER_SKILLS_DIR"' "$SEED_SCRIPT"; then
  echo "FAIL: seed-skills.sh still mkdir's USER_SKILLS_DIR as the invoking user/root" >&2
  exit 1
fi

# --- functional: seed as the current user into an isolated HOME ---
mkdir -p "$TMP_DIR/source/demo-skill"
cat >"$TMP_DIR/source/demo-skill/SKILL.md" <<'EOF'
---
name: demo-skill
---
demo
EOF

FAKE_HOME="$TMP_DIR/home/$(id -un)"
mkdir -p "$FAKE_HOME"

HOME="$FAKE_HOME" "$SEED_SCRIPT" "$TMP_DIR/source"

AISH_DIR="$FAKE_HOME/.config/aish"
SKILL_FILE="$AISH_DIR/skills/demo-skill/SKILL.md"
if [[ ! -f "$SKILL_FILE" ]]; then
  echo "FAIL: expected seeded skill at $SKILL_FILE" >&2
  find "$FAKE_HOME" -print >&2 || true
  exit 1
fi

# Original user symptom: must be able to create config.yaml in the config dir.
touch "$AISH_DIR/config.yaml"

echo "seed-skills ownership smoke test passed."
