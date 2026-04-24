#!/usr/bin/env bash
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
require_env R2_BUCKET
require_env R2_ENDPOINT
require_env AWS_ACCESS_KEY_ID
require_env AWS_SECRET_ACCESS_KEY

DOWNLOAD_PREFIX="${DOWNLOAD_PREFIX:-download}"
CDN_BASE_URL="${CDN_BASE_URL:-https://cdn.aishell.ai}"
VERSION="${VERSION#v}"
ARTIFACT_ROOT="${ARTIFACT_ROOT%/}"
VALIDATION_RETRY_COUNT="${VALIDATION_RETRY_COUNT:-10}"
VALIDATION_RETRY_DELAY_SECS="${VALIDATION_RETRY_DELAY_SECS:-2}"

mapfile -t ARTIFACT_FILES < <(
  find "$ARTIFACT_ROOT" -type f \
    \( -name "aish-${VERSION}-linux-*.tar.gz" \
       -o -name "aish-${VERSION}-linux-*.tar.gz.sha256" \) \
    | sort
)

if [[ "${#ARTIFACT_FILES[@]}" -eq 0 ]]; then
  echo "No release artifacts found under ${ARTIFACT_ROOT}" >&2
  exit 1
fi

mapfile -t BUNDLE_FILES < <(
  find "$ARTIFACT_ROOT" -type f -name "aish-${VERSION}-linux-*.tar.gz" | sort
)

if [[ "${#BUNDLE_FILES[@]}" -eq 0 ]]; then
  echo "No release bundles found under ${ARTIFACT_ROOT}" >&2
  exit 1
fi

for bundle_path in "${BUNDLE_FILES[@]}"; do
  if [[ ! -f "${bundle_path}.sha256" ]]; then
    echo "Missing checksum for $(basename "$bundle_path")" >&2
    exit 1
  fi
done

upload_object() {
  local source_path="$1"
  local destination_key="$2"
  local cache_control="$3"
  shift 3

  aws s3 cp "$source_path" "s3://${R2_BUCKET}/${destination_key}" \
    --endpoint-url "$R2_ENDPOINT" \
    --cache-control "$cache_control" \
    "$@"
}

for artifact_path in "${ARTIFACT_FILES[@]}"; do
  artifact_name="$(basename "$artifact_path")"
  release_key="${DOWNLOAD_PREFIX}/releases/${VERSION}/${artifact_name}"
  cache_control="public, max-age=31536000, immutable"
  content_type_args=()

  if [[ "$artifact_name" == *.sha256 ]]; then
    content_type_args=(--content-type text/plain)
  fi

  echo "Uploading ${artifact_name} to ${release_key}"
  upload_object "$artifact_path" "$release_key" "$cache_control" "${content_type_args[@]}"
done

latest_file="$(mktemp)"
trap 'rm -f "$latest_file"' EXIT
printf '%s' "$VERSION" > "$latest_file"

echo "Updating ${DOWNLOAD_PREFIX}/latest"
upload_object "$latest_file" "${DOWNLOAD_PREFIX}/latest" "no-store" --content-type text/plain

validate_url() {
  local url="$1"
  local attempt=1
  local exit_code=0

  while (( attempt <= VALIDATION_RETRY_COUNT )); do
    if curl -fsSI --connect-timeout 10 --max-time 30 "$url" >/dev/null; then
      return 0
    else
      exit_code=$?
    fi
    if (( attempt == VALIDATION_RETRY_COUNT )); then
      return "$exit_code"
    fi
    sleep $((VALIDATION_RETRY_DELAY_SECS * attempt))
    ((attempt += 1))
  done
}

validated_urls=(
  "${CDN_BASE_URL%/}/${DOWNLOAD_PREFIX}/latest"
)

for artifact_path in "${ARTIFACT_FILES[@]}"; do
  artifact_name="$(basename "$artifact_path")"
  validated_urls+=("${CDN_BASE_URL%/}/${DOWNLOAD_PREFIX}/releases/${VERSION}/${artifact_name}")
done

for url in "${validated_urls[@]}"; do
  echo "Validating ${url}"
  validate_url "$url"
done

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "## CDN Publish"
    echo
    echo "- Version: ${VERSION}"
    echo "- Bucket: ${R2_BUCKET}"
    echo "- Latest URL: ${CDN_BASE_URL%/}/${DOWNLOAD_PREFIX}/latest"
    echo
    echo "### Published artifacts"
    for artifact_path in "${ARTIFACT_FILES[@]}"; do
      artifact_name="$(basename "$artifact_path")"
      echo "- ${DOWNLOAD_PREFIX}/releases/${VERSION}/${artifact_name}"
    done
  } >> "$GITHUB_STEP_SUMMARY"
fi
