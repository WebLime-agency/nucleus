# Workspace, Project, Session

Nucleus uses a three-level model:

- workspace: global root, settings, router profiles, discovered projects
- project: local working context under the workspace
- session: unit of AI work that can be ad hoc, single-project, or multi-project

Working directory behavior should follow the workspace and active-project model, not arbitrary per-session free text.
