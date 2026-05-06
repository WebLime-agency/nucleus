# Architecture

Nucleus is a local AI control plane.

The Rust daemon is the product core. It owns:

- session lifecycle
- model routing
- machine operations
- auth
- persistence
- HTTP and WebSocket contracts
- update and restart logic

The web app in this repo is one client of that daemon. Future macOS, iOS, or other clients should talk to the same daemon contracts instead of reimplementing backend logic.

## Runtime Shape

Nucleus has two runtime shapes.

### Development

- daemon on a private backend port
- SvelteKit dev server on the assigned UI port

This exists for UI iteration speed.

### Installed Product

- one daemon process
- one public bind
- built web assets served by the daemon
- REST and WebSocket on the same origin
- token auth on `/api/*` and `/ws`
- managed releases tracking `stable`, `beta`, or `nightly` channel manifests

That is the target deployment model because it keeps the boundary clean and makes future clients easier to ship.

The daemon serves the web bundle from the active managed release. The installed product does not pull git branches or rebuild from source.

## Product Boundary

The daemon is the system of record.

Clients may:

- read snapshots
- send mutations
- subscribe to live updates
- store local presentation state
- store local auth tokens for reconnect

Clients may not:

- invent backend truth
- bypass daemon-owned actions
- redefine routing, auth, or session lifecycle
- become the source of truth for durable product state

## Persistence

Nucleus uses hybrid persistence.

SQLite stores structured operational truth such as:

- sessions
- turns
- workspace settings
- router profiles
- auth token hashes
- audit events

The state directory stores larger or local-only artifacts such as:

- plaintext local auth tokens
- transcripts
- memory documents
- scratch outputs
- future attachments and playbooks

## Context Model

There are two layers of durable context:

1. Public, committed product context in `docs/` and `include/`
2. Local, private operator context in `.nucleus/include/`

The public layer explains what Nucleus is and why it behaves the way it does.

The private layer is for local deployment notes, active priorities, and operator-specific context that should not ship in the repo.
