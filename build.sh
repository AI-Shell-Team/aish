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

restore_rust_path() {
    if [[ -f "$HOME/.cargo/env" ]]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    fi
}

ensure_rust_target() {
    local target="$1"

    if command -v rustup >/dev/null 2>&1; then
        if ! rustup target list --installed | grep -q "$target"; then
            echo -e "${YELLOW}Installing target $target...${NC}"
            rustup target add "$target"
        fi
        return 0
    fi

    if rustc --print target-libdir --target "$target" >/dev/null 2>&1; then
        return 0
    fi

    echo -e "${RED}Error: rustup is not available and target $target is not installed.${NC}"
    echo -e "${YELLOW}Install rustup or preinstall the Rust target before running build.sh.${NC}"
    exit 1
}

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
    restore_rust_path
fi

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    echo -e "${RED}Error: cargo and rustc must be available to build AISH.${NC}"
    echo -e "${YELLOW}Install Rust or ensure ~/.cargo/env is loadable in this environment.${NC}"
    exit 1
fi

echo -e "${BLUE}Building AI Shell (Rust)...${NC}"

# Check for musl target
if [[ "$TARGET" == *musl* ]]; then
    ensure_rust_target "$TARGET"

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
