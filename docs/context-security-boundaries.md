# Context and Security Boundaries

## Purpose

This document defines the durable boundary between prompt-visible context and confidential runtime material in Nucleus.

Nucleus has several daemon-owned product capabilities that all use workspace/project/session scope, but they do not carry the same safety expectations:

- Memory is durable context intended to be read by models in future turns.
- Prompt includes are deterministic context files intended to be read by models.
- Skills are procedural instructions and activation metadata intended to guide model behavior.
- MCPs/actions are runtime capabilities governed by the daemon.
- Vault is confidential execution-time material resolved only by the daemon for approved runtime consumers.

The core rule:

> Anything that may enter a prompt, transcript, log, browser response, or model-visible tool result must be treated as public-to-the-model context. Vault secrets must never cross that boundary.

## Vocabulary

- **Prompt-visible context**: text that can be rendered into a model prompt or model-visible tool result.
- **Memory entry**: accepted durable context that may be included in future turns.
- **Memory candidate**: proposed durable context pending review. Candidates are not prompt-visible until accepted, but they are still not allowed to store secrets.
- **Prompt include**: deterministic file-based context from `include/`, `.nucleus/include/`, or legacy promptinclude files.
- **Skill**: procedural capability and activation instructions. Skills may reference tools and concepts but must not contain secrets.
- **MCP record**: tool/resource server configuration and metadata. MCP records may contain non-secret Vault references.
- **Browser artifact**: daemon-owned session/job evidence captured from the Browser runtime, such as readable snapshots, screenshots, download metadata, downloaded files, and annotation metadata.
- **Vault secret**: confidential execution-time material such as API tokens, provider API keys, cookies, private keys, passwords, bearer credentials, recovery phrases, database URLs with credentials, or `.env` values. Existing provider credentials are secret material even before they are migrated into Vault.
- **Vault reference**: a non-secret pointer to a Vault secret, scoped and policy-gated by the daemon, such as `vault://workspace/SUPABASE_ACCESS_TOKEN`.
- **Secret resolution**: daemon-only operation that decrypts or retrieves a secret for an approved consumer.
- **Consumer**: runtime entity requesting secret use, such as an MCP server, action, tool, workflow, or project environment.

## Allowed and forbidden flows

### Memory

Allowed:

- Store durable preferences, decisions, project notes, constraints, known ports, known repo paths, stable workflow notes, and non-secret operational facts.
- Store non-secret notes that a credential exists in Vault, for example: `Project X uses a project-scoped Vercel token stored in Vault.`
- Store a non-secret Vault reference only if the reference itself is operationally useful and contains no secret value.

Forbidden:

- API tokens, bearer headers, cookies, private keys, passwords, recovery phrases, SSH private keys, database URLs with embedded credentials, `.env` values, OAuth refresh tokens, or provider access tokens.
- Raw terminal output that includes credentials.
- Screenshots, logs, or pasted snippets that contain credentials.
- Secret values in memory candidates, even if pending and not prompt-visible yet.

### Prompt includes and skills

Allowed:

- Stable instructions, architecture notes, project conventions, and tool usage guidance.
- Non-secret references to Vault-managed credentials.

Forbidden:

- Any credential value.
- Any durable copy of a secret intended to avoid using Vault.
- Machine-specific private tokens or passwords.

### MCP/action configuration

Allowed:

- Non-secret metadata.
- Safe headers without sensitive values.
- Vault references such as `vault://workspace/CLOUDFLARE_API_TOKEN`.
- Legacy env var references for advanced/operator fallback, with UI guidance toward Vault.

Forbidden:

- Plaintext secret values in MCP records or action definitions.
- Secret-bearing headers in client-visible API responses.
- Provider API keys, router target keys, workspace profile keys, MCP env values, and MCP header values in normal browser-visible API responses. Responses may return non-secret metadata, an empty value, or a redacted placeholder, but never the raw credential.

### Transcripts, logs, and audit events

Allowed:

- Non-secret evidence and event metadata.
- Redacted placeholders such as `[REDACTED_SECRET]`.
- Secret names/references when useful and non-sensitive.
- Instance-local product log records with timestamp, level, category, source, event name, safe message, and safe related IDs or metadata.

Forbidden:

- Decrypted Vault values.
- Request/response bodies containing credentials unless redacted before storage.
- Authorization headers, cookies, or private keys.
- Raw model prompts/responses, full command output, provider request/response payloads, or stdout/stderr streams in generic instance logs unless a future feature shapes and redacts them explicitly before persistence.

Instance logs are local support/debugging artifacts. They live under the active daemon state directory and are queryable through authenticated daemon APIs and Workspace -> Logs. They are not Memory, prompt includes, transcripts, or agent-visible prompt context.

### Browser artifacts

Allowed:

- Persist readable page text, screenshots, daemon-generated refs, download metadata, downloaded files, and annotation metadata as session/job artifacts.
- Include local artifact paths, page URLs, page titles, suggested download filenames, capture timestamps, and ref counts in artifact metadata.
- Use Browser artifacts as evidence for UI verification and task completion.

Forbidden:

