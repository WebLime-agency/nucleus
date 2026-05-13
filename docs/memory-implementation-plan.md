# Nucleus Durable Memory Implementation Plan

## Purpose

Finish Nucleus durable memory as a daemon-owned product capability.

The first implementation should deliver two focused PRs:

1. **PR 1 — Memory prompt integration + real Memory UI**
2. **PR 2 — Memory candidates + explicit/automatic capture loop**

These PRs should keep memory separate from Skills, MCPs, prompt includes, raw transcripts, and Vault secrets while making memory useful in actual turns.

Vault/security work can proceed in parallel, but it must share the same workspace/project/session scoping language and daemon-owned policy/audit conventions. Memory must never become the storage location for secrets, secret references intended for execution, decrypted values, or credential-bearing transcripts. If automatic memory extraction sees credentials, tokens, cookies, private keys, authorization headers, or similarly sensitive material, it should redact or skip that content and record only a non-secret operational note when useful.

## Storage and retrieval direction

Use SQLite as the source of truth for memory records and candidates.

Treat memory as daemon-owned **context providers**:

- accepted short-form memory becomes prompt-visible context layers
- pending candidates remain review-only records and never enter prompts
- searchable memory is a derived provider backed first by SQLite FTS5
- semantic/vector memory is an optional future provider, not the canonical store

Do **not** store canonical memory as loose Markdown files. Markdown files are easy to inspect, but they are too sloppy as the product source of truth: weak schema, weak status lifecycle, weak scoping, weak dedupe, weak UI/API ergonomics, and awkward migrations.

Do **not** introduce FAISS/LangChain as the first Nucleus memory backend. Nucleus is a Rust daemon with an existing SQLite persistence layer. A Python/vector sidecar would add packaging, release, migration, model, and runtime complexity before the product memory loop exists.

Do **not** store secrets in memory entries or memory candidates. Durable memory is prompt-visible context. Vault secrets are execution-time confidential material and must remain outside prompts, transcripts, memory, include files, skills, and client-visible API responses.

Recommended staged storage model:

### Stage A — SQLite canonical records

Canonical memory lives in SQLite tables:

- `memory_entries` for accepted durable memory
- `memory_candidates` for pending/rejected/accepted proposals
- future `memory_events` or audit records for memory lifecycle history if the existing audit event system is not enough

PR 1 and PR 2 should complete this stage.

### Stage B — SQLite FTS5 searchable context provider

Add SQLite FTS5 virtual tables for local lexical retrieval once entries/candidates exist. This gives Nucleus fast, embedded, dependency-light search and deterministic managed-release behavior.

Use this as the first built-in `memory.search` implementation. It should be good enough for many Nucleus memory queries because local developer/operator memory often contains exact project names, repo names, ports, file paths, model names, commands, decisions, issue IDs, and proper nouns.

`rusqlite` currently uses the bundled SQLite feature. If FTS5 availability needs to be explicit, add the appropriate bundled feature or verify it in a storage test.

### Stage C — Retrieval provider interface

After SQLite FTS5 works, introduce a daemon-owned retrieval abstraction so search can support lexical, semantic, or hybrid retrieval without changing the product memory lifecycle.

Shape:

- canonical memory remains in SQLite
- all indexes are derived and rebuildable
- prompt compilation depends on accepted memory records, not directly on a vector store
- search tools/API depend on a provider interface
- provider status and rebuild diagnostics are visible in the daemon/API

Potential provider implementations:

- `SqliteFtsMemorySearchProvider`
- `HybridMemorySearchProvider`
- `SqliteVecMemorySearchProvider` if SQLite vector extension packaging is stable enough
- Rust-native embedded vector provider if it can be shipped cleanly
- external/vector service provider only as an opt-in advanced backend, not the local product default

### Stage D — Optional semantic retrieval

Add embeddings only after the lifecycle and FTS provider are working.

Preferred future shape:

- embeddings are derived indexes, never source of truth
- embedding provider/model metadata is stored with each vector index version
- index rebuild is supported when embedding config changes
- memory search can combine FTS score, vector score, scope priority, recency, and use count

### Why this direction

