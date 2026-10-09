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
function add(request, expected, scope = 'ABI', reasonPattern) {
  requests.push(typeof request === 'string' ? request : JSON.stringify(request));
  checks.push({ id: request.id || 'malformed request', expected, scope, reasonPattern });
}
for (const [path, scope] of [
  ['fixtures/milestone.jsonl', 'milestone'],
  ['fixtures/milestone-decoder.jsonl', 'independent decoder'],
  ['fixtures/builtins.jsonl', 'builtin semantic/cost goldens'],
  ['fixtures/builtins-candidate.jsonl', 'builtin official and wire policies'],
  ['fixtures/builtins-decoder.jsonl', 'builtin decoder goldens'],
]) {
  for (const fixture of readCases(path)) {
    const expected = { traces: [], ...fixture.expected };
    if (expected.kind === 'decode') expected.budget = null;
    add(makeRequest(fixture), expected, scope);
  }
}
for (const fixture of readCases('fixtures/builtins-unsupported.jsonl')) {
  // Scope assertions only: unsupported results must never be conformance passes.
  // The six still-deferred original fixtures are retained verbatim here. The
  // seventh, bare addInteger, is now an explicit strict builtin golden with its
  // original provenance retained in provenance.graduation.
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

// Raw boundary encodings below follow the pinned Flat bit grammar. Arithmetic
// expectations are JavaScript BigInt calculations, independent of the evaluator.
function naturalBits(value) {
  const bits = BigInt(value).toString(2);
  const groups = [];
  for (let end = bits.length; end > 0; end -= 7) {
    groups.push(bits.slice(Math.max(0, end - 7), end).padStart(7, '0'));
  }
  return groups.map((group, index) => (index + 1 < groups.length ? '1' : '0') + group).join('');
}
function integerBits(value) {
  return '0100100000' + naturalBits(value >= 0n ? 2n * value : -2n * value - 1n);
}
const builtinBits = tag => '0111' + tag.toString(2).padStart(7, '0');
const binaryBits = (tag, left, right) => '00110011' + builtinBits(tag) + left + right;
const integerTerm = value => ['constant', ['integer', value.toString()]];
const builtinSuccess = (value, cpu, mem) => ({ status: 'success', term: integerTerm(value),
  budget: { cpu: cpu.toString(), mem: mem.toString() }, traces: [] });
function customBuiltin(id, term, coefficients, budget, expected, reasonPattern) {
  const request = modified('abi/builtin/' + id);
  request.program.hex = flat(term);
  request.profile.cost_model.parameters.fill('0');
  for (const [index, coefficient] of Object.entries(coefficients)) {
    request.profile.cost_model.parameters[index] = coefficient.toString();
  }
  request.mode.budget = budget;
  rehash(request);
  add(request, expected, 'builtin ABI and portable limits', reasonPattern);
}
const maximumBudget = { cpu: '9223372036854775807', mem: '9223372036854775807' };
const twoWordAdd = binaryBits(0, integerBits(1n << 64n), integerBits(1n));
const twoWordSum = (1n << 64n) + 1n;
// max(argument words)=2: CPU=-3+2*2=1, memory=5-2*2=1.
for (const [name, cpu, mem] of [['exact', '1', '1'], ['cpu-short', '0', '1'], ['mem-short', '1', '0']]) {
  customBuiltin('signed/' + name, twoWordAdd, { 0: -3, 1: 2, 2: 5, 3: -2 }, { cpu, mem },
    name === 'exact' ? builtinSuccess(twoWordSum, 1, 1)
      : { status: 'failure', kind: 'budget_exhausted', budget: { cpu: '1', mem: '1' }, traces: [] });
}
customBuiltin('signed/wide-cancellation', twoWordAdd,
  { 0: -9223372036854775808n, 1: 9223372036854775807n, 2: 4, 3: -2 }, maximumBudget,
  builtinSuccess(twoWordSum, 9223372036854775806n, 0));
for (const [name, coefficients] of [
  ['negative-cpu', { 0: -5, 1: 2 }], ['negative-memory', { 2: 3, 3: -2 }],
]) {
  customBuiltin(name, twoWordAdd, coefficients, maximumBudget, { status: 'unsupported' }, /negative/);
}
for (const [name, coefficients] of [
  ['cpu-overflow', { 1: 9223372036854775807n }], ['memory-overflow', { 3: 9223372036854775807n }],
]) {
  customBuiltin(name, twoWordAdd, coefficients, maximumBudget,
    { status: 'failure', kind: 'budget_exhausted', budget: null, traces: [] });
}
customBuiltin('zero-model', twoWordAdd, {}, { cpu: '0', mem: '0' }, builtinSuccess(twoWordSum, 0, 0));

// Both inputs are inside semantics E's range. Their 3163-word product needs
// 10,004,569 portable work units, above the independent ten-million allowance.
const costlyInteger = integerBits(1n << 202368n);
customBuiltin('work-limit', binaryBits(2, costlyInteger, costlyInteger), {}, { cpu: '0', mem: '0' },
  { status: 'unsupported' }, /machine work bound/);

// One borrowed 25,001-byte integer feeds a balanced addition tree. All operands
// stay within E's range and total work stays below two million units. The 255
// generated results fit the 8 MiB payload cap; 511 results exceed it. This
// separates generated payload accounting from input/AST/output/work limits.
const variableOne = '0000' + naturalBits(1n);
const largeInteger = 1n << 200000n;
let sumTree = variableOne;
for (let depth = 1; depth <= 9; depth++) {
  sumTree = binaryBits(0, sumTree, sumTree);
  if (depth < 8) continue;
  const term = '00110010' + sumTree + integerBits(largeInteger);
  customBuiltin('generated-payload/' + (2 ** depth), term, {}, { cpu: '0', mem: '0' },
    depth === 8 ? builtinSuccess(largeInteger * 256n, 0, 0) : { status: 'unsupported' },
    depth === 9 ? /runtime constant payload exceeds 8388608 bytes/ : undefined);
}

export function assertExpectedOutcomes(responses) {
  assert.equal(responses.length, checks.length);
  responses.forEach((response, index) => {
    const { id, scope, expected, reasonPattern } = checks[index];
    assert.equal(response.schema_version, 1, `${scope}: ${id}: schema`);
    assert.equal(response.engine, 'uplc-core', `${scope}: ${id}: engine`);
    assert.match(response.revision, /^0\.1\.0\+git\.(?:[a-f0-9]{40}(?:[a-f0-9]{24})?(?:\.dirty)?|unknown)$/,
      `${scope}: ${id}: build revision`);
    for (const [field, value] of Object.entries(expected)) {
      assert.deepEqual(response.outcome[field], value, `${scope}: ${id}: ${field}`);
    }
    if (reasonPattern) assert.match(response.outcome.reason, reasonPattern, `${scope}: ${id}: resource`);
  });
}

export function coverageSummary() {
  const counts = new Map();
  for (const { scope } of checks) counts.set(scope, (counts.get(scope) || 0) + 1);
  return [...counts].map(([scope, count]) => `${count} ${scope}`).join(', ');
}

export async function nativeResponses() {
  const output = await new Promise((resolve, reject) => {
    const child = spawn(process.env.UPLC_NATIVE || root + 'target/debug/uplc-native', []);
    let stdout = '', stderr = '';
    // This deadline bounds the entire several-hundred-request batch, including
    // large independent integer goldens and the real generated-payload probe.
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error('native adapter timed out')); }, 30000);
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
