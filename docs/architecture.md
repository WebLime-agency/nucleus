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
- recent instance log index

The state directory stores larger or local-only artifacts such as:

- plaintext local auth tokens
- instance log files under `logs/`
- transcripts
- memory documents
- scratch outputs
- future attachments and playbooks

Instance logs are product support/debugging events owned by the daemon. They are written as structured JSONL under `<state-dir>/logs/events.jsonl`, indexed in SQLite for authenticated Workspace -> Logs APIs, redacted before persistence, and kept out of prompt context.

## Context Model

There are two layers of durable context:

1. Public, committed product context in `docs/` and `include/`
2. Local, private operator context in `.nucleus/include/`

The public layer explains what Nucleus is and why it behaves the way it does.

The private layer is for local deployment notes, active priorities, and operator-specific context that should not ship in the repo.

## Worker Context Compaction

Long-running worker jobs keep their active conversation in `WorkerCheckpoint.conversation`. Before each Utility Worker model call, the daemon estimates the compiled prompt size with a character-count heuristic and compares it with a conservative model-keyed context threshold. When the next call is likely to exceed the threshold, the daemon asks the configured Utility Worker model to summarize the oldest safe checkpoint window.

Compaction preserves daemon-owned prompt layers, accepted memory, skill layers, MCP catalogs, and tool catalogs because those are rebuilt into the compiled turn outside the checkpoint window. It also preserves the most recent checkpoint turns verbatim and avoids the assistant turn associated with a still-pending tool action.

Successful compaction replaces the selected checkpoint window with a synthetic `system` message marked `compacted=true` and carrying the original checkpoint range. The summary includes a `[Compacted: <range> via <model>]` marker plus preserved identifiers, artifact ids, file paths, image attachment metadata, and user preferences. The daemon frames compacted content as untrusted historical summary rather than new instructions. Prompt compilation replays compacted summaries as non-authoritative user history, and replays image attachments from compacted metadata as a separate non-system history turn so multimodal context remains available without producing system-role image payloads. The daemon records `memory.compaction.applied` audit events for successful rewrites and `memory.compaction.failed` when the compaction model returns malformed output or the compaction call fails. Failed compaction leaves the original checkpoint untouched and the worker continues with the uncompacted prompt.
