import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { finalSmokeFixture } from './fixtures/production-worker-transport.mjs';
import { seedMaintenance } from './production-maintenance/seed.mjs';
import worker from './production-maintenance/worker.mjs';

for (const path of ['/', '/api/build', '/api/health', '/_app/immutable/app.js']) {
  test(`maintenance fails closed on ${path}`, async () => {
    const result = await worker.fetch(new Request(`https://example.invalid${path}`));
    assert.equal(result.status, 503);
    assert.equal(result.headers.get('cache-control'), 'no-store');
    assert.equal(result.headers.get('content-type'), 'text/plain; charset=utf-8');
    assert.equal(result.headers.get('x-robots-tag'), 'noindex');
    assert.equal(await result.text(), 'NNTune is temporarily unavailable.\n');
  });
}

test('maintenance dry-run config has no routes, bindings or public preview', () => {
  const config = readFileSync(new URL('./production-maintenance/wrangler.toml', import.meta.url), 'utf8');
  const settings = config.split('\n').filter((line) => line && !line.startsWith('#'));
  assert.deepEqual(settings, ['name = "nntune-web"', 'main = "worker.mjs"',
    'compatibility_date = "2026-10-01"', 'workers_dev = false', 'preview_urls = false']);
});

const version = '12345678-1234-1234-1234-123456789abc';
const workflow = readFileSync(new URL('../.github/workflows/deploy-production.yml', import.meta.url), 'utf8');
const sha = 'a'.repeat(40);
const workerId = 'b'.repeat(32);
const checkedAt = '2026-10-01T12:00:00Z';
const now = () => Date.parse(checkedAt) + 60_000;
const routeProof = JSON.stringify({ worker_id: workerId, sha, checked_at: checkedAt, route_count: 0 });

test('only exact-main bootstrap uses the protected production job after staging admission', (t) => {
  const preflight = workflow.match(/id: target\n        env:\n((?: {10}[^\n]*\n)+)        run: \|\n((?: {10}[^\n]*\n)+)/);
  assert.ok(preflight);
  assert.match(preflight[1], /^ {10}DEPLOY_MODE: \$\{\{ inputs\.mode \}\}$/m);
  assert.doesNotMatch(preflight[1], /secrets\./);
  const staging = workflow.split('\n  staging-evidence:\n')[1].split('\n  deploy-production:\n')[0];
  assert.match(staging, /needs: \[preflight\][\s\S]*?environment:\n      name: staging\n      deployment: false/);
  const guard = staging.match(/UNPROTECTED_CF_CONFIG: \$\{\{ secrets\.PRODUCTION_CLOUDFLARE_API_TOKEN != '' \|\| secrets\.PRODUCTION_CLOUDFLARE_ACCOUNT_ID != '' \}\}\n        run: \|\n([\s\S]*?)          python3 -/);
  assert.ok(guard);
  for (const value of ['false', 'true', '']) {
    assert.equal(spawnSync('/bin/bash', ['-c', guard[1]], {
      env: { UNPROTECTED_CF_CONFIG: value }, timeout: 5_000,
    }).status, value === 'false' ? 0 : 1);
  }
  const job = workflow.split('\n  bootstrap-maintenance:\n')[1].split('\n  release:\n')[0];
  assert.match(job, /if: inputs.mode == 'bootstrap-maintenance'\n    needs: \[preflight, staging-evidence\]/);
  assert.match(job, /group: production-worker-mutation\n      cancel-in-progress: false\n    environment: production/);
  const gate = job.match(/steps:\n      - name: Require the exact Release Manager-accepted SHA\n        env:\n(?: {10}[^\n]*\n)*? {10}ACCEPTED_SHA: \$\{\{ vars\.PRODUCTION_ACCEPTED_SHA \}\}\n        run: \|\n((?: {10}[^\n]*\n)+)      - uses: actions\/checkout@/);
  assert.ok(gate);
  assert.match(job.slice(gate.index + gate[0].length), /\n      - name: Seed only the pinned, empty maintenance Worker\n        env:\n(?: {10}[^\n]*\n)+        run: node ci\/production-maintenance\/seed\.mjs\n/);
  for (const [accepted, attempt] of [[sha, '1'], ['c'.repeat(40), '1'], [sha, '2']]) {
    const env = { DEPLOY_SHA: sha, ACCEPTED_SHA: accepted, GITHUB_RUN_ATTEMPT: attempt };
    assert.equal(spawnSync('/bin/bash', ['-c', gate[1]], { env, timeout: 5_000 }).status,
      accepted === sha && attempt === '1' ? 0 : 1);
  }
  const workspace = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'maintenance-preflight-'));
  t.after(() => rmSync(workspace, { recursive: true, force: true }));
  const output = join(workspace, 'github-output');
  for (const [overrides, refusal] of [
    [{ DEPLOY_MODE: 'promote' }, /Application promotion is disabled/],
    [{ INPUT_SHA: 'c'.repeat(40) }, /must match the dispatched main commit/],
    [{}],
  ]) {
    const result = spawnSync('/bin/bash', ['-c', preflight[2].replace(/^ {10}/gm, '')], {
      encoding: 'utf8', timeout: 5_000, env: {
        PATH: process.env.PATH, GITHUB_OUTPUT: output, GITHUB_REF: 'refs/heads/main', GITHUB_SHA: sha,
        INPUT_SHA: sha, DEPLOY_MODE: 'bootstrap-maintenance', PRODUCTION_DEPLOY_ENABLED: 'true',
        UNPROTECTED_ACCEPTED_SHA: '',
        PRODUCTION_DOMAIN: 'example.com', PRODUCTION_DOMAIN_DEFAULT: 'example.invalid',
        ...overrides,
      },
    });
    assert.equal(result.status, refusal ? 1 : 0, result.stderr);
    if (refusal) assert.match(result.stdout, refusal);
    assert.equal(existsSync(output) ? readFileSync(output, 'utf8') : '', refusal ? '' : `sha=${sha}\ndomain=example.com\n`);
  }
});

