# Nucleus source-development ports

Nucleus is not a generic standalone Vite app. Development requires a daemon and a web client.

Source-checkout web and daemon processes must come from the same branch/worktree. Do not pair a feature-branch web UI with a managed release daemon, and do not reuse managed release state for branch testing.

Default source-dev pair:

- daemon: `127.0.0.1:5299`
- Vite web UI: `5300`

Example:

```bash
state_dir=$(mktemp -d)
NUCLEUS_STATE_DIR="$state_dir" \
NUCLEUS_BIND=127.0.0.1:5299 \
cargo run -p nucleus-daemon

# second shell
NUCLEUS_DAEMON_ORIGIN=http://127.0.0.1:5299 \
NUCLEUS_WEB_PORT=5300 \
npm run dev:web

# after testing: stop both processes, then remove "$state_dir"
```

The source web config defaults to the scratch pair above. Reserved managed-release ports should only be used for intentional diagnostics.

Private machine-specific topology, including exact managed instance ports, service names, state directories, and Tailscale URLs, belongs in ignored `.nucleus/include/` context rather than committed repo docs.
