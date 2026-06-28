#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compute="${repo_root}/scripts/compute-release-version.sh"
check_work="${repo_root}/scripts/check-stable-release-work.sh"
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

output_value() {
  local key="$1"
  local file="$2"

  awk -F= -v key="${key}" '$1 == key { print substr($0, index($0, "=") + 1); exit }' "${file}"
}

write_stable_manifest() {
  local path="$1"
  local version="$2"
  local channel="${3:-stable}"

  cat > "${path}" <<JSON
{
  "product": "Nucleus",
  "channel": "${channel}",
  "generated_at": 1,
  "releases": [
    {
      "release_id": "nucleus-${channel}-${version}",
      "version": "${version}",
      "channel": "${channel}",
      "published_at": 1,
      "artifacts": []
    }
  ]
}
JSON
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

no_new_work_dir="${tmpdir}/no-new-work"
make_git_repo "${no_new_work_dir}"
git -C "${no_new_work_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
git -C "${no_new_work_dir}" update-ref refs/remotes/origin/main HEAD
no_new_work_manifest="${tmpdir}/no-new-work-manifest.json"
write_stable_manifest "${no_new_work_manifest}" "0.1.0"
no_new_work_output="${tmpdir}/no-new-work.out"
(cd "${no_new_work_dir}" && "${check_work}" origin/main "${no_new_work_manifest}") >"${no_new_work_output}"
assert_eq "false" "$(output_value should_publish "${no_new_work_output}")" "no-new-work should not publish"
assert_eq "v0.1.0" "$(output_value latest_tag "${no_new_work_output}")" "no-new-work latest tag"
assert_eq "0.1.0" "$(output_value latest_tag_version "${no_new_work_output}")" "no-new-work latest tag version"
assert_eq "version_present" "$(output_value manifest_status "${no_new_work_output}")" "no-new-work manifest status"

missing_version_dir="${tmpdir}/missing-version"
make_git_repo "${missing_version_dir}"
git -C "${missing_version_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
git -C "${missing_version_dir}" update-ref refs/remotes/origin/main HEAD
missing_version_manifest="${tmpdir}/missing-version-manifest.json"
write_stable_manifest "${missing_version_manifest}" "0.0.9"
missing_version_output="${tmpdir}/missing-version.out"
(cd "${missing_version_dir}" && "${check_work}" origin/main "${missing_version_manifest}") >"${missing_version_output}"
assert_eq "true" "$(output_value should_publish "${missing_version_output}")" "missing-version should publish"
assert_eq "version_absent" "$(output_value manifest_status "${missing_version_output}")" "missing-version manifest status"

missing_manifest_dir="${tmpdir}/missing-manifest"
make_git_repo "${missing_manifest_dir}"
git -C "${missing_manifest_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
git -C "${missing_manifest_dir}" update-ref refs/remotes/origin/main HEAD
missing_manifest_output="${tmpdir}/missing-manifest.out"
(cd "${missing_manifest_dir}" && "${check_work}" origin/main "${tmpdir}/does-not-exist.json") >"${missing_manifest_output}"
assert_eq "true" "$(output_value should_publish "${missing_manifest_output}")" "missing-manifest should publish"
assert_eq "unavailable" "$(output_value manifest_status "${missing_manifest_output}")" "missing-manifest status"

malformed_manifest_dir="${tmpdir}/malformed-manifest"
make_git_repo "${malformed_manifest_dir}"
git -C "${malformed_manifest_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
git -C "${malformed_manifest_dir}" update-ref refs/remotes/origin/main HEAD
malformed_manifest="${tmpdir}/malformed-manifest.json"
printf '{not-json\n' > "${malformed_manifest}"
malformed_manifest_output="${tmpdir}/malformed-manifest.out"
(cd "${malformed_manifest_dir}" && "${check_work}" origin/main "${malformed_manifest}") >"${malformed_manifest_output}"
assert_eq "true" "$(output_value should_publish "${malformed_manifest_output}")" "malformed-manifest should publish"
assert_eq "malformed" "$(output_value manifest_status "${malformed_manifest_output}")" "malformed-manifest status"

malformed_shape_dir="${tmpdir}/malformed-shape"
make_git_repo "${malformed_shape_dir}"
git -C "${malformed_shape_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
git -C "${malformed_shape_dir}" update-ref refs/remotes/origin/main HEAD
malformed_shape_manifest="${tmpdir}/malformed-shape-manifest.json"
cat > "${malformed_shape_manifest}" <<'JSON'
{
  "product": "Nucleus",
  "channel": "stable",
  "generated_at": 1,
  "releases": {
    "current": {
      "release_id": "nucleus-stable-0.1.0",
      "version": "0.1.0",
      "channel": "stable",
      "published_at": 1,
      "artifacts": []
    }
  }
}
JSON
malformed_shape_output="${tmpdir}/malformed-shape.out"
(cd "${malformed_shape_dir}" && "${check_work}" origin/main "${malformed_shape_manifest}") >"${malformed_shape_output}"
assert_eq "true" "$(output_value should_publish "${malformed_shape_output}")" "malformed-shape should publish"
assert_eq "malformed" "$(output_value manifest_status "${malformed_shape_output}")" "malformed-shape status"

channel_mismatch_dir="${tmpdir}/channel-mismatch"
make_git_repo "${channel_mismatch_dir}"
git -C "${channel_mismatch_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
git -C "${channel_mismatch_dir}" update-ref refs/remotes/origin/main HEAD
channel_mismatch_manifest="${tmpdir}/channel-mismatch-manifest.json"
write_stable_manifest "${channel_mismatch_manifest}" "0.1.0" "nightly"
channel_mismatch_output="${tmpdir}/channel-mismatch.out"
(cd "${channel_mismatch_dir}" && "${check_work}" origin/main "${channel_mismatch_manifest}") >"${channel_mismatch_output}"
assert_eq "true" "$(output_value should_publish "${channel_mismatch_output}")" "channel-mismatch should publish"
assert_eq "channel_mismatch" "$(output_value manifest_status "${channel_mismatch_output}")" "channel-mismatch status"

new_work_dir="${tmpdir}/new-work"
make_git_repo "${new_work_dir}"
git -C "${new_work_dir}" tag -a "v0.1.0" -m "Release v0.1.0"
printf 'new work\n' > "${new_work_dir}/FEATURE.md"
git -C "${new_work_dir}" add FEATURE.md
git -C "${new_work_dir}" commit -q -m "new work"
git -C "${new_work_dir}" update-ref refs/remotes/origin/main HEAD
new_work_manifest="${tmpdir}/new-work-manifest.json"
write_stable_manifest "${new_work_manifest}" "0.1.0"
new_work_output="${tmpdir}/new-work.out"
(cd "${new_work_dir}" && "${check_work}" origin/main "${new_work_manifest}") >"${new_work_output}"
assert_eq "true" "$(output_value should_publish "${new_work_output}")" "new-work should publish"
assert_eq "v0.1.0" "$(output_value latest_tag "${new_work_output}")" "new-work latest tag"
assert_eq "source_changed" "$(output_value manifest_status "${new_work_output}")" "new-work manifest status"
new_work_version="$(cd "${new_work_dir}" && VERSION_MODE=auto BUMP=patch "${compute}")"
assert_eq "0.1.1" "${new_work_version}" "new-work version bump"

exact_tag_dir="${tmpdir}/exact-tag-filter"
make_git_repo "${exact_tag_dir}"
git -C "${exact_tag_dir}" tag -a "v0.4.7" -m "Release v0.4.7"
git -C "${exact_tag_dir}" tag -a "v9.9.9-rc1" -m "Release v9.9.9-rc1"
exact_tag_actual="$(cd "${exact_tag_dir}" && VERSION_MODE=auto BUMP=patch "${compute}")"
assert_eq "0.4.8" "${exact_tag_actual}" "exact-tag-filter"

merged_tag_dir="${tmpdir}/merged-tag-filter"
make_git_repo "${merged_tag_dir}"
merged_base_branch="$(git -C "${merged_tag_dir}" branch --show-current)"
git -C "${merged_tag_dir}" tag -a "v0.4.7" -m "Release v0.4.7"
git -C "${merged_tag_dir}" switch -q -c unrelated
printf 'unrelated\n' > "${merged_tag_dir}/UNRELATED.md"
git -C "${merged_tag_dir}" add UNRELATED.md
git -C "${merged_tag_dir}" commit -q -m "unrelated"
git -C "${merged_tag_dir}" tag -a "v9.9.9" -m "Release v9.9.9"
git -C "${merged_tag_dir}" switch -q "${merged_base_branch}"
merged_tag_actual="$(cd "${merged_tag_dir}" && VERSION_MODE=auto BUMP=patch "${compute}")"
assert_eq "0.4.8" "${merged_tag_actual}" "merged-tag-filter"

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
mkdir -p "${fixture}/apps/web/src/lib/nucleus"
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
cat > "${fixture}/apps/web/src/lib/nucleus/compatibility.ts" <<'TS'
import packageJson from '../../../package.json';

export const CURRENT_CLIENT_VERSION = packageJson.version;
TS
git -C "${fixture}" init -q
git -C "${fixture}" config user.name "Release Test"
git -C "${fixture}" config user.email "release-test@example.invalid"
git -C "${fixture}" add Cargo.toml apps/web/package.json apps/web/src/lib/nucleus/compatibility.ts
git -C "${fixture}" commit -q -m "fixture"

"${update_files}" 0.1.1 "${fixture}"

grep -q '^version = "0.1.1"$' "${fixture}/Cargo.toml" || fail "Cargo.toml version was not updated"
jq -e '.version == "0.1.1"' "${fixture}/apps/web/package.json" >/dev/null || fail "package.json version was not updated"
grep -q "packageJson.version" "${fixture}/apps/web/src/lib/nucleus/compatibility.ts" || fail "web client version should derive from package.json"

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
