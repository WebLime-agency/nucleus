# Promotion Pipeline Follow-Up

Status: completed in this implementation pass

This document captures the near-term workflow fixes discovered while promoting the managed release
update-contract work from `dev` to `main`.

These items are intentionally separate from the managed release updater backlog because they belong
to the GitHub promotion pipeline rather than the daemon/client update model.

## Delivered

- `scripts/validate-promotion-bootstrap.sh` now validates bootstrap candidates against both direct
  hotfix-equivalent history and explicit cherry-pick metadata preserved in earlier promotion
  commits on `main`
- `Nightly Promote` now verifies the disposable promotion branch itself and publishes the required
  `Rust` and `Web` check-runs directly onto the promotion head, so branch protection no longer
  needs an extra trigger commit
- `Nightly Promote` now waits for its auto-merged promotion PR and advances
  `promotion/dev-last-promoted` in the same workflow run, because GitHub does not reliably trigger
  the separate cursor workflow from `GITHUB_TOKEN` PR activity
- GitHub Actions workflow dependencies were refreshed to `actions/checkout@v6` and
  `actions/setup-node@v6`, removing the Node 20 runner deprecation path that showed up in CI

## Completed Work

### 1. Harden bootstrap cursor validation

- stop relying on `git cherry` alone when validating `bootstrap_sha`
- account for previously squash-promoted `dev` history that is already represented in `main`
- prefer validation against the actual promotion PR history or a durable recorded cursor source
- add a regression case that covers mixed history after earlier squash promotions

Why this mattered:
- the first rerun of `Nightly Promote` on 2026-05-06 failed because the bootstrap cursor was
  derived too early in the `dev` history, which caused the workflow to re-cherry-pick commits that
  had already landed in `main`

### 2. Make promotion PR CI satisfy branch protection without a trigger commit

- ensure the promotion PR gets normal branch-protection-recognized `Rust` and `Web` checks
- avoid depending on a `workflow_dispatch` CI run for the promotion branch head
- remove the need for an extra empty `chore: trigger release ci` commit on `promote/dev-to-main`
- verify auto-merge can complete end to end immediately after the workflow creates or updates the PR

Why this mattered:
- the repaired promotion PR on 2026-05-06 was correct, but GitHub kept it `BLOCKED` until a normal
  `pull_request` CI run was attached to the promotion head

### 3. Refresh GitHub Actions usage for current runner defaults

- review Actions versions that still rely on deprecated Node 20 runner behavior
- upgrade workflow actions where current releases are available
- keep the promotion and CI workflows free of runner deprecation warnings

Why this mattered:
- the successful promotion CI run emitted a Node 20 deprecation warning, which is not breaking yet
  but will become avoidable maintenance churn

## Acceptance Checks

- a bootstrap run against a repo with earlier squash promotions creates the correct promotion PR on
  the first attempt
- `Nightly Promote` can create or update the promotion PR and enable auto-merge without any manual
  branch pushes
- the promotion PR merges after `Rust` and `Web` pass, and `Nightly Promote` moves
  `promotion/dev-last-promoted` to the promoted `dev` head in the same run
