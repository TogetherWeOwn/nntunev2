import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import worker from './production-maintenance/worker.mjs';
import { requireAbsent } from './production-maintenance/check-absent.mjs';

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

test('maintenance deploy config has no routes, bindings or public preview', () => {
  const config = readFileSync(new URL('./production-maintenance/wrangler.toml', import.meta.url), 'utf8');
  const settings = config.split('\n').filter((line) => line && !line.startsWith('#'));
  assert.deepEqual(settings, ['name = "nntune-web"', 'main = "worker.mjs"',
    'compatibility_date = "2026-10-01"', 'workers_dev = false', 'preview_urls = false']);
});

test('only a complete inventory without the production Worker admits creation', () => {
  const inventory = { success: true, errors: [], result: [{ id: 'nntune-web-staging' }] };
  requireAbsent(inventory, 'nntune-web');
  assert.throws(() => requireAbsent({ ...inventory, result: [{ id: 'nntune-web' }] }, 'nntune-web'), /already exists/);
  for (const change of [{ success: false }, { result_info: { cursor: 'next' } }, { result: [{}] }]) {
    assert.throws(() => requireAbsent({ ...inventory, ...change }, 'nntune-web'), /Cannot establish/);
  }
});

const workflow = readFileSync(new URL('../.github/workflows/deploy-production.yml', import.meta.url), 'utf8');
test('bootstrap shell dry-run refuses before deploy on a wrong ref, SHA or inventory', () => {
  const step = workflow.match(/- name: Initialize the first-release maintenance rollback target\n([\s\S]+?)        run: \|\n((?:          [^\n]*\n)+)/);
  assert.ok(step);
  assert.match(step[1], /if: inputs.mode == 'bootstrap-maintenance'/);
  const script = step[2].replace(/^ {10}/gm, '');
  const sha = 'a'.repeat(40);
  const run = (env = {}) => spawnSync('/bin/bash', ['-c', `
    node() { test "$1" = ci/production-maintenance/check-absent.mjs && return "$INVENTORY_STATUS"; }
    pnpm() { printf '%s\\n' "$@"; }
    ${script}`], {
    encoding: 'utf8', timeout: 5_000,
    env: { GITHUB_REF: 'refs/heads/main', DEPLOY_SHA: sha, ACCEPTED_SHA: sha, INVENTORY_STATUS: '0', WORKER_NAME: 'nntune-web', ...env },
  });
  for (const env of [{ GITHUB_REF: 'refs/heads/topic' }, { ACCEPTED_SHA: '' }, { INVENTORY_STATUS: '1' }]) {
    const refused = run(env);
    assert.equal(refused.status, 1);
    assert.doesNotMatch(refused.stdout, /wrangler/);
  }
  const armed = run();
  assert.equal(armed.status, 0);
  assert.deepEqual(armed.stdout.trim().split('\n'), [
    '--dir', 'apps/web', 'exec', 'wrangler', 'deploy', '--name', 'nntune-web',
    '--config', '../../ci/production-maintenance/wrangler.toml',
  ]);
});

test('rollback dry-run restores the recorded 100% maintenance version', async () => {
  const seed = workflow.match(/- name: Require one-time production seed deployment\n[\s\S]+?        run: \|\n((?:          [^\n]*\n)+)/)?.[1];
  const runbook = readFileSync(new URL('../docs/ops.md', import.meta.url), 'utf8');
  const rollback = 'wrangler rollback "$MAINTENANCE_VERSION" --name nntune-web --yes';
  assert.ok(seed && runbook.includes(`\`${rollback}\``));
  const id = '12345678-1234-1234-1234-123456789abc';
  const fixture = JSON.stringify([{ versions: [{ percentage: 100, version_id: id }] }]);
  const env = { PATH: process.env.PATH, WRANGLER_VERSION: 'fixture', WORKER_NAME: 'nntune-web' };
  const run = (script) => spawnSync('/bin/bash', ['-e', '-c', script], { encoding: 'utf8', timeout: 5_000, env });
  const baseline = run(`npx() { printf '%s' '${fixture}'; }\n${seed.replace(/^ {10}/gm, '')}`);
  assert.equal(baseline.status, 0, baseline.stderr);
  assert.ok(baseline.stdout.endsWith(`rollback target ${id}\n`));
  const restore = run(`wrangler() { printf '%s ' "$@"; }\nMAINTENANCE_VERSION=${id}\n${rollback}`);
  assert.equal(restore.status, 0);
  assert.equal(restore.stdout.trim(), `rollback ${id} --name nntune-web --yes`);
  assert.equal((await worker.fetch()).status, 503);
});
