# Model-Agnostic Runtime Session Prompt

```text
Work in `/home/eba/tools/nucleus-pr27` only. Do not touch `/home/eba/tools/nucleus-eba`.

Goal:
Make Nucleus the source of truth for prompt assembly, project context, skills, MCP registry metadata, tool semantics, and runtime behavior. Providers should be thin protocol backends, not owners of context or tools.

Current implementation boundary:
- Fresh installs default to `local-openai`, an OpenAI-compatible route at `http://127.0.0.1:20128/v1`.
- Claude and Codex are planned protocol backends only. CLI model execution is disabled.
- `main` and `utility` are runtime/compiler roles. Text prompts use the daemon worker lane selected by role; image prompts use route resolution for the requested role.
- Nucleus skills are active prompt/include behavior through persisted manifests, deterministic activation, and workspace-relative include fragments.
- MCP servers are persisted and compiled as registry metadata only until a Nucleus-owned MCP execution loop lands.
- The daemon-owned worker tool loop is the executable tool path for supported local tools.

Implementation direction:
- Evolve the existing daemon prompt builder rather than replacing it.
- Keep include discovery as an input source into compiled prompt context.
- Treat skill `include_paths` as workspace-relative files; do not use them as arbitrary filesystem reads.
- Preserve provider-neutral prompt ownership in Nucleus.
- Do not use provider CLIs as the model execution path.
- Do not mark Claude/Codex runtimes ready until real protocol backends or loopback bridges exist.
```
