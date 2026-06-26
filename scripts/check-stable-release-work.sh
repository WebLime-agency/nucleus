#!/usr/bin/env bash
set -euo pipefail

source_ref="${1:-origin/main}"

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

# TODO(#486): Also account for prior post-tag publish failures before no-oping.
if [ "${source_sha}" = "${latest_tag_sha}" ]; then
  should_publish="false"
else
  should_publish="true"
fi

printf 'should_publish=%s\n' "${should_publish}"
printf 'latest_tag=%s\n' "${latest_tag}"
printf 'latest_tag_sha=%s\n' "${latest_tag_sha}"
printf 'source_sha=%s\n' "${source_sha}"
