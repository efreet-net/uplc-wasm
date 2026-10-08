import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const root = fileURLToPath(new URL('../', import.meta.url));
const readCases = path => readFileSync(root + path, 'utf8').trim().split('\n').map(JSON.parse);
export const requests = [];
const checks = [];
const makeRequest = ({ id, program, mode, profile: path }) => {
  const { id: profileId, language, protocol_major, cost_model } = JSON.parse(readFileSync(root + path, 'utf8'));
  return { schema_version: 1, id, program, mode,
    profile: { id: profileId, language, protocol_major, cost_model } };
};
function add(request, expected, scope = 'ABI') {
  requests.push(typeof request === 'string' ? request : JSON.stringify(request));
  checks.push({ id: request.id || 'malformed request', expected, scope });
}
for (const [path, scope] of [
  ['fixtures/milestone.jsonl', 'milestone'],
  ['fixtures/milestone-decoder.jsonl', 'independent decoder'],
]) {
  for (const fixture of readCases(path)) {
    const expected = { traces: [], ...fixture.expected };
    if (expected.kind === 'decode') expected.budget = null;
    add(makeRequest(fixture), expected, scope);
  }
}
for (const fixture of readCases('fixtures/milestone-unsupported.jsonl')) {
  // Scope assertions only: unsupported results must never be conformance passes.
  add(makeRequest(fixture), { status: 'unsupported' }, 'unsupported scope');
}
for (const fixture of readCases('fixtures/smoke.jsonl')) {
  add(makeRequest(fixture), fixture.program.format === 'flat' ? fixture.expected : { status: 'unsupported' }, 'legacy smoke');
}
const baseline = JSON.parse(requests[0]);
const baselineExpected = checks[0].expected;
const modified = id => ({ ...structuredClone(baseline), id });
for (const cpu of ['9007199254740993', '9223372036854775807', '9223372036854775808', '+1']) {
  const request = modified('abi/budget/' + cpu);
  request.mode.budget.cpu = cpu;
  add(request, ['9007199254740993', '9223372036854775807'].includes(cpu)
    ? baselineExpected : { status: 'infrastructure_error' });
}
add('{}', { status: 'infrastructure_error' });
add('{broken json', { status: 'infrastructure_error' });
const invalidMode = modified('abi/counting/extra-fields');
invalidMode.mode = { kind: 'counting', budget: { cpu: '1', mem: '1' } };
add(invalidMode, { status: 'infrastructure_error' });

function rehash(request) {
  request.profile.cost_model.sha256 = createHash('sha256')
    .update('[' + request.profile.cost_model.parameters.join(',') + ']').digest('hex');
}
const label = modified('abi/profile-label');
label.profile.id = 'the profile ID does not select coefficients';
add(label, baselineExpected);
const precise = modified('abi/precise-consumed-budget');
precise.profile.cost_model.parameters[21] = '9007199254740993';
precise.profile.cost_model.parameters[22] = '9007199254740993';
precise.mode.budget = { cpu: '9223372036854775807', mem: '9223372036854775807' };
rehash(precise);
// Startup (100,100) plus one constant under this explicit custom model.
add(precise, { ...baselineExpected, budget: { cpu: '9007199254741093', mem: '9007199254741093' } });
const overflow = structuredClone(precise);
overflow.id = 'abi/overflow';
overflow.profile.cost_model.parameters[21] = '9223372036854775807';
rehash(overflow);
add(overflow, { status: 'failure', kind: 'budget_exhausted', budget: null, traces: [] });
const wrongHash = modified('abi/invalid-hash');
wrongHash.profile.cost_model.parameters[21] = '1';
add(wrongHash, { status: 'infrastructure_error' });
const wrongLength = modified('abi/invalid-model-length');
wrongLength.profile.cost_model.parameters.pop();
rehash(wrongLength);
add(wrongLength, { status: 'infrastructure_error' });
const negative = modified('abi/negative-machine-cost');
negative.profile.cost_model.parameters[21] = '-1';
rehash(negative);
add(negative, { status: 'unsupported' });
const historical = modified('abi/historical-profile');
historical.profile.protocol_major = 10;
add(historical, { status: 'unsupported' });
const zero = modified('abi/zero-model-zero-budget');
for (const [start, end] of [[17, 32], [193, 196]]) {
  for (let index = start; index <= end; index++) zero.profile.cost_model.parameters[index] = '0';
}
zero.mode.budget = { cpu: '0', mem: '0' };
rehash(zero);
add(zero, { ...baselineExpected, budget: { cpu: '0', mem: '0' } });

// Independent boundary encodings from the Flat spec: 4-bit term tags followed
// by exact 0*1 filler. These probe portable limits, not additional corpus goldens.
function flat(termBits) {
  const padded = termBits + '0'.repeat(7 - termBits.length % 8) + '1';
  return '010000' + padded.match(/.{8}/g).map(bits => parseInt(bits, 2).toString(16).padStart(2, '0')).join('');
}
for (const [depth, supported] of [[128, true], [129, false]]) {
  const request = modified('abi/output-depth/' + depth);
  request.program.hex = flat('0010'.repeat(depth) + '0110');
  let term = ['error'];
  for (let index = 0; index < depth; index++) term = ['lambda', term];
  add(request, supported ? { ...baselineExpected, term } : { status: 'unsupported' });
}
const deep = modified('abi/ast-depth/512');
deep.program.hex = flat('01010001'.repeat(256) + '0100100110');
// 256 force/delay pairs and a unit: 513 machine events plus startup.
add(deep, { status: 'success', term: ['constant', ['unit']], budget: { cpu: '8208100', mem: '51400' }, traces: [] });
const tooDeep = modified('abi/ast-depth/513');
tooDeep.program.hex = flat('01010001'.repeat(256) + '01010100100110');
add(tooDeep, { status: 'unsupported' });

export function assertExpectedOutcomes(responses) {
  assert.equal(responses.length, checks.length);
  responses.forEach((response, index) => {
    const { id, scope, expected } = checks[index];
    assert.equal(response.schema_version, 1, `${scope}: ${id}: schema`);
    assert.equal(response.engine, 'uplc-core', `${scope}: ${id}: engine`);
    assert.ok(response.revision, `${scope}: ${id}: revision`);
    for (const [field, value] of Object.entries(expected)) {
      assert.deepEqual(response.outcome[field], value, `${scope}: ${id}: ${field}`);
    }
  });
}

export async function nativeResponses() {
  const output = await new Promise((resolve, reject) => {
    const child = spawn(process.env.UPLC_NATIVE || root + 'target/debug/uplc-native', []);
    let stdout = '', stderr = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error('native adapter timed out')); }, 10000);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', data => {
      stdout += data;
      if (stdout.length > 16 * 1024 * 1024) { child.kill('SIGKILL'); reject(new Error('response too large')); }
    });
    child.stderr.on('data', data => { stderr += data; });
    child.on('error', error => { clearTimeout(timer); reject(error); });
    child.stdin.on('error', error => { clearTimeout(timer); reject(error); });
    child.on('close', code => {
      clearTimeout(timer);
      if (code !== 0) reject(new Error(`native adapter exited ${code}: ${stderr}`));
      else resolve(stdout);
    });
    child.stdin.end(requests.join('\n') + '\n');
  });
  const responses = output.trim().split('\n').map(JSON.parse);
  if (responses.length !== requests.length) throw new Error('native response count mismatch');
  return responses;
}
