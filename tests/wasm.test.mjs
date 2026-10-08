import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import test from 'node:test';
import { requests, nativeResponses } from './requests.mjs';

const require = createRequire(import.meta.url);
const { evaluate_json } = require('../pkg/node/uplc_wasm.js');

test('the packaged release Wasm API matches the native API on the same requests', async () => {
  const expected = await nativeResponses();
  // Reuse the same instance to exercise the exported API across multiple calls.
  for (let pass = 0; pass < 3; pass++) {
    requests.forEach((request, i) => assert.deepEqual(JSON.parse(evaluate_json(request)), expected[i]));
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
