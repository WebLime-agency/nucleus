#!/usr/bin/env bash
set -euo pipefail

version_mode="${VERSION_MODE:-auto}"
bump="${BUMP:-patch}"
explicit_version="${VERSION:-}"

semver_re='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
stable_tag_re='^v([0-9]+)\.([0-9]+)\.([0-9]+)$'

bootstrap_error() {
  cat >&2 <<'EOF'
::error::No vX.Y.Z release tag was found. Cut a one-time annotated v0.1.0 tag against the current main HEAD before publishing:
  git fetch origin main
  git tag -a v0.1.0 origin/main -m "Release v0.1.0"
  git push origin v0.1.0
EOF
}

case "${version_mode}" in
  auto|explicit) ;;
  *)
    echo "::error::version_mode must be auto or explicit; got '${version_mode}'" >&2
    exit 1
    ;;
esac

case "${bump}" in
  patch|minor|major) ;;
  *)
    echo "::error::bump must be patch, minor, or major; got '${bump}'" >&2
    exit 1
    ;;
esac

if [ "${version_mode}" = "explicit" ]; then
  if [ -z "${explicit_version}" ]; then
    echo "::error::version is required when version_mode=explicit" >&2
    exit 1
  fi

  if ! [[ "${explicit_version}" =~ ${semver_re} ]]; then
    echo "::error::version must be a semantic version like MAJOR.MINOR.PATCH, optionally with prerelease/build metadata; got '${explicit_version}'" >&2
    exit 1
  fi
fi

latest_tag="$(git tag --merged HEAD --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname | awk '/^v[0-9]+\.[0-9]+\.[0-9]+$/ { print; exit }')"
if [ -z "${latest_tag}" ]; then
  bootstrap_error
  exit 1
fi

if [ "${version_mode}" = "explicit" ]; then
  printf '%s\n' "${explicit_version}"
  exit 0
fi

if ! [[ "${latest_tag}" =~ ${stable_tag_re} ]]; then
  echo "::error::Latest matching release tag '${latest_tag}' is not an exact vMAJOR.MINOR.PATCH tag" >&2
  exit 1
fi

major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"

case "${bump}" in
  patch)
    patch=$((patch + 1))
    ;;
  minor)
    minor=$((minor + 1))
    patch=0
    ;;
  major)
    major=$((major + 1))
    minor=0
    patch=0
    ;;
esac

printf '%s.%s.%s\n' "${major}" "${minor}" "${patch}"
