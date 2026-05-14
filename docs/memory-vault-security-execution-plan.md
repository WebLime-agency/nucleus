# Memory, Vault, and Security Execution Plan

## Purpose

This is the executable master plan for completing Nucleus durable memory, Vault, and the surrounding security posture.

Use this document to drive future implementation sessions. Each session should:

1. Read this execution plan.
2. Read the phase-specific source plans linked below.
3. Implement the current phase only unless explicitly instructed otherwise.
4. Run relevant checks.
5. Update the status table and phase notes in this document before finishing.
6. Link PRs, commits, releases, and follow-up decisions here as work lands.

Source plans:

- Durable memory: [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)
- Vault/security: [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- Context/security boundaries: [`docs/context-security-boundaries.md`](context-security-boundaries.md)
- Architecture: [`docs/architecture.md`](architecture.md)
- Workspace model: [`docs/workspace-model.md`](workspace-model.md)
- Repo workflow: [`docs/repo-workflow.md`](repo-workflow.md)
- Managed release: [`docs/managed-release.md`](managed-release.md)

## Non-negotiable product boundaries

- The daemon is the system of record for memory, Vault, auth, MCPs, and persistence.
- Memory is prompt-visible durable context.
- Vault is confidential execution-time material.
- Vault secrets must never enter prompts, memory, transcripts, logs, audit payloads, or browser-visible API responses.
- Memory candidates must redact or skip credentials.
- Vault must not be a masked settings table. It must be passphrase-protected, encrypted at rest, policy-gated, auditable, and daemon-resolved.
- Vault unlock/create/update must require a safe origin: localhost or HTTPS by default.
- Project and workspace scoping must be enforced in the daemon, not only in UI filters.
- UI should use existing Nucleus/shadcn-style primitives and shell/container patterns.

## Status legend

- `not_started` — no implementation started.
- `in_progress` — active PR/work exists but not merged.
- `merged` — merged to `dev`.
- `released` — included in managed stable release and installed into EBA when relevant.
- `blocked` — cannot proceed until listed dependency is resolved.
- `completed` — planning/docs-only phase is coherent and ready for the next phase.

## Master status table

| Phase | Track | Title | Status | Depends on | PR / release notes |
| --- | --- | --- | --- | --- | --- |
| 0 | Planning | Plan split and master execution plan | completed | none | Planning docs reviewed for consistency; no implementation started. |
| 1 | Security | Network posture + secure-origin + redaction primitives | completed | Phase 0 | PR #134 includes posture/redaction primitives plus provider/API credential response hardening. |
| 2 | Memory | Prompt integration + real Memory UI | completed | Phase 0 | Phase 2 implementation committed on `feat/memory-prompt-ui`. |
| 3 | Vault | Passphrase-protected local Vault backend | completed | Phase 1 | PR #137 merged into `dev` at `724eb2115e02d2d660de41ee890724ebac85fab6`; UI/MCP remain deferred. |
| 4 | Memory | Candidates + explicit/automatic capture loop | completed | Phase 1, Phase 2 | PR #141 merged into `dev` at `c3e0f60ce23b9878a0d331cc1a6cc6d67c56e5b4`; not released. |
| 5 | Vault | Workspace Vault UI + policy model | completed | Phase 3 | PR #143 merged into `dev` at `0fbe03ee9e331c69eb896348cefdc373ba521511`; not released. |
| 6 | Vault/MCP | MCP `vault_bearer` integration | completed | Phase 3, Phase 5 | PR #145 merged into `dev` at `30478a9c4424d511b7a1298536053e26e5c22595`; not released. |
| 7 | Vault | Project Vaults | in_progress | Phase 5 | Local Phase 7 work on `feat/project-vaults`; not merged or released. |
| 8 | Memory | SQLite FTS5 searchable memory provider | not_started | Phase 4 |  |
| 9 | Security | Built-in/guided HTTPS and bind-mode hardening | not_started | Phase 1 |  |
| 10 | Release | Stable managed release and EBA verification | not_started | Phases required by release scope |  |
| 11 | Future | Retrieval provider interface and optional semantic search | not_started | Phase 8 |  |
| 12 | Future | Optional external Vault providers / OS keychain wrapping | not_started | Phase 6 |  |

## Phase 0 — Planning docs

Status: `completed`

Goal: Split the combined planning context into durable docs that future sessions can use without needing the original conversation.

Tasks:

- [x] Create [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md).
- [x] Create [`docs/context-security-boundaries.md`](context-security-boundaries.md).
- [x] Review [`docs/memory-implementation-plan.md`](memory-implementation-plan.md) so it links to the dedicated security docs and does not try to own all Vault implementation details.
- [x] Create this execution plan.
- [x] Commit docs cleanly.

Exit criteria:

- Future sessions can start from this document and understand the implementation sequence.
- Memory, Vault, and boundary docs agree on terminology and scope.

Completion notes:

- Reviewed the execution plan, Vault/security plan, context/security boundaries, and memory implementation plan together for terminology and sequencing consistency.
- Verified the core boundary language is aligned across the docs: Memory is prompt-visible durable context; Vault is confidential execution-time material; Vault secrets must never enter prompts, memory, transcripts, logs, audit payloads, or browser-visible API responses.
- Confirmed Phase 1 remains the next implementation phase and no backend/frontend feature work was started in Phase 0.
- Ran a lightweight markdown link sanity check against the four planning docs; no broken relative links were found.
- Deferred implementation details remain in later phases as documented rather than being started here.

## Phase 1 — Security posture, secure-origin, and redaction primitives

Status: `completed`

Source docs:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- [`docs/context-security-boundaries.md`](context-security-boundaries.md)

Goal: Build the shared security substrate needed by both Memory and Vault.

Backend tasks:

- Add daemon helper to classify request origin as safe/unsafe for secret operations.
- Add daemon representation of network posture:
  - bind address
  - detected interfaces
  - localhost-only vs Tailscale/private-interface vs LAN/all-interface exposure
  - HTTPS active/inactive
  - current origin Vault-safe yes/no
- Add API endpoint or extend system/settings endpoint to expose non-secret network/security posture.
- Add central redaction utilities for:
  - registered exact secret values
  - Authorization headers
  - cookies
  - common token/key field names
  - URLs with credentials
  - PEM/private-key blocks where practical
- Ensure audit/log helpers can call redaction before persistence/output.

Web tasks:

- Add/extend Settings security/network surface.
- Show current bind and exposure status.
- Warn when Nucleus is bound to `0.0.0.0` or LAN over plain HTTP.
- Explain localhost, Tailscale/private VPN, LAN, and custom modes.
- Do not add Vault secret UI yet; this phase only makes posture visible.

Tests:

- Unit tests for origin classification.
- Unit tests for redaction helpers.
- Storage/API tests if new endpoint is added.

Exit criteria:

- Nucleus can report whether current origin is safe for Vault operations.
- Redaction helpers exist and are reusable by memory candidate extraction and future Vault resolution.
- Settings UI surfaces network posture clearly.

Completion notes:

- Branch: `feat/security-posture-redaction`.
- Commit: `c932880`.
- Added daemon secure-origin classification helpers for localhost/loopback HTTP, HTTPS, and unsafe plain HTTP non-loopback origins.
- Added daemon security posture reporting through the existing Settings summary, including configured bind, exposure classification, HTTPS status, current origin Vault-safe status, and non-secret warnings.
- Added central redaction primitives for sensitive headers, common secret field names, URLs with embedded credentials, PEM private-key blocks, and registered exact secret values. Broad log/audit adoption is intentionally deferred to later phases to avoid risky churn.
- Added Settings connection-card UI fields for security posture and warnings; no Vault secret management UI was added.
- Checks run: `cargo fmt --all --check` passed; `npm run check:web` passed; `npm run build:web` passed.
- PR #134 repair rebased Phase 1 onto latest `origin/dev`, resolved the duplicate daemon `url.workspace = true` dependency entry, and reran clean-worktree validation successfully.
- Clean-worktree checks after repair: `cargo fmt --all --check`, `cargo test -p nucleus-daemon security`, `cargo test -p nucleus-daemon redacts`, `npm run check:web`, and `npm run build:web` all passed.
- Remaining: wire redaction into any future audit/log persistence call sites introduced by later phases.

Follow-up: provider/API credential response hardening

- Branch: `fix/redact-provider-secrets-api`.
- Goal: treat existing provider API keys, router target keys, workspace profile keys, MCP env/header values, and job/tool debug payload credentials as secret material even before the Vault migration.
- API responses must not return raw provider credentials or other secret-like config. Keep write/upsert request behavior for setting new values, but redact or empty sensitive values in normal browser-visible responses.
- Included in PR #134 as `fix: redact provider secrets from API responses`.
- Checks run in the clean PR repair worktree: `cargo fmt --all --check`, `cargo test -p nucleus-daemon security`, `cargo test -p nucleus-daemon redacts`, `npm run check:web`, and `npm run build:web` all passed.

## Phase 2 — Memory prompt integration + real Memory UI

Status: `completed`

Source doc:

- [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)

Goal: Make accepted durable memory useful in actual turns and expose real operator controls.

Backend tasks:

- Extend protocol memory types.
- Migrate `memory_entries` fields.
- Fix prompt/include split so include content is in compiled prompt context, not only debug counts.
- Add memory context provider for accepted enabled workspace/project/session memory.
- Add budgeting/truncation/debug counts.
- Add or update memory APIs as needed.
- Add tests proving provider-visible compiled prompt includes prompt includes and accepted memory.

Web tasks:

- Build real Memory page using existing Nucleus/shadcn-style primitives.
- List accepted memory with scope/kind/status/source/tags/enabled state.
- Add create/edit/archive/delete or disable flows according to current API support.
- Add clear copy that memory is prompt-visible context and must not contain secrets.

Tests/checks:

- `npm run check:web`
- relevant web build/checks
- Rust tests for storage/prompt compilation touched
- `cargo fmt --all --check`

Exit criteria:

- Accepted memory appears in matching future compiled turns.
- Prompt includes are represented in compiled context.
- Memory UI can manage accepted memory.
- No candidate extraction yet.

Completion notes:

- Implemented on branch `feat/memory-prompt-ui`.
- Commit: branch HEAD for `feat: add memory prompt integration and management UI`.
- Extended Memory protocol/storage/API handling with accepted/manual defaults, validation/normalization, and prompt-visible safety redaction for stored text.
- Fixed compiled prompt assembly so prompt include contents become provider-visible compiled layers before memory and skills.
- Added accepted-memory prompt layers with workspace/project/session scope filtering, conservative budgeting/truncation, and debug counts.
- Replaced the placeholder Memory page with a real accepted-memory management UI and prompt-visible/secret warning copy.
- Checks run: `cargo fmt --all --check`, `cargo test -p nucleus-daemon memory`, `cargo test -p nucleus-daemon compiled_turn_includes_prompt_includes_and_accepted_memory`, full `cargo test`, `npm run check:web`, and `npm run build:web`.
- Deferred: candidate extraction/review, Vault backend/UI, MCP `vault_bearer`, semantic/vector memory, and FTS search remain out of Phase 2 scope.

## Phase 3 — Passphrase-protected local Vault backend

Status: `completed`

Source docs:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- [`docs/context-security-boundaries.md`](context-security-boundaries.md)

Goal: Implement the real local Vault backend.

Backend tasks:

- Add crypto dependencies and storage migrations.
- Implement Vault initialization with user passphrase.
- Use Argon2id KDF with stored random salt and tunable parameters.
- Use XChaCha20-Poly1305 or equivalent AEAD.
- Implement envelope encryption with workspace/project scope keys.
- Bind ciphertext to scope/name/version metadata with AAD.
- Implement lock/unlock state in daemon memory.
- Lock on daemon restart by default.
- Add idle timeout and manual lock.
- Add metadata-only Vault APIs.
- Enforce safe origin for init/unlock/create/update/delete.
- Add audit events without secret values.
- Add memory hygiene with `secrecy`/`zeroize` patterns where practical.

Web tasks:

- Minimal Vault status/init/unlock/lock UI may be included here or deferred to Phase 5.
- If included, do not build full management UI yet.

Tests:

- KDF/encryption/decryption round trip.
- Wrong passphrase fails.
- Tampered ciphertext/AAD fails.
- Locked Vault cannot resolve secrets.
- API never returns secret values.
- Unsafe origins are rejected for secret material operations.

Exit criteria:

- Local Vault can initialize, lock, unlock, create/update/delete encrypted secrets through daemon APIs.
- No reveal endpoint exists.
- Secret values are never returned by API.

Completion notes:

- Implemented on branch `feat/vault-backend` and merged via PR #137 (`724eb2115e02d2d660de41ee890724ebac85fab6`).
- Added passphrase-protected local Vault backend with Argon2id KDF, XChaCha20-Poly1305 encryption, per-scope encrypted keys, per-secret nonces, and AAD binding for encrypted scope keys/secrets.
- Added daemon-owned lock/unlock runtime state; Vault locks by default on daemon start because only encrypted state is persisted.
- Added metadata-only Vault APIs for status, init, unlock, lock, create/update/list/delete secrets. No reveal endpoint exists.
- Enforced safe-origin checks for init, unlock, create, update, and delete operations.
- Added redacted audit events for Vault lifecycle and secret metadata changes without secret values.
- Cleanup added endpoint-level status/list/lock/update/delete coverage, restart persistence coverage, and storage-level Vault persistence/delete cascade coverage before PR.
- Checks run: `cargo fmt --all --check`, `cargo test -p nucleus-daemon vault`, `cargo test -p nucleus-storage vault`, and full `cargo test`.
- Deferred to later phases: Workspace Vault management UI/policy editing, daemon-only secret resolution for MCP consumers, MCP `vault_bearer`, idle-timeout tuning surface, secret reveal/test endpoint, project Vault UI, external providers/keychain wrapping.

## Phase 4 — Memory candidates + explicit/automatic capture loop

Status: `completed`

Source docs:

- [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)
- [`docs/context-security-boundaries.md`](context-security-boundaries.md)

Goal: Add memory candidate lifecycle and capture loop without polluting prompt-visible memory.

Backend tasks:

- Add `memory_candidates` protocol/storage/API.
- Add accept/reject/dismiss helpers.
- Add explicit remember path.
- Add automatic candidate extraction after successful turns.
- Use shared redaction helpers from Phase 1 before candidate storage.
- Ensure pending/rejected candidates never enter prompt context.
- Add dedupe guardrails.
- Add audit events.

Web tasks:

- Add candidate review section to Memory page.
- Support accept, edit-and-accept, reject, dismiss/delete.
- Show evidence/reason/confidence without exposing credentials.

Tests:

- Candidate CRUD and accept/reject lifecycle.
- Pending/rejected candidates do not enter prompt context.
- Explicit remember creates accepted memory.
- Extraction failures do not fail user turns.
- Credential-like content is rejected or redacted.

Exit criteria:

- Automatic extraction creates pending candidates only.
- Operator can accept/reject/edit candidates.
- Accepted candidates become memory entries.
- Credential-like content is not stored.

Completion notes:

- PR #141 merged into `dev` at `c3e0f60ce23b9878a0d331cc1a6cc6d67c56e5b4`; Phase 4 is completed but not released.
- Implemented `memory_candidates` protocol types, SQLite schema/indexes, storage helpers, and daemon APIs.
- Added candidate accept/reject/dismiss lifecycle; accepting a candidate creates an accepted `memory_entries` row and links `accepted_memory_id`.
- Added explicit remember path that creates accepted memory directly with `source_kind = explicit_remember`.
- Added automatic post-turn extraction after successful visible assistant turns; automatic extraction creates pending candidates only and never accepted memory.
- Pending, rejected, and dismissed candidates remain review-only and are not prompt-visible; accepted candidate memory enters future matching compiled prompt context.
- Applied Phase 1 redaction and credential-like content guardrails before candidate/entry storage, including extracted content, evidence, reason, and metadata.
- Added deterministic dedupe guardrails for repeated extraction/candidate creation.
- Added non-secret audit events for candidate creation, acceptance, rejection, dismissal, explicit remember, and extraction start/completion/failure.
- Added Memory UI candidate review section with accept, edit-and-accept, reject, and dismiss actions.
- PR #141 checks passed: Promotion, Rust, and Web.

## Phase 5 — Workspace Vault UI and policy model

Status: `completed`

Source doc:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)

