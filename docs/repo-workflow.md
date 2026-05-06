# Repo Workflow

`main` is the protected release branch.

`dev` is the protected integration branch.

Daily work happens on short-lived feature branches:

1. branch from `dev`
2. open a PR back into `dev`
3. let CI pass on the PR
4. merge into `dev`
5. let the nightly promotion workflow cut a disposable promotion branch from `main`
6. let that workflow cherry-pick the exact `dev` range recorded by the promotion cursor
7. let the promotion PR back into `main` auto-merge after CI passes
8. let the nightly promotion workflow advance the cursor after the promotion PR merges successfully
9. publish managed release artifacts through the explicit channel workflow when a product channel should move

Rules:

- do not push directly to `main`
- do not push directly to `dev` unless you are repairing the branch itself
- keep feature branches narrow and disposable
- let `main` move only through the nightly promotion path unless there is an explicit hotfix
- `dev` should keep linear history
- the nightly promotion branch must start from `main`, not `dev`
- nightly promotion must use a durable promotion cursor instead of branch-diff heuristics
- nightly promotion must cherry-pick the exact cursor range from `dev` so squash-merging the promotion PR does not requeue old `dev` commits
- the promotion PR should auto-merge with squash so the release branch stays disposable
- nightly promotion must verify the disposable promotion branch itself and publish the required `Rust` and `Web` checks on that promotion head
- do not rely on `pull_request` or `push` workflows firing from `GITHUB_TOKEN` activity during promotion
- do not publish public product artifacts from feature branches

CI expectations:

- Rust: `cargo fmt --all --check` and `cargo test`
- Web: `npm run check:web` and `npm run build:web`

Branch settings that should stay in place:

- `dev` protected with required PRs and CI
- `main` protected with required PRs and CI
- repo auto-merge enabled so the nightly promotion PR can land itself once checks finish
- `main` may keep linear history on or off; the promotion path no longer depends on merge commits

Promotion branch contract:

- branch name: `promote/dev-to-main`
- base branch: `main`
- commits on the branch are created by nightly cherry-picking the exact cursor range from `dev` onto `main`
- the PR is disposable and should be recreated or force-updated by the workflow as needed
- the workflow publishes the required `Rust` and `Web` checks directly onto the promotion head after verifying that branch

Promotion cursor contract:

- cursor ref: git tag `promotion/dev-last-promoted`
- the tag points to the latest `dev` commit that has successfully landed in `main`
- the nightly workflow builds the promotion branch from `origin/main`
- the nightly workflow promotes the exact ordered range `promotion/dev-last-promoted..origin/dev`
- the cursor advances only after the promotion PR merges successfully
- `Nightly Promote` advances the cursor in the same workflow run after auto-merge because GitHub does not reliably fire follow-up workflows from `GITHUB_TOKEN` pull request activity
- `Advance Promotion Cursor` remains a fallback for human-merged promotion PRs
- squash-merging the promotion PR is safe because the cursor does not depend on `main` retaining `dev` ancestry

Bootstrap rule:

- the first run after enabling the cursor-based workflow must provide a known-good `bootstrap_sha`
- `bootstrap_sha` must be the latest `dev` commit already represented in `main`; if nothing past the branch point has landed yet, use `git merge-base origin/main origin/dev`
- validate `bootstrap_sha` with `scripts/validate-promotion-bootstrap.sh`, which checks both direct hotfix-equivalent commits and explicit cherry-pick metadata preserved in earlier promotion history
- after that first promotion PR merges, the nightly workflow creates or updates the cursor tag automatically

Managed release channel publishing:

- workflow: `Publish Managed Release`
- scheduled runs publish `nightly` from `dev`
- manual `stable` runs default to `main`
- manual `beta` and `nightly` runs default to `dev`
- source refs can be overridden for recovery or staging, but public stable releases should normally come from promoted `main`
- channel release tags are `nucleus-channel-stable`, `nucleus-channel-beta`, and `nucleus-channel-nightly`
- channel manifests are release assets named `manifest-stable.json`, `manifest-beta.json`, and `manifest-nightly.json`
- official channel artifacts include `bin/nucleus-daemon`, `bin/nucleus`, and the matching embedded web bundle
