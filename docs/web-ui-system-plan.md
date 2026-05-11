# Web UI System Plan

## Status

Proposed

## Summary

Nucleus web will standardize on a shadcn-style component system as the default approach for product UI.

The current web app already uses Tailwind, Bits UI, Lucide icons, shared class utilities, and a small set of shared `ui` primitives. However, adoption is incomplete and inconsistent. Some surfaces use shared components, while others still rely on route-local markup and one-off styling.

This plan defines the target architecture and an incremental migration path so the UI can be improved in phases without requiring a full redesign or a large rewrite.

## Goals

- make shadcn-style shared components the default for web UI work
- reduce route-local, repeated, class-heavy UI implementations
- establish a clear layering model for primitives, app-level compositions, and feature-level components
- improve visual consistency across navigation, workspace surfaces, chat, and the rest of the app
- migrate incrementally by surface area so the work can be chunked into small PRs

## Non-goals

- a one-shot redesign of the entire web client
- pixel-perfect parity with any external shadcn example app
- replacing Nucleus product concepts with generic dashboard patterns
- blocking feature work until every existing screen is migrated

## Current state

Today, the web app has a partial shared UI foundation:

- Tailwind is established
- Bits UI is available as the primitive layer
- Lucide icons are used across the app
- `components/ui` exists, but only covers a narrow set of primitives today
- some product UI is still hand-rolled directly in route files or feature-local components

Current shared `ui` coverage is limited and includes components such as:

- `badge`
- `button`
- `card`
- `dropdown-menu`

This is a workable starting point, but it is not yet a coherent design system.

## Decision

Nucleus web will use the following defaults:

- **shadcn-style shared components are the default**
- **Bits UI is the primitive layer when primitives are needed**
- **Tailwind is the styling foundation**
- **route files should compose shared components instead of inventing repeated UI patterns inline**

In practice, that means new UI work should usually start by asking:

1. does a shared `ui` primitive already exist?
2. if not, should one be added?
3. if the pattern is product-specific, should it become an app-level shared component?

## Target architecture

### 1. Primitive layer

Location:

- `apps/web/src/lib/components/ui/*`

Purpose:

- generic, reusable, product-agnostic UI primitives
- shadcn-style wrappers around Bits UI and Tailwind where appropriate
- the default building blocks for buttons, inputs, menus, dialogs, tabs, and similar controls

Examples:

- `button`
- `input`
- `textarea`
- `select`
- `label`
- `tabs`
- `dialog`
- `sheet`
- `scroll-area`
- `separator`
- `tooltip`
- `badge`
- `card`
- `dropdown-menu`

### 2. App composition layer

Location:

- `apps/web/src/lib/components/app/*`

Purpose:

- product-level shared patterns built from `ui/*`
- shared layout and shell pieces that are specific to Nucleus web
- reusable compositions that appear across multiple features

Examples:

- `page-header`
- `section-card`
- `sidebar-nav`
- `sidebar-nav-item`
- `empty-state`
- `loading-state`
- `error-state`
- `status-badge`

### 3. Feature layer

Locations such as:

- `apps/web/src/lib/components/workspace/*`
- `apps/web/src/lib/components/session/*`
- `apps/web/src/lib/components/dashboard/*`

Purpose:

- feature-specific compositions only
- should consume `ui/*` and `app/*`
- should not redefine generic primitives that belong in the shared system

### 4. Route layer

Purpose:

- routes assemble screens from shared components
- route files should contain screen structure and data flow
- avoid repeated inline style patterns when the same pattern appears more than once

## Layering rules

1. prefer `components/ui/*` for generic primitives
2. prefer `components/app/*` for product-wide shared compositions
3. keep feature-specific components in feature folders only when they are not broadly reusable
4. if a route contains repeated class-heavy markup that appears in multiple places, extract it
5. use Bits UI through shared wrappers where practical instead of ad hoc route usage
6. keep one canonical class composition pattern through the existing `cn()` utility

## Migration strategy

Migration will happen by surface area, not by random file-by-file cleanup.

This keeps the work focused, reduces churn, and makes it easier to ship improvements in small, reviewable PRs.

## Phase 0: foundation and rules

Purpose:

- establish the conventions before migrating major surfaces

Scope:

- document this plan
- confirm naming and placement conventions for `ui`, `app`, and feature components
- identify the first missing primitives needed for sidebar and layout work
- define a lightweight review rule that new web UI should prefer shared components by default

Expected outputs:

