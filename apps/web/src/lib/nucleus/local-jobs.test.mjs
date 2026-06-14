import assert from 'node:assert/strict';
import test from 'node:test';

import { localJobBadgeVariant, localJobCanToggle, localJobLastRunFailed } from './local-jobs.ts';

function job(result) {
  return {
    unit: 'placeholder-cleanup.timer',
    title: 'Placeholder Cleanup',
    backend: 'systemd-user',
    enabled: true,
    unit_file_state: 'enabled',
    manageable: true,
    active_state: 'active',
    schedule: {
      next_elapse_at: null,
      interval_hint: null,
      raw: ''
    },
    last_fired_at: null,
    last_exit: {
      code: null,
      result,
      at: null
    },
    triggered_unit: 'placeholder-cleanup.service'
  };
}

test('local job result classification marks non-success results failed', () => {
  for (const result of ['exit-code', 'timeout']) {
    assert.equal(localJobLastRunFailed(job(result)), true);
    assert.equal(localJobBadgeVariant(job(result)), 'destructive');
  }
});

test('local job result classification keeps success and never-run non-failed', () => {
  assert.equal(localJobLastRunFailed(job('success')), false);
  assert.equal(localJobBadgeVariant(job('success')), 'default');
  assert.equal(localJobLastRunFailed(job('unknown')), false);
  assert.equal(localJobBadgeVariant(job('unknown')), 'default');
  assert.equal(localJobLastRunFailed(job('')), false);
  assert.equal(localJobBadgeVariant(job('')), 'default');
});

test('local job toggle availability follows daemon manageability', () => {
  assert.equal(localJobCanToggle(job('success')), true);
  assert.equal(localJobCanToggle({ ...job('success'), unit_file_state: 'static', manageable: false }), false);
});
