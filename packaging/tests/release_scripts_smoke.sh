#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON_BIN="${PYTHON:-python3}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

SANDBOX="$TMP_DIR/repo"
mkdir -p "$SANDBOX/packaging/scripts"
cp \
  "$ROOT_DIR/packaging/scripts/release_metadata.py" \
  "$ROOT_DIR/packaging/scripts/update_release_files.py" \
  "$SANDBOX/packaging/scripts/"

cat > "$SANDBOX/Cargo.toml" <<'EOF'
[workspace.package]
version = "0.2.0"
EOF

cat > "$SANDBOX/Cargo.lock" <<'EOF'
[[package]]
name = "aish-core"
version = "0.2.0"

[[package]]
name = "serde"
version = "1.0.228"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "demo"
EOF

cat > "$SANDBOX/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- Upcoming beta release note

## [0.2.0] - 2026-04-03

### Added

- Previous stable note
EOF

"$PYTHON_BIN" "$SANDBOX/packaging/scripts/update_release_files.py" \
  --version 1.0.0-beta.1 \
  --date 2026-05-09

grep -Fq 'version = "1.0.0-beta.1"' "$SANDBOX/Cargo.toml"
grep -Fq '## [1.0.0-beta.1] - 2026-05-09' "$SANDBOX/CHANGELOG.md"
grep -Fq 'Upcoming beta release note' "$SANDBOX/CHANGELOG.md"
grep -Fq 'name = "aish-core"' "$SANDBOX/Cargo.lock"
grep -Fq 'version = "1.0.0-beta.1"' "$SANDBOX/Cargo.lock"
grep -Fq 'name = "serde"' "$SANDBOX/Cargo.lock"
grep -Fq 'version = "1.0.228"' "$SANDBOX/Cargo.lock"

if "$PYTHON_BIN" "$SANDBOX/packaging/scripts/update_release_files.py" \
  --version 1.0.0-beta.1 \
  --date 2026-05-09 > "$TMP_DIR/duplicate.out" 2>&1; then
  echo "Expected duplicate changelog version to be rejected" >&2
  exit 1
fi
grep -Fq 'already contains a section for version 1.0.0-beta.1' "$TMP_DIR/duplicate.out"

cat > "$SANDBOX/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- No unreleased changes yet.

## [1.0.0-beta.1] - 2026-05-09

### Changed

- Beta release note
EOF

"$PYTHON_BIN" "$SANDBOX/packaging/scripts/update_release_files.py" \
  --version 1.0.0 \
  --date 2026-05-10

grep -Fq '## [1.0.0] - 2026-05-10' "$SANDBOX/CHANGELOG.md"
grep -Fq 'Beta release note' "$SANDBOX/CHANGELOG.md"
grep -Fq '## [Unreleased]' "$SANDBOX/CHANGELOG.md"
grep -Fq -- '- No unreleased changes yet.' "$SANDBOX/CHANGELOG.md"

cat > "$SANDBOX/Cargo.toml" <<'EOF'
[workspace.package]
version = "1.0.0-beta.1"
EOF

cat > "$SANDBOX/CHANGELOG.md" <<'EOF'
# Changelog

## [1.0.0-beta.1] - 2026-05-09

### Changed

- Beta release note

## [0.2.0] - 2026-04-03

### Added

- Previous stable note
EOF

"$PYTHON_BIN" "$SANDBOX/packaging/scripts/release_metadata.py" \
  --expected-version v1.0.0-beta.1 \
  --json-file "$TMP_DIR/metadata.json" \
  --summary-file "$TMP_DIR/summary.md"

grep -Fq '"version": "1.0.0-beta.1"' "$TMP_DIR/metadata.json"
grep -Fq '"tag": "v1.0.0-beta.1"' "$TMP_DIR/metadata.json"
grep -Fq 'Beta release note' "$TMP_DIR/summary.md"

"$PYTHON_BIN" "$SANDBOX/packaging/scripts/release_metadata.py" \
  --print-json > "$TMP_DIR/metadata-default.json"
grep -Fq '"version": "1.0.0-beta.1"' "$TMP_DIR/metadata-default.json"
grep -Fq 'Beta release note' "$TMP_DIR/metadata-default.json"

if "$PYTHON_BIN" "$SANDBOX/packaging/scripts/release_metadata.py" \
  --expected-version 01.0.0 > "$TMP_DIR/invalid.out" 2>&1; then
  echo "Expected invalid semver input to be rejected" >&2
  exit 1
fi
grep -Fq "Invalid version '01.0.0'" "$TMP_DIR/invalid.out"

echo "Release script smoke tests passed."