Nucleus needs reliable local product behavior more than it needs immediate semantic search. The memory loop should first be correct, inspectable, scoped, editable, and prompt-integrated. Search should start with SQLite FTS5, then semantic retrieval can be layered on through a provider abstraction after the core lifecycle is real.

## Vocabulary

- **Context layer**: a daemon-compiled prompt section with an explicit kind, scope, title, source, and budgeted content.
- **Memory entry**: accepted durable memory that can be included/recalled in future turns.
- **Memory candidate**: proposed memory extracted from a session/turn, pending review or promotion.
- **Prompt include**: deterministic file-based context from `include/`, `.nucleus/include/`, and legacy promptinclude files.
- **Transcript**: raw session history and evidence source, not memory.
- **Skill**: procedural capability and activation instructions.
- **MCP**: external tool/resource server registry metadata and runtime capability.
- **Vault secret**: confidential execution-time material such as API tokens, cookies, private keys, passwords, or bearer credentials. Vault secrets are never prompt-visible memory.
- **Vault reference**: a non-secret pointer to a secret, scoped and policy-gated by the daemon. Vault references may appear in MCP/action configuration, but should not be promoted into memory unless the reference itself is operationally useful and contains no sensitive value.
- **Search provider**: a daemon-owned retrieval implementation. SQLite FTS5 is the first target provider; semantic/vector providers are optional derived indexes later.

## Memory and Vault boundary

Memory and Vault are sibling daemon-owned capabilities, but they have opposite visibility requirements:

- Memory is prompt-visible durable context.
- Vault is confidential execution-time material.

This plan owns memory implementation. Vault and network/security implementation are specified in [`docs/vault-security-implementation-plan.md`](vault-security-implementation-plan.md). Cross-cutting rules for prompts, memory, transcripts, MCPs, Vault references, redaction, audit events, and secure-origin behavior are specified in [`docs/context-security-boundaries.md`](context-security-boundaries.md).

Memory work must follow those boundary rules:

- No plaintext secret may be stored in `memory_entries`, `memory_candidates`, prompt includes, skills, transcripts, audit payloads, or client-visible API responses.
- Memory candidate extraction must treat credential-like content as sensitive and redact or skip it.
- Accepted memory may mention that a credential exists only in non-secret terms, for example: "Project X uses a project-scoped Supabase token stored in Vault."
- Memory search/retrieval must never query or return Vault secret material.

Coordination with Vault/security:

- Memory PR 1 can proceed independently from Vault backend work.
- Memory PR 2 candidate extraction should use the shared redaction helpers from the Vault/security track, or include equivalent redaction and consolidate immediately after.
- Memory and Vault should reuse the same `scope_kind` / `scope_id` conventions and audit/redaction patterns.

## Context provider model

Nucleus should compile prompt context from typed providers instead of mixing unrelated context into raw prompt text.

Initial providers:

1. **Platform provider**
   - produces Nucleus runtime/system contract layers
2. **Prompt include provider**
   - reads deterministic include files
   - produces `kind = "include"` or `kind = "project"` layers
3. **Memory provider**
   - reads accepted memory entries
   - produces `kind = "memory"` layers
4. **Skill provider**
   - reads active skill manifests/packages/includes
   - produces `kind = "skill"` layers
5. **MCP/tool provider**
   - contributes tool catalogs, not memory text

The compiled turn should be the single source of truth for what providers see. Debug summaries must describe the same layers that are actually rendered into provider prompts.

## Data model

### Extend `memory_entries`

Current fields should remain compatible:

- `id`
- `scope_kind`
- `scope_id`
- `title`
- `content`
- `tags_json`
- `enabled`
- `created_at`
- `updated_at`

Add fields:

- `status TEXT NOT NULL DEFAULT 'accepted'`
  - allowed: `accepted`, `archived`
- `memory_kind TEXT NOT NULL DEFAULT 'note'`
  - initial allowed values: `note`, `fact`, `preference`, `decision`, `project_note`, `solution`, `constraint`, `todo`
- `source_kind TEXT NOT NULL DEFAULT 'manual'`
  - initial allowed values: `manual`, `candidate`, `explicit_remember`, `import`, `system`
