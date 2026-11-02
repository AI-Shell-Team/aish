#!/usr/bin/env bash
# shellcheck disable=SC2029,SC2153
set -euo pipefail

require_env() {
        local name="$1"
        if [[ -z "${!name:-}" ]]; then
                echo "Missing required environment variable: ${name}" >&2
                exit 1
        fi
}

require_env VERSION
require_env ARTIFACT_ROOT
require_env WEBSITE_SSH_HOST
require_env WEBSITE_SSH_USER
require_env WEBSITE_REPO_ROOT

VERSION="${VERSION#v}"
ARTIFACT_ROOT="${ARTIFACT_ROOT%/}"
WEBSITE_REPO_ROOT="${WEBSITE_REPO_ROOT%/}"
WEBSITE_SSH_PORT="${WEBSITE_SSH_PORT:-22}"
SITE_BASE_URL="${WEBSITE_SITE_BASE_URL:-https://www.aishell.ai/repo}"
SITE_BASE_URL="${SITE_BASE_URL%/}"
SSH_KEY_PATH="${WEBSITE_SSH_KEY_PATH:-${HOME}/.ssh/aishell_website}"
KNOWN_HOSTS_PATH="${WEBSITE_SSH_KNOWN_HOSTS_PATH:-${HOME}/.ssh/aishell_website_known_hosts}"
DRY_RUN="${DRY_RUN:-0}"