Goal: Make Workspace Vault usable as a product surface.

Backend tasks:

- Add allowed-consumer policy APIs if not completed in Phase 3.
- Add usage metadata updates.
- Add validation/test hooks where provider-specific validation exists.

Web tasks:

- Add `Workspace -> Vault` tab/page.
- Add first-run setup flow if Vault is uninitialized.
- Add unlock/lock UI.
- Add secret list with metadata only.
- Add create/update/replace/delete flows.
- Add manage-access policy UI.
- Add copy-reference action.
- Do not show decrypted values after submit.
- Block or explain unsafe-origin restrictions.

Tests/checks:

- Web form validation.
- API parsing robust to locked/uninitialized states.
- Standard web checks.

Exit criteria:

- User can initialize/unlock Vault and manage workspace secrets without touching env files.
- UI copy clearly explains security posture and no-reveal behavior.

Completion notes:

- PR #143 merged into `dev` at `0fbe03ee9e331c69eb896348cefdc373ba521511`; Phase 5 is completed but not released.
- Added Workspace Vault page/tab.
- Added first-run initialization flow.
- Added unlock/lock UI.
- Added metadata-only workspace secret list.
- Added create/replace/delete secret flows.
- Preserved no-reveal behavior after submit; browser-visible responses remain metadata-only.
- Added copy `vault://workspace/...` reference action.
- Added allowed-consumer policy management UI.
- Added metadata-only policy APIs for listing/upserting/deleting allowed-consumer policies.
- Policy write/delete operations require a safe origin and unlocked Vault.
- Added non-secret audit events for policy metadata changes.
- PR #143 checks passed: Promotion, Rust, and Web.
- Phase 6 is now `in_progress`; Phase 7+ remain `not_started`. No Project Vaults, FTS/search, semantic memory, promotion, release, or managed install work has started.

