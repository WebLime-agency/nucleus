#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: validate-promotion-bootstrap.sh --main-ref <ref> --dev-ref <ref> --bootstrap-sha <sha>
EOF
  exit 2
}

main_ref=""
dev_ref=""
bootstrap_sha=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --main-ref)
      [ "$#" -ge 2 ] || usage
      main_ref="$2"
      shift 2
      ;;
    --dev-ref)
      [ "$#" -ge 2 ] || usage
      dev_ref="$2"
      shift 2
      ;;
    --bootstrap-sha)
      [ "$#" -ge 2 ] || usage
      bootstrap_sha="$2"
      shift 2
      ;;
    *)
      usage
      ;;
  esac
done

[ -n "${main_ref}" ] || usage
[ -n "${dev_ref}" ] || usage
[ -n "${bootstrap_sha}" ] || usage

git rev-parse --verify "${main_ref}^{commit}" >/dev/null
git rev-parse --verify "${dev_ref}^{commit}" >/dev/null
git rev-parse --verify "${bootstrap_sha}^{commit}" >/dev/null

if ! git merge-base --is-ancestor "${bootstrap_sha}" "${dev_ref}"; then
  echo "Bootstrap cursor ${bootstrap_sha} is not an ancestor of ${dev_ref}." >&2
  exit 1
fi

branch_point="$(git merge-base "${main_ref}" "${dev_ref}")"
git rev-parse --verify "${branch_point}^{commit}" >/dev/null

mapfile -t dev_commits < <(
  git rev-list \
    --reverse \
    --ancestry-path \
    "${branch_point}..${dev_ref}"
)

declare -A promoted_by_main=()
while IFS= read -r promoted_sha; do
  [ -n "${promoted_sha}" ] || continue
  promoted_by_main["${promoted_sha}"]=1
done < <(
  git log --format=%B "${main_ref}" \
    | sed -n 's/.*cherry picked from commit \([0-9a-f]\{40\}\).*/\1/p'
)

scan_dir="$(mktemp -d)"
cleanup() {
  if [ -d "${scan_dir}" ]; then
    git worktree remove --force "${scan_dir}" >/dev/null 2>&1 || rm -rf "${scan_dir}"
  fi
}
trap cleanup EXIT

git worktree add --detach "${scan_dir}" "${main_ref}" >/dev/null

scan_git() {
  git -C "${scan_dir}" "$@"
}

scanned_floor="${branch_point}"
for commit in "${dev_commits[@]}"; do
  if scan_git cherry-pick -n "${commit}" >/dev/null 2>&1; then
    if scan_git diff --quiet && scan_git diff --cached --quiet; then
      scanned_floor="${commit}"
      scan_git reset --hard HEAD >/dev/null
      continue
    fi

    scan_git reset --hard HEAD >/dev/null
    break
  fi

  scan_git cherry-pick --abort >/dev/null 2>&1 || scan_git reset --hard HEAD >/dev/null
  break
done

explicit_floor="${branch_point}"
for commit in "${dev_commits[@]}"; do
  if [ "${promoted_by_main[${commit}]+set}" = set ]; then
    explicit_floor="${commit}"
  fi
done

suggested_bootstrap="${scanned_floor}"
if git merge-base --is-ancestor "${suggested_bootstrap}" "${explicit_floor}" \
  && [ "${suggested_bootstrap}" != "${explicit_floor}" ]; then
  suggested_bootstrap="${explicit_floor}"
fi

if [ "${bootstrap_sha}" != "${suggested_bootstrap}" ] \
  && git merge-base --is-ancestor "${bootstrap_sha}" "${suggested_bootstrap}"; then
  echo "Bootstrap cursor ${bootstrap_sha} is earlier than the latest dev commit already represented in ${main_ref}." >&2
  echo "Use bootstrap_sha=${suggested_bootstrap} instead." >&2
  exit 1
fi

latest_valid="${suggested_bootstrap}"

if [ "${bootstrap_sha}" != "${suggested_bootstrap}" ]; then
  if ! git merge-base --is-ancestor "${suggested_bootstrap}" "${bootstrap_sha}"; then
    echo "Bootstrap cursor ${bootstrap_sha} is not on the expected promoted ancestry path from ${suggested_bootstrap} to ${dev_ref}." >&2
    exit 1
  fi

  mapfile -t replay_commits < <(
    git rev-list \
      --reverse \
      --ancestry-path \
      "${suggested_bootstrap}..${bootstrap_sha}"
  )

  for commit in "${replay_commits[@]}"; do
    if scan_git cherry-pick -n "${commit}" >/dev/null 2>&1; then
      if scan_git diff --quiet && scan_git diff --cached --quiet; then
        latest_valid="${commit}"
        scan_git reset --hard HEAD >/dev/null
        continue
      fi

      scan_git reset --hard HEAD >/dev/null
      echo "Bootstrap cursor ${bootstrap_sha} skips unpromoted changes beginning at ${commit}." >&2
      echo "Use bootstrap_sha=${latest_valid} instead." >&2
      exit 1
    fi

    scan_git cherry-pick --abort >/dev/null 2>&1 || scan_git reset --hard HEAD >/dev/null
    echo "Bootstrap cursor ${bootstrap_sha} cannot be validated past ${latest_valid} because ${commit} does not replay cleanly on ${main_ref}." >&2
    echo "Use bootstrap_sha=${latest_valid} instead." >&2
    exit 1
  done
fi

echo "Validated bootstrap_sha=${bootstrap_sha} against ${main_ref} and ${dev_ref}." >&2
