import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  createSessionRequestSchema,
  jobSummarySchema,
  sessionSummarySchema,
  updateSessionRequestSchema
} from './schemas.ts';

test('session request schemas accept canonical attachment modes', () => {
  for (const attachment_mode of ['new_worktree', 'project_root', 'scratch']) {
    assert.equal(createSessionRequestSchema.parse({ attachment_mode }).attachment_mode, attachment_mode);
    assert.equal(updateSessionRequestSchema.parse({ attachment_mode }).attachment_mode, attachment_mode);
  }
});

test('session summary keeps legacy workspace mode while reading attachment fields', () => {
  const parsed = sessionSummarySchema.parse({
    id: 'session-one',
    title: 'Session one',
    profile_id: '',
    profile_title: '',
    route_id: '',
    route_title: '',
    project_id: '',
    project_title: '',
    project_path: '',
    provider: 'codex',
    model: '',
    provider_base_url: '',
    provider_api_key: '',
    working_dir: '/tmp/project',
    working_dir_kind: 'managed_git_worktree',
    workspace_mode: 'isolated_worktree',
    attachment_mode: 'new_worktree',
    worktree_id: 'worktree-one',
    scope: 'workspace',
    project_count: 0,
    projects: [],
    state: 'active',
    provider_session_id: '',
    last_error: '',
    last_message_excerpt: '',
    turn_count: 0,
    created_at: 1,
    updated_at: 1
  });

  assert.equal(parsed.workspace_mode, 'isolated_worktree');
  assert.equal(parsed.attachment_mode, 'new_worktree');
  assert.equal(parsed.worktree_id, 'worktree-one');
});

test('session summary preserves missing attachment mode for legacy workspace fallback', () => {
  const parsed = sessionSummarySchema.parse({
    id: 'legacy-worktree-session',
    title: 'Legacy worktree session',
    profile_id: '',
    profile_title: '',
    route_id: '',
    route_title: '',
    project_id: 'nucleus',
    project_title: 'nucleus',
    project_path: '/tmp/nucleus',
    provider: 'codex',
    model: '',
    provider_base_url: '',
    provider_api_key: '',
    working_dir: '/tmp/nucleus-worktree',
    working_dir_kind: 'managed_git_worktree',
    workspace_mode: 'isolated_worktree',
    scope: 'project',
    project_count: 1,
    projects: [],
    state: 'active',
    provider_session_id: '',
    last_error: '',
    last_message_excerpt: '',
    turn_count: 0,
    created_at: 1,
    updated_at: 1
  });

  assert.equal(parsed.workspace_mode, 'isolated_worktree');
  assert.equal(parsed.attachment_mode, '');
});

test('job summary accepts merged publication status', () => {
  const parsed = jobSummarySchema.parse({
    id: 'job-merged-publication',
    session_id: 'session-one',
    parent_job_id: null,
    template_id: null,
    title: 'Merge PR',
    purpose: 'Merge the PR into dev',
    trigger_kind: 'session_prompt',
    state: 'completed',
    requested_by: 'user',
    prompt_excerpt: 'merge it into the dev branch',
    root_worker_id: null,
    visible_turn_id: null,
    result_summary: 'PR merged into dev.',
    last_error: '',
    publication_requested: true,
    publication_status: 'merged',
    pr_url: 'https://github.com/WebLime-agency/nucleus/pull/369',
    target_branch: 'dev',
    worker_count: 0,
    pending_approval_count: 0,
    artifact_count: 0,
    created_at: 1,
    updated_at: 1
  });

  assert.equal(parsed.publication_status, 'merged');
});
