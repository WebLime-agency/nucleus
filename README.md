# Nucleus

Nucleus is a local AI control plane. The Rust daemon is the product core. It owns sessions, routing, machine operations, auth, persistence, and the HTTP/WebSocket contracts that every client uses.

The SvelteKit app in this repo is one client. Future native clients should talk to the same daemon contracts instead of reimplementing backend logic.

Repo workflow lives in [docs/repo-workflow.md](docs/repo-workflow.md).

Shared product context starts in [AGENTS.md](AGENTS.md).

Managed release install and recovery docs live in [docs/managed-release.md](docs/managed-release.md).

## Repo Layout

```text
apps/
  web/           SvelteKit web client
clients/         future native clients
crates/
  daemon/        Rust HTTP/WebSocket server
  cli/           operator CLI
  core/          shared constants and product-level helpers
  protocol/      shared wire types
  storage/       SQLite + state-dir planning
  adapters-*/    provider and compatibility adapters
docs/
  rfc/           focused design documents
  backlog/       implementation checkpoints
  decisions/     durable product decisions
include/         committed prompt-time product context
```

## Current Product Shape

- bearer-token auth is enforced on `/api/*` and `/ws`
- `GET /health` stays public
- the daemon can serve the built web app directly from `apps/web/build`
- REST handles bootstrap reads and mutations
- WebSocket handles live telemetry, session updates, and prompt progress
- structured operational truth lives in SQLite
- larger artifacts live in the state directory on disk

## State Directory

Default state root:

```text
~/.nucleus
```

Important paths:

- database: `~/.nucleus/nucleus.db`
- local auth token: `~/.nucleus/local-auth-token`
- transcripts: `~/.nucleus/transcripts/`
- memory docs: `~/.nucleus/memory/`
- scratch work: `~/.nucleus/scratch/`

To isolate multiple installs on the same machine, set `NUCLEUS_STATE_DIR` per instance.

## Product Context

Nucleus keeps durable product context in two layers:

- public repo context in `AGENTS.md`, `docs/`, and `include/`
- private local operator context in `.nucleus/include/`

Use the public layer for architecture, stable decisions, and roadmap material that should ship with the project.

Use the private layer for local deployment notes, active priorities, and operator-specific context that should not be committed.

## CLI

The binary name is `nucleus`.

Current commands:

```bash
nucleus health
nucleus auth local-token
nucleus setup local
nucleus setup server
nucleus setup client --server-url http://mini-server:5201 --token <TOKEN>
nucleus install-service --enable
nucleus release install --channel stable --enable
```

What they do:

- `auth local-token` prints the current local bearer token
- `setup local` prepares a same-machine instance
- `setup server` prepares a remotely reachable instance and prints the local, host, and Tailscale URLs when available
- `setup client` validates a server URL and token
- `install-service` writes a `systemd --user` unit on Linux and can enable it immediately
- `release install` installs a managed release from the selected product channel

## Local Development

Development still uses a split runtime:

- daemon on `127.0.0.1:42240`
- Vite dev server on `http://mini-server:5201`

Rust:

```bash
cargo test
```

Web:

```bash
source ~/.nvm/nvm.sh
npm run check:web
npm run build:web
```

If you want the Vite client during development:

```bash
source ~/.nvm/nvm.sh
npm run dev:web
```

## Production-Style Local Run

Build the web app first:

```bash
source ~/.nvm/nvm.sh
npm run build:web
```

Then run the daemon with the built web output:

```bash
NUCLEUS_BIND=0.0.0.0:5201 \
NUCLEUS_WEB_DIST_DIR=/home/eba/tools/nucleus/apps/web/build \
cargo run -p nucleus-daemon
```

The web UI, REST API, and WebSocket stream now come from the same server.

Retrieve the access token:

```bash
cargo run -p nucleus-cli --bin nucleus -- auth local-token
```

## Service Install

On Linux, the CLI can install a `systemd --user` service that runs the daemon and serves the production web build:

```bash
source ~/.nvm/nvm.sh
npm run build:web
cargo run -p nucleus-cli --bin nucleus -- install-service --enable --bind 0.0.0.0:5201
```

That unit writes the key runtime env vars:

- `NUCLEUS_INSTANCE_NAME`
- `NUCLEUS_STATE_DIR`
- `NUCLEUS_BIND`
- `NUCLEUS_REPO_ROOT`
- `NUCLEUS_WEB_DIST_DIR`
- `NUCLEUS_SYSTEMD_UNIT`

## Managed Release Install

Managed releases are the public product install path. They track release channels rather than git branches.

```bash
nucleus release install --channel stable --enable --bind 0.0.0.0:5201
```

The default channel manifests are published as GitHub release assets:

- `stable`: `https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-stable/manifest-stable.json`
- `beta`: `https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-beta/manifest-beta.json`
- `nightly`: `https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-nightly/manifest-nightly.json`

The managed artifact includes the daemon, the operator CLI, and the matching embedded web bundle. See [docs/managed-release.md](docs/managed-release.md) for channel switching, update, publishing, and rollback details.

## Tailscale

Nucleus does not need a separate web server for tailnet access. Bind the daemon to a reachable address, then use the server URL and bearer token from another device.

Typical direct tailnet URL:

```text
http://mini-server:5201
```

When Tailscale MagicDNS is available, `nucleus setup server` also prints the fully qualified Tailscale hostname.

## Auth Model

- the daemon auto-provisions a local bearer token on first start
- the token hash is stored in SQLite
- the plaintext token is stored in the state directory outside the repo
- the browser client stores the token locally and sends it on every API request and WebSocket connection

## Status

The repo already includes:

- host telemetry
- CPU, memory, and process control surfaces
- daemon-owned sessions
- router profiles and workspace defaults
- background prompt jobs with live progress
- include directory discovery for prompt assembly
- daemon-managed update checks and apply flow for contributor git installs
- managed-release install/update/restart flow for channel artifacts
- stable, beta, and nightly channel publishing automation

## License

MIT. See [LICENSE](LICENSE).
