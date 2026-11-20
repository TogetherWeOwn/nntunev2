import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
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

test('only exact-main bootstrap uses the protected production job after staging admission', () => {
  const preflight = workflow.match(/id: target\n        env:\n((?: {10}[^\n]*\n)+)        run: \|\n((?: {10}[^\n]*\n)+)/);
  assert.ok(preflight);
  assert.match(preflight[1], /^ {10}DEPLOY_MODE: \$\{\{ inputs\.mode \}\}$/m);
  assert.match(workflow, /staging-evidence:\n    name: staging-evidence\n    needs: \[preflight\]/);
  const job = workflow.split('\n  bootstrap-maintenance:\n')[1].split('\n  release:\n')[0];
  assert.match(job, /if: inputs.mode == 'bootstrap-maintenance'\n    needs: \[preflight, staging-evidence\]/);
  assert.match(job, /group: production-worker-mutation\n      cancel-in-progress: false\n    environment: production/);
  assert.match(job, /ACCEPTED_SHA: \$\{\{ vars\.PRODUCTION_ACCEPTED_SHA \}\}[\s\S]*run: node ci\/production-maintenance\/seed\.mjs/);
  for (const [overrides, diagnostic] of [
    [{ DEPLOY_MODE: 'promote' }, /Application promotion is disabled/],
    [{ INPUT_SHA: 'c'.repeat(40) }, /must match the dispatched main commit/],
    [{}, /^sha=a{40}$/m],
  ]) {
    const result = spawnSync('/bin/bash', ['-c', preflight[2].replace(/^ {10}/gm, '')], {
      encoding: 'utf8', timeout: 5_000, env: {
        PATH: process.env.PATH, GITHUB_OUTPUT: '/dev/stdout', GITHUB_REF: 'refs/heads/main', GITHUB_SHA: sha,
        INPUT_SHA: sha, DEPLOY_MODE: 'bootstrap-maintenance', PRODUCTION_DEPLOY_ENABLED: 'true',
        UNPROTECTED_ACCEPTED_SHA: '', PRODUCTION_DOMAIN: 'example.com', PRODUCTION_DOMAIN_DEFAULT: 'example.invalid',
        ...overrides,
      },
    });
    const refused = Object.keys(overrides).length > 0;
    assert.equal(result.status, refused ? 1 : 0, result.stderr);
    assert.match(result.stdout, diagnostic);
    if (refused) assert.doesNotMatch(result.stdout, /^sha=|^domain=/m);
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
    const list = (result, count) => ({ result, result_info: { page: 1, count, total_count: count, total_pages: 1 } });
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

test('wrong allocation or version fails readback without retry or deletion', async () => {
  for (const [id, percentage] of [[version, 99], ['aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 100]]) {
    const { calls, run } = cloudflareFixture({ readback: { result: { deployments: [{ versions: [{ version_id: id, percentage }] }] } } });
    await assert.rejects(run(), /exactly the new seed version at 100%/);
    assert.deepEqual(calls.map(([method]) => method), ['GET', 'GET', 'GET', 'GET', 'POST', 'GET']);
  }
});

test('automatic rollback passes the selected maintenance UUID to the locked binary without npm fallback', () => {
  const seed = workflow.match(/- name: Require one-time production seed deployment\n[\s\S]+?        run: \|\n((?:          [^\n]*\n)+)/)?.[1];
  assert.ok(seed);
  const fixture = JSON.stringify([{ versions: [{ percentage: 100, version_id: version }] }]);
  const baseline = spawnSync('/bin/bash', ['-c', `npx() { printf '%s' '${fixture}'; }\n${seed.replace(/^ {10}/gm, '')}`], {
    encoding: 'utf8', timeout: 5_000, env: { PATH: process.env.PATH, WORKER_NAME: 'nntune-web' },
  });
  assert.equal(baseline.status, 0, baseline.stderr);
  assert.ok(baseline.stdout.endsWith(`rollback target ${version}\n`));
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
