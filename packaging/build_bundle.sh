#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

load_project_version() {
  python3 - "$ROOT_DIR/pyproject.toml" <<'PY'
from __future__ import annotations

import re
import sys


pyproject_path = sys.argv[1]
in_project_section = False

with open(pyproject_path, encoding="utf-8") as handle:
    for raw_line in handle:
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            in_project_section = line == "[project]"
            continue
        if in_project_section:
            match = re.match(r'^version\s*=\s*"([^"]+)"\s*$', line)
            if match:
                print(match.group(1))
                raise SystemExit(0)

raise SystemExit(f"Could not find project.version in {pyproject_path}")
PY
}

VERSION="${VERSION:-${1:-}}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(load_project_version)"
fi
ARCH="${ARCH:-${2:-$(uname -m)}}"
PLATFORM="${PLATFORM:-${4:-linux}}"
OUTPUT_DIR="${OUTPUT_DIR:-${3:-dist/release}}"
BUNDLE_NAME="aish-${VERSION}-${PLATFORM}-${ARCH}"
STAGE_DIR="build/bundle/${BUNDLE_NAME}"
ROOTFS_DIR="${STAGE_DIR}/rootfs"

if [[ ! -x "dist/aish" || ! -x "dist/aish-sandbox" ]]; then
  echo "Binary artifacts are missing, building them first..."
  make build-binary
fi

rm -rf "$STAGE_DIR"
mkdir -p "$ROOTFS_DIR" "$OUTPUT_DIR"

make install NO_BUILD=1 DESTDIR="$ROOTFS_DIR"

install -m 0755 packaging/scripts/install-bundle.sh "${STAGE_DIR}/install.sh"
install -m 0755 packaging/scripts/uninstall-bundle.sh "${STAGE_DIR}/uninstall.sh"

cat > "${STAGE_DIR}/README.txt" <<EOF
AI Shell bundle ${VERSION} (${ARCH})

Install:
  sudo ./install.sh

Uninstall:
  sudo ./uninstall.sh
EOF

tar -C "$(dirname "$STAGE_DIR")" -czf "${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz" "$(basename "$STAGE_DIR")"
sha256sum "${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz" > "${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz.sha256"

echo "Created bundle: ${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz"