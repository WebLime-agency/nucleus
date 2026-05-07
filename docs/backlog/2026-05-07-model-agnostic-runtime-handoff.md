# Model-Agnostic Runtime Handoff

Read first:

- [RFC 0002](../rfc/0002-model-agnostic-prompt-runtime.md)
- [Session prompt](./2026-05-07-model-agnostic-runtime-session-prompt.md)
- `crates/daemon/src/main.rs`
- `crates/daemon/src/runtime.rs`
- `crates/daemon/src/agent.rs`
- `crates/core/src/lib.rs`
- `crates/protocol/src/lib.rs`
- `crates/storage/src/lib.rs`
- `apps/web/src/lib/nucleus/schemas.ts`
- `apps/web/src/lib/nucleus/client.ts`

Current patch status:

- Runtime execution is protocol-only for the supported path. OpenAI-compatible HTTP is ready; Claude/Codex are planned and reject execution instead of shelling out.
- Route targets carry `base_url` and `api_key`, and those fields are preserved through session creation, prompt routing, reroute, and execution-session construction.
- Fresh install defaults and seeded workspace profiles use `route:local-openai`.
- `main` and `utility` role selection is real for daemon workers and prompt target resolution.
- Skills and MCPs have minimal daemon/storage/client APIs. Skills participate in prompt/include behavior through enabled manifests and workspace-relative include paths. MCPs are registry metadata only until the execution loop exists.

Remaining follow-up:

- Add native provider HTTP backends or loopback bridges for Claude/Codex if those providers should become executable.
- Implement MCP server introspection and execution through the Nucleus tool loop before surfacing MCP tools as executable.
- Move more compiler orchestration out of `crates/daemon/src/main.rs` once the protocol surface stabilizes.
