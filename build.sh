#!/usr/bin/env bash
# Build script for AI Shell binaries
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

echo "🚀 Building AI Shell binaries..."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

if [ ! -f "pyproject.toml" ]; then
    echo -e "${RED}❌ Error: pyproject.toml not found. Please run this script from the project root.${NC}"
    exit 1
fi

BUILD_VENV="${ROOT_DIR}/.build-venv"
TIKTOKEN_CACHE_DIR="${AISH_BUILD_TIKTOKEN_CACHE_DIR:-build/tiktoken_cache}"
USE_UV=0
if command -v uv >/dev/null 2>&1; then
    USE_UV=1
fi

if [ "$USE_UV" -eq 1 ]; then
    echo -e "${BLUE}📦 Syncing dependencies with uv...${NC}"
    uv sync
    PYTHON_RUN=(uv run python)
    PYINSTALLER_RUN=(uv run --with pyinstaller pyinstaller)
else
    if ! command -v python3 >/dev/null 2>&1; then
        echo -e "${RED}❌ Error: neither uv nor python3 is available.${NC}"
        exit 1
    fi

    echo -e "${BLUE}📦 Creating local build virtualenv...${NC}"
    python3 -m venv "$BUILD_VENV"
    "$BUILD_VENV/bin/python" -m pip install --upgrade pip
    "$BUILD_VENV/bin/python" -m pip install . pyinstaller
    PYTHON_RUN=("$BUILD_VENV/bin/python")
    PYINSTALLER_RUN=("$BUILD_VENV/bin/python" -m PyInstaller)
fi

# Clean previous builds
echo -e "${YELLOW}🧹 Cleaning previous builds...${NC}"
rm -rf dist/ build/ *.spec.backup

if [ "${AISH_CLEAN_TIKTOKEN_CACHE:-0}" = "1" ]; then
    echo -e "${YELLOW}🧹 Removing cached tiktoken data...${NC}"
    rm -rf "$TIKTOKEN_CACHE_DIR"
fi

echo -e "${BLUE}🧠 Prefetching tiktoken cache...${NC}"
"${PYTHON_RUN[@]}" packaging/prefetch_tiktoken_cache.py --cache-dir "$TIKTOKEN_CACHE_DIR"

# Build using PyInstaller spec file
echo -e "${BLUE}🔨 Building binary with PyInstaller...${NC}"
"${PYINSTALLER_RUN[@]}" aish.spec

# Check if build was successful
if [ -f "dist/aish" ] && [ -f "dist/aish-sandbox" ]; then
    echo -e "${GREEN}✅ Binary built successfully!${NC}"
    echo -e "${GREEN}📍 Location: dist/aish${NC}"
    echo -e "${GREEN}📍 Location: dist/aish-sandbox${NC}"
    
    # Get file sizes
    SIZE_MAIN=$(du -h dist/aish | cut -f1)
    SIZE_SANDBOX=$(du -h dist/aish-sandbox | cut -f1)
    echo -e "${GREEN}🔍 Size (aish): ${SIZE_MAIN}${NC}"
    echo -e "${GREEN}🔍 Size (aish-sandbox): ${SIZE_SANDBOX}${NC}"
    
    # Make executable
    chmod +x dist/aish dist/aish-sandbox
    
    # Test the binaries
    echo -e "${BLUE}🧪 Testing binaries...${NC}"
    if ./dist/aish --help > /dev/null 2>&1; then
        echo -e "${GREEN}✅ Binary test passed!${NC}"
        if ./dist/aish-sandbox --help > /dev/null 2>&1; then
            echo -e "${GREEN}✅ Sandbox binary test passed!${NC}"
        else
            echo -e "${YELLOW}⚠️  Sandbox binary has some issues but was built successfully${NC}"
        fi
        if command -v script >/dev/null 2>&1 && bash ./packaging/scripts/smoke-installed-aish.sh ./dist/aish > /dev/null 2>&1; then
            echo -e "${GREEN}✅ Interactive binary smoke passed!${NC}"
        else
            echo -e "${YELLOW}⚠️  Interactive binary smoke did not complete successfully${NC}"
        fi
    else
        echo -e "${YELLOW}⚠️  Binary has some issues but was built successfully${NC}"
        echo -e "${YELLOW}   (This may be due to LiteLLM/tiktoken packaging complexity)${NC}"
    fi
    
    echo ""
    echo -e "${YELLOW}📋 Usage:${NC}"
    echo -e "  ${GREEN}./dist/aish --help${NC}    # Show help"
    echo -e "  ${GREEN}./dist/aish run${NC}       # Start shell"
    echo -e "  ${GREEN}./dist/aish config${NC}    # Show config"
    echo ""
    echo -e "${YELLOW}📦 Deploy to Linux:${NC}"
    echo -e "  ${GREEN}scp dist/aish user@server:/usr/local/bin/${NC}"
    echo -e "  ${GREEN}ssh user@server 'chmod +x /usr/local/bin/aish'${NC}"
else
    echo -e "${RED}❌ Build failed! Expected binaries not found (dist/aish, dist/aish-sandbox).${NC}"
    exit 1
fi

echo -e "${GREEN}🎉 Build completed successfully!${NC}" 