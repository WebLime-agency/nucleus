import assert from 'node:assert/strict';
import test from 'node:test';

import {
  localJobBadgeVariant,
  localJobCanRemoveLiteralAllowlistEntry,
  localJobCanRun,
  localJobCanToggle,
  localJobHasLiteralAllowlistEntry,
  localJobHasNonLiteralAllowlistMatch,
  localJobLastRunFailed,
  localJobMatchesAllowlistGlob
} from './local-jobs.ts';

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

test('local job run availability requires a triggered service', () => {
  assert.equal(localJobCanRun(job('success')), true);
  assert.equal(localJobCanRun({ ...job('success'), triggered_unit: 'placeholder.target' }), false);
  assert.equal(localJobCanRun({ ...job('success'), triggered_unit: '' }), false);
  assert.equal(localJobCanRun({ ...job('success'), triggered_unit: '   ' }), false);
});

test('local job allowlist helpers distinguish exact entries from broader globs', () => {
  const summary = job('success');

  assert.equal(localJobMatchesAllowlistGlob('placeholder-*.timer', summary.unit), true);
  assert.equal(localJobMatchesAllowlistGlob('placeholder-cleanu?.timer', summary.unit), true);
  assert.equal(localJobMatchesAllowlistGlob('other-*.timer', summary.unit), false);
  assert.equal(localJobHasLiteralAllowlistEntry(summary, [summary.unit, 'placeholder-*.timer']), true);
  assert.equal(localJobHasNonLiteralAllowlistMatch(summary, [summary.unit, 'placeholder-*.timer']), true);
  assert.equal(localJobCanRemoveLiteralAllowlistEntry(summary, [summary.unit]), true);
  assert.equal(localJobCanRemoveLiteralAllowlistEntry(summary, [summary.unit, 'placeholder-*.timer']), false);
});
