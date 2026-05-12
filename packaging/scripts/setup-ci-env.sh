#!/usr/bin/env bash
set -euo pipefail

# Set up CI build environment for Rust + musl.
#
# NOTE: GitHub Actions workflows use dtolnay/rust-toolchain instead of this
# script. This file is kept for self-hosted runners or container environments
# where the action is not available.

if command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y curl build-essential musl-tools pkg-config libssl-dev
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y curl gcc musl-devel openssl-devel
elif command -v yum >/dev/null 2>&1; then
    yum install -y curl gcc openssl-devel
else
    echo "No supported package manager found" >&2
    exit 1
fi

# Install Rust if not already available
if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

rustup target add x86_64-unknown-linux-musl

cargo --version
rustc --version
