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

normalize_bundle_arch() {
  case "$1" in
    x86_64|amd64)
      printf 'amd64'
      ;;
    aarch64|arm64)
      printf 'arm64'
      ;;
    *)
      printf '%s' "$1"
      ;;
  esac
}

VERSION="${VERSION:-${1:-}}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(load_cargo_version)"
fi
ARCH="$(normalize_bundle_arch "${ARCH:-${2:-amd64}}")"
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

# Create placeholder aish-sandbox
SANDBOX_BIN="${ROOTFS_DIR}/usr/bin/aish-sandbox"
cat > "$SANDBOX_BIN" <<'SANDBOX'
#!/usr/bin/env bash
echo "aish-sandbox: not yet implemented in the Rust version" >&2
exit 1
SANDBOX
chmod 755 "$SANDBOX_BIN"

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