- `source_id TEXT NOT NULL DEFAULT ''`
- `confidence REAL NOT NULL DEFAULT 1.0`
- `created_by TEXT NOT NULL DEFAULT 'user'`
  - initial allowed values: `user`, `assistant`, `utility_worker`, `system`
- `last_used_at INTEGER`
- `use_count INTEGER NOT NULL DEFAULT 0`
- `supersedes_id TEXT NOT NULL DEFAULT ''`
- `metadata_json TEXT NOT NULL DEFAULT '{}'`

Indexes:

- `(scope_kind, scope_id, enabled, status)`
- `(memory_kind)`
- `(source_kind, source_id)`
- `(last_used_at)`

### Add `memory_candidates`

Candidates should be isolated from accepted memory.

Fields:

- `id TEXT PRIMARY KEY`
- `scope_kind TEXT NOT NULL`
- `scope_id TEXT NOT NULL`
- `session_id TEXT NOT NULL DEFAULT ''`
- `turn_id_start TEXT NOT NULL DEFAULT ''`
- `turn_id_end TEXT NOT NULL DEFAULT ''`
- `candidate_kind TEXT NOT NULL DEFAULT 'note'`
- `title TEXT NOT NULL`
- `content TEXT NOT NULL`
- `tags_json TEXT NOT NULL DEFAULT '[]'`
- `evidence_json TEXT NOT NULL DEFAULT '[]'`
- `reason TEXT NOT NULL DEFAULT ''`
- `confidence REAL NOT NULL DEFAULT 0.0`
- `status TEXT NOT NULL DEFAULT 'pending'`
  - allowed: `pending`, `accepted`, `rejected`, `dismissed`, `superseded`
- `dedupe_key TEXT NOT NULL DEFAULT ''`
- `accepted_memory_id TEXT NOT NULL DEFAULT ''`
- `created_by TEXT NOT NULL DEFAULT 'utility_worker'`
- `created_at INTEGER NOT NULL DEFAULT (unixepoch())`
- `updated_at INTEGER NOT NULL DEFAULT (unixepoch())`
- `metadata_json TEXT NOT NULL DEFAULT '{}'`

Indexes:

- `(status, created_at)`
- `(scope_kind, scope_id, status)`
- `(session_id, status)`
- `(dedupe_key, status)`

### Add FTS tables in the search follow-up

Use FTS for accepted entries and possibly candidates:

- `memory_entries_fts(title, content, tags, content='memory_entries', content_rowid=...)` if rowid mapping is practical
- or an external-content-free FTS table maintained by storage upsert/delete helpers
- optionally `memory_candidates_fts` for candidate review and dedupe support

The first two PRs do not need full search if scoped prompt injection is budgeted deterministically and candidate dedupe can use normalized keys. FTS should land in the first search/retrieval follow-up as the initial `SqliteFtsMemorySearchProvider`.

## Scope policy

Supported initial scopes:

- `workspace` / `workspace`
- `project` / project id
- `session` / session id

Later scopes:

- `profile` / profile id
- `agent` / agent id

Memory selection for a turn:

1. workspace accepted memory
2. active project accepted memory
3. current session accepted memory

Only include enabled, accepted entries.

Use stable ordering:

1. scope priority
2. memory kind priority
3. title
4. id

Budgeting:

- add a conservative total memory character budget, e.g. 12k chars initially
- add per-entry max chars, e.g. 2k chars
- truncate with a clear marker if needed
- include debug counts for total, included, skipped, and truncated memory entries

## PR 1 — Memory prompt integration + real Memory UI

### Goal

Make accepted durable memory actually useful in turns and expose real operator controls in the web UI.

### Backend tasks

1. Extend protocol types.
   - Update `MemoryEntry` with the new fields.
   - Update `MemoryEntryUpsertRequest` to accept new optional fields.
   - Add memory-layer debug fields to `CompiledTurnDebugSummary`:
     - `memory_count`
     - `memory_included_count`
     - `memory_skipped_count`
     - `memory_truncated_count`
   - Add either:
     - `memory_layers: Vec<CompiledPromptLayer>` to `CompiledTurn`, preferred, or
     - include memory in `project_layers` with `kind = "memory"` if avoiding protocol expansion.

