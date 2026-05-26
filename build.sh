#!/usr/bin/env bash
# Build script for AI Shell (Rust)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

TARGET="${AISH_BUILD_TARGET:-x86_64-unknown-linux-musl}"

echo -e "${BLUE}Building AI Shell (Rust)...${NC}"

# Check for musl target
if [[ "$TARGET" == *musl* ]]; then
    if ! rustup target list --installed | grep -q "$TARGET"; then
        echo -e "${YELLOW}Installing target $TARGET...${NC}"
        rustup target add "$TARGET"
    fi

    if ! command -v musl-gcc &>/dev/null && ! dpkg -l musl-tools &>/dev/null 2>&1; then
        if command -v apt-get &>/dev/null; then
            echo -e "${YELLOW}Installing musl-tools...${NC}"
            sudo apt-get update && sudo apt-get install -y musl-tools
        elif command -v brew &>/dev/null; then
            echo -e "${RED}Error: musl cross-compilation on macOS requires a cross toolchain.${NC}"
            echo -e "${YELLOW}Install with: brew install filosottile/musl-cross/musl-cross${NC}"
            exit 1
        else
            echo -e "${RED}Error: musl-tools not found and no supported package manager detected.${NC}"
            echo -e "${YELLOW}Please install musl-tools or musl-gcc for your platform manually.${NC}"
            exit 1
        fi
    fi
fi

# Build release binary
echo -e "${BLUE}Compiling release binary ($TARGET)...${NC}"
cargo build --release --target "$TARGET"

BINARY="target/$TARGET/release/aish"

if [[ -f "$BINARY" ]]; then
    echo -e "${GREEN}Build successful!${NC}"
    SIZE=$(du -h "$BINARY" | cut -f1)
    echo -e "${GREEN}  Location: $BINARY${NC}"
    echo -e "${GREEN}  Size: $SIZE${NC}"

    # Quick smoke test
    echo -e "${BLUE}Running smoke test...${NC}"
    if "$BINARY" --help > /dev/null 2>&1; then
        echo -e "${GREEN}  Smoke test passed!${NC}"
    else
        echo -e "${YELLOW}  Warning: --help returned non-zero (may be expected for PTY binary)${NC}"
    fi
else
    echo -e "${RED}Build failed! Binary not found: $BINARY${NC}"
    exit 1
fi

echo -e "${GREEN}Build completed successfully!${NC}"
