# Skills + MCP + persistence implementation plan

Status legend:
- `not started`
- `in progress`
- `blocked`
- `done`

## Goal

Implement Skills, MCPs, and the first durable Nucleus persistence/storage pattern as one coordinated plan.

That means:
- Nucleus stores the durable truth
- Nucleus persistence becomes a first-class backend layer, not ad hoc feature state
- Nucleus discovers and governs MCP servers/tools
- Nucleus attaches Skills through its own state
- Nucleus exposes both through compiled turns
- MCP execution flows through the Utility Worker / Nucleus action path
- persistence for these resources establishes the reusable Nucleus-owned storage pattern
- clients do not invent or own this state themselves

---

## Phase 0 — Existing groundwork
**Status:** `partially done`

### Already present
- compiled-turn contract has placeholders for:
  - `skill_layers`
  - `mcp_catalog`
- prompt rendering already knows how to display:
  - Skill layers
  - MCP metadata
- runtime execution path already exists for:
  - Nucleus prompt assembly
  - Utility Worker-driven turn execution

### Missing
- persistent source-of-truth objects
- population of `skill_layers`
- population of `mcp_catalog`
- executable MCP bridge
- management APIs/UI
- reusable persistence/storage conventions for these resources

---

## Phase 1 — Nucleus-owned resource model
**Status:** `complete`

### Purpose
Create the durable backend entities that represent Skills and MCPs.

### Scope
Add Nucleus-owned records for:

#### MCP
- `mcp_servers`
  - id
  - workspace_id
  - name/title
  - transport
  - launch/config metadata
  - enabled/disabled
  - sync status
  - timestamps
- `mcp_tools`
  - server_id
  - tool name
  - description
  - input schema snapshot
  - discovery metadata
  - timestamps

#### Skills
- `skill_packages`
  - id
  - name
  - version
  - manifest
  - instruction payload
- `skill_installations`
  - package_id
  - workspace/project/session scope
  - enabled/disabled
  - pinned version
  - timestamps

#### Policy / control
- approvals
- allowlists/blocklists
- secret references
- execution limits/status

### Exit criteria
- structs/types defined
- resource ownership and scopes documented
- the plan explicitly defines persistence as the next phase for these same resources

---

## Phase 2 — Persistence storage foundation + daemon/API surface
**Status:** `complete`

### Purpose
Implement storage for Skills and MCPs while also establishing the reusable Nucleus persistence pattern for durable backend resources.

### Scope

#### Storage foundation
Define the general Nucleus persistence conventions for:
- IDs
- timestamps
- enabled/disabled state
- status/error fields
- workspace/project/session scoping
- migration structure
- storage access/repository pattern

#### Concrete persisted resources
Add durable storage for:
- `mcp_servers`
- `mcp_tools`
- `skill_packages`
- `skill_installations`

#### Daemon/API surface
Add daemon/API support for:
- create/list/update/delete MCP servers
- inspect MCP server health/status
- trigger MCP discovery/sync
- list discovered MCP tools
- install/list/enable/disable Skills
- attach Skills by workspace/project/session scope
- inspect errors/status

### Principles
- web app is a client only
- durable truth lives in Nucleus
- this phase should create patterns reusable beyond Skills/MCP
- secret material should not live in session prompt state

### Exit criteria
- schema/migrations exist for the first Skills/MCP resources
- resources survive restart
- daemon/API can manage them through Nucleus-owned persistence
- the storage approach is suitable as the starting pattern for broader Nucleus persistence

---

## Phase 3 — MCP discovery and catalog sync
**Status:** `complete`

### Purpose
Turn MCP from empty metadata slots into real discovered catalog entries.

### Scope
Start with one transport:
- `stdio` first

Implement:
- MCP server registration in Nucleus
- server process launch/connect flow
- tool discovery
- normalized tool snapshot persistence
- sync status/error reporting

### Exit criteria
- a registered stdio MCP server can be discovered and synced through the daemon/API
- discovered tools are stored durably
- discovery failures are inspectable

---

## Phase 4 — Prompt assembly integration
**Status:** `complete`

### Purpose
Populate compiled turns from Nucleus-owned Skill and MCP state.

### Scope

#### Skills
- collect enabled Skills for workspace/project/session
- convert them into deterministic compiled prompt layers
- populate `skill_layers`

#### MCP
- collect available registered MCP server/tool summaries
- populate `mcp_catalog`

### Important boundary
At this phase, MCP in prompt assembly may still be descriptive only.
Execution does not have to be enabled yet.

