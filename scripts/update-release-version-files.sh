#!/usr/bin/env bash
set -euo pipefail

version="${1:-}"
repo_root="${2:-$(pwd)}"

if [ -z "${version}" ]; then
  echo "usage: $0 VERSION [REPO_ROOT]" >&2
  exit 2
fi

semver_re='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
if ! [[ "${version}" =~ ${semver_re} ]]; then
  echo "::error::version must be a semantic version like MAJOR.MINOR.PATCH, optionally with prerelease/build metadata; got '${version}'" >&2
  exit 1
fi

cd "${repo_root}"

tmp_cargo="$(mktemp)"
awk -v target="${version}" '
  BEGIN {
    in_workspace_package = 0
    updated = 0
  }
  /^\[workspace\.package\]$/ {
    in_workspace_package = 1
  }
  /^\[/ && $0 !~ /^\[workspace\.package\]$/ {
    in_workspace_package = 0
  }
  in_workspace_package && /^version = "/ && updated == 0 {
    print "version = \"" target "\""
    updated = 1
    next
  }
  { print }
  END {
    if (updated != 1) {
      exit 42
    }
  }
' Cargo.toml > "${tmp_cargo}" || {
  status=$?
  rm -f "${tmp_cargo}"
  if [ "${status}" -eq 42 ]; then
    echo "::error::Could not find [workspace.package] version in Cargo.toml" >&2
  fi
  exit "${status}"
}
mv "${tmp_cargo}" Cargo.toml

tmp_package="$(mktemp)"
jq --arg version "${version}" '.version = $version' apps/web/package.json > "${tmp_package}"
mv "${tmp_package}" apps/web/package.json

if [ -f Cargo.lock ]; then
  cargo update --workspace --offline
fi

if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  unexpected_diff="$(
    git diff --unified=0 -- Cargo.toml Cargo.lock apps/web/package.json |
      awk '
        /^diff --git / { next }
        /^index / { next }
        /^--- / { next }
        /^\+\+\+ / { next }
        /^@@ / { next }
        /^[-+]version = "/ { next }
        /^[-+]  "version": "/ { next }
        { print }
      '
  )"

  if [ -n "${unexpected_diff}" ]; then
    echo "::error::Version update changed unexpected lines:" >&2
    printf '%s\n' "${unexpected_diff}" >&2
    exit 1
  fi
fi