## Phase 6 — MCP `vault_bearer` integration

Status: `completed`

Source docs:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- [`docs/context-security-boundaries.md`](context-security-boundaries.md)

Goal: Remove normal-user dependence on daemon environment variables for MCP auth.

Backend tasks:

- Add MCP auth mode `vault_bearer`.
- Add Vault reference parser for `vault://workspace/...` and `vault://project/<project_id>/...`.
- Implement daemon-only secret resolution for MCP discovery/invocation.
- Inject bearer token into outbound MCP request without exposing it to UI/model/logs/audit.
- Keep `bearer_env` as advanced/operator fallback.
- Add status states such as `vault_locked`, `vault_secret_missing`, `vault_policy_denied` as needed.

Web tasks:

- Update MCP auth UI to prefer Vault.
- Add inline `Add to Vault` flow for missing credentials.
- Add `Save and test` flow.
- Show non-secret Vault reference and allowed consumer state.
- Explain that copying files/env vars is not needed for normal users.

Tests:

- Vault-backed MCP discovery succeeds with valid secret.
- Locked Vault blocks with safe error.
- Policy-denied secret use fails without leaking value.
- Logs/audit/API do not include secret value.

Exit criteria:

- Supabase/Vercel/Cloudflare MCP credentials can be configured from UI and stored in Vault.
- MCP check/discovery uses daemon-side Vault resolution.
- Env vars are no longer the primary product path.

