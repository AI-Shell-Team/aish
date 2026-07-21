#!/usr/bin/env bash
set -euo pipefail

PURGE_CONFIG=0
BUNDLE_SYSTEMD_UNITDIR="/etc/systemd/system"
INSTALL_ROOT="${AISH_INSTALL_ROOT:-}"
INSTALL_PREFIX=""
SKIP_SYSTEMD="${AISH_SKIP_SYSTEMD:-0}"

usage() {
	cat <<'EOF'
Usage: sudo ./uninstall.sh [--purge-config] [--prefix=PATH]

Removes AI Shell binaries and bundled skills.
EOF
}

require_root() {
	if [[ -n "$INSTALL_ROOT" ]] || [[ -n "$INSTALL_PREFIX" ]]; then
		return
	fi
	if [[ "${EUID}" -ne 0 ]]; then
		echo "This uninstaller must run as root." >&2
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

binary_target_dir() {
	printf '%s\n' "/usr/local/bin"
}

remove_systemd_units() {
	if [[ -n "$INSTALL_PREFIX" ]]; then
		return
	fi
	if [[ -z "$INSTALL_ROOT" && "$SKIP_SYSTEMD" != "1" && -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
		systemctl disable --now aish-sandbox.socket >/dev/null 2>&1 || true
		systemctl stop --no-block aish-sandbox.service >/dev/null 2>&1 || true
		systemctl reset-failed aish-sandbox.service >/dev/null 2>&1 || true
	fi

	rm -f \
		"$(target_path "${BUNDLE_SYSTEMD_UNITDIR}/aish-sandbox.service")" \
		"$(target_path "${BUNDLE_SYSTEMD_UNITDIR}/aish-sandbox.socket")"

	if [[ -z "$INSTALL_ROOT" && "$SKIP_SYSTEMD" != "1" && -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
		systemctl daemon-reload >/dev/null 2>&1 || true
	fi
}

while [[ $# -gt 0 ]]; do
	case "$1" in
		--purge-config)
			PURGE_CONFIG=1
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

BIN_DIR="$(binary_target_dir)"

remove_systemd_units

rm -f "$(target_path "${BIN_DIR}/aish")" "$(target_path "${BIN_DIR}/aish-uninstall")"

# Remove legacy system skills tree from older installers (no longer shipped).
rm -rf "$(target_path "/usr/local/share/aish/skills")"
rmdir --ignore-fail-on-non-empty "$(target_path "/usr/local/share/aish")" >/dev/null 2>&1 || true
if [[ "$PURGE_CONFIG" -eq 1 ]]; then
	rm -f "$(target_path "/etc/aish/security_policy.yaml")"
	rmdir --ignore-fail-on-non-empty "$(target_path "/etc/aish")" >/dev/null 2>&1 || true
fi

echo "AI Shell removed successfully."