# RFC 0002: Model-Agnostic Prompt Runtime

## Status

Draft

## Problem

Nucleus must own prompt assembly, project context, skills, MCP registry state, and tool semantics. Provider choice should only affect protocol lowering and transport.

The legacy failure mode was that seeded defaults and runtime probing still treated Claude/Codex CLIs as usable model runtimes while the product direction requires thin protocol backends and Nucleus-owned context.

## Decision

Nucleus uses a provider-neutral compiled-turn model and a protocol-only execution boundary.

The compiled turn is built from:

- platform runtime contract
- workspace/profile/session state
- compiler role: `main` or `utility`
- attached projects and include files
- active skills and workspace-relative skill include fragments
- MCP registry metadata
- conversation history
- current user turn
- capability flags

Providers do not own prompt context, skills, MCPs, or tool semantics.

## Current Implementation Boundary

- OpenAI-compatible HTTP is the supported executable model runtime.
- Fresh installs seed `local-openai` at `http://127.0.0.1:20128/v1` for both `main` and `utility`.
- Claude and Codex are planned protocol backends. They remain visible as future routes but are not executable and do not shell out to CLIs.
- Route targets persist `base_url` and `api_key` so OpenAI-compatible transport/auth survives route selection and reroute.
- Skills are Nucleus-owned prompt/include behavior. Enabled `always` skills activate deterministically; enabled `auto` skills activate when a configured trigger appears in the user turn. Skill include paths are resolved under the workspace root.
- MCP servers are Nucleus-owned registry metadata only. They are not advertised as executable tools until the Nucleus MCP execution loop exists.
- The daemon-owned worker tool loop is the executable local tool path.
- Worker model responses are parsed through a canonical Nucleus action boundary. Syntax failures, valid JSON with the wrong action shape, and unknown action names are distinct failure classes. Provider-shaped compatibility parsing is a bounded adapter path, not an open-ended source of new action semantics.
- Worker step, action, and wall-clock budgets are safety rails, not task-length semantics. Workspace Settings owns the granular default run budget for new turns. Each session can inherit the workspace default or choose a preset such as focused, extended, marathon, or unbounded from the composer. Budget exhaustion should produce a visible checkpoint that can be continued instead of a dead-end failure.

## Non-Goals

- Provider-managed project memory.
- Provider-managed skill or MCP configuration.
- CLI-based model execution as a fallback.
- Claiming MCP tool execution before Nucleus owns the execution loop.

## Follow-Up

- Add real protocol or loopback backends for additional providers.
- Implement MCP introspection and execution under the same tool approval and result contract as existing daemon tools.
- Continue moving compiler implementation from daemon orchestration into reusable core modules where practical.
