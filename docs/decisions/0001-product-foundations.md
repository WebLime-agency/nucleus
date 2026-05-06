# 0001: Product Foundations

## Status

Accepted

## Context

Nucleus needs to be more than a dashboard. It is intended to become the durable control plane for local AI work, machine operations, and future multi-client workflows.

That requires clear ownership boundaries and a stable way to retain product context over time.

## Decisions

### 1. The daemon owns product truth

The Rust daemon is authoritative for:

- sessions
- routing
- auth
- persistence
- machine actions
- update flow
- HTTP and WebSocket contracts

### 2. Clients are steering surfaces

Clients may render state, send mutations, and subscribe to events, but they do not define durable backend behavior.

### 3. Public and private product context are split

Committed product context lives in:

- `AGENTS.md`
- `docs/`
- `include/`

Local private operator context lives in:

- `.nucleus/include/`

### 4. Prompt assembly supports shared-first layering

Committed shared include files should be loaded before local private overrides so public product truth remains visible and private notes can extend or refine it.

### 5. Stable decisions are written down

Broad product decisions belong in `docs/decisions/`.

Prompt-time summaries belong in `include/`.

## Consequences

Benefits:

- future sessions have durable product memory
- contributors and AI tools share the same source material
- private operator context stays local
- public repo history captures why the product is shaped the way it is

Tradeoffs:

- docs and prompt summaries must stay in sync
- private local notes can drift if they are not periodically promoted