if [[ "$WEBSITE_REPO_ROOT" != /* ]]; then
        echo "WEBSITE_REPO_ROOT must be an absolute path: ${WEBSITE_REPO_ROOT}" >&2
        exit 1
fi

if [[ "$VERSION" == *-* ]]; then
        CHANNEL="beta"
        REMOTE_CHANNEL_DIR="${WEBSITE_REPO_ROOT}/beta"
        PUBLIC_CHANNEL_URL="${SITE_BASE_URL}/beta"
else
        CHANNEL="stable"
        REMOTE_CHANNEL_DIR="${WEBSITE_REPO_ROOT}"
        PUBLIC_CHANNEL_URL="${SITE_BASE_URL}"
fi

REMOTE_VERSION_DIR="${REMOTE_CHANNEL_DIR}/v${VERSION}"
PUBLIC_VERSION_URL="${PUBLIC_CHANNEL_URL}/v${VERSION}"
STAGE_DIR="$(mktemp -d)"
REMOTE_TMP_DIR=""
SSH_TARGET=""
SSH_OPTIONS=()

cleanup() {
        local exit_code=$?

        rm -rf "$STAGE_DIR"
        if [[ -n "$REMOTE_TMP_DIR" && "${#SSH_OPTIONS[@]}" -gt 0 && "$DRY_RUN" != "1" ]]; then
                ssh "${SSH_OPTIONS[@]}" "$SSH_TARGET" \
                        "rm -rf $(printf '%q' "$REMOTE_TMP_DIR")" >/dev/null 2>&1 || true
        fi
        exit "$exit_code"
}

trap cleanup EXIT

verify_checksum() {
        local archive_path="$1"
        local checksum_path="$2"
        local expected actual

        expected="$(awk 'NR==1 {print $1}' "$checksum_path")"
        if [[ ! "$expected" =~ ^[A-Fa-f0-9]{64}$ ]]; then
                echo "Invalid checksum file: $(basename "$checksum_path")" >&2
                exit 1
        fi

        actual="$(sha256sum "$archive_path" | awk '{print $1}')"
        if [[ "$actual" != "$expected" ]]; then
                echo "Checksum mismatch for $(basename "$archive_path")" >&2
                exit 1
        fi
}

copy_artifact() {
        local artifact_name="$1"
        local matches=()

        mapfile -t matches < <(find "$ARTIFACT_ROOT" -type f -name "$artifact_name" -print | sort)
        if [[ "${#matches[@]}" -ne 1 ]]; then
                echo "Expected exactly one ${artifact_name} under ${ARTIFACT_ROOT}, found ${#matches[@]}" >&2
                exit 1
        fi

        cp "${matches[0]}" "${STAGE_DIR}/${artifact_name}"
}

for arch in amd64 arm64; do
        bundle="aish-${VERSION}-linux-${arch}.tar.gz"
        copy_artifact "$bundle"
        copy_artifact "${bundle}.sha256"
        verify_checksum "${STAGE_DIR}/${bundle}" "${STAGE_DIR}/${bundle}.sha256"
done

printf '%s\n' "$VERSION" > "${STAGE_DIR}/latest"
chmod 0644 "$STAGE_DIR"/* "${STAGE_DIR}/latest"

if [[ "$DRY_RUN" == "1" ]]; then
        echo "Dry run: channel=${CHANNEL}"
        echo "Dry run: remote_version_dir=${REMOTE_VERSION_DIR}"
        echo "Dry run: public_version_url=${PUBLIC_VERSION_URL}"
        ls -lah "$STAGE_DIR"
        exit 0
fi

SSH_TARGET="${WEBSITE_SSH_USER}@${WEBSITE_SSH_HOST}"
SSH_OPTIONS=(
        -i "$SSH_KEY_PATH"
        -p "$WEBSITE_SSH_PORT"
        -o BatchMode=yes
        -o IdentitiesOnly=yes
        -o StrictHostKeyChecking=yes
        -o "UserKnownHostsFile=${KNOWN_HOSTS_PATH}"
)
RSYNC_SSH="ssh -i $(printf '%q' "$SSH_KEY_PATH") -p $(printf '%q' "$WEBSITE_SSH_PORT") -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$(printf '%q' "$KNOWN_HOSTS_PATH")"

SSH_RUN_ID="${GITHUB_RUN_ID:-$$}"
REMOTE_TMP_DIR="${REMOTE_VERSION_DIR}.tmp.${SSH_RUN_ID}.${RANDOM}"
REMOTE_LATEST_TMP="${REMOTE_CHANNEL_DIR}/.latest.tmp.${SSH_RUN_ID}.${RANDOM}"

echo "Publishing ${VERSION} to ${SSH_TARGET}:${REMOTE_VERSION_DIR}"
ssh "${SSH_OPTIONS[@]}" "$SSH_TARGET" \
        "mkdir -p $(printf '%q' "$REMOTE_TMP_DIR")"

rsync -az --chmod=D755,F644 -e "$RSYNC_SSH" \
        --exclude latest \
        "${STAGE_DIR}/" "${SSH_TARGET}:${REMOTE_TMP_DIR}/"

ssh "${SSH_OPTIONS[@]}" "$SSH_TARGET" \
        "if [ -e $(printf '%q' "$REMOTE_VERSION_DIR") ]; then mv $(printf '%q' "$REMOTE_VERSION_DIR") $(printf '%q' "${REMOTE_TMP_DIR}.previous"); fi; if mv $(printf '%q' "$REMOTE_TMP_DIR") $(printf '%q' "$REMOTE_VERSION_DIR"); then rm -rf $(printf '%q' "${REMOTE_TMP_DIR}.previous"); else mv $(printf '%q' "${REMOTE_TMP_DIR}.previous") $(printf '%q' "$REMOTE_VERSION_DIR"); exit 1; fi"
REMOTE_TMP_DIR=""

latest_source="${STAGE_DIR}/latest"
rsync -az --chmod=F644 -e "$RSYNC_SSH" \
        "$latest_source" "${SSH_TARGET}:${REMOTE_LATEST_TMP}"
ssh "${SSH_OPTIONS[@]}" "$SSH_TARGET" \
        "mv -f $(printf '%q' "$REMOTE_LATEST_TMP") $(printf '%q' "${REMOTE_CHANNEL_DIR}/latest")"

resolved_version="$(curl -fsSL --connect-timeout 10 --max-time 30 "${PUBLIC_CHANNEL_URL}/latest" | tr -d '[:space:]')"
if [[ "$resolved_version" != "$VERSION" ]]; then
        echo "Website latest mismatch: expected ${VERSION}, got ${resolved_version}" >&2
        exit 1
fi

for arch in amd64 arm64; do
        bundle="aish-${VERSION}-linux-${arch}.tar.gz"
        curl -fsSI --connect-timeout 10 --max-time 30 "${PUBLIC_VERSION_URL}/${bundle}" >/dev/null
        curl -fsSI --connect-timeout 10 --max-time 30 "${PUBLIC_VERSION_URL}/${bundle}.sha256" >/dev/null
done

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        {
                echo "## Official Website Publish"
                echo
                echo "- Version: ${VERSION}"
                echo "- Channel: ${CHANNEL}"
                echo "- Version URL: ${PUBLIC_VERSION_URL}/"
                echo "- Latest URL: ${PUBLIC_CHANNEL_URL}/latest"
        } >> "$GITHUB_STEP_SUMMARY"
fi

echo "Published ${VERSION} to ${PUBLIC_VERSION_URL}/"
