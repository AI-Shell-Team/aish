#!/usr/bin/env bash
# Build release packages using nfpm
# https://nfpm.goreleaser.com/
#
# Usage:
#   ./packaging/build_with_nfpm.sh [deb|rpm|apk]
#   ./packaging/build_with_nfpm.sh all  # Build all formats
#
# Environment variables:
#   VERSION: Override version (default: extract from pyproject.toml)
#   RELEASE: Package release number (default: 1)
#   ARCH: Target architecture (default: current system arch)

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Default values
VERSION="${VERSION:-}"
RELEASE="${RELEASE:-1}"
ARCH="${ARCH:-$(uname -m)}"

# Map architecture names
case "$ARCH" in
  x86_64)
    NFPM_ARCH="amd64"
    ;;
  aarch64)
    NFPM_ARCH="arm64"
    ;;
  armv7l)
    NFPM_ARCH="armhf"
    ;;
  *)
    NFPM_ARCH="$ARCH"
    ;;
esac

usage() {
  cat <<EOF
🚀 AI Shell Release Builder with nfpm

Usage: $0 [OPTIONS] [FORMAT]

Formats:
  deb       Debian/Ubuntu package
  rpm       RHEL/Fedora/CentOS package
  apk       Alpine package
  all       Build all formats (default)

Options:
  -v, --version VERSION   Package version (default: from pyproject.toml)
  -r, --release RELEASE   Package release number (default: 1)
  -a, --arch ARCH         Target architecture (default: $(uname -m))
  -h, --help              Show this help

Examples:
  $0                      # Build all formats with default version
  $0 deb                  # Build only .deb package
  $0 -v 1.2.3 rpm         # Build.rpm with version 1.2.3
  $0 -v 1.2.3 -r 2 deb    # Build.deb with version 1.2.3-2

Environment Variables:
  VERSION                 Override package version
  RELEASE                 Package release number
  ARCH                    Target architecture
EOF
}

# Parse arguments
while [[ $# -gt 0 ]]; do
  case "$1" in
    -v|--version)
      VERSION="$2"
      shift 2
      ;;
    -r|--release)
      RELEASE="$2"
      shift 2
      ;;
    -a|--arch)
      ARCH="$2"
      NFPM_ARCH="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
   deb|rpm|apk|all)
      FORMAT="$1"
      shift
      ;;
    *)
     echo -e "${RED}❌ Unknown option: $1${NC}"
      usage
      exit 1
      ;;
  esac
done

FORMAT="${FORMAT:-all}"

# Get version from pyproject.toml if not specified
if [ -z "$VERSION" ]; then
  VERSION=$(grep '^version' pyproject.toml | head -1 | cut -d'"' -f2)
  if [ -z "$VERSION" ]; then
    VERSION="0.0.0"
  fi
fi

echo -e "${BLUE}📦 AI Shell Release Builder${NC}"
echo -e "   Version: ${GREEN}${VERSION}${NC}"
echo -e "   Release: ${GREEN}${RELEASE}${NC}"
echo -e "   Arch:    ${GREEN}${NFPM_ARCH}${NC}"
echo -e "   Format:  ${GREEN}${FORMAT}${NC}"
echo ""

# Check prerequisites
echo -e "${BLUE}🔍 Checking prerequisites...${NC}"

if ! command -v nfpm &> /dev/null; then
  echo -e "${RED}❌ nfpm not found!${NC}"
  echo -e "   Install it with:"
  echo -e "     pip install nfpm"
  echo -e "   Or download from: https://github.com/goreleaser/nfpm/releases"
  exit 1
fi

if [ ! -f "dist/aish" ] || [ ! -f "dist/aish-sandbox" ]; then
  echo -e "${YELLOW}⚠️  Binaries not found in dist/${NC}"
  echo -e "${BLUE}🔨 Building binaries...${NC}"
  ./build.sh
fi

echo -e "${GREEN}✅ Prerequisites check passed${NC}"
echo ""

# Create output directory
OUTPUT_DIR="releases"
mkdir -p "$OUTPUT_DIR"

# Build function
build_package() {
  local pkg_type=$1
  
  echo -e "${BLUE}🔨 Building ${pkg_type} package...${NC}"
  
  # Export environment variables for nfpm
  export VERSION
  export RELEASE
  export NFPM_ARCH
  
  # Build with nfpm
  if nfpm pkg -f nfpm.yaml -p "$pkg_type" -p "$OUTPUT_DIR"; then
   echo -e "${GREEN}✅ ${pkg_type} package built successfully!${NC}"
  else
   echo -e "${RED}❌ Failed to build ${pkg_type} package${NC}"
    return 1
  fi
}

# Build requested packages
case "$FORMAT" in
  all)
    build_package "deb"
    build_package "rpm"
    build_package "apk"
    ;;
  *)
    build_package "$FORMAT"
    ;;
esac

echo ""
echo -e "${GREEN}🎉 Release packages built successfully!${NC}"
echo -e "   Output directory: ${BLUE}${OUTPUT_DIR}${NC}"
echo ""
echo -e "${YELLOW}📦 Generated packages:${NC}"
ls -lh "$OUTPUT_DIR"/*.{deb,rpm,apk} 2>/dev/null || echo "   No packages found"
echo ""
echo -e "${YELLOW}📋 Next steps:${NC}"
echo -e "   Install .deb:  sudo dpkg -i ${OUTPUT_DIR}/aish_${VERSION}_*.deb"
echo -e "   Install .rpm:  sudo rpm -ivh ${OUTPUT_DIR}/aish-${VERSION}-*.rpm"
echo -e "   Install .apk:  sudo apk add --allow-untrusted ${OUTPUT_DIR}/aish_${VERSION}_*.apk"