test('production requires the independently accepted SHA before checkout or credential use', () => {
  const production = workflow.split('\n  deploy-production:\n')[1];
  const match = production.match(/steps:\n      - name: Require the exact Release Manager-accepted SHA\n(.*?)        run: \|\n((?:          [^\n]*\n)+)/s);
  assert.ok(match);
  assert.doesNotMatch(match[1], /if:|secrets\./);
  assert.match(match[1], /ACCEPTED_SHA: \$\{\{ vars\.PRODUCTION_ACCEPTED_SHA \}\}/);
  assert.ok(production.indexOf(match[0]) < production.indexOf('uses: actions/checkout@'));
  const deploy = 'a'.repeat(40);
  for (const accepted of [deploy, 'b'.repeat(40), '']) {
    const result = spawnSync('/bin/bash', ['-c', match[2].replace(/^ {10}/gm, '')], {
      encoding: 'utf8', timeout: 5_000, env: { DEPLOY_SHA: deploy, ACCEPTED_SHA: accepted },
    });
    assert.equal(result.status, accepted === deploy ? 0 : 1);
  }
});

function cloudflareFixture(overrides = {}) {
  const account = 'd'.repeat(32);
  const root = `https://api.cloudflare.com/client/v4/accounts/${account}/workers`;
  const pinned = `/workers/${workerId}`;
  const script = '/scripts/nntune-web';
  const query = '?page=1&per_page=2';
  const deployments = `${script}/deployments${query}`;
  const state = { id: workerId, name: 'nntune-web', references: { domains: [] },
    subdomain: { enabled: false, previews_enabled: false } };
  const seed = { id: version, annotations: { 'workers/tag': sha }, bindings: [] };
  const calls = [];
  let written = false;
  const request = async (url, options) => {
    assert.ok(url.startsWith(root + '/'));
    const path = url.slice(root.length);
    calls.push([options.method, path]);
    assert.equal(options.headers.Authorization, 'Bearer fixture-only');
    assert.equal(options.redirect, 'error');
    const entries = written ? [{ versions: [{ version_id: version, percentage: 100 }] }] : [];
    const list = (result, count) => ({ result, result_info: { page: 1, count, total_count: count } });
    let envelope;
    if (options.method === 'POST') {
      assert.equal(path, `${pinned}/versions?deploy=true`);
      const code = readFileSync(new URL('./production-maintenance/worker.mjs', import.meta.url));
      assert.deepEqual(JSON.parse(options.body), {
        main_module: 'worker.mjs', compatibility_date: '2026-10-01', bindings: [], annotations: seed.annotations,
        modules: [{ name: 'worker.mjs', content_type: 'application/javascript+module', content_base64: code.toString('base64') }],
      });
      written = true;
      envelope = { result: seed };
    } else if (path === pinned) {
      envelope = { result: { ...state, deployed_on: written ? checkedAt : null, ...overrides.worker } };
    } else if (path === deployments) {
      envelope = { ...list({ deployments: entries }, entries.length), ...(written ? overrides.readback : overrides.deployments) };
    } else if (path === `${pinned}/versions${query}`) {
      envelope = { ...list([], 0), ...overrides.versions };
    } else if (path === `${script}/settings`) {
      envelope = { result: { bindings: [], ...overrides.settings } };
    } else {
      assert.equal(path, `${pinned}/versions/${version}`);
      envelope = { result: seed };
    }
    return Response.json({ success: true, errors: [], ...envelope });
  };
  return { calls, run: (input = {}) => seedMaintenance({ account, token: 'fixture-only', worker: workerId,
    sha, routeProof, now, request, ...input }) };
}

