'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { mkdirSync, mkdtempSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { test } = require('node:test');

const launcher = require.resolve('../../../../.github/scripts/staging_trial_cleanup.cjs');

test('reports unconfirmed when cleanup confirmation evidence is missing', (t) => {
  const bundle = mkdtempSync(join(tmpdir(), 'trial-cleanup-report-'));
  t.after(() => rmSync(bundle, { recursive: true }));
  mkdirSync(join(bundle, 'worker'));
  writeFileSync(join(bundle, 'worker/shim.mjs'), `
    export default {
      fetch(_request, env) {
        const { status, body } = JSON.parse(env.CLEANUP_SCOPE);
        return new Response(body, { status });
      },
    };
  `);
  writeFileSync(join(bundle, 'index.js'), 'export {};');
  // An empty WASM module supplies the launcher's required bundle entry.
  writeFileSync(join(bundle, 'index_bg.wasm'), Buffer.from('0061736d01000000', 'hex'));

  const cases = [
    ['verification pending', [bundle], { status: 503, body: 'unconfirmed' }, 'unconfirmed', 1],
    ['complete evidence', [bundle], { status: 200, body: 'confirmed' }, 'confirmed', 0],
    ['success without evidence', [bundle], { status: 200, body: null }, 'unconfirmed', 1],
    ['missing bundle path', [], null, 'unconfirmed', 1],
  ];
  for (const [label, args, receipt, verdict, exitCode] of cases) {
    const run = spawnSync(process.execPath, [launcher, ...args], {
      env: { CLEANUP_SCOPE: JSON.stringify(receipt) }, encoding: 'utf8', timeout: 15_000,
    });
    assert.ifError(run.error);
    assert.equal(run.status, exitCode, label);
    assert.equal(run.stdout, `Private staging fixture cleanup: ${verdict}\n`, label);
    assert.equal(run.stderr, '', label);
  }
});