2. Migrate storage.
   - Add the new `memory_entries` columns using idempotent migration helpers.
   - Preserve existing records as accepted manual memory.
   - Add validation/normalization helpers for:
     - scope kind
     - status
     - memory kind
     - source kind
     - created by
     - confidence clamping
     - JSON field decoding

3. Fix prompt/include split.
   - This is a release-blocking part of PR 1, not a later cleanup.
   - Current issue: `compile_session_turn()` discovers prompt include sources and updates debug counts, but those include contents are not carried into the compiled turn that provider execution uses.
   - Remove the split between `assemble_prompt_input()` rendered raw text and `compile_session_turn()` structured context.
   - Convert prompt include sources into compiled layers.
   - Populate `project_layers` or an equivalent `include_layers`/context-layer field.
   - Ensure compiled turns sent to providers include the same context represented by debug summaries.
   - Ensure prompt include layers are rendered before memory and skills so stable product/project context remains foundational.
   - Keep daemon-owned prompt assembly as the source of truth.
   - Add regression tests proving include file text appears in the provider-visible compiled prompt, not only in debug metadata.

4. Add memory compilation.
   - Implement `collect_compiled_memory_layers(state, session)` as the first memory context provider.
   - Select only enabled, accepted memory matching workspace/project/session scope.
   - Apply budget and truncation.
   - Prefer aggregated scope blocks for prompt cleanliness:
     - `memory:workspace`
     - `memory:project:<project_id>`
     - `memory:session:<session_id>`
   - Within each block, render individual entries with kind/title/source markers.
   - If per-entry layers are simpler for the first patch, keep their IDs stable and preserve the same ordering/budget semantics.
   - Return `CompiledPromptLayer` records with:
     - `kind = "memory"`
     - `scope = scope_kind`
     - `title = memory block title`
     - `source_path = memory:<scope>` or `memory:<id>`
     - `content = rendered memory content`
   - Add memory layers to the compiled turn.
   - Update debug summary.

5. Ensure provider rendering consumes layers.
   - The provider execution path must render:
     - platform system layers
     - prompt include/project layers
     - memory layers
     - skill layers
     - user turn
   - Required order:
     1. platform/runtime contract
     2. global/workspace/project/session prompt includes
     3. accepted memory layers
     4. active skill layers
     5. user request and images
   - Avoid any split where debug compiled turns contain context that provider calls do not see.
   - Avoid any split where legacy raw prompt rendering includes context but structured compiled turns do not.

6. Improve API behavior.
   - Preserve existing `/api/memory` routes.
   - Support new fields in create/update.
   - Add optional filtering query params if straightforward:
     - `scope_kind`
     - `scope_id`
     - `status`
     - `enabled`
   - Keep list-all behavior acceptable for small initial memory sets.

7. Add tests.
   - Existing memory rows migrate correctly.
   - Memory upsert/list/delete handles new fields.
   - Enabled accepted workspace memory appears in compiled turns.
   - Disabled memory does not appear.
   - Archived memory does not appear.
   - Project-scoped memory appears only when that project is active.
   - Session-scoped memory appears only for that session.
   - Prompt includes are actually present in compiled turn layers/content.
   - Debug summary counts match included/skipped/truncated memory.

### Web UI tasks

1. Replace placeholder Memory page.
   - Load `fetchMemory()`.
   - Show entries grouped by scope/status/kind.
   - Show counts: total, enabled, scopes.

2. Add memory entry CRUD.
   - Create entry.
   - Edit entry.
   - Enable/disable entry.
   - Archive/delete entry.
   - Edit tags.
   - Edit scope and memory kind.

3. Add safe empty/loading/error states.

4. Update TypeScript schemas for new fields.

5. Add basic UI tests or type-check coverage where existing patterns support it.

### Documentation tasks

1. Update `docs/skills-mcp-plan.md` to mark memory UI/prompt integration accurately.
2. Add/adjust concise memory description in `include/` if stable.
3. Document memory as distinct from prompt includes, transcripts, Skills, and MCPs.