Completion notes:

- PR #145 merged into `dev` at `30478a9c4424d511b7a1298536053e26e5c22595`; Phase 6 is completed but not released.
- Added MCP `vault_bearer` auth mode for remote MCP discovery and invocation.
- Added daemon-side Workspace Vault reference resolution for `vault://workspace/...`.
- Safely deferred `vault://project/...` behavior until Phase 7.
- Enforced Vault allowed-consumer policy for MCP read access.
- Injected resolved bearer token only into outbound MCP HTTP auth.
- Preserved `bearer_env` / `env_bearer` fallback as the advanced/operator path.
- Added safe failure states: `vault_locked`, `vault_secret_missing`, and `vault_policy_denied`.
- Updated MCP UI guidance and status handling for Vault-backed auth.
- Added metadata-only Vault usage recording.
- Confirmed no secret value exposure in API, sync state, audit events, tool catalogs, or UI surfaces.
- PR #145 checks passed: Promotion, Rust, and Web.
- Phase 7+ remain `not_started`; Project Vaults, FTS/search, semantic memory, promotion, release, and managed install work have not started.

## Phase 7 — Project Vaults

Status: `in_progress`

Source docs:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- [`docs/workspace-model.md`](workspace-model.md)

Goal: Add project-scoped Vaults with daemon-enforced isolation.

