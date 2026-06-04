import assert from 'node:assert/strict';
import { test } from 'node:test';

import { profileCheckResultSchema } from './schemas.ts';

const failureOutcomes = [
  'missing_api_key',
  'missing_base_url',
  'adapter_unavailable',
  'invalid_api_key',
  'model_not_found',
  'timeout',
  'empty_response',
  'malformed_response',
  'unreachable'
];

test('profile check schema parses a successful result', () => {
  const parsed = profileCheckResultSchema.parse({
    role: 'utility',
    outcome: 'ok',
    message: 'Model responded.',
    json_object: 'unknown',
    transport: 'non_streaming',
    action_contract: 'passed',
    latency_ms: 42
  });

  assert.equal(parsed.role, 'utility');
  assert.equal(parsed.outcome, 'ok');
  assert.equal(parsed.transport, 'non_streaming');
  assert.equal(parsed.action_contract, 'passed');
  assert.equal(parsed.latency_ms, 42);
});

test('profile check schema defaults unknown fingerprint fields', () => {
  const parsed = profileCheckResultSchema.parse({
    role: 'utility',
    outcome: 'ok',
    message: 'Model responded.'
  });

  assert.equal(parsed.json_object, 'unknown');
  assert.equal(parsed.transport, 'unknown');
  assert.equal(parsed.action_contract, 'unknown');
});

test('profile check schema parses classified failure results', () => {
  for (const outcome of failureOutcomes) {
    const parsed = profileCheckResultSchema.parse({
      role: 'main',
      outcome,
      message: `${outcome} message`,
      http_status: outcome === 'invalid_api_key' ? 401 : undefined
    });

    assert.equal(parsed.role, 'main');
    assert.equal(parsed.outcome, outcome);
  }
});

test('profile check schema rejects unknown outcomes', () => {
  assert.throws(() =>
    profileCheckResultSchema.parse({
      role: 'utility',
      outcome: 'surprise_success',
      message: 'Nope.'
    })
  );
});
