# Local AI Control Plane RFC

Status: Draft
Date: 2026-05-04
Owner: eba

## Summary

This RFC defines the replacement direction for the current stack of local AI control tools:

- Mission Control
- Hermes
- ZeroClaw

The new system is not a sync utility. It is a local AI control plane that owns execution directly.

Current Mission Control remains in place as the compatibility bridge for syncing MCPs, skills, and rules into legacy runtimes until the new system is ready to take over real work.

Naming decision for the current draft:

- product name: `Nucleus`
- launch domain strategy: Weblime subdomain
- no separate name for the main dashboard surface

## Product Definition

The new system is a local AI operations runtime for a machine, its agent workloads, and its automations.

It should eventually:

- orchestrate sessions across providers
- own secrets and tool registration centrally
- manage automations and background jobs
- expose host and process observability
- provide approvals, policy, and audit history
- coordinate multiple agents

It should not define itself around mirroring config into other products.

## Problem

The current stack is fragmented by responsibility:

- Mission Control owns canonical config and fans it out
- older session tooling owns session control and review workflows
- legacy file-centric runtimes own a subset of agent execution and prompt behavior
- Hermes and ZeroClaw cover overlapping automation and runtime concerns

This creates:

- duplicated state
- duplicated auth and secret handling
- unclear source of truth
- slow and fragile sync loops
- UI state that depends on expensive validation
- operational complexity across multiple runtimes

The long-term fix is to own runtime behavior directly rather than orchestrating several partially overlapping tools forever.

## Strategic Direction

The new system should replace other tools in phases.

Core rule:

- central ownership of execution is the product
- compatibility syncing is temporary migration glue

That means:

- skills become centrally managed playbooks, profiles, or instruction packs
- MCPs become centrally managed tool endpoints or tool registrations
- secrets are resolved at execution time by the control plane
- sessions are created and governed by the new system itself
- third-party config generation exists only as an adapter where still required

## V1 Kill Target

V1 does not try to replace every tool at once.

V1 target:

- replace the current daily local AI session control, chat flow, and usage visibility surface
- replace the current Mission Control host-status dashboards for CPU, memory, and other core system metrics

V1 should prove:

- host operations can be owned directly by Nucleus
- session orchestration is viable once the host layer is in place
- provider adapters are viable
- the daemon can own durable truth
- the web UI is fast and operationally clear

V1 delivery order:

1. host operations parity first
2. provider and session orchestration second

The first win is not model orchestration. The first win is replacing the existing machine-operations surface with a faster, cleaner, daemon-owned system.

V1 explicitly does not need to:

- replace every legacy runtime completely
- replace Hermes completely
- replace ZeroClaw completely
- provide native clients
- perform broad desktop automation
- support arbitrary remote fleets

## Non-Goals

These are out of scope for V1:

- becoming a generic desktop assistant
- acting with unrestricted system permissions by default
- replacing every existing agent runtime in one release
- maintaining permanent sync-first architecture
- shipping a macOS native client before the daemon and web control plane are stable

## Architecture

### Core shape

- Rust daemon as the source of truth
- SvelteKit frontend used strictly as UI
- SQLite for local durable state
- REST for bootstrap reads and mutations
- WebSocket for live updates and event streams

Production shape:

- one Rust service
- one static frontend bundle
- no Node server in the runtime path

### Repository and distribution model

Nucleus should ship from a single monorepo.

That monorepo should contain:

- the Rust daemon
- the operator CLI
- the web UI
- shared protocol and domain crates
- future native clients
- shared docs, migrations, and scripts

This follows the useful part of the existing server-first monorepo pattern:

- one source tree
- multiple distributable surfaces
- server-first architecture
- room for additional clients over time without fragmenting the project

The key rule is that the monorepo is a packaging and development model, not a reason to blur system boundaries.

- the daemon still owns runtime truth
- clients remain clients
- shared crates carry protocol and domain contracts

### Frontend rule

SvelteKit is allowed for:

- routing
- layouts
- forms
- component organization
- client-side state and presentation