Backend tasks:

- Ensure project scope keys exist and are distinct from workspace scope keys.
- Enforce project context for project secret resolution.
- Add project Vault APIs or scope-aware reuse of Workspace Vault APIs.
- Add tests for cross-project deny behavior.

Web tasks:

- Add `Project -> Vault` tab/section.
- Show project-scoped secrets and allowed consumers.
- Support create/update/delete/manage access.
- Make workspace vs project scope explicit.

Exit criteria:

- Project secrets are encrypted under project scope keys.
- Project secrets resolve only for matching project context and allowed consumers.
- UI clearly distinguishes Workspace Vault from Project Vault.

Completion notes:

- Local work in progress on `feat/project-vaults`.
- Added daemon-enforced project Vault scope validation for list/create/update flows and project-scoped policy list/upsert/delete operations while preserving workspace scope behavior.
- Added project-scoped Vault reference parsing for `vault://project/<project_id>/...` with matching project-context enforcement for MCP `vault_bearer` resolution.
- Added project isolation coverage for distinct workspace/project scope keys, cross-project context failures, locked Vault failures, metadata-only API/audit surfaces, and workspace Vault regression behavior.
- Updated Workspace Vault UI to switch between Workspace and Project scopes, manage project-scoped secrets/policies, and copy `vault://project/<project_id>/...` references without revealing values.
- Phase 8+ remain `not_started`; FTS/search, semantic memory, promotion, release, and managed install work have not started.