### PR 1 exit criteria

- A user can create a memory in the UI.
- The daemon persists it.
- The memory appears in future compiled turns when scope matches.
- Disabled/archived/out-of-scope memory does not appear.
- Prompt includes are no longer merely counted; they are represented in compiled prompt context.
- Tests prove the above.

## PR 2 — Memory candidates + capture loop

### Goal

Add the mechanism that notices useful long-term information and proposes it without polluting accepted memory.

### Backend tasks

1. Add protocol types.
   - `MemoryCandidate`
   - `MemoryCandidateUpsertRequest`
   - `MemoryCandidateSummary`
   - `MemoryCandidateAcceptRequest`
   - `MemoryCandidateRejectRequest` if useful, or use status update.

2. Add storage.
   - Create `memory_candidates` table.
   - Add list/upsert/delete/update-status helpers.
   - Add accept helper that:
     - creates a `memory_entries` row
     - links `accepted_memory_id`
     - marks candidate `accepted`
     - preserves evidence/source metadata
   - Add reject/dismiss helper.

3. Add API routes.
   - `GET /api/memory/candidates`
   - `POST /api/memory/candidates`
   - `PUT /api/memory/candidates/{candidate_id}`
   - `POST /api/memory/candidates/{candidate_id}/accept`
   - `POST /api/memory/candidates/{candidate_id}/reject`
   - `DELETE /api/memory/candidates/{candidate_id}`

4. Add explicit remember action.
   - Add a daemon-owned tool/action or command path to create accepted memory directly from user intent.
   - Examples:
     - `/remember <text>`
     - Nucleus tool descriptor `memory.create`
     - UI action on a turn: `Save as memory`
   - Explicit remember should bypass candidate status and create accepted memory with:
     - `source_kind = explicit_remember`
     - `created_by = user` or `assistant` depending on caller

5. Add automatic candidate extraction.
   - After a successful assistant turn, enqueue a utility job.
   - Use recent session turns as input, with a bounded character budget.
   - Ask for structured JSON candidates only.
   - Store candidates as `pending`.
   - Never auto-write accepted memory in the first version.
   - If utility extraction fails or returns invalid JSON, log/audit but do not affect the user turn.

6. Candidate extraction prompt requirements.
   - Return JSON array only.
   - Each item includes:
     - `scope_kind`
     - `scope_id`
     - `candidate_kind`
     - `title`
     - `content`
     - `tags`
     - `confidence`
     - `reason`
     - `evidence`
   - Only propose complete, stable, future-useful information.
   - Do not propose vague topics.
   - Do not propose transient facts unless they are part of a durable decision/state.
   - Do not propose assistant instructions, chain-of-thought, or ephemeral execution details.
   - Do not propose secrets or credential-bearing values, including API tokens, cookies, bearer headers, private keys, passwords, recovery phrases, `.env` values, or database URLs with credentials.
   - If a durable operational note involves credentials, propose only a non-secret memory such as "This project uses a project-scoped Vercel token stored in Vault" and include no value.
   - Prefer fewer, merged candidates over many tiny candidates.

7. Dedupe guardrails.
   - Compute a normalized `dedupe_key` from scope + kind + title/content hash.
   - Do not create duplicate pending candidates with the same key.
   - If a matching accepted memory already exists, either skip the candidate or mark it as possible update via metadata.

8. Add audit events.
   - candidate extraction started/completed/failed
   - candidate created
   - candidate accepted
   - candidate rejected
   - explicit memory created

10. Add secret redaction guardrails.
   - Run candidate content and evidence through the shared redactor before storage.
   - Reject or redact candidate values that match registered secret values or sensitive token/key patterns.
   - Ensure extraction prompts explicitly classify credentials as non-memory.
   - Add tests proving candidate extraction does not store obvious credentials.

11. Add tests.
   - Candidate CRUD works.
   - Candidate accept creates accepted memory and links IDs.
   - Rejected candidates do not enter prompt context.
   - Pending candidates do not enter prompt context.
   - Duplicate extraction does not create duplicate pending candidates.
   - Explicit remember creates accepted memory.
   - Extraction failures do not fail the user turn.

