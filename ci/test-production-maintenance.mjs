import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import worker from './production-maintenance/worker.mjs';
import { seedMaintenance } from './production-maintenance/seed.mjs';

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

const code = readFileSync(new URL('./production-maintenance/worker.mjs', import.meta.url));
const version = '12345678-1234-1234-1234-123456789abc';
for (const conflict of [false, true]) {
  test(conflict ? 'a late canonical name conflict leaves the existing deployment intact' :
    'maintenance uses only the new Worker ID and publishes its 100% version', async () => {
    const account = 'a'.repeat(32);
    const id = 'b'.repeat(32);
    const root = `https://api.cloudflare.com/client/v4/accounts/${account}/workers/workers`;
    const subdomain = { enabled: false, previews_enabled: false };
    let created;
    let live;
    let calls = 0;
    const ok = (result) => Response.json({ success: true, errors: [], result });
    const seed = seedMaintenance({ account, token: 'fixture-token', worker: 'nntune-web', request: async (url, options) => {
      const data = JSON.parse(options.body);
      assert.equal(options.redirect, 'error');
      switch (++calls) {
        case 1:
          assert.equal(`${options.method} ${url}`, `POST ${root}`);
          assert.match(data.name, /^nntune-maint-[0-9a-f]{32}$/);
          assert.deepEqual(data, { name: data.name, subdomain });
          created = { ...data, id, deployed_on: null };
          return ok(created);
        case 2:
          assert.equal(`${options.method} ${url}`, `POST ${root}/${id}/versions?deploy=true`);
          assert.deepEqual(data, { main_module: 'worker.mjs', compatibility_date: '2026-10-01',
            modules: [{ name: 'worker.mjs', content_type: 'application/javascript+module', content_base64: code.toString('base64') }] });
          if (conflict) live = { id: 'c'.repeat(32), version: 'existing-production-version' };
          return ok({ id: version });
        case 3:
          assert.equal(`${options.method} ${url}`, `PATCH ${root}/${id}`);
          assert.deepEqual(data, { name: 'nntune-web', subdomain });
          if (live) return Response.json({ success: false, errors: [{ code: 10040 }] }, { status: 409 });
          live = { id, version };
          return ok({ ...created, ...data });
        default:
          assert.fail('unexpected write');
      }
    } });
    if (conflict) {
      await assert.rejects(seed, /PATCH refused: HTTP 409/);
      assert.deepEqual(live, { id: 'c'.repeat(32), version: 'existing-production-version' });
    } else {
      assert.equal(await seed, version);
      assert.deepEqual(live, { id, version });
    }
    assert.equal(calls, 3);
  });
}

const workflow = readFileSync(new URL('../.github/workflows/deploy-production.yml', import.meta.url), 'utf8');
test('both production modes require the independently accepted SHA before checkout or credential use', () => {
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

test('automatic rollback passes the selected maintenance UUID to the locked binary without npm fallback', () => {
  const seed = workflow.match(/- name: Require one-time production seed deployment\n[\s\S]+?        run: \|\n((?:          [^\n]*\n)+)/)?.[1];
  assert.ok(seed);
  const fixture = JSON.stringify([{ versions: [{ percentage: 100, version_id: version }] }]);
  for (const id of [version, '87654321-4321-4321-4321-cba987654321']) {
    const baseline = spawnSync('/bin/bash', ['-c', `npx() { printf '%s' '${fixture}'; }\n${seed.replace(/^ {10}/gm, '')}`], {
      encoding: 'utf8', timeout: 5_000,
      env: { PATH: process.env.PATH, WORKER_NAME: 'nntune-web', DEPLOY_MODE: 'bootstrap-maintenance', SEED_VERSION: id },
    });
    assert.equal(baseline.status, id === version ? 0 : 1, baseline.stderr);
    if (id === version) assert.ok(baseline.stdout.endsWith(`rollback target ${version}\n`));
  }
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
