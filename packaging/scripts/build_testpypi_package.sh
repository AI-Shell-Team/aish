#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PACKAGE_TEMPLATE_DIR="$ROOT_DIR/packaging/pypi"
STAGE_DIR="${STAGE_DIR:-$ROOT_DIR/build/testpypi/package}"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist/testpypi}"
TARGET="${AISH_BUILD_TARGET:-x86_64-unknown-linux-musl}"
PYTHON_BIN="${PYTHON:-python3}"
NO_BUILD="${NO_BUILD:-0}"
REPOSITORY_LABEL="${AISH_PYPI_REPOSITORY_LABEL:-PyPI}"

platform_tag_for_target() {
  case "$1" in
    x86_64-unknown-linux-musl)
      printf '%s\n' "manylinux2014_x86_64"
      ;;
    aarch64-unknown-linux-musl)
      printf '%s\n' "manylinux2014_aarch64"
      ;;
    *)
      echo "Unsupported PyPI wheel target: $1" >&2
      return 1
      ;;
  esac
}

load_cargo_version() {
  grep -A5 '^\[workspace\.package\]' "$ROOT_DIR/Cargo.toml" \
    | grep '^version' \
    | head -1 \
    | sed 's/version.*=.*"\([^"]*\)".*/\1/'
}

VERSION="${VERSION:-}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(load_cargo_version)"
fi

PLATFORM_TAG="$(platform_tag_for_target "$TARGET")"

BINARY="$ROOT_DIR/target/$TARGET/release/aish"
if [[ "$NO_BUILD" != "1" ]]; then
  AISH_BUILD_TARGET="$TARGET" "$ROOT_DIR/build.sh"
elif [[ ! -x "$BINARY" ]]; then
  echo "Binary artifact missing: $BINARY" >&2
  exit 1
fi

rm -rf "$STAGE_DIR" "$DIST_DIR"
mkdir -p "$STAGE_DIR/src/aish_rust/bin" "$DIST_DIR"
cp -a "$PACKAGE_TEMPLATE_DIR/." "$STAGE_DIR/"

printf '__version__ = "%s"\n' "$VERSION" > "$STAGE_DIR/src/aish_rust/_version.py"
install -m 0755 "$BINARY" "$STAGE_DIR/src/aish_rust/bin/aish"

env \
  -u PIP_INDEX_URL \
  -u PIP_EXTRA_INDEX_URL \
  -u PIP_FIND_LINKS \
  -u PIP_NO_INDEX \
  -u PIP_CONFIG_FILE \
  AISH_PYPI_PLATFORM_TAG="$PLATFORM_TAG" \
  "$PYTHON_BIN" -m build --wheel --outdir "$DIST_DIR" "$STAGE_DIR"

echo "Built ${REPOSITORY_LABEL} package(s):"
ls -1 "$DIST_DIR"