### Exit criteria
- `skill_layers` is populated from stored Skill attachments
- `mcp_catalog` is populated from discovered MCP state
- prompt output is deterministic and sourced from backend truth

---

## Phase 5 — Skill packaging and installation model
**Status:** `complete`

### Purpose
Define the minimum viable Skill format and lifecycle.

### Scope
Define a Skill manifest such as:
- name
- version
- instructions
- optional description
- required/optional actions or MCP dependencies
- optional scope restrictions

Add:
- package install path
- enable/disable path
- version pinning
- attachment rules by scope

### Recommended starting point
Begin with local/developer-installed Skills before designing publishing/signing.

### Exit criteria
- one or more Skills can be installed locally
- Skills can be attached to a workspace/project/session
- attached Skills reliably appear in compiled turns

---

## Phase 6 — MCP execution bridge through Nucleus Actions
**Status:** `complete`

### Purpose
Make MCP tools actually executable through the Nucleus runtime.

### Scope
Implement:
- daemon-side MCP invocation service
- Utility Worker mediation
- Action-shaped execution surface
- streaming/result forwarding where appropriate
- audit/logging

### Required architecture rule
The web app must not call MCP servers directly.
Execution must flow through Nucleus.

### Exit criteria
- Utility Worker can invoke an MCP-backed Action through Nucleus
- invocation is logged/auditable
- execution follows the same control-plane path as other Nucleus actions

---

## Phase 7 — Policy, safety, and secrets
**Status:** `not started`

### Purpose
Make Skills and MCPs safe and governable enough for real usage.

### Scope
Add:
- workspace/project scoping
- per-server enablement
- per-tool allow/deny rules
- approval requirements for sensitive actions
- timeouts
- concurrency limits
- retry/termination behavior
- secret reference handling

### Exit criteria
- sensitive MCP use can be restricted or require approval
- secrets are not leaked into prompt/session state
- failures and limits are visible and controllable

---

## Phase 8 — UX and operator surfaces
**Status:** `in progress`

### Purpose
Expose the new backend capabilities in the product UI.

### Scope
Workspace UI should get distinct navigation items/tabs for:
- Memory
- Skills
- MCP

Memory remains for general memory only.
Skills and MCPs should not be managed inside Memory.

Potential UI areas:
- workspace settings or workspace area for MCP server management
- MCP discovery/sync status
- Skill installation/enablement UI
- session/project attached Skills display
- approval UI for MCP-backed Actions
- debug/status surfaces for failures

### Exit criteria
- users can manage Skills from a dedicated Skills area
- users can manage MCPs from a dedicated MCP area
- Memory remains separate and focused on general memory
- status and failures are visible without logs
- the UI reflects Nucleus-owned truth rather than local shadow state

---

## Phase 9 — Packaging, trust, and distribution
**Status:** `not started`

### Purpose
Support stronger delivery and upgrade stories after local/dev mode works.

### Scope
Later additions:
- signed Skill packages
- published registries/sources
- provenance
- compatibility metadata
- rollback
- upgrade flows

### Exit criteria
- versioned upgrade path exists
- trust/provenance model is documented
- installs/upgrades are reproducible

---

## Recommended implementation order

1. Phase 1 — resource model
2. Phase 2 — persistence storage foundation + daemon/API
3. Phase 3 — MCP discovery
4. Phase 4 — prompt assembly integration
5. Phase 5 — Skill packaging/install
6. Phase 6 — MCP execution bridge
7. Phase 7 — policy/safety
8. Phase 8 — UX
9. Phase 9 — packaging/trust/distribution

---

## Recommended MVP cut

A good first usable slice would be:
- Phase 1 complete
- Phase 2 complete
- Phase 3 complete for stdio only
- Phase 4 complete
- Phase 5 complete with local-only Skills
- partial Phase 6 for basic MCP tool execution

That would give Nucleus:
- durable MCP server records
- discovered MCP tool catalog
- durable Skill attachments
- the first reusable Nucleus persistence/storage pattern
- compiled turn population
- initial real MCP execution through Nucleus

---

## Current status summary

| Phase | Name | Status |
|---|---|---|
| 0 | Existing groundwork | partially done |
| 1 | Nucleus-owned resource model | complete |
| 2 | Persistence storage foundation + daemon/API surface | complete |
| 3 | MCP discovery and catalog sync | complete |
| 4 | Prompt assembly integration | complete |
| 5 | Skill packaging and installation model | complete |
| 6 | MCP execution bridge through Nucleus Actions | complete |
| 7 | Policy, safety, and secrets | not started |
| 8 | UX and operator surfaces | in progress |
| 9 | Packaging, trust, and distribution | not started |
