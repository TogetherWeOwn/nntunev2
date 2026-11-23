import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { finalSmokeRollback } from '../production-maintenance/final-smoke-rollback.mjs';

export function finalSmokeFixture(t, { checkedAt, now, sha, version, workerId }, outcome = 'ack') {
  const temp = mkdtempSync(join(tmpdir(), 'final-smoke-'));
  t.after(() => rmSync(temp, { recursive: true, force: true }));
  const id = (n) => `00000000-0000-0000-0000-${String(n).padStart(12, '0')}`;
  const candidate = id(10);
  const account = 'd'.repeat(32);
  const references = { domains: [{ hostname: 'test.example.com', id: 'fixture' }], queues: [] };
  const module = readFileSync(new URL('../production-maintenance/worker.mjs', import.meta.url));
  const custody = { account, worker: workerId, sha, domain: 'test.example.com', references,
    checked_at: new Date(now()).toISOString(), private_routes_frozen: true, exclusive_writers: true,
    evidence_sha256: 'e'.repeat(64), maintenance: { version, deployment: id(1), sha: 'f'.repeat(40),
      module_sha256: createHash('sha256').update(module).digest('hex') } };
  const env = { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: 'fixture-only', WORKER_ID: workerId,
    DEPLOY_SHA: sha, DOMAIN: custody.domain, GITHUB_REPOSITORY: 'example/fixture', GITHUB_RUN_ID: '12',
    GITHUB_RUN_ATTEMPT: '1', GITHUB_JOB: 'deploy-production', GITHUB_REF: 'refs/heads/main', GITHUB_SHA: sha,
    GITHUB_WORKFLOW_SHA: sha, GITHUB_EVENT_NAME: 'workflow_dispatch', RUNNER_TEMP: temp,
    GITHUB_WORKFLOW_REF: 'example/fixture/.github/workflows/deploy-production.yml@refs/heads/main',
    INTENT_KEY: 'a'.repeat(64), PROMOTED_VERSION: candidate, PREV_VERSION: version };
  const allocation = (version_id, percentage = 100) => ({ version_id, percentage });
  const deployment = (n, versions, annotations) => ({ id: id(n), versions, annotations,
    created_on: new Date(now() + n * 1000).toISOString() });
  const rows = [deployment(1, [allocation(version)])];
  const seed = { id: version, bindings: [], annotations: { 'workers/tag': custody.maintenance.sha } };
  const versions = [seed, { id: candidate, annotations: { 'workers/tag': `${sha}-12-1` } }];
  const state = { id: workerId, name: 'nntune-web', deployed_on: checkedAt, references: structuredClone(references),
    subdomain: { enabled: false, previews_enabled: false } };
  const calls = [];
  const request = async (url, options) => {
    const root = `https://api.cloudflare.com/client/v4/accounts/${account}/workers`;
    assert.ok(url.startsWith(root + '/'));
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers.Authorization, 'Bearer fixture-only');
    const path = url.slice(root.length);
    calls.push(options.method);
    const response = (result, count) => Response.json({ success: true, errors: [], result,
      result_info: { page: 1, total_count: count, count } });
    if (options.method === 'POST') {
      assert.equal(path, '/scripts/nntune-web/deployments');
      const { strategy, versions, annotations, ...extra } = JSON.parse(options.body);
      assert.deepEqual({ strategy, versions, extra }, { strategy: 'percentage', versions: [allocation(version)], extra: {} });
      assert.match(annotations['workers/message'], /^First-release maintenance recovery [a-f0-9]{32}$/);
      if (outcome !== 'unresolved') rows.unshift(deployment(5, versions, annotations));
      if (outcome !== 'ack') throw new Error('simulated lost response');
      return response(rows[0]);
    }
    const pinned = `/workers/${workerId}`;
    switch (path) {
      case pinned: return response(state);
      case `${pinned}/versions/${version}`: return response(seed);
      case `${pinned}/versions?page=1&per_page=100`: return response(versions, versions.length);
      case '/scripts/nntune-web/deployments?page=1&per_page=100': return response({ deployments: rows }, rows.length);
      default: assert.fail('Unexpected offline request');
    }
  };
  const run = (command, overrides) => finalSmokeRollback(command,
    { ...env, ROLLBACK_CUSTODY: JSON.stringify(custody), ...overrides }, request, now);
  const shift = () => rows.unshift(deployment(4, [allocation(candidate)]),
    deployment(3, [allocation(candidate, 50), allocation(version, 50)]),
    deployment(2, [allocation(candidate, 5), allocation(version, 95)]));
  const prepare = async () => { await run('check-seed'); await run('record'); shift(); await run('pin-final'); };
  return { calls, custody, rows, versions, seed, state, id, temp, run, prepare };
}
