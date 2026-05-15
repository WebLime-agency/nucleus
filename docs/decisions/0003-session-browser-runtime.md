# 0003: Session browser runtime

## Status

Accepted

## Context

Nucleus needs a shared browser surface for UI verification, task automations, and basic browsing inside the session environment. A simple embedded iframe would not give agents reliable page inspection, screenshots, click/type actions, or session-scoped storage.

## Decision

Nucleus models Browser as a daemon-owned, session-scoped runtime. The web client renders a Browser drawer as a viewer/controller, while the daemon owns browser lifecycle, session association, artifacts, action contracts, and future policy enforcement.

The runtime uses a daemon-supervised Playwright/Chromium sidecar behind the daemon REST contracts. Source checkouts resolve the sidecar from `scripts/browser-sidecar.mjs`; managed releases must package it at `current/scripts/browser-sidecar.mjs` with the matching Playwright Node modules under `current/node_modules/`.

Browser snapshots expose readable page text and daemon-generated refs. First-class agent tools may navigate, snapshot, screenshot, click, type, fill, scroll, press, and submit through those refs. Ref actions are preferred over invented selectors because the daemon can tie them to the current page snapshot.

Browser snapshots, screenshots, downloads, and annotation metadata are persisted as session/job artifacts. Annotation point inspection is user-facing: a Browser annotation may carry an operator comment that is appended into the session as a user turn with non-secret point and element metadata.

## Consequences

- Browser state belongs to sessions, not to the web client.
- Future native clients can use the same daemon browser APIs.
- Browser snapshots, screenshots, downloads, and annotations are session artifacts by default, not Memory.
- Cookies, storage, downloads, local/private-network access, and Vault boundaries must remain explicit in the security model.
- Agent-facing browser tools should operate on daemon-generated refs instead of invented selectors.
- Navigation failures should surface as page errors or tool errors, not be silently swallowed.
- Screencast polling must stop when there are no websocket clients or when frame broadcast fails, and it must ask the sidecar to stop the screencast before exiting.
