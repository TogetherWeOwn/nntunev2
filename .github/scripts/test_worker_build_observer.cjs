'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const observer = path.join(__dirname, 'worker_build_observer.cjs');

test('rejects a modified CLI before executing its code', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'worker-observer-'));
  try {
    const dist = path.join(dir, 'wrangler-dist');
    fs.mkdirSync(dist);
    const cli = path.join(dist, 'cli.js');
    const receipt = path.join(dir, 'receipt.json');
    const marker = path.join(dir, 'executed');
    fs.writeFileSync(cli, `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'bad');\n`);
    const result = spawnSync(process.execPath, ['--require', observer, cli], {
      env: { WORKER_ORACLE_CLI: cli, WORKER_ORACLE_OUTPUT: receipt },
      encoding: 'utf8', timeout: 10000,
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Wrangler CLI hash mismatch/);
    assert.equal(fs.existsSync(marker), false);
    assert.equal(fs.existsSync(receipt), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('fails when the build boundary is never reached', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'worker-observer-'));
  try {
    const dist = path.join(dir, 'wrangler-dist');
    fs.mkdirSync(dist);
    const cli = path.join(dist, 'cli.js');
    const receipt = path.join(dir, 'receipt.json');
    fs.writeFileSync(cli, '');
    const result = spawnSync(process.execPath, ['--require', observer, '-e', 'process.exit(0)'], {
      env: { WORKER_ORACLE_CLI: cli, WORKER_ORACLE_OUTPUT: receipt },
      encoding: 'utf8', timeout: 10000,
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Wrangler build observation missing/);
    assert.equal(fs.existsSync(receipt), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('rejects an observation before verifying the CLI even if caught', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'worker-observer-'));
  try {
    const dist = path.join(dir, 'wrangler-dist');
    fs.mkdirSync(dist);
    const cli = path.join(dist, 'cli.js');
    const receipt = path.join(dir, 'receipt.json');
    fs.writeFileSync(cli, '');
    const code = `try { globalThis.__workerBuildReceipt({ name: 'main', type: 'esm', content: 'x' }, [], []); } catch {} process.exit(0)`;
    const result = spawnSync(process.execPath, ['--require', observer, '-e', code], {
      env: { WORKER_ORACLE_CLI: cli, WORKER_ORACLE_OUTPUT: receipt },
      encoding: 'utf8', timeout: 10000,
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Wrangler build observation missing/);
    assert.equal(fs.existsSync(receipt), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('rejects an invalid target path', () => {
  const result = spawnSync(process.execPath, ['--require', observer, '-e', ''], {
    env: { WORKER_ORACLE_CLI: __filename, WORKER_ORACLE_OUTPUT: path.join(os.tmpdir(), 'receipt') },
    encoding: 'utf8', timeout: 10000,
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid oracle paths/);
});
