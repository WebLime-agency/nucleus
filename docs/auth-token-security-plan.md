# Auth Token Security Plan

## Status

Proposed

## Summary

Nucleus should move from a retrievable plaintext local bearer token toward a safer credential lifecycle.

The current implementation stores the local API token as a `0600` plaintext file in the instance state directory and stores only a hash in SQLite. That is workable for early local development, but it is not the long-term product model. A local control plane that can operate on files, run commands, and manage sessions should treat its own access token as a first-class secret.

This plan defines the target model, migration path, and implementation phases for token rotation, one-time token reveal, platform credential storage, and safer browser/client handling.

## Goals

- stop treating the local auth token as a durable retrievable plaintext value by default
- keep the daemon as the authority for auth, token validation, and rotation
- provide a clean token rotation command and Settings UI action
- show newly generated tokens once, then store only verifiable secret material server-side
- support secure client-side persistence through OS credential stores where available
- keep a deliberate headless/server fallback for environments without an OS credential store
- avoid fake encryption where encrypted data and the decryption key live side by side
- make token recovery and rotation understandable for non-technical users

## Non-goals

- replacing bearer-token auth with full account management
- adding remote identity, OAuth, or cloud auth
- making browser local storage a trusted secret vault
- requiring a desktop native app before the web client remains usable
- breaking existing managed installs during migration

## Current State

Server-side state:

- plaintext token file: `<state-dir>/local-auth-token`
- token hash in SQLite: `app_settings.auth.local_token_hash`
- token validation compares the provided bearer token against the stored hash
- if the plaintext token file is missing or mismatched at daemon startup, Nucleus generates a new token and updates the hash

Client-side state:

- the web client asks the user for the token when auth fails
- the web client persists the token locally for reconnect
- REST requests use `Authorization: Bearer <token>`
- WebSocket connections currently pass the token as a query parameter

Operational issues:

- the daemon can read the token back at any time
- the CLI prints the current token with `nucleus auth local-token`
- manual rotation is possible but awkward and undocumented as a product flow
- query-string WebSocket auth can expose tokens in logs or diagnostics
- browser storage is convenient but not a strong secret boundary

## Target Model

Nucleus should distinguish between three concepts:

1. **Server verifier**
   - the daemon stores only a token hash or equivalent verifier
   - the verifier is enough to validate future requests
   - the verifier is not enough to recover the original token

2. **One-time token material**
   - generated during setup or rotation
   - shown once to the operator
   - never recoverable from daemon state afterward

3. **Client credential persistence**
   - native clients store tokens in OS secure storage
   - the web client can keep a browser-scoped token for convenience, but the UI should describe it as browser-local client storage, not server-side recovery
   - headless/server installs may opt into a plaintext `0600` token file as an explicit compatibility mode

## Security Principles

- Do not encrypt a token file with a key stored next to it.
- Prefer one-way verification over reversible storage.
- Treat token rotation as a normal lifecycle action, not an emergency-only manual repair.
- Make the old token invalid immediately after rotation.
- Do not print existing tokens after the one-time reveal window.
- Keep migration backward-compatible so existing users do not get locked out unexpectedly.
- Keep instance boundaries clear: official/dev, EBA, and any other install have separate state directories and separate tokens.

## Proposed Design

### Server Storage

Replace durable plaintext token storage with verifier-only auth state:

- keep `auth.local_token_hash` or migrate to a stronger key derivation format
- add token metadata:
  - `auth.token_created_at`
  - `auth.token_rotated_at`
  - `auth.token_id` or short fingerprint for UI display
  - `auth.token_storage_mode`
- stop requiring `<state-dir>/local-auth-token` for normal operation after migration

The token itself should only exist at generation time and in clients that have been explicitly given it.

### Setup and Rotation

Add CLI commands:

```bash
nucleus auth rotate-token
nucleus auth status
```

`rotate-token` should:

- require access to the state directory or an authenticated daemon API call
- generate a new token
- store only the verifier and metadata server-side
- invalidate the previous token immediately
- print the new token once
- clearly warn that it cannot be retrieved again

`status` should show:

