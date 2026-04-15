#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 /path/to/aish" >&2
    exit 1
fi

BINARY_PATH="$1"
if [[ ! -x "$BINARY_PATH" ]]; then
    echo "aish binary is not executable: $BINARY_PATH" >&2
    exit 1
fi

if ! command -v script >/dev/null 2>&1; then
    echo "Missing required command: script" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

HOME_DIR="$TMP_DIR/home"
XDG_CONFIG_HOME_DIR="$TMP_DIR/xdg-config"
XDG_DATA_HOME_DIR="$TMP_DIR/xdg-data"
mkdir -p "$HOME_DIR" "$XDG_CONFIG_HOME_DIR" "$XDG_DATA_HOME_DIR"

INFO_OUTPUT="$({
    env \
        HOME="$HOME_DIR" \
        XDG_CONFIG_HOME="$XDG_CONFIG_HOME_DIR" \
        XDG_DATA_HOME="$XDG_DATA_HOME_DIR" \
        "$BINARY_PATH" info
} 2>&1)"

if [[ "$INFO_OUTPUT" != *"AI Shell"* ]]; then
    echo "Installed binary info output did not contain expected banner" >&2
    echo "$INFO_OUTPUT" >&2
    exit 1
fi

RUN_OUTPUT="$({
    printf 'exit\n' | env \
        HOME="$HOME_DIR" \
        XDG_CONFIG_HOME="$XDG_CONFIG_HOME_DIR" \
        XDG_DATA_HOME="$XDG_DATA_HOME_DIR" \
        script -qec "$BINARY_PATH run --model openai/gpt-4o-mini --api-key dummy" /dev/null
} 2>&1)"

if [[ "$RUN_OUTPUT" != *"AI Shell v"* ]]; then
    echo "Installed binary run smoke did not reach interactive shell banner" >&2
    echo "$RUN_OUTPUT" >&2
    exit 1
fi
