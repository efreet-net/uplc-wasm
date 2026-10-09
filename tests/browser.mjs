import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { chromium, firefox } from 'playwright';
import { root, requests, nativeResponses, assertExpectedOutcomes, coverageSummary } from './requests.mjs';

const [moduleSource, moduleBytes] = await Promise.all([
  readFile(root + 'pkg/web/uplc_wasm.js'), readFile(root + 'pkg/web/uplc_wasm_bg.wasm'),
]);
let browser;
try {
  const name = process.env.BROWSER || 'chromium';
  if (!['chromium', 'firefox'].includes(name)) throw new Error('BROWSER must be chromium or firefox');
  browser = await ({ chromium, firefox }[name]).launch({
    headless: true, executablePath: process.env.BROWSER_EXECUTABLE,
  });
  const page = await browser.newPage();
  const actual = await page.evaluate(async ({ source, bytes, inputs }) => {
    const wasm = await import('data:text/javascript;base64,' + source);
    await wasm.default({ module_or_path: new Uint8Array(bytes) });
    return inputs.map(input => JSON.parse(wasm.evaluate_json(input)));
  }, { source: moduleSource.toString('base64'), bytes: [...moduleBytes], inputs: requests });
  const native = await nativeResponses();
  assertExpectedOutcomes(native);
  assertExpectedOutcomes(actual);
  assert.deepEqual(actual, native);
  console.log(`${actual.length} native/${name} API checks passed: ${coverageSummary()}`);
} finally {
  await browser?.close();
}
