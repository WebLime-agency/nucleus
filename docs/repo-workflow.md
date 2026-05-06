# Repo Workflow

`main` is the protected release branch.

`dev` is the protected integration branch.

Daily work happens on short-lived feature branches:

1. branch from `dev`
2. open a PR back into `dev`
3. let CI pass on the PR
4. merge into `dev`
5. let the nightly promotion workflow cut a disposable promotion branch from `main`
6. let that workflow cherry-pick the patch-unique commits from `dev`
7. let the promotion PR back into `main` auto-merge after CI passes

Rules:

- do not push directly to `main`
- do not push directly to `dev` unless you are repairing the branch itself
- keep feature branches narrow and disposable
- let `main` move only through the nightly promotion path unless there is an explicit hotfix
- `dev` should keep linear history
- the nightly promotion branch must start from `main`, not `dev`
- nightly promotion must use patch-based cherry-picks so `main` hotfixes do not poison future promotions
- the promotion PR should auto-merge with squash so the release branch stays disposable

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
- commits on the branch are created by nightly cherry-picking the patch-unique `dev` commits onto `main`
- the PR is disposable and should be recreated or force-updated by the workflow as needed
