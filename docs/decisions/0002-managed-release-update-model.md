# 0002: Managed Release Update Model

## Status

Proposed

## Context

Nucleus currently exposes update controls from the daemon, but the implementation still behaves like a source checkout operator flow.

Today, the daemon update path:

- inspects the live git checkout
- treats the active checkout branch as the update target
- runs `git pull --ff-only`
- rebuilds the daemon from source
- rebuilds the embedded web bundle from source when needed
- restarts or reexecs the daemon process

That model is acceptable for contributors running Nucleus from a clone, but it is too fragile for the product shape Nucleus is aiming for.

The product direction already assumes:

- the daemon is the product core
- the web UI is one first-class client of the daemon
- future native clients should connect to the same daemon contracts
- releases should feel safe and comfortable for normal users

Those goals require a release model that is independent from local git checkout state and local build toolchain availability.

They also require a compatibility contract between the daemon and clients so that future clients can reason about whether a server is safe to talk to.

## Decision

Nucleus will support two explicit install modes:

1. `dev_checkout`
2. `managed_release`

### 1. `dev_checkout`

`dev_checkout` is for contributors and local source-based operators.

Properties:

- runs from a git checkout
- may use git-based update operations
- may use local Cargo and Node toolchains
- may expose branch and commit details directly
- is not the default product update experience

Git-based self-update remains allowed only for `dev_checkout`.

### 2. `managed_release`

`managed_release` is the normal product install mode.

Properties:

- runs from versioned release artifacts, not from a source checkout
- follows a release channel, not a git branch
- updates by downloading, verifying, swapping, and restarting artifacts
- does not require local Rust or Node toolchains
- is the intended mode for normal users

`managed_release` is the release model that Nucleus should optimize for as the product grows beyond the web client.

## Release Model

Nucleus release channels are:

- `stable`
- `beta`
- `nightly`

Branch semantics and release semantics are separate:

- `main` remains the release source branch for maintainers
- `dev` remains the integration branch for maintainers
- installed Nucleus products follow release channels

Users should not be described as "tracking `main`".

Instead:

- CI builds release artifacts from the appropriate source branch state
- those artifacts are published to a release channel
- installed daemons track the chosen release channel

This keeps source control concerns separate from product update concerns.

## Daemon-Owned Update State

The daemon will become the authority for durable update configuration and update history.

At minimum, Nucleus should persist:

- `install_kind`
- `tracked_channel`
- `tracked_version`
- `tracked_release_id` or equivalent release identifier
- `last_successful_check_at`
- `last_successful_target_version`
- `last_attempt_at`
- `last_attempt_result`
- `last_error`
- `restart_required`

The daemon should also keep runtime facts separate from tracked-release facts:

- current running version
- current executable or install location
- current checkout branch or commit when in `dev_checkout`
- tracked release channel
- latest known release on that channel

Failed checks must not overwrite or masquerade as fresh release facts.

## Update UX Contract

The Settings surface should distinguish between:

- current install identity
- tracked release target
- last successful check
- latest attempted check
- latest update error
- update availability
- restart requirement

This is required so the UI does not show stale remote facts as if they came from a successful fresh check.

For `managed_release`, the UI should speak in terms of:

- installed version
- channel
- latest available version
- checked at
- restart status

For `dev_checkout`, the UI may additionally show:

- repository root
- branch
- current commit
- dirty worktree state

## Artifact Update Contract

`managed_release` updates must be artifact-based.

Expected flow:

1. Check the configured release channel.
2. Resolve the latest compatible release artifact for the current platform.
3. Download the artifact and checksum.
4. Verify integrity.
5. Stage the new daemon and bundled web assets.
6. Atomically swap into place.
7. Preserve a rollback copy of the previous install.
8. Restart the daemon cleanly.

The release artifact should contain the daemon and the matching embedded web bundle for that release.

This means the web UI served by the daemon updates automatically when the daemon updates.

## Client Compatibility Contract

Nucleus will introduce explicit daemon-to-client compatibility metadata.

At minimum, the daemon should expose:

- `server_version`
- `minimum_client_version`
- `minimum_server_version` when relevant to client-authored flows
- capability flags or surface-version markers

Clients must not infer compatibility from unrelated transport or decode failures.

Instead, they should read server-authored compatibility metadata and decide whether:

- the connection is fully supported
- the connection is degraded but usable
- the client must refuse or warn

This is required before shipping additional first-class clients.

## Distribution Boundaries

The Nucleus daemon release path and native client app release path are separate concerns.

The daemon:

- owns server updates
- owns embedded web client updates
- owns compatibility metadata

Future native clients:

- should use their own distribution path
- should rely on daemon compatibility metadata
- should not depend on the daemon performing git operations

## Non-Goals

This decision does not define:

- the exact GitHub promotion workflow for moving `dev` into `main`
- the exact app-store or native-client release process
- whether `beta` and `nightly` should be exposed in the first public UI pass

Those are related but separate decisions.

## Consequences

Benefits:

- normal users get a stable updater model
- update checks become clearer and more truthful
- the embedded web UI version always matches the daemon release
- future clients can negotiate compatibility explicitly
- source checkout workflows remain available without defining the product path

Tradeoffs:

- CI and release packaging must become more sophisticated
- the daemon protocol will need new metadata fields
- install-mode detection and migration must be designed carefully
- there will be a transition period where both update models exist

## Rollout Plan

### Phase 1: Correctness and state separation

- add durable update settings in daemon-owned storage
- distinguish tracked release target from current runtime facts
- stop reusing stale update facts on failed checks
- update the Settings UI to reflect current status truthfully

### Phase 2: Install mode split

- detect and expose `dev_checkout` vs `managed_release`
- restrict git-based update operations to `dev_checkout`
- define the release-channel config contract for `managed_release`

### Phase 3: Artifact-based release updates

- publish platform release artifacts with checksums
- add CLI and daemon flows for channel-based artifact upgrades
- add rollback-safe swap and restart behavior

### Phase 4: Compatibility metadata

- add explicit server/client compatibility fields and capability markers
- update the web client to consume them
- require future native clients to use the same contract

## Follow-Up

Implementation should begin with a dedicated design and execution pass focused on:

- update state schema
- install mode detection
- managed release packaging
- release channel resolution
- compatibility metadata

The GitHub promotion workflow should be handled in a separate workstream.
