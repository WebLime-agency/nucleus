# Scripts

Automation and development helpers for the Nucleus monorepo will live here.

`browser-sidecar.mjs` is a runtime asset, not only a development helper. Managed release packaging must ship it under `current/scripts/` with the matching Playwright Node modules so `nucleus-daemon` can launch the daemon-owned Browser runtime outside a source checkout.