test('empty Worker is seeded once, read back at 100%, and refuses re-entry', async () => {
  const { calls, run } = cloudflareFixture();
  assert.equal(await run(), version);
  assert.deepEqual(calls.map(([method]) => method), ['GET', 'GET', 'GET', 'GET', 'POST', 'GET', 'GET', 'GET']);
  await assert.rejects(run(), /not confirmed undeployed and unexposed/);
  assert.equal(calls.filter(([method]) => method === 'POST').length, 1);
});

test('incumbent, exposed or uncertain Worker never reaches a write', async () => {
  for (const [overrides, diagnostic] of [
    [{ worker: { deployed_on: checkedAt } }, /not confirmed undeployed/],
    [{ worker: { id: 'c'.repeat(32) } }, /not confirmed undeployed/],
    [{ worker: { references: { domains: [{ hostname: 'example.com' }] } } }, /unexposed/],
    [{ worker: { subdomain: { enabled: true, previews_enabled: false } } }, /unexposed/],
    [{ worker: { subdomain: { enabled: false } } }, /unexposed/],
    [{ deployments: { result: { deployments: [{}] } } }, /exactly 0 deployments/],
    [{ deployments: { result_info: { page: 1, count: 0, total_pages: 0 } } }, /exactly 0 deployments/],
    [{ versions: { result_info: { page: 1, count: 0, total_count: 1 } } }, /exactly 0 versions/],
    [{ settings: { bindings: [{ type: 'secret_text' }] } }, /not confirmed empty/],
    [{ settings: { bindings: null } }, /not confirmed empty/],
  ]) {
    const { calls, run } = cloudflareFixture(overrides);
    await assert.rejects(run(), diagnostic);
    assert.ok(calls.every(([method]) => method === 'GET'));
  }
});

test('missing, stale, mismatched or nonempty route proof refuses before Cloudflare access', async () => {
  const { calls, run } = cloudflareFixture();
  for (const proof of ['', routeProof.replace(sha, 'c'.repeat(40)), routeProof.replace('12:00', '11:00'),
    routeProof.replace('12:00', '12:10'), routeProof.replace('"route_count":0', '"route_count":1')]) {
    await assert.rejects(run({ routeProof: proof }), /custodian route proof/i);
  }
  assert.deepEqual(calls, []);
});

