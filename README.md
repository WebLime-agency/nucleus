# Nucleus

Nucleus is a local AI control plane. It is being built to unify local session control, routing, and machine operations inside one daemon-owned system.

## Current Focus

V1 is intentionally narrow:

- replace the daily local AI session surface currently handled by older local tooling
- replace the current Mission Control host dashboards for CPU, memory, and process operations

The long-term direction is one daemon-owned system of record for local agent work, model routing, approvals, automation, and host visibility.

## What Is In This Repo

This repository is a monorepo for the Nucleus runtime and clients:

- Rust daemon
- operator CLI
- SvelteKit web UI
- shared protocol and domain crates
- future native clients

```text
apps/
  web/           SvelteKit web UI
clients/         future native clients
crates/
  daemon/        Rust HTTP/WebSocket server
  cli/           operator CLI
  core/          shared product constants and domain concepts
  protocol/      wire types shared across surfaces
  storage/       persistence planning and state layout
  adapters-*/    provider and compatibility adapters
docs/
  rfc/           architecture and product RFCs
  backlog/       implementation checklists
migrations/      database migrations
scripts/         repo automation
```

## Architecture

Core runtime rules:

- the Rust daemon owns runtime truth and persistence
- the web app is a client, not the backend
- REST handles bootstrap reads and mutations
- WebSocket handles live state and event streaming

Current frontend defaults:

- Zod at the SvelteKit boundary
- Tailwind CSS for styling
- shadcn-svelte style primitives for reusable UI structure

## Persistence

Nucleus uses a hybrid persistence model:

- SQLite for structured operational truth
- filesystem storage for transcripts, attachments, playbooks, memory documents, and other artifacts
- optional indexing and search later

Default local state lives outside the repository:

- state root: `~/.nucleus`
- SQLite database: `~/.nucleus/nucleus.db`

If you want multiple local installs on the same machine, set `NUCLEUS_STATE_DIR` per install so each runtime gets its own isolated state tree.

## Instance Configuration

These environment variables let multiple Nucleus installs run side by side without sharing state or ports:

- `NUCLEUS_INSTANCE_NAME` - label shown in the UI
- `NUCLEUS_STATE_DIR` - state root for SQLite, scratch, transcripts, and artifacts
- `NUCLEUS_BIND` - daemon bind address, for example `127.0.0.1:42240`
- `NUCLEUS_REPO_ROOT` - explicit git checkout root for update checks
- `NUCLEUS_WEB_PORT` - Vite dev server port
- `NUCLEUS_DAEMON_ORIGIN` - web-to-daemon proxy target, for example `http://127.0.0.1:42240`

Example split for two local installs:

```bash
# upstream / official checkout
export NUCLEUS_INSTANCE_NAME="Nucleus Dev"
export NUCLEUS_STATE_DIR="$HOME/.nucleus-dev"
export NUCLEUS_BIND="127.0.0.1:42240"
export NUCLEUS_WEB_PORT="5202"
export NUCLEUS_DAEMON_ORIGIN="http://127.0.0.1:42240"

# personal daily-use checkout
export NUCLEUS_INSTANCE_NAME="Nucleus EBA"
export NUCLEUS_STATE_DIR="$HOME/.nucleus-eba"
export NUCLEUS_BIND="127.0.0.1:42241"
export NUCLEUS_WEB_PORT="5201"
export NUCLEUS_DAEMON_ORIGIN="http://127.0.0.1:42241"
```

## Local Development

Prerequisites:

- Rust toolchain
- Node.js and npm
- provider CLIs you intend to route through, such as `codex` or `claude`

Rust workspace:

```bash
cargo check
cargo run -p nucleus-daemon
```

Web UI:

```bash
source ~/.nvm/nvm.sh
npm install
npm run dev:web
```

Assigned web port: `5201`

Useful access URLs while the dev server is running:

- `http://127.0.0.1:5201`
- `http://localhost:5201`

If you bind the web dev server to `0.0.0.0`, you can also reach it from your LAN or tailnet using your machine hostname or IP on port `5201`.

The daemon binds to `127.0.0.1:42240` by default. Override it with `NUCLEUS_BIND`.

The web app reads `NUCLEUS_WEB_PORT` and `NUCLEUS_DAEMON_ORIGIN` during development, so separate checkouts can point at different daemons cleanly.

## Current Runtime Surface

Today there are two practical ways to talk to Nucleus:

- terminal checks through the CLI and raw daemon APIs
- the browser dashboard

Current browser surfaces:

- host overview, CPU, memory, and process operations
- daemon-managed sessions on `/sessions`
- daemon audit visibility and runtime status

Useful smoke checks:

```bash
cargo run -q -p nucleus-cli -- health
curl http://127.0.0.1:42240/api/health
curl http://127.0.0.1:42240/api/overview
```

## Status

The project is in active early development. Expect API churn while the daemon contracts and session model settle.

The current repo already includes:

- daemon-owned host telemetry
- process inspection and termination flows
- workspace and project discovery
- router profiles and workspace model defaults
- background prompt jobs with live websocket progress
- prompt include discovery from workspace, project, and session roots
- daemon-owned update checks plus in-app update notifications for git-based installs

## License

MIT. See [LICENSE](LICENSE).
