#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOTFS_DIR="${SCRIPT_DIR}/rootfs"
SYSTEMD_DIR="${SCRIPT_DIR}/systemd"
MIN_GLIBC_VERSION="2.28"
FORCE_CONFIG_OVERWRITE=0
INSTALL_ROOT="${AISH_INSTALL_ROOT:-}"
INSTALL_PREFIX=""
SKIP_SYSTEMD="${AISH_SKIP_SYSTEMD:-0}"

usage() {
	cat <<'EOF'
Usage: sudo ./install.sh [--force-config] [--prefix=PATH]

Installs AI Shell binaries, default skills, and the security policy.

Options:
	--prefix=PATH       Install into PATH instead of system directories (no sudo needed)
	                    Example: --prefix=/home/$USER/.local

Environment:
	AISH_INSTALL_ROOT   Install into a staging root instead of /
	AISH_SKIP_SYSTEMD   Skip systemd checks and service enablement
EOF
}

version_ge() {
	local current="$1"
	local minimum="$2"
	[[ "$(printf '%s\n%s\n' "$minimum" "$current" | sort -V | head -n1)" == "$minimum" ]]
}

require_root() {
	if [[ -n "$INSTALL_ROOT" ]] || [[ -n "$INSTALL_PREFIX" ]]; then
		return
	fi
	if [[ "${EUID}" -ne 0 ]]; then
		echo "This installer must run as root." >&2
		exit 1
	fi
}

target_path() {
	local absolute_path="$1"
	if [[ -n "$INSTALL_ROOT" ]]; then
		printf '%s%s\n' "$INSTALL_ROOT" "$absolute_path"
	elif [[ -n "$INSTALL_PREFIX" ]]; then
		printf '%s%s\n' "$INSTALL_PREFIX" "$absolute_path"
	else
		printf '%s\n' "$absolute_path"
	fi
}

require_command() {
	local command_name="$1"
	local hint="$2"
	if ! command -v "$command_name" >/dev/null 2>&1; then
		echo "Missing runtime dependency: ${command_name}. ${hint}" >&2
		exit 1
	fi
}

check_glibc() {
	local glibc_version
	if ! glibc_version="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')"; then
		echo "Unable to detect glibc version via getconf." >&2
		exit 1
	fi
	if ! version_ge "$glibc_version" "$MIN_GLIBC_VERSION"; then
		echo "glibc ${MIN_GLIBC_VERSION}+ is required. Detected: ${glibc_version}." >&2
		exit 1
	fi
}

check_runtime_dependencies() {
	if [[ ! -d "$ROOTFS_DIR" ]]; then
		echo "Bundle payload not found: ${ROOTFS_DIR}" >&2
		exit 1
	fi
}

binary_target_dir() {
	if [[ -n "$INSTALL_PREFIX" ]]; then
		printf '%s\n' "/usr/local/bin"
	else
		printf '%s\n' "/usr/local/bin"
	fi
}

install_file() {
	local source_path="$1"
	local destination_path
	destination_path="$(target_path "$2")"
	local mode="$3"
	if ! install -D -m "$mode" "$source_path" "$destination_path" 2>/dev/null; then
		echo "Warning: Failed to install $source_path to $destination_path" >&2
		return 0
	fi
}

install_config() {
	local source_path="$1"
	local destination_path
	destination_path="$(target_path "$2")"
	if [[ -f "$destination_path" && "$FORCE_CONFIG_OVERWRITE" -ne 1 ]]; then
		echo "Preserving existing config: ${destination_path}"
		return
	fi
	install_file "$source_path" "$2" 0644
}

install_tree() {
	local source_dir="$1"
	local destination_dir
	destination_dir="$(target_path "$2")"
	if ! install -d "$destination_dir" 2>/dev/null; then
		echo "Warning: Failed to create directory $destination_dir" >&2
		return 0
	fi
	if ! cp -a "$source_dir/." "$destination_dir/" 2>/dev/null; then
		echo "Warning: Failed to copy files from $source_dir to $destination_dir" >&2
		return 0
	fi
}

install_systemd_units() {
	if [[ -n "$INSTALL_PREFIX" ]]; then
		return
	fi
	if [[ ! -d "$SYSTEMD_DIR" ]]; then
		echo "Warning: systemd unit payload not found: ${SYSTEMD_DIR}" >&2
		return
	fi

	local unit_dir="/lib/systemd/system"
	local service_target socket_target
	service_target="$(target_path "${unit_dir}/aish-sandbox.service")"
	socket_target="$(target_path "${unit_dir}/aish-sandbox.socket")"
	install -D -m 0644 "$SYSTEMD_DIR/aish-sandbox.socket" "$socket_target"
	sed "s|@AISH_BINDIR@|${BIN_DIR}|g" "$SYSTEMD_DIR/aish-sandbox.service.in" > "$service_target"
	chmod 0644 "$service_target"

	if [[ -n "$INSTALL_ROOT" || "$SKIP_SYSTEMD" == "1" ]]; then
		return
	fi
	if [[ ! -d /run/systemd/system ]] || ! command -v systemctl >/dev/null 2>&1; then
		echo "Systemd is not active; installed units but did not enable aish-sandbox.socket." >&2
		return
	fi

	systemctl daemon-reload >/dev/null 2>&1 || true
	systemctl enable aish-sandbox.socket >/dev/null 2>&1 || true
	if systemctl is-active --quiet aish-sandbox.socket || systemctl is-active --quiet aish-sandbox.service; then
		systemctl restart aish-sandbox.socket >/dev/null 2>&1 || true
	else
		systemctl start aish-sandbox.socket >/dev/null 2>&1 || true
	fi
}

while [[ $# -gt 0 ]]; do
	case "$1" in
		--force-config)
			FORCE_CONFIG_OVERWRITE=1
			shift
			;;
		--prefix=*)
			INSTALL_PREFIX="${1#*=}"
			shift
			;;
		-h|--help)
			usage
			exit 0
			;;
		*)
			echo "Unknown option: $1" >&2
			usage >&2
			exit 1
			;;
	esac
done

require_root
check_glibc
check_runtime_dependencies

BIN_DIR="$(binary_target_dir)"

install_file "$ROOTFS_DIR/usr/bin/aish" "${BIN_DIR}/aish" 0755
install_file "$SCRIPT_DIR/uninstall.sh" "${BIN_DIR}/aish-uninstall" 0755
install_config "$ROOTFS_DIR/etc/aish/security_policy.yaml" "/etc/aish/security_policy.yaml"
install_file "$ROOTFS_DIR/usr/share/doc/aish/skills-guide.md" "/usr/local/share/aish/skills-guide.md" 0644

if [[ -d "$ROOTFS_DIR/usr/share/aish/skills" ]]; then
	install_tree "$ROOTFS_DIR/usr/share/aish/skills" "/usr/local/share/aish/skills"
fi

install_systemd_units

if [[ -n "$INSTALL_ROOT" ]]; then
	echo "AI Shell installed successfully into ${INSTALL_ROOT}."
	exit 0
fi

if [[ -n "$INSTALL_PREFIX" ]]; then
	echo "AI Shell installed successfully into ${INSTALL_PREFIX}."
	echo "Add ${INSTALL_PREFIX}${BIN_DIR} to your PATH if needed:"
	echo "  export PATH=\"${INSTALL_PREFIX}${BIN_DIR}:\$PATH\""
	exit 0
fi

echo "AI Shell installed successfully."