import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';

const require = createRequire(import.meta.url);
const { evaluate_json } = require('../pkg/node/uplc_wasm.js');

for await (const line of createInterface({ input: process.stdin, crlfDelay: Infinity })) {
  try {
    process.stdout.write(evaluate_json(line) + '\n');
  } catch (error) {
    let id = '';
    try { id = JSON.parse(line).id; } catch { /* malformed transport */ }
    process.stdout.write(JSON.stringify({ schema_version: 1, id, engine: 'uplc-wasm',
      revision: '0.1.0', outcome: { status: 'infrastructure_error', diagnostic: String(error) } }) + '\n');
  }
}
