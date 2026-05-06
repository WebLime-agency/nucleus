# Managed Release Follow-On Work

Status: parked follow-up plan

This document captures the next work that should follow the managed release updater and daemon/client
contract changes shipped in the current implementation pass.

These items are intentionally separate from the current update-model PR.

## Priority Order

### 1. Real systemd managed-release validation

- install a managed release onto a real Linux user session with `systemctl --user`
- verify `enable --now`, restart from the Settings page, reconnect behavior, and daemon relaunch
- confirm the daemon always comes back from `current/bin/nucleus-daemon`
- confirm the daemon serves `current/web` after restart

Why this is next:
- the code path is implemented, but local automated tests do not prove systemd behavior end to end

### 2. Browser-level update UX QA

- verify the Settings page wording and controls for both `dev_checkout` and `managed_release`
- verify tracked-channel edits, tracked-ref edits, and update-state refresh behavior
- verify update toast suppression after failed checks
- verify reconnect and restart messaging after daemon restart

Why this is next:
- the web app builds and typechecks cleanly, but this still needs manual browser validation

### 3. Compatibility policy enforcement

- define when `minimum_client_version` must be set for a release
- define when `minimum_server_version` matters for client-authored flows
- decide which capability flags should remain strings and which should become explicit booleans
- add client behavior for hard-fail vs degraded-mode compatibility mismatches

Why this is next:
- the transport and schema are in place now, but the policy is still intentionally permissive

### 4. Release publishing and distribution automation

- publish channel manifests and artifacts to the real distribution location
- define artifact retention and rollback retention policy
- add signing or stronger provenance guarantees if needed beyond checksum verification
- document the operator install path for managed releases

Why this is next:
- managed release install/update logic now exists locally, but production distribution is still a separate layer

## Recommended Extras

- add daemon tests for rollback/previous-link behavior after more than one managed release is installed
- add integration checks for manifest channel mismatches and missing target artifacts
- consider a cleanup policy for old `releases/` directories and downloaded archives
- add operator docs for switching channels and recovering from a failed restart