test('wrong allocation, version or page count fails readback without retry or deletion', async () => {
  for (const [version_id, percentage, total_pages] of [[version, 99, 1], ['aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 100, 1], [version, 100, 2]]) {
    const { calls, run } = cloudflareFixture({ readback: { result_info: { page: 1, count: 1, total_count: 1, total_pages },
      result: { deployments: [{ versions: [{ version_id, percentage }] }] } } });
    await assert.rejects(run(), total_pages === 1 ? /exactly the new seed version at 100%/ : /complete first page/);
    assert.deepEqual(calls.map(([method]) => method), ['GET', 'GET', 'GET', 'GET', 'POST', 'GET']);
  }
});

test('automatic rollback passes the selected maintenance UUID to the locked binary without npm fallback', () => {
  const workspace = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'locked-wrangler-'));
  try {
    const bin = join(workspace, 'apps/web/node_modules/.bin');
    const shim = fileURLToPath(new URL('./locked-wrangler/npx', import.meta.url));
    const run = (cli) => spawnSync(shim, [cli, 'rollback', version, '--name', 'nntune-web', '--yes'], {
      encoding: 'utf8', timeout: 5_000, env: { GITHUB_WORKSPACE: workspace },
    });
    assert.notEqual(run('wrangler').status, 0);
    mkdirSync(bin, { recursive: true });
    writeFileSync(join(bin, 'wrangler'), '#!/bin/sh\nprintf "%s " "$@"\n', { mode: 0o755 });
    const restore = run('wrangler');
    assert.equal(restore.status, 0);
    assert.equal(restore.stdout.trim(), `rollback ${version} --name nntune-web --yes`);
    assert.notEqual(run('wrangler@latest').status, 0);
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
});

const rollbackFixture = { checkedAt, now, sha, version, workerId };

test('production rollout requires the verified seed and a unique attempt version before shifting traffic', async (t) => {
  const { calls, custody, versions, seed, run } = finalSmokeFixture(t, rollbackFixture);
  await assert.rejects(run('check-seed', { ROLLBACK_CUSTODY: JSON.stringify({ ...custody, checked_at: checkedAt.replace('12:', '11:') }) }), /custody/);
  assert.deepEqual(calls, []);
  seed.bindings.push({ type: 'secret_text' });
  await assert.rejects(run('check-seed'), /Maintenance version drifted/);
  seed.bindings.pop();
  await run('check-seed');
  versions.push(versions[1]);
  await assert.rejects(run('record'), /did not resolve uniquely/);
  assert.ok(calls.every((method) => method === 'GET'));
});

test('final smoke restores only recorded maintenance and reconciles ambiguity without a second write', async (t) => {
  const afterLongRollout = () => now() + 31 * 60 * 1000;
  for (const outcome of ['ack', 'lost', 'unresolved']) {
    const { calls, prepare, run } = finalSmokeFixture(t, rollbackFixture, outcome);
    await prepare(afterLongRollout);
    assert.ok(calls.every((method) => method === 'GET'));
    if (outcome === 'unresolved') {
      await assert.rejects(run('restore', {}, afterLongRollout), /reconcile allocation/);
      await assert.rejects(run('restore', {}, afterLongRollout), /EEXIST/);
    } else {
      assert.match(await run('restore', {}, afterLongRollout), outcome === 'ack' ? /allocation verified/ : /reconciled by readback/);
      assert.match(await run('restore', {}, afterLongRollout), /already verified/);
    }
    assert.deepEqual(calls.slice(calls.indexOf('POST') + 1), Array(6).fill('GET'));
    assert.equal(calls.filter((method) => method === 'POST').length, 1);
  }
});

test('final smoke refuses stale or tampered intent, changed targets and newer allocations before writing', async (t) => {
  const { calls, custody, prepare, rows, state, id, temp, run } = finalSmokeFixture(t, rollbackFixture);
  await prepare();
  await assert.rejects(run('restore', { GITHUB_RUN_ID: '13' }), /invalid or stale/);
  await assert.rejects(run('restore', { GITHUB_RUN_ATTEMPT: '2' }), /production rollback context/);
  await assert.rejects(run('restore', { ROLLBACK_CUSTODY: JSON.stringify({ ...custody, exclusive_writers: false }) }), /custody/);
  state.subdomain.previews_enabled = true;
  await assert.rejects(run('restore'), /previews drifted/);
  state.subdomain.previews_enabled = false;
  const [final] = rows;
  for (const versions of [final.versions, [{ version_id: id(9), percentage: 100 }]]) {
    rows.unshift({ ...final, id: id(8), created_on: new Date(now() + 8000).toISOString(), versions });
    await assert.rejects(run('restore'), /newer or foreign/);
    rows.shift();
  }
  const sealed = join(temp, 'production-rollback-intent.sealed');
  const bytes = readFileSync(sealed);
  bytes[bytes.length - 1] ^= 1;
  writeFileSync(sealed, bytes);
  await assert.rejects(run('restore'), /invalid or stale/);
  assert.ok(calls.every((method) => method === 'GET'));
});

test('only a failed executed final smoke can restore under the production lock', () => {
  const job = workflow.split('\n  deploy-production:\n')[1].split('\n  bootstrap-maintenance:\n')[0];
  assert.match(job, /group: production-worker-mutation\n      cancel-in-progress: false[\s\S]*?name: production/);
  const inheritance = workflow.match(/- name: Refuse inherited rollback custody and intent key\n[\s\S]*?        run: \|\n((?: {10}[^\n]+\n)+)/);
  assert.ok(inheritance && inheritance.index < workflow.indexOf('\n  staging-evidence:'));
  for (const value of ['false', 'true', '']) {
    const env = { UNPROTECTED_ROLLBACK_CONFIG: value };
    assert.equal(spawnSync('/bin/bash', ['-c', inheritance[1]], { env, timeout: 5_000 }).status, value === 'false' ? 0 : 1);
  }
  const steps = ['check-seed', 'Require one-time production seed deployment', 'Upload reviewed production version', 'rollback.mjs record',
    'Preserve sealed rollback intent', 'id: gradual', 'rollback.mjs pin-final', 'id: final-smoke',
    'rollback.mjs restore', 'Record successful production promotion'];
  let last = -1;
  for (const step of steps) {
    const index = job.indexOf(step);
    assert.ok(index > last, step);
    last = index;
  }
  assert.match(job, /if: failure\(\) && !cancelled\(\) && steps\.final-allocation\.outcome == 'success' && steps\.final-smoke\.outcome == 'failure'\n        env: \*rollback-env\n        run: node ci\/production-maintenance\/final-smoke-rollback\.mjs restore/);
  assert.match(job, /Record successful production promotion\n        if: inputs\.mode == 'promote'/);
});
