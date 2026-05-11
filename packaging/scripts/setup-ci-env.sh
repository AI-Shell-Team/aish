#!/usr/bin/env bash
set -euo pipefail

# Set up CI build environment for Rust + musl.
#
# NOTE: GitHub Actions workflows use dtolnay/rust-toolchain instead of this
# script. This file is kept for self-hosted runners or container environments
# where the action is not available.

ensure_rust_target() {
    local target="$1"

    if command -v rustup >/dev/null 2>&1; then
        rustup target add "$target"
        return 0
    fi

    if rustc --print target-libdir --target "$target" >/dev/null 2>&1; then
        return 0
    fi

    echo "Rust target $target is not available and rustup is not installed" >&2
    exit 1
}

if command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y curl build-essential musl-tools pkg-config libssl-dev
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y curl gcc openssl-devel
    dnf install -y pkgconf-pkg-config || dnf install -y pkgconf || true
elif command -v yum >/dev/null 2>&1; then
    yum install -y curl gcc openssl-devel
    yum install -y pkgconfig || yum install -y pkgconf || true
else
    echo "No supported package manager found" >&2
    exit 1
fi

# Install Rust if not already available
if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

if [[ -n "${GITHUB_PATH:-}" && -d "$HOME/.cargo/bin" ]]; then
    echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
fi

ensure_rust_target x86_64-unknown-linux-musl

cargo --version
rustc --version