## Phase 8 — SQLite FTS5 searchable memory provider

Status: `not_started`

Source doc:

- [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)

Goal: Add fast local lexical memory search as the first search provider.

Tasks:

- Add FTS5 tables/index maintenance.
- Add `SqliteFtsMemorySearchProvider`.
- Add `/api/memory/search`.
- Track `use_count` and `last_used_at` for included or recalled memory.
- Add tests verifying FTS availability in managed-release SQLite configuration.

Exit criteria:

- Accepted memory can be searched locally.
- Search indexes are derived/rebuildable, not source of truth.

Completion notes:

- Pending.

## Phase 9 — HTTPS and bind-mode hardening

Status: `not_started`

Source docs:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)
- [`docs/context-security-boundaries.md`](context-security-boundaries.md)

Goal: Make safe network exposure easy and visible.

Tasks:

- Add explicit bind-mode settings or install/update guidance:
  - localhost only
  - Tailscale/private interface only
  - LAN
  - custom
- Consider built-in local TLS or guided TLS/reverse-proxy/Tailscale-cert support.
- Update managed install flow to avoid casual `0.0.0.0` exposure without explicit choice/warning.
- Keep Vault operations blocked on unsafe origins by default.

Exit criteria:

- Users can understand and adjust how Nucleus is exposed.
- Sensitive operations require secure transport/origin.

Completion notes:

- Pending.

## Phase 10 — Stable managed release and EBA verification

Status: `not_started`

Source docs:

- [`docs/repo-workflow.md`](repo-workflow.md)
- [`docs/managed-release.md`](managed-release.md)

Goal: Ship completed slices through the normal Nucleus workflow.

Tasks for each release-sized slice:

- Start from latest `dev`.
- Use focused feature branches.
- Run standard checks:
  - `npm run check:web`
  - `npm run build:web`
  - `cargo fmt --all --check`
  - relevant Rust tests
  - full `cargo test` when feasible
