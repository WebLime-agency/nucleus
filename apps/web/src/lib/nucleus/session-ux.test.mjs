import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  activityFailureView,
  completionGateGroups,
  gateBadgeVariant,
  nextRuntimeTick,
  noActivity,
  childRouteLabel,
  reasoningActivityView,
  runtimeBadgeView,
  usageView
} from './session-ux.js';

function activeRecord(elapsedSeconds) {
  return {
    state: 'running',
    created_at: 1_000,
    last_resumed_at: null,
    updated_at: 1_000 + elapsedSeconds
  };
}

test('runtime badge snapshots at color thresholds', () => {
  assert.deepEqual(runtimeBadgeView(activeRecord(299), 1_299), {
    label: '4m 59s',
    tone: 'green',
    title: runtimeBadgeView(activeRecord(299), 1_299).title,
    elapsedSeconds: 299,
    start: 1_000
  });
  assert.equal(runtimeBadgeView(activeRecord(300), 1_300).tone, 'yellow');
  assert.equal(runtimeBadgeView(activeRecord(3_600), 4_600).tone, 'orange');
  assert.equal(runtimeBadgeView(activeRecord(14_400), 15_400).tone, 'red');
});

test('runtime badge advances on the 10s refresh tick', () => {
  const record = activeRecord(60);
  const first = runtimeBadgeView(record, 1_060);
  const second = runtimeBadgeView(record, nextRuntimeTick(1_060));

  assert.equal(first.label, '1m 0s');
  assert.equal(second.label, '1m 10s');
});

test('runtime badge starts from the later resume timestamp', () => {
  const badge = runtimeBadgeView(
    {
      state: 'running',
      created_at: 1_000,
      last_resumed_at: 2_000
    },
    2_030
  );

  assert.equal(badge.label, '30s');
  assert.equal(badge.start, 2_000);
});

test('reasoning activity flips stale after the 90s activity window', () => {
  const record = {
    state: 'running',
    last_reasoning: 'checking the next tool result',
    last_reasoning_at: 1_000
  };

  assert.equal(reasoningActivityView(record, 1_090).stale, false);
  assert.equal(reasoningActivityView(record, 1_091).stale, true);
  assert.equal(noActivity(record, 1_091), true);
});

test('usage display distinguishes missing prices from missing usage', () => {
  assert.equal(usageView({ token_usage_known: false }).label, 'tokens unknown');
  assert.equal(
    usageView({
      token_usage_known: true,
      prompt_tokens: 100,
      completion_tokens: 50,
      cached_tokens: 25,
      cost_usd_estimate: null
    }).label,
    '150 tokens · $— (no price)'
  );
});

test('child route label distinguishes explicit and inherited profiles', () => {
  assert.equal(childRouteLabel({ executor_route_title: 'Developer' }), 'Developer');
  assert.equal(childRouteLabel({ executor_route_id: 'reviewer' }), 'reviewer');
  assert.equal(childRouteLabel({}), '(inherits parent)');
});

test('completion gates are grouped by daemon-provided state', () => {
  const grouped = completionGateGroups({
    completion_gates: [
      { id: 'publication', state: 'blocked' },
      { id: 'validation', state: 'done' },
      { id: 'review', state: 'pending' }
    ]
  });

  assert.deepEqual(grouped.blocked.map((gate) => gate.id), ['publication']);
  assert.deepEqual(grouped.pending.map((gate) => gate.id), ['review']);
  assert.deepEqual(grouped.done.map((gate) => gate.id), ['validation']);
  assert.equal(gateBadgeVariant('blocked'), 'destructive');
  assert.equal(gateBadgeVariant('pending'), 'warning');
  assert.equal(gateBadgeVariant('done'), 'default');
});

test('activity failure view surfaces failed Python tool result', () => {
  const failure = activityFailureView({
    job: { title: 'Utility job', state: 'running', last_error: '' },
    tool_calls: [
      {
        tool_id: 'python.run',
        status: 'failed',
        error_detail: 'python executable not found',
        created_at: 1_000,
        completed_at: 1_005
      }
    ],
    command_sessions: []
  });

  assert.deepEqual(failure, {
    title: 'Python runtime failed',
    detail: 'python executable not found',
    state: 'failed'
  });
});

test('activity failure view prefers newer command failure detail', () => {
  const failure = activityFailureView({
    job: { title: 'Utility job', state: 'running', last_error: '' },
    tool_calls: [
      {
        tool_id: 'python.run',
        status: 'failed',
        error_detail: 'python executable not found',
        created_at: 1_000,
        completed_at: 1_005
      }
    ],
    command_sessions: [
      {
        title: 'Nucleus-owned Python runtime',
        command: '/usr/bin/python3',
        state: 'failed',
        last_error: 'command exited with status 1',
        created_at: 1_000,
        completed_at: 1_010
      }
    ]
  });

  assert.deepEqual(failure, {
    title: 'Nucleus-owned Python runtime',
    detail: 'command exited with status 1',
    state: 'failed'
  });
});

test('activity failure view does not let old command failures mask newer progress', () => {
  const failure = activityFailureView(
    {
      job: { title: 'Utility job', state: 'running', last_error: '' },
      tool_calls: [],
      command_sessions: [
        {
          title: 'Nucleus-owned command',
          command: 'npm test',
          state: 'failed',
          last_error: 'command exited with status 1',
          created_at: 1_000,
          completed_at: 1_005
        }
      ],
      workers: [{ state: 'running', last_error: '' }]
    },
    { status: 'running', created_at: 1_010 }
  );

  assert.equal(failure, null);
});

test('activity failure view keeps older failures that are still current worker errors', () => {
  const failure = activityFailureView(
    {
      job: { title: 'Utility job', state: 'running', last_error: '' },
      tool_calls: [
        {
          tool_id: 'python.run',
          status: 'failed',
          error_detail: 'python executable not found',
          created_at: 1_000,
          completed_at: 1_005
        }
      ],
      command_sessions: [],
      workers: [{ state: 'running', last_error: 'python.run failed: python executable not found' }]
    },
    { status: 'running', created_at: 1_010 }
  );

  assert.deepEqual(failure, {
    title: 'Python runtime failed',
    detail: 'python executable not found',
    state: 'failed'
  });
});

test('activity failure view does not let stale job errors mask newer progress', () => {
  const failure = activityFailureView(
    {
      job: {
        title: 'Utility job',
        state: 'running',
        last_error: 'python.run failed: python executable not found',
        created_at: 1_000,
        updated_at: 1_005
      },
      tool_calls: [],
      command_sessions: [],
      workers: [{ state: 'running', last_error: 'python.run failed: python executable not found' }]
    },
    { status: 'running', created_at: 1_010 }
  );

  assert.equal(failure, null);
});

test('activity failure view ignores historical failures after successful completion', () => {
  const failure = activityFailureView({
    job: { title: 'Utility job', state: 'completed', last_error: '', updated_at: 1_020 },
    tool_calls: [
      {
        tool_id: 'python.run',
        status: 'failed',
        error_detail: 'python executable not found',
        created_at: 1_000,
        completed_at: 1_005
      }
    ],
    command_sessions: []
  });

  assert.equal(failure, null);
});