### Web UI tasks

1. Add candidate review section to Memory page.
   - Pending candidates first.
   - Show title, kind, scope, confidence, reason, evidence, created time.
   - Actions:
     - accept
     - edit-and-accept
     - reject
     - dismiss/delete

2. Add accepted memory source details.
   - Show source kind/source id if available.
   - Show linked candidate/evidence where available.

3. Add explicit create flow polish.
   - Manual memory create remains easy.
   - Candidate review should not make accepted memory management harder.

### Documentation tasks

1. Document candidate lifecycle.
2. Document that automatic extraction proposes memory but does not accept it by default.
3. Document scope behavior.
4. Document explicit remember behavior.

### PR 2 exit criteria

- Nucleus can propose durable memory candidates from completed sessions/turns.
- Candidates remain separate from accepted memory.
- The operator can accept/reject/edit candidates in the UI.
- Accepted candidates become memory entries and appear in matching future compiled turns.
- Pending/rejected candidates never affect prompt context.
- Explicit remember creates accepted memory immediately.

## Follow-up PRs after the two core PRs

### Follow-up A — SQLite FTS5 searchable memory provider

- Add `SqliteFtsMemorySearchProvider` as the first built-in searchable context provider.
- Add FTS5-backed memory search over accepted memory entries.
- Optionally add FTS5-backed search over candidates for review/dedupe.
- Add `/api/memory/search`.
- Add future agent tool descriptor `memory.search` once tool execution should expose memory recall.
- Add `memory.get` only if search results need follow-up expansion by ID.
- Add internal search helper for candidate dedupe and future retrieval.
- Track `use_count` and `last_used_at` when memory is included or explicitly recalled.
- Add tests verifying FTS availability with bundled SQLite.

### Follow-up B — Retrieval provider interface and semantic search evaluation

- Add a daemon-owned memory retrieval interface that can support FTS-only, vector-only, or hybrid search.
- Keep SQLite as canonical truth.
- Keep all search/vector indexes derived and rebuildable.
- Add provider status/rebuild diagnostics to the daemon/API.
- Evaluate optional semantic backends after FTS works:
  - SQLite vector extension such as `sqlite-vec` if Rust/managed-release packaging is stable
  - Rust-native embedded vector index if it can be shipped cleanly
  - external/vector service only as an opt-in advanced backend
- Add embedding config/state tables only when a semantic provider is selected.
- Add index rebuild flow when embedding model/provider config changes.
- Do not add FAISS/LangChain unless benchmarks and packaging review show it is worth the extra runtime complexity.

### Follow-up C — Promotion and consolidation

- Add candidate update/supersede workflows.
- Add repeated-use promotion logic.
- Add merge/supersede suggestions.
- Add stale memory review/pruning UI.

### Follow-up D — Import/export

- Export accepted memory as JSON/Markdown for humans.
- Import memory with explicit scope and review.
- Keep imports as structured records, not canonical loose files.

## Non-goals for the first two PRs

- No vector DB requirement.
- No Python sidecar.
- No loose Markdown memory source of truth.
- No automatic acceptance of extracted memories.
- No large-scale memory compaction/dreaming system.
- No provider-native memory dependency.
- No Vault secret storage in memory records or candidates.
- No decrypted secret values in prompt context, memory search, transcripts, audit payloads, or UI state.
- No automatic memory extraction from credential-bearing content without redaction/skip guardrails.

## Acceptance checklist

- [ ] Memory is daemon-owned and stored in SQLite.
- [ ] Prompt includes are represented in compiled prompt context, not just counted.
- [ ] Accepted memory is represented in compiled prompt context with scope and budget controls.
- [ ] Memory UI can manage accepted memory.
- [ ] Candidate memory has a separate lifecycle from accepted memory.
- [ ] Automatic extraction creates pending candidates only.
- [ ] Explicit remember creates accepted memory.
- [ ] Memory extraction and storage reject or redact credential-like content.
- [ ] Memory and Vault share scope/audit/redaction conventions but remain separate product capabilities.
- [ ] Tests cover prompt inclusion, scoping, candidate lifecycle, and failure isolation.
