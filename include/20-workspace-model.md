# Workspace, Project, Session

Nucleus uses a three-level model:

- workspace: global root, settings, router profiles, discovered projects
- project: local working context under the workspace
- session: unit of AI work that can be ad hoc, single-project, or multi-project

Working directory behavior should follow the workspace and active-project model, not arbitrary per-session free text.

The workspace owns the default Utility Worker run budget: maximum steps, maximum actions, and wall-clock time for a turn. Sessions inherit that default unless the user selects a session-level preset such as Focused, Extended, Marathon, or Unbounded.

Run budgets are guardrails, not task semantics. When a turn reaches a budget, Nucleus should produce a visible checkpoint that can be continued instead of failing as if the task broke.