- whether auth is enabled
- token fingerprint
- created/rotated timestamps
- token storage mode
- token path only when plaintext compatibility mode is explicitly enabled

### Settings UI

Add an Auth section action:

- `Rotate access token`
- confirmation dialog explaining that existing browsers and clients will be signed out
- one-time reveal view for the new token
- copy button
- post-rotation reminder to update other clients

The UI must not display the current token once the one-time reveal view is closed.

### Web Client Handling

Short-term:

- keep browser storage for the web client, but label it accurately as browser-local persistence
- clear the stored token on auth failure or user disconnect
- avoid displaying token values except while the user is entering or copying them

Medium-term:

- stop sending WebSocket auth tokens in query parameters
- prefer a short-lived session ticket or a WebSocket subprotocol/header-compatible handshake pattern
- ensure tokens do not appear in routine logs, audit events, or error messages

### Native/Desktop Client Handling

When native clients exist, store Nucleus access tokens in OS credential storage:

- macOS Keychain
- Linux Secret Service/libsecret when available
- Windows Credential Manager

If OS storage is unavailable, the client should require explicit fallback confirmation before using a local plaintext credential file.

### Headless and Server Fallback

Some installations need a non-interactive token source. Support an explicit fallback mode:

- `auth.token_storage_mode=plaintext_file`
- token file remains `0600`
- CLI and UI label this as a compatibility mode
- rotation rewrites the file and verifier together

This mode should be opt-in for new installs once the verifier-only model is stable.

## Migration Plan

### Phase 1: Rotation Foundation

- add storage APIs for generating and rotating tokens
- add `nucleus auth rotate-token`
- add `nucleus auth status`
- add tests for old-token invalidation and new-token validation
- keep `local-auth-token` behavior for compatibility

### Phase 2: Settings UI

- add token status to `/api/settings`
- add authenticated `/api/settings/auth/rotate-token`
- add Settings UI rotation flow with one-time reveal
- add frontend schema/type tests for the new API shape
- ensure audit events do not include token values

### Phase 3: Verifier-Only Default

- introduce `auth.token_storage_mode`
- migrate new installs to verifier-only by default
- keep existing installs on plaintext compatibility mode until the operator rotates or opts in
- update setup output so the token is displayed once and not retrievable later
- update docs and managed-release notes

### Phase 4: Client Secret Hardening

- replace WebSocket query-token auth with a safer handshake/session-ticket flow
- add native-client credential-store integration when native clients exist
- provide a browser token reset/disconnect flow
- review logs and telemetry for accidental token exposure

## Testing Requirements

Backend tests:

- generating a token stores only the verifier in verifier-only mode
- rotating a token invalidates the old token immediately
- rotating a token returns the new token only in the rotation response
- daemon restart preserves verifier-only validation
- plaintext compatibility mode still works for existing installs
- token values never appear in audit event details

CLI tests:

- `nucleus auth rotate-token` prints a new token once
- `nucleus auth status` does not print the token
- state-dir targeting works for separate instances

Frontend tests:

- Settings auth schema validates token status and rotation responses
- rotation confirmation and one-time reveal states render correctly
- reconnect/auth failure clears stale browser token state where appropriate

Manual verification:

- official/dev instance can rotate without touching EBA state
- EBA managed instance can rotate through the normal installed product path
- old token fails after rotation
- new token works for REST and WebSocket connections

## Acceptance Criteria

- a user can rotate the Nucleus access token without deleting files manually
- the current token cannot be retrieved from the daemon after setup or rotation in verifier-only mode
- Settings exposes a clear rotation flow with one-time reveal
- old tokens stop working immediately after rotation
- instance separation remains explicit and testable
- documentation no longer describes plaintext token storage as the default long-term model

## Open Questions

- Should verifier-only mode be enabled immediately for all new installs or first gated behind a compatibility flag?
- Should managed installs rotate during update if they are still using legacy plaintext mode, or should rotation require explicit user action?
- What is the best WebSocket auth replacement that works cleanly in browsers without placing long-lived tokens in URLs?
- Should the web client keep using local storage, session storage, or an in-memory token by default once rotation is implemented?
- Do we need multiple active tokens per instance for multi-device use, or is single-token rotation acceptable for V1?

