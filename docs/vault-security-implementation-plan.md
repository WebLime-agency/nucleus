# Nucleus Vault and Security Implementation Plan

## Purpose

Build Nucleus Vault and the surrounding security posture as first-class product capabilities from day one, not as a thin UI over environment variables.

Vault must securely store and resolve credentials for MCPs, actions, project workflows, and future integrations without requiring third-party tools or OS-specific keychains. External providers may be added later, but the default local Vault must stand on its own for headless Linux, desktop Linux, macOS, Windows, managed releases, and source checkouts.

Related plans:

- Context/security boundaries: [`docs/context-security-boundaries.md`](context-security-boundaries.md)
- Durable memory: [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)
- Execution plan: [`docs/memory-vault-security-execution-plan.md`](memory-vault-security-execution-plan.md)

## Product principles

- Vault is confidential runtime material, not prompt-visible context.
- Secrets are encrypted at rest and decrypted only inside the daemon for approved consumers.
- The browser UI, prompts, transcripts, memory, logs, and client-visible API responses must never receive decrypted secret values.
- Workspace and project Vaults are first-class sibling surfaces to Memory and Include.
- The default local Vault should be secure without OS keychain, third-party SaaS, or external secret-manager installation.
- OS keychain and third-party providers are future optional providers, not day-one requirements.
- Security-sensitive operations should fail closed with clear UI copy.

## Threat model

Nucleus Vault should protect against:

- theft or backup of the Nucleus SQLite database
- theft or backup of the Nucleus state directory while the Vault is locked
- accidental disclosure through UI, logs, transcripts, prompt context, memory, audit events, or MCP error messages
- cross-project misuse of project-scoped secrets
- unauthorized consumers using secrets outside their allowed policy
- passive network observers when Vault operations are attempted from unsafe origins
- ciphertext tampering or row swapping in storage

Nucleus Vault cannot fully protect against:

- a compromised OS user account while the Vault is unlocked
- root/admin compromise of the machine
- malicious local processes that can attach to or inspect daemon memory
- intentionally untrusted local commands that receive secrets in their environment
- a user pasting secrets directly into chat or include files outside Vault

The product copy should be honest: Vault minimizes exposure and strongly protects locked-at-rest secrets, but no local vault can fully protect against an actively compromised runtime.

## Day-one security bar

The first Vault implementation must include:

1. User-created Vault passphrase that Nucleus does not store.
2. Argon2id key derivation with stored random salt and tunable parameters.
3. Envelope encryption with workspace/project scope keys.
4. Authenticated encryption, preferably XChaCha20-Poly1305.
5. Additional authenticated data binding ciphertext to vault id, scope, secret id/name, and version.
6. Locked/unlocked lifecycle.
7. Lock on daemon restart by default.
8. Idle timeout and manual lock.
9. No reveal endpoint in the first implementation.
10. Explicit allowed-consumer policies.
11. Daemon-only secret resolution.
12. Secure-origin enforcement for unlock/create/update.
13. Central redaction integration.
14. Audit events for Vault lifecycle and secret resolution.
15. Workspace Vault UI.
16. MCP `vault_bearer` integration.
17. Legacy `bearer_env` fallback retained for operators but not the preferred UI path.

## Storage model

Use SQLite for Vault metadata and encrypted blobs. Plaintext secrets must never be stored.

Suggested tables:

```sql
vault_state (
  id TEXT PRIMARY KEY,
  version INTEGER NOT NULL,
  vault_id TEXT NOT NULL,
  status TEXT NOT NULL,
  kdf_algorithm TEXT NOT NULL,
  kdf_params_json TEXT NOT NULL,
  salt BLOB NOT NULL,
  cipher TEXT NOT NULL,
  encrypted_root_check BLOB NOT NULL,
  root_check_nonce BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
```

```sql
vault_scope_keys (
  id TEXT PRIMARY KEY,
  vault_id TEXT NOT NULL,
  scope_kind TEXT NOT NULL,
  scope_id TEXT NOT NULL,
  encrypted_key BLOB NOT NULL,
  nonce BLOB NOT NULL,
  aad TEXT NOT NULL,
  key_version INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  rotated_at INTEGER
);
```

```sql
vault_secrets (
  id TEXT PRIMARY KEY,
  scope_key_id TEXT NOT NULL,
  scope_kind TEXT NOT NULL,
  scope_id TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  ciphertext BLOB NOT NULL,
  nonce BLOB NOT NULL,
  aad TEXT NOT NULL,
  version INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  last_used_at INTEGER,
  UNIQUE(scope_kind, scope_id, name)
);
```

```sql
vault_secret_policies (
  id TEXT PRIMARY KEY,
  secret_id TEXT NOT NULL,
  consumer_kind TEXT NOT NULL,
  consumer_id TEXT NOT NULL,
  permission TEXT NOT NULL,
  approval_mode TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(secret_id, consumer_kind, consumer_id, permission)
);
```

```sql
vault_secret_usages (
  id TEXT PRIMARY KEY,
  secret_id TEXT NOT NULL,
  consumer_kind TEXT NOT NULL,
  consumer_id TEXT NOT NULL,
  purpose TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER
);
```

## Cryptography

Preferred primitives:

- KDF: Argon2id
- Cipher: XChaCha20-Poly1305
- Randomness: OS CSPRNG
- Secret memory: use `secrecy`/`zeroize` patterns where practical

Key hierarchy:

