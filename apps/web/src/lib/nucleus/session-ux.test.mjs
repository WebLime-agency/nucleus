import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  nextRuntimeTick,
  noActivity,
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
