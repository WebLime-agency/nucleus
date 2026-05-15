# 0003: Session browser runtime

## Status

Accepted

## Context

Nucleus needs a shared browser surface for UI verification, task automations, and basic browsing inside the session environment. A simple embedded iframe would not give agents reliable page inspection, screenshots, click/type actions, or session-scoped storage.

## Decision

Nucleus models Browser as a daemon-owned, session-scoped runtime. The web client renders a Browser drawer as a viewer/controller, while the daemon owns browser lifecycle, session association, artifacts, action contracts, and future policy enforcement.

The initial implementation establishes daemon REST contracts, session browser state, manual navigation, readable page snapshots, and stable element references. The runtime implementation is intentionally replaceable: a daemon-supervised Playwright/Chromium sidecar can be attached behind the same contracts to provide true viewport streaming, screenshots, clicks, typing, downloads, and richer accessibility snapshots.

## Consequences

- Browser state belongs to sessions, not to the web client.
- Future native clients can use the same daemon browser APIs.
- Browser snapshots and screenshots are session artifacts by default, not Memory.
- Cookies, storage, downloads, local/private-network access, and Vault boundaries must remain explicit in the security model.
- Agent-facing browser tools should operate on daemon-generated refs instead of invented selectors.
