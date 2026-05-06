# Roadmap

This file tracks the durable product direction for Nucleus.

## Near-Term

The near-term goal is to make Nucleus a credible daily driver for local AI work.

That means finishing and stabilizing:

- daemon-owned sessions
- workspace and project activation
- router profiles and model settings
- machine telemetry and process control
- prompt assembly and durable context layering
- auth, managed-release updates, and restart flows
- web UI ergonomics for heavy daily use

Managed-release channels are now part of the product baseline: public installs follow `stable`, `beta`, or `nightly` manifests, while source checkouts remain contributor-only.

## V1

V1 should replace the current fragmented local setup for:

- session orchestration
- machine dashboards
- routing and model selection
- approvals and operator steering

The daemon should already be the product brain by that point. The web UI should be a strong first client, not a second backend.

## Later

Later phases can expand into:

- richer long-term memory controls
- automation and approval policies
- more capable native clients
- remote and multi-device workflows
- deeper system actions and agentic operations

## Promotion Rule

Only stable, broadly true direction belongs here.

Fast-moving experiments, local notes, and operator-specific judgment should stay in `.nucleus/include/` until they are mature enough to promote into the public repo.
