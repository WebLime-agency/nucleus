# Update System Follow-Up

Status: Active plan

## Why Revisit This

The current update flow started as a contributor checkout workflow. The managed release ADR makes that insufficient for the product direction:

- normal installs must follow release channels, not git branches
- the daemon must own durable update truth
- the embedded web build served by the daemon must match the daemon release
- future native clients need explicit compatibility metadata instead of guessing from failures

This plan is limited to the managed release update model and the daemon/client update contract. It does not cover the GitHub promotion pipeline.

## Product Rules

- `dev_checkout` is for contributors running from source
- `managed_release` is the real product install mode
- git-based self-update stays limited to `dev_checkout`
- `managed_release` must not shell out to `git pull`
- release-channel tracking and branch tracking stay separate
- the daemon remains the source of truth for update config, update history, restart state, and compatibility metadata

## First Safe Slice

The first slice should land before artifact packaging:

1. Split install identity into explicit `install_kind`.
   - `dev_checkout`
   - `managed_release`

2. Persist daemon-owned update state in `app_settings`.
   - Use a versioned update-state payload so config and history survive reconnects and restarts.

3. Separate durable tracked-target state from runtime facts.
   - current running version
   - current checkout ref and commit for `dev_checkout`
   - tracked release channel for `managed_release`
   - tracked git ref for `dev_checkout`
   - last successful check facts
   - last attempted check facts
   - latest error

4. Tighten the Settings UI and toast behavior.
   - show tracked target explicitly
   - show last successful check separately from the latest attempted check
   - keep stale successful facts visible as history, not as fresh current facts
   - only raise update toasts from a successful fresh check

5. Add compatibility metadata to the daemon contract.
   - include explicit surface markers and capability flags now
   - fill in minimum-version policy when native clients begin shipping

## Daemon-Owned Update State

Persist this state in `app_settings` under a versioned update-state record:

- `tracked_channel`
- `tracked_ref`
- `update_available`
- `last_successful_check_at`
- `last_successful_target_version`
- `last_successful_target_release_id`
- `last_successful_target_commit`
- `last_attempted_check_at`
- `last_attempt_result`
- `latest_error`
- `latest_error_at`
- `restart_required`

Do not persist runtime-derived facts that should be recomputed on startup:

- current running version
- current executable path
- current repo root
- current checkout ref
- current commit
- dirty worktree state
- compatibility capability set

## Protocol Contract

### Instance summary

- rename `install_mode` to `install_kind`
- make `repo_root` nullable so managed releases do not pretend every install is a repository checkout

### Settings summary

Add top-level `compatibility` metadata:

- `server_version`
- `minimum_client_version`
- `minimum_server_version`
- `surface_version`
- `capability_flags[]`

### Stream connected event

Mirror the same compatibility payload in the websocket `connected` event so future clients can evaluate the daemon contract immediately after connecting.

### Update status

Replace ambiguous update fields with explicit state:

- `install_kind`
- `tracked_channel`
- `tracked_ref`
- `repo_root`
- `current_ref`
- `remote_name`
- `remote_url`
- `current_commit`
- `current_commit_short`
- `latest_commit`
- `latest_commit_short`
- `latest_version`
- `latest_release_id`
- `update_available`
- `dirty_worktree`
- `restart_required`
- `last_successful_check_at`
- `last_attempted_check_at`
- `last_attempt_result`
- `latest_error`
- `latest_error_at`
- `state`
- `message`

Rules:

- `last_successful_*` only changes after a successful fresh check
- `last_attempted_*` changes for every check attempt
- `latest_error` records the latest failure without erasing the last successful facts
- `update_available` reflects the latest known successful check result, not a failed retry

## Settings UI Contract

The Settings page should distinguish:

- install kind
- current running version
- tracked target
- current ref for `dev_checkout`
- latest known target from the last successful check
- last successful check timestamp
- last attempted check timestamp and result
- latest error
- restart requirement

Rules:

- `managed_release` speaks in channels and versions
- `dev_checkout` may show repo root, tracked ref, current ref, commit, and dirty worktree state
- if the checkout is detached or on a different branch than the tracked ref, show an explicit error state instead of pretending update facts are current

## Toast Contract

- key dismissals by the latest known target identifier, not by remote commit alone
- only show the toast when:
  - `update_available` is true
  - `restart_required` is false
  - `last_attempt_result` is `success`
  - the target identifier differs from the dismissed value

If the latest attempt failed, keep the historical availability in Settings but suppress the "fresh update available" toast.

## Execution Slices

### Slice 1 - Update-state correctness and protocol hardening

- add the persisted daemon-owned update-state record
- expose `install_kind`
- expose tracked channel/ref and explicit check timestamps
- stop using `checked_at` as a catch-all field
- stop keying update toasts from stale remote commit data
- keep managed releases honest by exposing the tracked channel without implementing a fake updater

### Slice 2 - Install-mode split and configuration surfaces

- add install-time or CLI configuration for `tracked_channel` and `tracked_ref`
- lock git-based update actions to `dev_checkout`
- decide whether tracked refs become user-editable in the UI or CLI
- define migration for legacy source-based service installs

### Slice 3 - Artifact-based managed release updater

- publish channel manifests and platform artifacts with checksums
- resolve the latest compatible artifact for the tracked channel
- download, verify, stage, swap, and preserve rollback state
- restart the daemon onto the new artifact and embedded web bundle

### Slice 4 - Compatibility enforcement

- populate minimum client and server versions from policy
- make the web client consume compatibility metadata instead of assuming support
- require future Swift/macOS/iOS clients to check compatibility before enabling full interaction

## Edge Cases To Cover

- deleted tracked remote refs
- detached HEAD checkouts
- current checkout ref differs from tracked ref
- dirty worktrees
- restart failures after a successful update swap
- background reconnects after a daemon restart
- managed releases with no artifact support yet in the running build

## Open Questions

- Should `tracked_ref` be install-time configuration only, or should the daemon expose a mutation endpoint for contributor installs?
- When the tracked ref differs from the live checkout, should the UI remain read-only until the checkout returns to the tracked ref?
- Which compatibility capabilities deserve dedicated booleans later, and which should remain capability strings?