SvelteKit is not the system backend.

Core business logic should not live in SvelteKit endpoints or server actions.

Frontend defaults for V1:

- use Zod on the SvelteKit side for request, form, and response-shape validation at the UI boundary
- use Tailwind CSS as the default styling layer for the web client
- use a shadcn-svelte style component structure for core UI primitives on the web client

The intent is:

- Rust remains the authoritative runtime and persistence validator
- SvelteKit gets explicit schema validation for safer UI mutations and data handling
- Tailwind gives the web surface a fast, consistent styling system without inventing a custom CSS architecture early
- the component layer stays modular and replaceable instead of turning the first web client into ad hoc page CSS

### Rust daemon rule

The daemon owns:

- session lifecycle
- provider adapters
- tool registry
- secret resolution
- health engine
- scheduler and background jobs
- policy checks
- audit trail
- host telemetry
- persistence

### Persistence model

Nucleus should use a hybrid persistence model.

Structured operational truth belongs in SQLite:

- sessions
- runtimes
- approvals
- jobs
- audit history
- health cache
- tool and secret metadata

Unstructured or large artifacts belong on the filesystem:

- transcripts
- attachments
- playbooks
- memory documents
- generated reports
- scratch artifacts

This is intentionally not a pure file-memory system.

The control plane needs deterministic, queryable, single-writer state for operational truth. SQLite is the right default for that in V1.

The file layer exists alongside SQLite, not instead of it.

Planned layout:

- SQLite for structured state
- filesystem directories for artifacts and memory
- optional indexing/search layer on top later

## Core Concepts

### Runtime

A managed execution environment or adapter target.

Examples:

- Claude adapter
- Codex adapter
- system automation adapter

### Session

A first-class conversation or work unit owned by the control plane.

### Provider Adapter

A boundary module that knows how to create, steer, inspect, and stop sessions against a provider or engine.

### Tool Registry

The canonical registry for MCP endpoints, local tools, and future system-action tools.

### Secret Reference

A durable identifier for a secret, not a copied token blob scattered across multiple configs.

### Playbook

A reusable instruction pack or execution profile that replaces ad hoc skill syncing.

### Policy

The rules that govern what can run, what needs approval, and what is blocked.

### Job

A scheduled, background, or event-triggered unit of work.

### Audit Event

An immutable record of significant state changes and actions.

## System Boundaries

### What the daemon should own

- local session orchestration
- runtime state
- tool and MCP registration
- approvals and policy
- background checks
- host metrics and process snapshots
- automation triggers
- durable logs and audit history

### What adapters can own temporarily

- materializing config for legacy runtimes
- bridging into external CLIs or APIs
- compatibility flows during migration

### What the UI should never own

- source-of-truth runtime state
- secret resolution
- orchestration logic
- inferred health truth based on local heuristics

## Health Model

The current Mission Control pain point is expensive page-triggered validation.

The new system should use a cached health model.

Health state should be computed in the background and stored durably with:

- status
- checked_at
- stale_at
- error details
- retry/backoff metadata
- source of truth for the status

Examples of health surfaces:

- provider reachability
- auth state
- tool registry load status
- session adapter availability
- scheduler health
- host resource state

Page loads should read cached truth immediately and refresh in the background when needed.

## Security Model

Security needs to be built in early because the system is intended to grow into host automation.

V1 should include:

- explicit capability scopes
- per-adapter secret scoping
- approval gates for risky actions
- audit logs for writes and privileged actions
- dry-run support where practical
- hard kill switch for active jobs and sessions

## Suggested Repository Shape

```text
nucleus/
  apps/
    web/                   # SvelteKit app, static build
  crates/
    daemon/                # Axum server, WS, scheduler, persistence
    protocol/              # shared wire/domain types
    core/                  # business rules and domain model
    adapters-claude/
    adapters-codex/
    adapters-system/
    storage/
    cli/                   # admin and local operator CLI
  clients/                 # future native clients
  docs/
    rfc/
    backlog/
  migrations/
  scripts/
```

## Proposed API Contract

The session-control transport pattern is the right default:

- HTTP owns initial and heavy reads
- WebSocket owns realtime updates and replay
- the daemon owns durable truth

Initial surface examples:

- `GET /api/sessions`
- `GET /api/sessions/:id`
- `GET /api/runtimes`
- `GET /api/tools`
- `GET /api/health`
- `GET /api/audit`
- `POST /api/sessions`
- `POST /api/sessions/:id/messages`
- `POST /api/tools/validate`
- `POST /api/jobs/:id/run`

Realtime examples:

- session updates
- job state changes
- adapter status changes
- health updates
- approval requests

## Migration Strategy

### Phase 0: Design and scaffolding

- lock V1 scope
- choose the product name
- create the daemon and UI skeleton
- define domain types and API boundaries

### Phase 1: session-control replacement plus core host dashboards

- build Claude and Codex adapters
- own session list/detail/send/interrupt
- own approvals and audit trail
- ship a fast dashboard with reviewable session surfaces and core host-status panels

Success criteria:

- daily local session work can move into the new system
- basic host-status monitoring can move from Mission Control into the new system

### Phase 2: Controlled expansion

- add tool registry and MCP management as first-class runtime concepts
- add cached health engine
- add background jobs and scheduler
- add compatibility adapters where still needed

Success criteria:

- current Mission Control is no longer needed for most day-to-day operations

### Phase 3: broader legacy-runtime absorption

- replace prompt/profile behavior with playbooks and policy
- add host automation capabilities
- absorb the useful parts of Hermes and ZeroClaw into native daemon modules
- retire compatibility layers as direct ownership grows

Success criteria:

- the new system becomes the primary local AI control plane

## Compatibility Rule

Current Mission Control remains the bridge for existing sync-heavy workflows.

The new system may generate compatibility config only when one of these is true:

- a legacy runtime is still needed for active work
- a migration path would otherwise stall
- the adapter cost is materially lower than a direct replacement in the current phase

Compatibility generation is an adapter concern, not a core product pillar.

## Immediate Build Plan

### Step 1

Create the new repo and bootstrap:

- monorepo layout
- Rust workspace
- Axum daemon
- SQLite
- SvelteKit static UI
- shared protocol crate

### Step 2

Build the minimal session model:

- sessions table
- runtime table
- audit table
- health table

### Step 3

Implement provider adapters for:

- Claude
- Codex

### Step 4

Build the first UI surfaces:

- dashboard
- sessions list
- session detail
- approvals
- runtime status
- host status overview for CPU, memory, and core system metrics

### Step 5

Add background health checks and cached status.

### Step 6

Use it as the daily local session controller before expanding scope.

## Naming Direction

The product name for this draft is `Nucleus`.

Reasoning:

- it matches the central-core and source-of-truth role of the system
- it fits the long-term direction better than narrower `ops` names
- it can scale from session control into automations, policy, and system operations

### Domain strategy

The system does not require an exact-match standalone domain on day one.

Current approach:

- brand name: `Nucleus`
- launch URL: Weblime subdomain such as `nucleus.weblime.com`

This avoids:

- distorted brand spellings like `Nucleuss`
- awkward fallback domains that weaken the product name
- delaying implementation while chasing perfect domain ownership

### UI naming

There is no separate name for the main dashboard surface.

The app itself is `Nucleus`, and the root/home surface of the product is just the main Nucleus app UI.

## Open Questions

- Is V1 local-only, or should basic remote endpoint support exist from the start?
- Does the first adapter talk to provider CLIs, provider APIs, or both?
- Should playbooks be markdown-first, structured-first, or hybrid?
- What is the minimum approval model needed before system actions are allowed?
- Which current session workflows are essential on day one and which can wait?

## Decision

Proceed with this as a new product, not as a direct in-place rewrite of the current Mission Control app.

Locked decisions for the current draft:

- product name: `Nucleus`
- domain strategy: launch from a Weblime subdomain first
- no separate dashboard brand

Current Mission Control remains in service as the legacy bridge until the new control plane proves it can replace the existing daily local session surface.
