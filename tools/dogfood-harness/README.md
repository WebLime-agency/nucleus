# Nucleus Dogfood Harness

This is the Phase 0 reusable dogfood/regression harness for epic #417. It drives a live Nucleus managed install through the delegation ladder and writes a compact JSON PASS/FAIL report for each rung.

The harness is operator tooling only. It does not change daemon behavior and is not wired into CI or `cargo test`.

## Usage

Default target is the DevProjects install at `http://127.0.0.1:5202`:

```bash
cargo run -p nucleus-dogfood-harness -- \
  --output tools/dogfood-harness/reports/latest.json \
  --allow-failures
```

Useful options:

```text
--base-url <url>          Target Nucleus URL. Env: NUCLEUS_DOGFOOD_BASE_URL
--auth-token <token>      Token override. Env: NUCLEUS_DOGFOOD_AUTH_TOKEN or NUCLEUS_AUTH_TOKEN
--auth-token-path <path>  Token file. Default: /home/eba/.nucleus-dev-projects/local-auth-token
--project <id|slug>       Workspace project. Default: nucleus
--rungs <list|all>        Comma-separated rung names or all. Default: all
--timeout-secs <seconds>  Per-rung wall-clock timeout. Default: 900
--output <path>           JSON report path. Default: tools/dogfood-harness/reports/latest.json
--allow-failures          Exit 0 even when one or more rungs FAIL; intended for capturing known-red baselines
```

The token is read into memory and never printed. Prefer the token file or environment variable over passing the token as a shell argument.

## API Contract

Endpoint usage was matched to `apps/web/src/lib/nucleus/client.ts`:

- `GET /api/health`
- `GET /api/workspace`
- `POST /api/sessions`
- `GET /api/sessions/{session_id}/jobs` only to discover the new root job id
- `POST /api/sessions/{session_id}/prompt`
- `GET /api/jobs/{job_id}` for root polling and child detail capture
- `POST /api/jobs/{job_id}/cancel`
- `GET /api/approvals`
- `POST /api/approvals/{approval_id}/approve`
- `POST /api/approvals/{approval_id}/deny`
- `DELETE /api/sessions/{session_id}` for cleanup

The report uses `crates/protocol` response types through `nucleus-protocol` rather than local copies of the web Zod schemas.

## Approval Policy

The policy engine is deny-by-default and only processes approvals whose `job_id` belongs to the current root job tree. Other pending approvals on the same install are ignored.

Approved when scoped inside the owning worker worktree:

- Read/inspect tools: `project.inspect`, `fs.read_text`, `fs.list`, `rg.search`, `git.status`, `git.diff`
- Bounded read/build/test style `command.run`
- Bounded `tests.run`
- Bounded `python.run` when the script does not clearly reference external paths or network/process operations
- Worktree-local file mutations: `fs.apply_patch`, `fs.write_text`, `fs.move`, `fs.mkdir`

Denied:

- `git.stage_patch`, `git push`, commits, tags, manual branch creation, PR creation/update, publication, release, and GitHub tools
- Commands that look network-mutating, install/publish oriented, or not clearly read/build/test scoped
- Any command or path whose `cwd` or target path resolves outside the worker worktree
- Any unknown tool or ambiguous approval

Cleanup always attempts to cancel the root job, deny remaining current-run approvals, reset/clean only the harness-created managed worktree for that session, and delete the session so the managed worktree is removed.

## Ladder Rungs

- `read_only`: delegate one main child to run exactly `{"command":"sh","args":["-lc","printf NUCLEUS_COMMAND_RUN_PROBE"],"cwd":".","timeout_secs":20}`. PASS requires a utility root, at least one main child, a child command exit 0, accepted/completed child status without validation-evidence blocking, no duplicate fanout beyond the configured threshold, and root convergence.
- `edit_and_test`: delegate one main child to add a tiny helper plus unit test in the child worktree and run a focused test. PASS requires an edit, validation, accepted child status, root convergence, and no fanout.
- `feature_161`: delegate issue #161 implementation. PASS requires bounded single-child convergence to implementation+validation evidence or a precise blocker.
- `debug`: delegate a bounded diagnose-and-fix task. PASS requires bounded children and convergence to completion or a precise blocker.

To add a rung, add a `Rung` entry in `src/main.rs` with a prompt, fanout limit, and acceptance mode. If the acceptance criteria are new, add a new `Acceptance` variant and keep the report shape unchanged.

## Report

The JSON report includes:

- `install { url, version, routes }`, with routes limited to the workspace default profile and without API keys
- per rung `name`, `sessionId`, `rootJobId`, `status`, `reasons`, `rootWorker`, `children`, `counts`, `approvals`, and `keyEvents`
- `overall { passed, failed, total }`

The harness also prints a short human summary table.
