import { readFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const root = fileURLToPath(new URL('../', import.meta.url));
const fixtures = readFileSync(new URL('../fixtures/smoke.jsonl', import.meta.url), 'utf8').trim().split('\n').map(JSON.parse);
export const requests = fixtures.map(({ id, program, mode, profile: path }) => {
  const { id: profileId, language, protocol_major, cost_model } = JSON.parse(readFileSync(root + path, 'utf8'));
  return JSON.stringify({ schema_version: 1, id, program, mode,
    profile: { id: profileId, language, protocol_major, cost_model } });
});
for (const cpu of ['9007199254740993', '9223372036854775807', '9223372036854775808', '+1']) {
  const request = JSON.parse(requests[0]);
  request.id = 'abi/budget/' + cpu;
  request.mode.budget.cpu = cpu;
  requests.push(JSON.stringify(request));
}
requests.push('{}', '{broken json');

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
