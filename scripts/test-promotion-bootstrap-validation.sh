#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
validator_script="${repo_root}/scripts/validate-promotion-bootstrap.sh"

repo_dir="$(mktemp -d)"
cleanup() {
  rm -rf "${repo_dir}"
}
trap cleanup EXIT

git init -b main "${repo_dir}" >/dev/null
git -C "${repo_dir}" config user.name "Nucleus Test"
git -C "${repo_dir}" config user.email "nucleus@example.com"

cat > "${repo_dir}/README.md" <<'EOF'
base
EOF
git -C "${repo_dir}" add README.md
git -C "${repo_dir}" commit -m "base" >/dev/null

git -C "${repo_dir}" checkout -b dev >/dev/null

cat > "${repo_dir}/hotfix.txt" <<'EOF'
hotfix
EOF
git -C "${repo_dir}" add hotfix.txt
git -C "${repo_dir}" commit -m "hotfix equivalent" >/dev/null
hotfix_sha="$(git -C "${repo_dir}" rev-parse HEAD)"

cat > "${repo_dir}/AGENTS.md" <<'EOF'
repo rules
mobile note
EOF
git -C "${repo_dir}" add AGENTS.md
git -C "${repo_dir}" commit -m "mobile docs" >/dev/null
mobile_sha="$(git -C "${repo_dir}" rev-parse HEAD)"

cat > "${repo_dir}/workflow.txt" <<'EOF'
workflow repair
EOF
git -C "${repo_dir}" add workflow.txt
git -C "${repo_dir}" commit -m "workflow repair" >/dev/null

cat > "${repo_dir}/promotion.txt" <<'EOF'
explicit promotion metadata
EOF
git -C "${repo_dir}" add promotion.txt
git -C "${repo_dir}" commit -m "promotion metadata" >/dev/null
promotion_sha="$(git -C "${repo_dir}" rev-parse HEAD)"

cat > "${repo_dir}/future.txt" <<'EOF'
future change
EOF
git -C "${repo_dir}" add future.txt
git -C "${repo_dir}" commit -m "future change" >/dev/null
future_sha="$(git -C "${repo_dir}" rev-parse HEAD)"

git -C "${repo_dir}" checkout main >/dev/null
git -C "${repo_dir}" cherry-pick "${hotfix_sha}" >/dev/null

cat > "${repo_dir}/AGENTS.md" <<'EOF'
repo rules
mobile note
release-blocking note
EOF
cat > "${repo_dir}/workflow.txt" <<'EOF'
workflow repair
EOF
cat > "${repo_dir}/promotion.txt" <<'EOF'
explicit promotion metadata
EOF
git -C "${repo_dir}" add AGENTS.md workflow.txt promotion.txt
git -C "${repo_dir}" commit -m "$(cat <<EOF
chore: promote dev to main (#synthetic)

* synthetic squash promotion

(cherry picked from commit ${mobile_sha})
(cherry picked from commit ${promotion_sha})
EOF
)" >/dev/null

assert_invalid() {
  local candidate="$1"
  local output

  if output="$(
    cd "${repo_dir}"
    "${validator_script}" \
      --main-ref main \
      --dev-ref dev \
      --bootstrap-sha "${candidate}" \
      2>&1 \
      >/dev/null
  )"; then
    echo "Expected bootstrap candidate ${candidate} to be rejected." >&2
    exit 1
  fi

  if [[ "${output}" != *"Use bootstrap_sha=${promotion_sha} instead."* ]]; then
    echo "Unexpected validation output for ${candidate}:" >&2
    printf '%s\n' "${output}" >&2
    exit 1
  fi
}

assert_invalid "${hotfix_sha}"
assert_invalid "${future_sha}"

(
  cd "${repo_dir}"
  "${validator_script}" \
    --main-ref main \
    --dev-ref dev \
    --bootstrap-sha "${promotion_sha}" \
    >/dev/null
)

printf 'promotion bootstrap validation regression passed\n'
