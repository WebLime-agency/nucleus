# Repository Guidelines

`AGENTS.md` is the entrypoint for durable Nucleus product context.

Read these first:

- [docs/architecture.md](docs/architecture.md)
- [docs/workspace-model.md](docs/workspace-model.md)
- [docs/roadmap.md](docs/roadmap.md)
- [docs/decisions/0001-product-foundations.md](docs/decisions/0001-product-foundations.md)
- [docs/decisions/0002-managed-release-update-model.md](docs/decisions/0002-managed-release-update-model.md)
- [docs/managed-release.md](docs/managed-release.md)
- [docs/repo-workflow.md](docs/repo-workflow.md)

Prompt-time context rules:

- committed always-on product context lives in `include/`
- local private operator context lives in `.nucleus/include/`
- legacy `promptinclude/` and `*.promptinclude.md` remain supported, but new shared context should move to `include/`
- never put secrets, tokens, or machine-specific credentials in prompt include files

Durable product rules:

- the daemon owns sessions, routing, auth, persistence, updates, and machine actions
- clients render and steer, but they do not become a second backend
- the web client is a first-class client until native apps exist, so responsive phone and desktop behavior is release-blocking
- prefer shadcn/ui primitives and fix layout bugs at the shell/container level before adding page-specific patches
- stable product decisions belong in `docs/decisions/`
- roadmap changes belong in `docs/roadmap.md`
- prompt-time summaries should stay concise and live in `include/`

Promotion rule:

- if a private EBA note becomes stable and broadly true, promote it into the public docs and mirror the short version into `include/`