```text
Vault passphrase
  -> Argon2id
  -> root wrapping key
  -> workspace/project scope keys
  -> individual secrets
```

Additional authenticated data should bind encrypted data to its intended identity. Example shape:

```text
nucleus:vault:v1:<vault_id>:<scope_kind>:<scope_id>:<secret_id>:<secret_name>:<version>
```

If ciphertext is moved between rows, scopes, projects, or names, decryption should fail.

## Lock and unlock lifecycle

States:

- `uninitialized`
- `locked`
- `unlocked`
- `unlock_required`
- `corrupt_or_tampered`

Default behavior:

- Vault locks on daemon restart.
- Vault locks after configurable idle timeout.
- Manual lock is always available.
- Secret resolution fails closed while locked with `vault_locked`.
- Failed unlocks are audited without leaking passphrase details.

Future optional modes may include unattended local-machine unlock, OS keychain wrapping, or external providers, but these are not the default security story.

## API design

Vault APIs must never return decrypted secret values.

Suggested endpoints:

- `GET /api/vault/status`
- `POST /api/vault/init`
- `POST /api/vault/unlock`
- `POST /api/vault/lock`
- `GET /api/vault/secrets?scope_kind=&scope_id=`
- `POST /api/vault/secrets`
- `PATCH /api/vault/secrets/{secret_id}`
- `DELETE /api/vault/secrets/{secret_id}`
- `GET /api/vault/secrets/{secret_id}/policies`
- `PUT /api/vault/secrets/{secret_id}/policies`
- `POST /api/vault/secrets/{secret_id}/test` when a provider-specific validation exists

List/get responses should return metadata only:

```json
{
  "id": "...",
  "scope_kind": "workspace",
  "scope_id": "workspace",
  "name": "SUPABASE_ACCESS_TOKEN",
  "description": "Supabase MCP access token",
  "configured": true,
  "created_at": 0,
  "updated_at": 0,
  "last_used_at": null
}
```

No reveal endpoint in the first implementation.

## Policy model

Every secret should default to no runtime consumers.

Policy fields:

- consumer kind: `mcp`, `action`, `workflow`, `project_env`, future values
- consumer id
- permission: `bearer_auth`, `env_injection`, `header_injection`, future values
- approval mode: `always_allow`, `ask_per_session`, `ask_every_time`, `disabled`

Creating a secret from an MCP credential flow should grant only that MCP the required permission. Generic Vault creation should grant no consumers until the user chooses them.

## MCP integration

Add MCP auth mode:

```text
vault_bearer
```

References:

```text
vault://workspace/SUPABASE_ACCESS_TOKEN
vault://project/<project_id>/VERCEL_TOKEN
```

Known current mappings to migrate through UI:

- `cloudflare-api`, `cloudflare-bindings`, `cloudflare-builds`, `cloudflare-observability` -> `CLOUDFLARE_API_TOKEN`
- `supabase` -> `SUPABASE_ACCESS_TOKEN`
- `vercel` -> `VERCEL_TOKEN`

`cloudflare-docs`, `context7`, and `emdash-docs` do not require bearer credentials.

The MCP UI should provide:

- missing credential state
- `Add to Vault`
- `Save and test`
- visible non-secret Vault reference
- clear copy explaining that Nucleus injects the token server-side and never exposes it to the model

## Workspace and project UI

Workspace UI:

```text
Workspace -> Vault
```

Project UI:

```text
Project -> Vault
```

Vault list should show:

- secret name
- scope
- description
- configured status
- allowed consumers
- created/updated timestamps
- last used timestamp
- validation status when known
- actions: add, update/replace, delete, manage access, test

It should never show decrypted secret values after submit.

## Network and secure-origin security

Add a Network/Security settings surface that shows:

- current bind address
- detected interfaces
- whether the daemon is LAN-exposed
- whether the daemon is Tailscale/private-interface-only
- whether current origin is safe for Vault operations
- whether HTTPS is active
- local auth status

Recommended access modes:

- Localhost only: `127.0.0.1:<port>`
- Tailscale/private VPN only: bind to the Tailscale/private interface IP
- LAN: bind to LAN interface or `0.0.0.0` with clear warning
- Custom/public: advanced only, strongly prefer HTTPS/reverse proxy

Vault unlock/create/update should require localhost or HTTPS by default. Plain HTTP over LAN or VPN IP should be blocked unless an explicit insecure-development override is enabled.

## Redaction and audit

Use the shared boundary rules in [`docs/context-security-boundaries.md`](context-security-boundaries.md).

Vault must emit non-secret audit events for:

- initialization
- unlock success/failure
- lock
- secret create/update/delete
- policy changes
- secret resolve allowed/denied
- MCP auth resolved from Vault

All audit payloads must be redacted by default.

## Implementation phases

The master sequencing lives in [`docs/memory-vault-security-execution-plan.md`](memory-vault-security-execution-plan.md). Vault-specific phases are:

1. Security posture and shared redaction primitives.
2. Passphrase-protected local Vault backend.
3. Workspace Vault UI and policy model.
4. MCP Vault integration.
5. Project Vaults.
6. Optional provider extensions after the local Vault is complete.

## Non-goals for day one

- No plaintext secret storage.
- No browser-stored secrets.
- No reveal endpoint.
- No OS-keychain dependency as the default path.
- No third-party secret manager requirement.
- No automatic unattended unlock by default.
- No model-visible secret values.
- No prompt/memory/transcript secret storage.
