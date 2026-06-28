#!/usr/bin/env bash
set -euo pipefail

source_ref="${1:-origin/main}"
manifest_source="${2:-${STABLE_MANIFEST_SOURCE:-${STABLE_MANIFEST_URL:-}}}"
manifest_status="not_checked"

if [ -z "${manifest_source}" ]; then
  manifest_source="https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-stable/manifest-stable.json"
fi

bootstrap_error() {
  cat >&2 <<'EOF'
::error::No vX.Y.Z release tag was found. Cut a one-time annotated v0.1.0 tag against the current main HEAD before publishing:
  git fetch origin main
  git tag -a v0.1.0 origin/main -m "Release v0.1.0"
  git push origin v0.1.0
EOF
}

latest_tag="$(git tag --merged "${source_ref}" --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname | awk '/^v[0-9]+\.[0-9]+\.[0-9]+$/ { print; exit }')"
if [ -z "${latest_tag}" ]; then
  bootstrap_error
  exit 1
fi

source_sha="$(git rev-parse "${source_ref}^{commit}")"
latest_tag_sha="$(git rev-list -n 1 "${latest_tag}")"
latest_tag_version="${latest_tag#v}"

read_manifest() {
  local source="$1"
  local destination="$2"

  case "${source}" in
    http://*|https://*)
      curl -fsSL --retry 3 --connect-timeout 10 --max-time 30 "${source}" -o "${destination}"
      ;;
    file://*)
      local path="${source#file://}"
      [ -f "${path}" ] && cp "${path}" "${destination}"
      ;;
    *)
      [ -f "${source}" ] && cp "${source}" "${destination}"
      ;;
  esac
}

stable_manifest_contains_version() {
  local manifest_path="$1"
  local version="$2"

  jq -e \
    --arg version "${version}" \
    '.channel == "stable" and (.releases | type) == "array" and any(.releases[]; .version == $version)' \
    "${manifest_path}" >/dev/null
}

if [ "${source_sha}" = "${latest_tag_sha}" ]; then
  manifest_payload="$(mktemp)"
  trap 'rm -f "${manifest_payload}"' EXIT

  if ! read_manifest "${manifest_source}" "${manifest_payload}" >/dev/null 2>&1; then
    manifest_status="unavailable"
    should_publish="true"
  elif ! jq empty "${manifest_payload}" >/dev/null 2>&1; then
    manifest_status="malformed"
    should_publish="true"
  elif [ "$(jq -r '.channel // empty' "${manifest_payload}")" != "stable" ]; then
    manifest_status="channel_mismatch"
    should_publish="true"
  elif [ "$(jq -r '.releases | type' "${manifest_payload}")" != "array" ]; then
    manifest_status="malformed"
    should_publish="true"
  elif stable_manifest_contains_version "${manifest_payload}" "${latest_tag_version}"; then
    manifest_status="version_present"
    should_publish="false"
  else
    manifest_status="version_absent"
    should_publish="true"
  fi
else
  manifest_status="source_changed"
  should_publish="true"
fi

printf 'should_publish=%s\n' "${should_publish}"
printf 'latest_tag=%s\n' "${latest_tag}"
printf 'latest_tag_version=%s\n' "${latest_tag_version}"
printf 'latest_tag_sha=%s\n' "${latest_tag_sha}"
printf 'source_sha=%s\n' "${source_sha}"
printf 'manifest_status=%s\n' "${manifest_status}"
