# Repository Guidelines

`AGENTS.md` is the entrypoint for durable Nucleus product context.

Read these first:

- [docs/architecture.md](docs/architecture.md)
- [docs/workspace-model.md](docs/workspace-model.md)
- [docs/roadmap.md](docs/roadmap.md)
- [docs/decisions/0001-product-foundations.md](docs/decisions/0001-product-foundations.md)
- [docs/repo-workflow.md](docs/repo-workflow.md)

Prompt-time context rules:

- committed always-on product context lives in `include/`
- local private operator context lives in `.nucleus/include/`
- legacy `promptinclude/` and `*.promptinclude.md` remain supported, but new shared context should move to `include/`
- never put secrets, tokens, or machine-specific credentials in prompt include files

Durable product rules:

- the daemon owns sessions, routing, auth, persistence, updates, and machine actions
- clients render and steer, but they do not become a second backend
- stable product decisions belong in `docs/decisions/`
- roadmap changes belong in `docs/roadmap.md`
- prompt-time summaries should stay concise and live in `include/`

Promotion rule:

- if a private EBA note becomes stable and broadly true, promote it into the public docs and mirror the short version into `include/`
