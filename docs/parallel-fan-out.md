# Parallel Fan-Out

`spawn_child_jobs` is the daemon-owned primitive for parent-orchestrator workflows. A parent Utility Worker can split work into child jobs, let each child run its own worker/checkpoint/audit trail, and then continue after every child reaches a terminal state.

## Recommended Pattern

Use this shape for reliable fan-out:

1. The parent plans the fan-out and emits `spawn_child_jobs` with one entry per independent unit of work.
2. Each child receives a precise prompt and a dedicated `working_dir` whenever it may write files.
3. The parent waits for all child jobs to terminate, then reads the child reports and decides the next action.
4. Canceling the parent cancels the whole child subtree.

Child `working_dir` values are first-class fields and are scoped to the parent worker's read roots. They do not provide filesystem locking. If two children receive the same checkout path and write overlapping files, normal filesystem and Git conflicts can still happen. For code-writing fan-out, create one worktree per child and pass that path in the child proposal.

## Worktree Helper

A parent or operator playbook can prepare child worktrees from one base ref before spawning children:

```bash
create_child_worktree() {
  repo_root="$1"
  worktree_root="$2"
  child_name="$3"
  base_ref="${4:-origin/dev}"

  mkdir -p "$worktree_root"
  git -C "$repo_root" fetch origin dev --prune
  git -C "$repo_root" worktree add \
    "$worktree_root/$child_name" \
    -b "work/$child_name" \
    "$base_ref"
}
```

Then pass each resulting absolute path as that child's `working_dir`.

## Semantics

Each child job gets its own `JobRecord`, `WorkerRecord`, checkpoint, child report artifact, and worker run budget. The child inherits the configured Utility Worker model from the parent session; there is no per-child model override.

`WaitUntil::ChildJobsCompleted` treats `completed`, `failed`, and `canceled` as terminal states. This means one failed child does not hang the parent forever. The parent sees each child's state, result summary, last error, recent events, worker notes, and `child-report` artifact path in the aggregated child result.

SQLite is configured with WAL mode and a busy timeout, so parallel children can write checkpoints, audit events, artifacts, and job events concurrently. If future load tests expose a hot table, optimize that table directly instead of adding a second persistence path.
