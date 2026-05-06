# Repo Workflow

`main` is the protected release branch.

`dev` is the protected integration branch.

Daily work happens on short-lived feature branches:

1. branch from `dev`
2. open a PR back into `dev`
3. let CI pass on the PR
4. merge into `dev`
5. let the nightly promotion workflow open or update the `dev -> main` PR and enable auto-merge

Rules:

- do not push directly to `main`
- do not push directly to `dev` unless you are repairing the branch itself
- keep feature branches narrow and disposable
- let `main` move only through the nightly promotion path unless there is an explicit hotfix
- `dev` should keep linear history
- `main` should stay protected, but it must allow merge-commit promotion from `dev`
- the nightly `dev -> main` PR must use merge auto-merge so promotion keeps shared ancestry between the branches

CI expectations:

- Rust: `cargo fmt --all --check` and `cargo test`
- Web: `npm run check:web` and `npm run build:web`

Branch settings that should stay in place:

- `dev` protected with required PRs and CI
- `main` protected with required PRs and CI
- repo auto-merge enabled so the nightly promotion PR can land itself once checks finish
- `main` must not enforce linear history, because nightly promotion relies on merge commits
