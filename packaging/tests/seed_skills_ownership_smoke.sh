#!/usr/bin/env bash
# Regression coverage for embedded packaged skills:
#   1) install-bundle must not seed skills into user home
#   2) install-bundle must not install a host /usr/share or /usr/local skills tree
#   3) bundle build must not ship seed-skills.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALL_SCRIPT="$ROOT_DIR/packaging/scripts/install-bundle.sh"
BUILD_BUNDLE="$ROOT_DIR/packaging/build_bundle.sh"
SEED_SCRIPT="$ROOT_DIR/packaging/scripts/seed-skills.sh"

if [[ ! -f "$INSTALL_SCRIPT" ]]; then
  echo "FAIL: missing install script: $INSTALL_SCRIPT" >&2
  exit 1
fi

if [[ -e "$SEED_SCRIPT" ]]; then
  echo "FAIL: seed-skills.sh should be removed (skills are embedded): $SEED_SCRIPT" >&2
  exit 1
fi

if grep -qE 'seed-skills\.sh' "$INSTALL_SCRIPT"; then
  echo "FAIL: install-bundle must not invoke seed-skills.sh (skills are embedded)" >&2
  exit 1
fi

if [[ ! -f "$BUILD_BUNDLE" ]]; then
  echo "FAIL: missing build bundle script: $BUILD_BUNDLE" >&2
  exit 1
fi
if grep -qE 'seed-skills\.sh' "$BUILD_BUNDLE"; then
  echo "FAIL: build_bundle must not ship seed-skills.sh" >&2
  exit 1
fi

# Match forbidden destinations (not one exact install command form).
if grep -E '/usr/(local/)?share/aish/skills' "$INSTALL_SCRIPT"; then
  echo "FAIL: install-bundle still installs skills under /usr/share or /usr/local/share" >&2
  exit 1
fi

echo "embedded skills packaging smoke test passed."