- Promote Browser artifacts into Memory automatically.
- Treat Browser screenshots, snapshots, annotations, downloads, or cookies as Vault storage.
- Persist decrypted Vault values into Browser pages, Browser downloads, Browser artifact metadata, transcripts, logs, or model-visible tool results.
- Use Browser artifacts to bypass local/private URL or Vault safe-origin rules.

Browser artifacts may capture local/private URLs and private UI content because the daemon Browser can reach the operator's local network from the host. These artifacts stay under the active Nucleus state directory and inherit that instance's retention and backup posture. Operators should use scratch state for source-checkout Browser testing and remove it after verification.

Downloads are saved under the active state directory and may contain sensitive page output. The daemon may expose download metadata to the UI or model-visible tool result, but should avoid exposing file contents unless the user or an approved tool explicitly requests that content.

## Scope model

Memory, Vault, MCPs, and future action policies should reuse the same scope language:

- `workspace` / workspace id
- `project` / project id
- `session` / session id where applicable

Scope must be enforced by the daemon, not only by UI filtering.

Project-scoped Vault secrets must only be available in matching project context and only to explicitly allowed consumers. Workspace-scoped Vault secrets may be available across the workspace only according to explicit policy.

## Secret resolution rules

Secret resolution must be daemon-only.

The browser client, model prompt, model-visible tool arguments, session transcript, and memory system must never receive decrypted secret values.

Expected remote MCP bearer flow:

1. A tool call requests an MCP operation.
2. The daemon checks MCP enablement and policy.
3. The daemon checks that the Vault is unlocked.
4. The daemon checks the secret's allowed-consumer policy.
5. The daemon decrypts/resolves the secret in memory.
6. The daemon injects the secret into the outbound request, for example as an `Authorization` header.
7. The daemon redacts logs/errors/results.
8. The model receives only the MCP result, never the secret.

Local stdio/process environment injection is higher risk because the child process may print or exfiltrate environment variables. It should require explicit user policy and clearer warnings than remote MCP bearer injection.

## Redaction requirements

Nucleus should maintain central redaction helpers used by:

- memory candidate extraction
- memory candidate storage
- MCP discovery and invocation errors
- action output
- audit events
- logs
- transcripts
- UI toasts and error surfaces

Redaction should include:

- exact registered secret values for secrets resolved during a request/session
- Authorization headers
- cookies and set-cookie headers
- common token/key fields such as `token`, `access_token`, `refresh_token`, `api_key`, `secret`, `password`, `private_key`, and `client_secret`
- URLs with embedded credentials
- PEM/private-key blocks
- known high-entropy token patterns where practical

Redaction is a backstop, not a substitute for preventing secrets from crossing the boundary.

## Secure-origin requirements

Vault unlock, secret creation, and secret update submit highly sensitive material. These operations must require a safe origin.

Allowed by default:

- `http://localhost`
- `http://127.0.0.1`
- `https://...`

Blocked by default:

- plain HTTP access over LAN
- plain HTTP access over non-loopback private interfaces
- plain HTTP access over Tailscale IPs unless explicitly allowed by a development/operator override

Nucleus may still render normal non-secret pages over plain HTTP, but Vault write/unlock operations should be refused unless the origin is safe.

## Network exposure guidance

Bind address and TLS are separate controls.

- `127.0.0.1:<port>` means local machine only.
- a Tailscale interface address such as `100.x.y.z:<port>` means private VPN only.
- `0.0.0.0:<port>` means all interfaces, including LAN and VPN.
- HTTPS encrypts traffic, but HTTPS on `0.0.0.0` is still broadly exposed.

Nucleus should surface current network posture in Settings and guide users toward explicit access modes:

- Localhost only
- Tailscale/private VPN only
- LAN
- Custom/public

Vault operations should be held to a stricter standard than ordinary page rendering.

The Browser runtime may navigate to localhost, LAN, VPN, or other private URLs when the operator approves navigation. That reachability does not make those origins safe for Vault plaintext operations. Vault unlock/create/update must still require loopback HTTP or HTTPS unless an explicit development override is implemented.

## Audit event rules

Audit events should be structured, non-secret, and redacted by default.

Relevant event families:

- `memory.entry.created`
- `memory.entry.updated`
- `memory.candidate.created`
- `memory.candidate.accepted`
- `memory.candidate.rejected`
- `vault.initialized`
- `vault.unlocked`
- `vault.locked`
- `vault.unlock_failed`
- `vault.secret.created`
- `vault.secret.updated`
- `vault.secret.deleted`
- `vault.secret.policy_updated`
- `vault.secret.resolved`
- `vault.secret.resolve_denied`
- `mcp.auth.resolved_from_vault`

Never include decrypted secret values in audit payloads.

Audit events and instance logs are related but distinct:

- Audit events are the security and lifecycle trail for sensitive or privileged operations.
- Instance logs are the broader support/debugging surface for safe product events.
- A safe audit summary may also create an instance log entry, but this must not weaken audit redaction rules.

## Implementation references

- Memory plan: [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)
- Vault/security plan: [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- Execution plan: [`docs/memory-vault-security-execution-plan.md`](memory-vault-security-execution-plan.md)
