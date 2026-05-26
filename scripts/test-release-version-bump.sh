#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compute="${repo_root}/scripts/compute-release-version.sh"
update_files="${repo_root}/scripts/update-release-version-files.sh"

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_eq() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${actual}" != "${expected}" ]; then
    fail "${label}: expected '${expected}', got '${actual}'"
  fi
}

make_git_repo() {
  local dir="$1"
  mkdir -p "${dir}"
  git -C "${dir}" init -q
  git -C "${dir}" config user.name "Release Test"
  git -C "${dir}" config user.email "release-test@example.invalid"
  printf 'seed\n' > "${dir}/README.md"
  git -C "${dir}" add README.md
  git -C "${dir}" commit -q -m "seed"
}

run_compute_case() {
  local tag="$1"
  local bump="$2"
  local expected="$3"
  local label="$4"
  local dir="${tmpdir}/${label}"

  make_git_repo "${dir}"
  git -C "${dir}" tag -a "${tag}" -m "Release ${tag}"
  actual="$(cd "${dir}" && VERSION_MODE=auto BUMP="${bump}" "${compute}")"
  assert_eq "${expected}" "${actual}" "${label}"
}

run_compute_case "v0.1.0" "patch" "0.1.1" "auto-patch-010"
run_compute_case "v0.1.0" "minor" "0.2.0" "auto-minor-010"
run_compute_case "v0.1.0" "major" "1.0.0" "auto-major-010"
run_compute_case "v0.4.7" "patch" "0.4.8" "auto-patch-047"

no_tag_dir="${tmpdir}/no-tag"
make_git_repo "${no_tag_dir}"
if (cd "${no_tag_dir}" && VERSION_MODE=auto BUMP=patch "${compute}") >"${tmpdir}/no-tag.out" 2>"${tmpdir}/no-tag.err"; then
  fail "no matching tag should fail"
fi
grep -q "Cut a one-time annotated v0.1.0 tag" "${tmpdir}/no-tag.err" || fail "no-tag error should mention bootstrap tag"

explicit_bad_dir="${tmpdir}/explicit-bad"
make_git_repo "${explicit_bad_dir}"
git -C "${explicit_bad_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
if (cd "${explicit_bad_dir}" && VERSION_MODE=explicit VERSION=banana "${compute}") >"${tmpdir}/explicit-bad.out" 2>"${tmpdir}/explicit-bad.err"; then
  fail "malformed explicit version should fail"
fi
grep -q "version must be a semantic version" "${tmpdir}/explicit-bad.err" || fail "malformed explicit version should explain semver"

explicit_good_dir="${tmpdir}/explicit-good"
make_git_repo "${explicit_good_dir}"
git -C "${explicit_good_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
explicit_good="$(cd "${explicit_good_dir}" && VERSION_MODE=explicit VERSION=0.5.0 "${compute}")"
assert_eq "0.5.0" "${explicit_good}" "explicit-good"

fixture="${tmpdir}/fixture"
mkdir -p "${fixture}/apps/web"
cat > "${fixture}/Cargo.toml" <<'CARGO'
[workspace]
members = []

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
serde = "1"
CARGO
cat > "${fixture}/apps/web/package.json" <<'JSON'
{
  "name": "@nucleus/web",
  "private": true,
  "version": "0.1.0",
  "scripts": {
    "check": "node --version"
  }
}
JSON
git -C "${fixture}" init -q
git -C "${fixture}" config user.name "Release Test"
git -C "${fixture}" config user.email "release-test@example.invalid"
git -C "${fixture}" add Cargo.toml apps/web/package.json
git -C "${fixture}" commit -q -m "fixture"

"${update_files}" 0.1.1 "${fixture}"

grep -q '^version = "0.1.1"$' "${fixture}/Cargo.toml" || fail "Cargo.toml version was not updated"
jq -e '.version == "0.1.1"' "${fixture}/apps/web/package.json" >/dev/null || fail "package.json version was not updated"

version_diff="$(git -C "${fixture}" diff --no-ext-diff --unified=0 -- Cargo.toml apps/web/package.json | awk '/^[-+]version = "/ || /^[-+]  "version": "/ { print }')"
expected_version_diff="$(cat <<'DIFF'
-version = "0.1.0"
+version = "0.1.1"
-  "version": "0.1.0",
+  "version": "0.1.1",
DIFF
)"
assert_eq "${expected_version_diff}" "${version_diff}" "fixture version diff"

git -C "${fixture}" add Cargo.toml apps/web/package.json
git -C "${fixture}" commit -q -m "bump fixture"
"${update_files}" 0.1.1 "${fixture}"
git -C "${fixture}" diff --no-ext-diff --quiet || fail "idempotence"

echo "release version bump tests passed"
