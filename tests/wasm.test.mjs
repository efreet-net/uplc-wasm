import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import test from 'node:test';
import { root, requests, nativeResponses, assertExpectedOutcomes } from './requests.mjs';

const require = createRequire(import.meta.url);
const { evaluate_json } = require('../pkg/node/uplc_wasm.js');

test('release Wasm and native APIs match independent goldens and retain unsupported scope', async () => {
  const expected = await nativeResponses();
  assertExpectedOutcomes(expected);
  // Reuse the same instance to exercise the exported API across multiple calls.
  for (let pass = 0; pass < 3; pass++) {
    const actual = requests.map(request => JSON.parse(evaluate_json(request)));
    assertExpectedOutcomes(actual);
    assert.deepEqual(actual, expected);
  }
});

test('out-of-range budgets are transport errors in the actual Wasm artifact', () => {
  for (const request of requests.filter(r => r.includes('abi/budget/9223372036854775808'))) {
    assert.equal(JSON.parse(evaluate_json(request)).outcome.status, 'infrastructure_error');
  }
});

test('counting requests with extra fields are transport errors', () => {
  const request = requests.find(r => r.includes('abi/counting/extra-fields'));
  assert.equal(JSON.parse(evaluate_json(request)).outcome.status, 'infrastructure_error');
});

test('integer values and consumed costs above JavaScript precision remain exact strings', () => {
  const integer = requests.find(request => request.includes('milestone/probe/integer-above-js-precision'));
  assert.deepEqual(JSON.parse(evaluate_json(integer)).outcome.term,
    ['constant', ['integer', '9007199254740993']]);
  const request = requests.find(request => request.includes('abi/precise-consumed-budget'));
  assert.deepEqual(JSON.parse(evaluate_json(request)).outcome.budget,
    { cpu: '9007199254741093', mem: '9007199254741093' });
});

test('unrepresentable consumed totals return budget exhaustion with a null budget', () => {
  const request = requests.find(request => request.includes('abi/overflow'));
  const outcome = JSON.parse(evaluate_json(request)).outcome;
  assert.equal(outcome.status, 'failure');
  assert.equal(outcome.kind, 'budget_exhausted');
  assert.equal(outcome.budget, null);
});

test('builtin custom models preserve signed cancellation and reject negative computed charges', () => {
  const outcome = id => JSON.parse(evaluate_json(requests.find(request =>
    request.includes('"id":"' + id + '"')))).outcome;
  assert.deepEqual(outcome('abi/builtin/signed/wide-cancellation').budget,
    { cpu: '9223372036854775806', mem: '0' });
  for (const dimension of ['cpu', 'memory']) {
    assert.equal(outcome('abi/builtin/negative-' + dimension).status, 'unsupported');
    const { status, kind, budget, traces } = outcome('abi/builtin/' + dimension + '-overflow');
    assert.deepEqual({ status, kind, budget, traces },
      { status: 'failure', kind: 'budget_exhausted', budget: null, traces: [] });
  }
});

test('zero-cost builtins retain portable work and generated-payload bounds', () => {
  const outcome = id => JSON.parse(evaluate_json(requests.find(request => request.includes(id)))).outcome;
  assert.equal(outcome('abi/builtin/generated-payload/256').status, 'success');
  assert.match(outcome('abi/builtin/generated-payload/512').reason, /runtime constant payload/);
  assert.match(outcome('abi/builtin/work-limit').reason, /machine work bound/);
});

test('native and release Wasm accept the exact UTF-8 body limit and reject one byte more', () => {
  const limit = 8 * 1024 * 1024;
  const request = JSON.parse(requests[0]);
  request.id = 'abi/transport-size';
  request.profile.id = 'multibyte-λ';
  let body = JSON.stringify(request);
  body += ' '.repeat(limit - Buffer.byteLength(body, 'utf8'));
  assert.equal(Buffer.byteLength(body, 'utf8'), limit);
  assert.equal(body.length, limit - 1);
  const wasm = JSON.parse(evaluate_json(body));
  assert.deepEqual(wasm.outcome, {
    status: 'success', term: ['constant', ['integer', '0']],
    budget: { cpu: '16100', mem: '200' }, traces: [],
  });
  const native = input => spawnSync(process.env.UPLC_NATIVE || root + 'target/debug/uplc-native', [], {
    input, encoding: 'utf8', timeout: 10000, maxBuffer: 16 * 1024 * 1024,
  });
  for (const framing of ['\n', '']) {
    // LF framing is separate from the body; an EOF-terminated final body is
    // accepted too. Both responses must match the actual release Wasm export.
    const result = native(body + framing);
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    assert.ok(result.stdout.endsWith('\n'));
    assert.deepEqual(JSON.parse(result.stdout), wasm);
  }
  body += ' ';
  assert.equal(body.length, limit); // Unicode character count is insufficient.
  assert.equal(Buffer.byteLength(body, 'utf8'), limit + 1);
  assert.equal(JSON.parse(evaluate_json(body)).outcome.status, 'infrastructure_error');
  // An overlong native frame terminates without draining or responding. This
  // deliberate framing policy differs from a complete-body Wasm error envelope.
  const oversized = native(body);
  assert.ifError(oversized.error);
  assert.equal(oversized.status, 1);
  assert.equal(oversized.stdout, '');
  assert.match(oversized.stderr, /JSONL request too large/);
});
