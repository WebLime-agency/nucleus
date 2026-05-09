# Dev server for `nucleus`

- Project: `nucleus` (path: `nucleus`)
- Port: **5201** (bind `0.0.0.0`)
- Preferred package manager: `npm`.

Nucleus is not a generic standalone Vite app. Development has two valid modes:

1. Split dev mode:
   - backend Nucleus daemon on `127.0.0.1:42240`
   - Vite web dev server on `http://mini-server:5201`
   - start the web server with `npm run dev:web` only after the backend is running
2. Production-style local mode:
   - build web assets with `npm run build:web`
   - run `nucleus-daemon` on `0.0.0.0:5201` with `NUCLEUS_WEB_DIST_DIR` pointing at `apps/web/build`

If using an already-running daemon on another port, set the proxy explicitly before starting Vite, for example:

```bash
NUCLEUS_WEB_PORT=5201 \
NUCLEUS_DAEMON_ORIGIN=http://127.0.0.1:42240 \
npm run dev:web
```

Do not use `NUCLEUS_DAEMON_ORIGIN=http://127.0.0.1:5202` for official Nucleus
source-checkout testing. Port `5202` is the EBA managed instance, backed by
`/home/eba/.nucleus-eba`; using it from the official/dev UI pollutes the test
boundary and makes the UI look like it is operating on the wrong install.

Do not replace the Nucleus daemon with a web-only Vite process unless a backend daemon is also available through `NUCLEUS_DAEMON_ORIGIN`. Otherwise `/api/*` and `/ws` will fail and the UI will show request failures.

Framework defaults (Astro 4321, Vite 5173) are NOT published outside this
project's allocated port - always pass `--port` and `--host` explicitly or the
server won't be reachable at mini-server:<port>.

## Vite / Astro config requirements

The dev server MUST be reachable at `http://mini-server:5201`. Two things
must be true in the project's config file (`vite.config.ts` or
`astro.config.mjs`):

1. `server.port` hardcoded to **5201** (do NOT leave it at the framework
   default of 5173/4321, and do NOT rely solely on the CLI `--port` flag).
2. `server.allowedHosts` includes `mini-server` - Vite/Astro block unknown
   Host headers by default and will 403 browser requests from the Mac
   otherwise.

Minimum config:

```ts
server: {
  host: '0.0.0.0',
  port: 5201,
  allowedHosts: ['mini-server', 'localhost', '127.0.0.1']
}
```

If the existing config doesn't match, fix the config file BEFORE starting the
dev server. Do not start the server and hope it works - the user cannot reach
it from their Mac if `allowedHosts` is missing, even when the port is right.
