# Local Nucleus Instance Topology

This host intentionally keeps stable managed installs separate from source-checkout development.

## Canonical managed instances

| Role | URL | Service | State dir | Install root | Notes |
| --- | --- | --- | --- | --- | --- |
| Official/stable release | `http://mini-server:5201` | `nucleus-daemon.service` | `/home/eba/.nucleus` | `/home/eba/.nucleus/managed` | The official release instance and web UI update target. |
| DevProjects daily driver | `http://mini-server:5202` | `nucleus-dev-projects.service` | `/home/eba/.nucleus-dev-projects` | `/home/eba/tools/nucleus-dev-projects` | Daily driver for general dev-projects work. |
| WBL/DGA daily driver | `http://mini-server:5203` | `nucleus-wbl-dga.service` | `/home/eba/.nucleus-wbl-dga` | `/home/eba/tools/nucleus-wbl-dga` | Daily driver for WebLime/DGA client work. |

Ports `5201`, `5202`, and `5203` are not source-development ports. Do not run Vite, scratch daemons, or browser experiments on them.

## Source-checkout and browser development

Source-checkout work should be disposable:

1. Create a scratch state directory under `/tmp`.
2. Start the daemon on a non-canonical local port, usually `127.0.0.1:5299`.
3. Start Vite on a non-canonical web port, usually `5300`.
4. Stop both processes after the test.
5. Delete the scratch state directory and any temporary browser profile directories.

Example:

```bash
state_dir=$(mktemp -d /tmp/nucleus-source-dev.XXXXXX)
NUCLEUS_STATE_DIR="$state_dir" \
NUCLEUS_BIND=127.0.0.1:5299 \
cargo run -p nucleus-daemon

# Separate shell
NUCLEUS_DAEMON_ORIGIN=http://127.0.0.1:5299 \
NUCLEUS_WEB_PORT=5300 \
npm run dev:web

# Cleanup after testing
rm -rf "$state_dir"
```

The browser runtime is daemon-owned and may create browser profiles, screenshots, readable snapshots, downloads, and annotation artifacts under the active daemon state directory. This is another reason browser work should use scratch state until it is promoted to managed release.

Managed-release-style Browser verification should check the installed layout, not the source checkout:

```text
current/bin/nucleus-daemon
current/scripts/browser-sidecar.mjs
current/node_modules/playwright
current/node_modules/playwright-core
```

The sidecar must be resolvable from the managed release `current` tree when the daemon is started by systemd with `NUCLEUS_INSTALL_KIND=managed_release` and `NUCLEUS_INSTALL_ROOT` set.

## Verification

```bash
systemctl --user status nucleus-daemon.service nucleus-dev-projects.service nucleus-wbl-dga.service --no-pager
curl -sS http://127.0.0.1:5201/health
curl -sS http://127.0.0.1:5202/health
curl -sS http://127.0.0.1:5203/health
```
