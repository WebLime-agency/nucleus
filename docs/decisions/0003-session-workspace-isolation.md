# 0003 Session Workspace Isolation

Status: accepted

## Context

Nucleus sessions used to run directly in a project's physical checkout by default. Multiple sessions attached to the same project could therefore share one git working tree. Branch changes, uncommitted edits, dirty state, and dev servers could cross session boundaries and make one work track accidentally operate on another work track's files.

## Decision

Nucleus sessions have an explicit workspace mode:

- `isolated_worktree` creates a Nucleus-managed git worktree for the session and is the preferred/default mode for code-writing project sessions.
- `shared_project_root` keeps the legacy behavior of running in the project root, but Nucleus records git state and warns when another active session shares the checkout or risky git commands may affect other sessions.
- `scratch_only` uses the Nucleus scratch directory and does not mutate the project checkout.

For git-backed projects, isolated sessions create worktrees under the Nucleus state directory at `worktrees/<project-id>/<session-id>/` and create a session branch such as `work/<project-slug>/<session-short-id>`. Session records persist the source project path, git root, worktree path, branch, base ref, HEAD, dirty state, untracked count, remote tracking branch, and workspace warnings.

Long-running command sessions record the owning session, project, worktree path, branch, and detected port so clients can distinguish which session/worktree owns a dev server.

## Consequences

- Two code-writing sessions for the same project no longer silently share branch or dirty state by default.
- Shared checkout mode remains available for compatibility, but warnings are visible in session/job metadata.
- Clean managed worktrees can be removed with session deletion. Dirty managed worktrees are refused so work is not lost accidentally.
- Existing sessions migrate with nullable/default workspace metadata and remain loadable.
