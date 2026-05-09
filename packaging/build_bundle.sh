#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

load_cargo_version() {
  grep -A5 '^\[workspace\.package\]' "$ROOT_DIR/Cargo.toml" \
    | grep '^version' \
    | head -1 \
    | sed 's/version.*=.*"\([^"]*\)".*/\1/'
}

VERSION="${VERSION:-${1:-}}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(load_cargo_version)"
fi
ARCH="${ARCH:-${2:-amd64}}"
PLATFORM="${PLATFORM:-${4:-linux}}"
TARGET="${AISH_BUILD_TARGET:-x86_64-unknown-linux-musl}"
OUTPUT_DIR="${OUTPUT_DIR:-${3:-dist/release}}"
BUNDLE_NAME="aish-${VERSION}-${PLATFORM}-${ARCH}"
STAGE_DIR="build/bundle/${BUNDLE_NAME}"
ROOTFS_DIR="${STAGE_DIR}/rootfs"

# Build if binary is missing
BINARY="target/${TARGET}/release/aish"
if [[ ! -x "$BINARY" ]]; then
  echo "Binary artifact missing, building first..."
  AISH_BUILD_TARGET="$TARGET" ./build.sh
fi

rm -rf "$STAGE_DIR"
mkdir -p "$ROOTFS_DIR" "$OUTPUT_DIR"

# Install into rootfs using Makefile
make install NO_BUILD=1 DESTDIR="$ROOTFS_DIR" TARGET="$TARGET"

install -m 0755 packaging/scripts/install-bundle.sh "${STAGE_DIR}/install.sh"
install -m 0755 packaging/scripts/uninstall-bundle.sh "${STAGE_DIR}/uninstall.sh"
mkdir -p "${STAGE_DIR}/systemd"
install -m 0644 packaging/systemd/aish-sandbox.service.in "${STAGE_DIR}/systemd/aish-sandbox.service.in"
install -m 0644 packaging/systemd/aish-sandbox.socket "${STAGE_DIR}/systemd/aish-sandbox.socket"

cat > "${STAGE_DIR}/README.txt" <<EOF
AI Shell bundle ${VERSION} (${ARCH})

Install:
  sudo ./install.sh

The installer enables aish-sandbox.socket on systemd hosts. Set
AISH_SKIP_SYSTEMD=1 to install files without touching systemd.

Uninstall:
  sudo ./uninstall.sh
EOF

tar -C "$(dirname "$STAGE_DIR")" -czf "${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz" "$(basename "$STAGE_DIR")"
sha256sum "${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz" > "${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz.sha256"

echo "Created bundle: ${OUTPUT_DIR}/${BUNDLE_NAME}.tar.gz"
