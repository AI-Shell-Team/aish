#!/usr/bin/env bash
set -euo pipefail

# Set up CI build environment for Rust + musl.
#
# NOTE: GitHub Actions workflows use dtolnay/rust-toolchain instead of this
# script. This file is kept for self-hosted runners or container environments
# where the action is not available.

default_build_target() {
    case "${RUNNER_ARCH:-$(uname -m)}" in
        X64|x86_64|amd64)
            printf 'x86_64-unknown-linux-musl\n'
            ;;
        ARM64|aarch64|arm64)
            printf 'aarch64-unknown-linux-musl\n'
            ;;
        *)
            printf 'x86_64-unknown-linux-musl\n'
            ;;
    esac
}

TARGET="${AISH_BUILD_TARGET:-$(default_build_target)}"

target_cc_wrapper_name() {
    case "$1" in
        x86_64-unknown-linux-musl)
            printf 'x86_64-linux-musl-gcc\n'
            ;;
        aarch64-unknown-linux-musl)
            printf 'aarch64-linux-musl-gcc\n'
            ;;
        *)
            return 1
            ;;
    esac
}

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

configure_musl_cc() {
    local target="$1"
    local wrapper_name
    local wrapper_dir
    local target_env_name
    local cargo_target_env_name
    local wrapper_path
    local compiler_bin

    if [[ "$target" != *musl* ]]; then
        return 0
    fi

    wrapper_name="$(target_cc_wrapper_name "$target")" || return 0
    wrapper_dir="${HOME}/.cargo/bin"
    wrapper_path="${wrapper_dir}/${wrapper_name}"
    mkdir -p "$wrapper_dir"

    if command -v "$wrapper_name" >/dev/null 2>&1; then
        wrapper_path="$(command -v "$wrapper_name")"
    else
        if command -v musl-gcc >/dev/null 2>&1; then
            compiler_bin="$(command -v musl-gcc)"
        elif command -v gcc >/dev/null 2>&1; then
            compiler_bin="$(command -v gcc)"
        else
            echo "No usable C compiler found for musl target ${target}" >&2
            exit 1
        fi

        cat > "$wrapper_path" <<EOF
#!/usr/bin/env bash
exec "$compiler_bin" "$@"
EOF
        chmod +x "$wrapper_path"
    fi

    if [[ -n "${GITHUB_PATH:-}" ]]; then
        echo "$wrapper_dir" >> "$GITHUB_PATH"
    fi

    if [[ -n "${GITHUB_ENV:-}" ]]; then
        target_env_name="${target//-/_}"
        cargo_target_env_name="${target_env_name^^}"
        {
            echo "CC_${target_env_name}=$wrapper_path"
            echo "CC_${target}=$wrapper_path"
            echo "TARGET_CC=$wrapper_path"
            echo "CARGO_TARGET_${cargo_target_env_name}_LINKER=$wrapper_path"
        } >> "$GITHUB_ENV"
    fi
}

if command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y curl build-essential musl-tools pkg-config libssl-dev tar gzip findutils
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y curl gcc make tar gzip findutils openssl-devel
    dnf install -y pkgconf-pkg-config || dnf install -y pkgconf || true
elif command -v yum >/dev/null 2>&1; then
    yum install -y curl gcc make tar gzip findutils openssl-devel
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

ensure_rust_target "$TARGET"
configure_musl_cc "$TARGET"

cargo --version
rustc --version