- this plan in `docs/`
- agreed folder structure
- initial backlog of missing primitives and app-level compositions

Current status:

- complete enough to proceed with implementation phases
- the plan should remain temporary and be removed once the rollout is finished and replaced by a shorter durable architecture doc

## Phase 1: sidebar

Status: complete

Phase 1 review summary:

- shared app-level sidebar components are in place under `apps/web/src/lib/components/app/sidebar/*`
- the main app sidebar is composed from shared sidebar components rather than route-local markup
- core sidebar row states are standardized through the shared sidebar item patterns
- project and session navigation lists have shared app-level implementations
- `ui/scroll-area` is implemented and adopted for workspace shell navigation scrolling
- the remaining question about extracting another reusable sidebar section/group shell was reviewed and is not currently needed
- the current shared sidebar surfaces are consistent enough to treat Phase 1 as complete

Purpose:

- create the first high-impact shared navigation system

Why first:

- the sidebar is always visible
- it strongly shapes the feel of the product
- it gives the rest of the app a stable shell to build on

Scope:

- main app sidebar
- session and navigation item patterns used there
- consistent selected, hover, muted, and destructive states
- sidebar section labels and grouping
- sidebar scrolling behavior and spacing conventions
- badges or status indicators used inside navigation rows where needed

Shared components used for this phase:

- `app/sidebar/*` shared sidebar compositions are in place
- `ui/scroll-area` is implemented and adopted in the workspace shell
- no additional `app/sidebar-section` extraction is currently needed
- no separate shared status badge was required for Phase 1

Definition of done for Phase 1:

- [x] there is one canonical sidebar pattern for Nucleus web
- [x] sidebar rows are built from shared components rather than route-local styling
- [x] spacing, states, and icon treatment are consistent
- [x] the sidebar no longer feels like a one-off surface compared with the rest of the app

Phase 1 closeout notes:

- the app sidebar is already composed through shared `app/sidebar/*` components
- the workspace shell now uses shared `ui/scroll-area` primitives instead of one-off scroll handling
- the remaining Phase 1 question about extracting another sidebar section shell was reviewed and is not currently necessary
- Phase 2 can proceed from the current shared shell without additional Phase 1 structural work

## Phase 2: workspace

Purpose:

- migrate the workspace surfaces that are touched often onto the shared system

Why second:

- workspace is already showing drift from the intended system
- once the sidebar patterns exist, workspace navigation and section layout become easier to standardize

Scope:

- workspace sub-navigation
- workspace headers
- workspace section cards and panels
- settings-style sections and layout patterns
- repeated controls and feedback states used in workspace pages

Likely surfaces:

- profiles
- memory
- diagnostics
- settings and other workspace configuration pages

Shared components now in place for Phase 2:

- `app/workspace/workspace-page-header.svelte`
- `app/workspace/workspace-segmented-control.svelte`
- `app/workspace/workspace-stat-card.svelte`
- `app/workspace/workspace-empty-state.svelte`
- `app/workspace/workspace-meter-panel.svelte`
- `app/workspace/workspace-info-tile.svelte`
- `app/workspace/workspace-note-grid.svelte`

Status update:

- Phase 2 workspace extraction is now implemented across the current workspace surfaces.
- Diagnostics now uses shared page-header, segmented-control, stat-card, empty-state, and meter-panel building blocks.
- Workspace home and memory now use the shared workspace page-header pattern.
- Settings now uses shared workspace page-header, info-tile, and note-grid patterns for repeated layout blocks.

- `app/page-header`
- `app/section-card`
- `app/empty-state`
- `app/loading-state`
- `app/error-state`
- any missing form primitives required by workspace forms

Definition of done for Phase 2:

- the workspace surfaces touched in active development use shared app and `ui` components
- repeated workspace panels are no longer hand-rolled in multiple places
- workspace pages feel like part of the same product shell as the sidebar

## Phase 3: chat canvas

Status: complete enough to treat the shared session canvas as the canonical implementation

Phase 3 audit summary:

- the root route already renders a shared session surface through `apps/web/src/routes/+page.svelte`
- the primary chat/session experience is consolidated in `apps/web/src/lib/components/app/session/session-workspace.svelte`
- the session canvas already centralizes composer behavior, transcript rendering, activity state, approvals, job detail loading, and session-level settings and draft handling
- the product is no longer relying on a route-local one-off chat canvas for the main session experience
- the main gap found in the audit was documentation drift rather than missing chat-canvas migration work

Purpose:

- standardize the main daily-driver work surface after navigation and workspace foundations are in place

Why third:

- chat is highly visible and heavily used
- it benefits from shared spacing, surfaces, and feedback patterns
- earlier phases reduce the risk of reinventing shell and state patterns inside the chat area

Scope covered by the current implementation:

- message list chrome around content is handled inside the shared session workspace
- the composer shell, textarea sizing, image attachments, and action controls are centralized in the shared session workspace
- approval handling, worker and action activity summaries, and job detail loading are centralized there as well
- empty, loading, degraded, and reconnecting states are represented through shared session-level state handling rather than scattered route-local logic
- transcript anchoring and auto-scroll behavior are managed in one canonical session surface
- session-level actions and draft settings are part of the shared session workspace flow

Shared components and primitives confirmed during the audit:

- `app/session/session-workspace.svelte` is the canonical app-level chat/session composition
- Phase 2 shared workspace primitives remain in use where workspace-style layouts are needed outside the chat canvas
- the current session surface already uses shared UI primitives for common controls instead of a standalone route-local buildout

Definition of done for Phase 3:

- [x] the chat canvas uses a shared app-level session surface
- [x] composer and session-level controls are centralized rather than duplicated in routes
- [x] activity, approval, and feedback states are managed in the shared session canvas
- [x] route-local styling is reduced in the main session surface

Phase 3 closeout notes:

- the implementation is concentrated in a large `session-workspace.svelte` file, so future work should focus on refinement and extraction rather than treating Phase 3 as unstarted
- if maintainability starts to suffer, the next cleanup should split the session workspace into smaller shared subcomponents, but that is follow-up work rather than a blocker for Phase 3 completion

## Phase 4: rest of the web UI

Purpose:

- migrate the remaining product surfaces onto the same system

Scope:

- dashboard tables and status surfaces
- dialogs, menus, and one-off controls still using local patterns
- forms not already covered in workspace work
- auth, update, restart, and machine-operation flows
- remaining route-local cards, badges, and panels

Shared components likely needed:

- any remaining foundation primitives
- table-related patterns as needed
- additional app-level feedback or status compositions

Definition of done for Phase 4:

- the remaining web UI mostly composes shared `ui` and app-level components
- repeated route-local patterns have been removed or intentionally kept as exceptions
- the app presents one coherent visual language across its primary surfaces

## Priority component backlog

The following primitives should be considered first because they unlock the migration phases above.

### Foundation primitives

- `button` (expand variants only if needed)
- `input`
- `textarea`
- `select`
- `label`
- `separator`
- `scroll-area`
- `tabs`
- `tooltip`
- `dialog`
- `sheet` or drawer
- `badge`
- `card`
- `dropdown-menu`

Status from phases 1-3 audit:

- `scroll-area` is implemented and adopted
- sidebar shared app compositions are in place
- workspace shared app compositions are in place
- the main session/chat surface is already consolidated at the app level
- the remaining backlog should now be prioritized mainly against Phase 4 needs rather than earlier migration assumptions

### Form support

- form field wrapper patterns
- validation and help text presentation
- consistent field spacing and label usage

### App-level shared compositions

- `page-header`
- `section-card`
- `sidebar-nav`
- `sidebar-nav-item`
- `empty-state`
- `loading-state`
- `error-state`
- `status-badge`

## Rules for new UI work during migration

Before adding new route-local UI:

1. check whether the pattern already exists in `components/ui`
2. if the pattern is generic, add or extend a shared `ui` primitive
3. if the pattern is product-specific but broadly reusable, add it to `components/app`
4. only keep it local to a route or feature when there is a clear reason it should stay local

This plan should guide forward motion, not freeze development. New features can still ship during migration, but they should move the app toward the shared system rather than further away from it.

## Definition of done for the overall effort

This migration will be considered successful when:

- shadcn-style shared components are the default for new web UI work
- the sidebar, workspace, chat canvas, and remaining primary screens all rely on shared system components
- Bits UI usage is usually wrapped through `components/ui` rather than scattered ad hoc in routes
- repeated route-local card, nav, form, and panel patterns have been extracted
- spacing, states, and shell behavior feel consistent across the app

## Notes

This plan is intentionally incremental. It does not require a dedicated redesign sprint before useful improvements can land.

The order of work is deliberate:

1. sidebar
2. workspace
3. chat canvas
4. rest of the web UI

That sequence should let Nucleus improve visible product consistency quickly while keeping implementation risk manageable.
