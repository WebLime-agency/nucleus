# Phase 0 Backlog

Status: Draft

## Goals

- establish the monorepo
- make the Rust daemon and web UI build cleanly
- lock the first API and domain boundaries
- prepare for Phase 1 implementation

## Immediate Tasks

- [x] scaffold the monorepo
- [x] create the Rust workspace
- [x] create the first SvelteKit web app
- [x] wire the first daemon-backed dashboard view
- [x] copy the current product RFC into the repo
- [x] decide the first persisted state layout
- [x] define the session domain types
- [x] define the runtime domain types
- [x] define the host-status API surface
- [x] define the approval and audit event model
- [x] choose the first daemon runtime port
- [x] add basic SQLite wiring
- [x] define the host telemetry model beyond the current overview shell
- [x] define the process inspection and process-action safety model
- [x] add a WebSocket scaffold beside the HTTP health route
- [x] define the first host operations UI contract for CPU, memory, disk, and processes

## Locked Frontend Decisions

- SvelteKit remains UI-only. The daemon owns system behavior.
- Zod should be used on the SvelteKit side for form, mutation, and response-shape validation.
- Tailwind CSS should be the default styling layer for the web client.
- shadcn-svelte style primitives should be the default component structure for the web client.

## Revised Next Phase

The next phase should be host-operations parity, not model orchestration.

Mission Control already provides a concrete target:

- CPU dashboard with per-core visibility
- memory dashboard
- top-process inspection
- process termination from the UI
- machine-level stats such as disk and memory usage

Nucleus should replace that surface first.

## What Phase 1 Should Achieve

Phase 1 is where Nucleus stops being a good shell and becomes the machine operations console.

It should deliver:

- daemon-owned CPU, memory, disk, and process telemetry
- process lists that are useful operationally, not just technically correct
- safe process termination flows from the UI, scoped conservatively at first
- live host updates over a proper event channel, so the UI is not limited to polling
- a stronger frontend shell built on Tailwind, composed from shadcn-svelte style primitives, and guarded by Zod at the UI boundary

If Phase 1 lands cleanly, we should be able to say:

- Nucleus has replaced the current Mission Control host dashboards for daily use
- the daemon, not a Svelte endpoint, is the source of truth for machine operations
- the web UI is now an operational console instead of a read-only dashboard shell
- the base is strong enough to begin provider adapters and session orchestration next

## What Phase 2 Should Achieve

Phase 2 should move into AI runtime ownership.

It should deliver:

- the first real session lifecycle owned by the daemon
- the first provider adapter contracts for Claude and Codex
- the approval and audit model shape for agent work

If Phase 2 lands cleanly, we should be able to say:

- Nucleus can create and track sessions as first-class runtime objects
- the repo has the right contracts in place for replacing the older session-controller workflows next

Phase 2 is now underway with:

- daemon-managed Claude and Codex session creation, updates, and prompting
- runtime readiness probing with cached health
- the first daemon action catalog
- the first audit trail surfaced in the web UI

## Current Persistence Decision

Nucleus uses a hybrid persistence model:

- SQLite for structured operational state
- filesystem storage for transcripts, attachments, playbooks, and memory artifacts
- future indexing/search layered on top when needed

## Phase 1 Entry Criteria

Phase 1 starts when:

- `cargo check` passes for the workspace
- the web app builds statically
- the repo contains the first daemon and UI shells
- the web UI reads live daemon state instead of placeholders
- the API boundary for host status and process operations is documented