- Open PR into `dev`.
- Address review/CI.
- Merge through normal workflow.
- Promote/publish managed stable release when slice is intended for stable.
- Update `/home/eba/tools/nucleus-eba` to the new stable release.
- Verify EBA daemon on port `5202` or updated bind target.

EBA verification examples:

- Memory entries appear in prompt debug/compiled context.
- Memory candidates can be accepted/rejected and do not store credentials.
- Vault can initialize/unlock/lock.
- Vault secrets never appear in API responses/logs/transcripts.
- MCP credentials can be added through Vault UI.
- Supabase/Vercel/Cloudflare MCP auth errors are resolved through Vault-backed auth.
- Unsafe-origin Vault operations are blocked.
- Network posture UI correctly identifies `0.0.0.0`, LAN, localhost, and Tailscale/private binding.

Completion notes:

- Pending.

## Phase 11 — Retrieval provider interface and optional semantic search

Status: `not_started`

Source doc:

- [`docs/memory-implementation-plan.md`](memory-implementation-plan.md)

Goal: Add retrieval abstraction after FTS works.

Tasks:

- Add memory retrieval provider interface.
- Keep SQLite memory records canonical.
- Keep vector/semantic indexes derived and rebuildable.
- Evaluate `sqlite-vec`, Rust-native vector search, or external providers only after lifecycle/search are working.

Completion notes:

- Pending.

## Phase 12 — Optional external Vault providers / OS keychain wrapping

Status: `not_started`

Source doc:

- [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md)

Goal: Add optional provider extensions after the local Vault is complete.

Potential providers:

- OS keychain wrapping where reliable.
- 1Password references.
- Bitwarden Secrets Manager.
- Infisical.
- OpenBao/HashiCorp Vault.
- Cloud secret managers.

Rules:

- External providers are optional, not required for the main security story.
- Nucleus should store references where provider-owned secrets remain external.
- Provider integrations must preserve daemon-only secret resolution and policy/audit boundaries.

Completion notes:

- Pending.

## Instructions for future implementation sessions

When asked to work on this plan:

1. Identify the current phase from the master status table.
2. Read the linked source docs for that phase.
3. Check git status before editing.
4. Keep the branch focused on one phase or a clearly bounded slice of one phase.
5. Do not weaken the context/security boundaries to make implementation easier.
6. Add or update tests for the phase's exit criteria.
7. Run relevant checks before handoff/PR.
8. Update this document:
   - change phase status
   - add PR link or branch name
   - add completion notes
   - add follow-up tasks if scope was intentionally deferred
9. If a stable EBA release is requested, follow repo workflow and managed-release docs rather than stopping at local implementation.

## Deferred operator notes and follow-ups

- Operator/manager sessions should review executor reports, maintain gates, and provide prompts/checklists. They should not directly patch, commit, push, merge, promote, or release unless explicitly asked.
- Keep implementation, cleanup, PR/release, and verification work in separate focused sessions/worktrees.
- Phase 3 is merged/completed via PR #137. Phase 4 is merged/completed via PR #141. Phase 5 is merged/completed via PR #143. Phase 6 is merged/completed via PR #145. Phase 7 is `in_progress`; Phase 8 and later remain `not_started`.
- Main worktree cleanup was completed after Phase 2. The stale dirty branch was cleaned back to current dev. Future implementation work should still prefer fresh clean worktrees.
- A Node/toolchain runtime-resolution experiment was preserved separately and should be reviewed later as its own focused PR. It must not be mixed into Memory/Vault/Security phase work.
- Memory UI currently treats edited entries mostly as manual/user entries. After candidate capture and explicit remember flows exist, revisit preserving richer source metadata during edits.
- Vault and Memory boundaries remain strict:
  - Memory is prompt-visible context.
  - Vault is never prompt-visible.
  - Vault list/status APIs must never return plaintext secret values.
  - Secrets must never enter prompts, Memory, transcripts, logs, audit events, debug payloads, or normal browser-visible responses.
  - Plaintext Vault operations, including unlock/create/update secret flows, must require safe-origin/security posture checks.
- Old temporary worktrees should be pruned in a later maintenance pass, not during active Vault/Memory implementation.
- Stable release/promotion should happen only when explicitly requested after the selected phase batch is complete.
