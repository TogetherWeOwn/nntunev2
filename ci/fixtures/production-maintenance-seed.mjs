import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

export function seedTransport({ account, checkedAt, sha, version, workerId }, overrides = {}) {
  const root = `https://api.cloudflare.com/client/v4/accounts/${account}/workers`;
  const pinned = `/workers/${workerId}`;
  const script = '/scripts/nntune-web';
  const query = '?page=1&per_page=2';
  const state = { id: workerId, name: 'nntune-web', references: { domains: [] },
    subdomain: { enabled: false, previews_enabled: false } };
  const deployment = { id: '00000000-0000-0000-0000-000000000001',
    versions: [{ version_id: version, percentage: 100 }] };
  const calls = [];
  let seed;
  const request = async (url, options) => {
    assert.ok(url.startsWith(root + '/'));
    const path = url.slice(root.length);
    calls.push([options.method, path]);
    assert.equal(options.headers.Authorization, 'Bearer fixture-only');
    assert.equal(options.redirect, 'error');
    const deployed = seed && overrides.submission !== 'partial';
    const entries = deployed ? [deployment] : [];
    const list = (result, count) => ({ result, result_info: { page: 1, count, total_count: count } });
    let envelope;
    if (options.method === 'POST') {
      assert.equal(path, `${pinned}/versions?deploy=true`);
      const code = readFileSync(new URL('../production-maintenance/worker.mjs', import.meta.url));
      const payload = JSON.parse(options.body);
      const annotations = payload.annotations;
      assert.equal(annotations['workers/tag'], sha);
      assert.deepEqual(payload, {
        main_module: 'worker.mjs', compatibility_date: '2026-10-01', bindings: [], annotations,
        modules: [{ name: 'worker.mjs', content_type: 'application/javascript+module', content_base64: code.toString('base64') }],
      });
      if (overrides.submission !== 'no-commit') seed = { id: version, annotations, bindings: [], ...overrides.seed };
      if (overrides.submission) throw new Error('synthetic private provider failure');
      envelope = { result: { id: overrides.acknowledged ?? version } };
    } else if (path === pinned) {
      if (overrides.unregistered) return new Response(null, { status: 404 });
      envelope = { result: { ...state, deployed_on: deployed ? checkedAt : null, ...overrides.worker } };
    } else if (path === `${script}/deployments${query}`) {
      envelope = { ...list({ deployments: entries }, entries.length), ...(seed ? overrides.readback : overrides.deployments) };
    } else if (path === `${pinned}/versions${query}`) {
      envelope = { ...list(seed ? [seed] : [], seed ? 1 : 0),
        ...(seed ? overrides.versionReadback : overrides.versions) };
    } else if (path === `${script}/settings`) {
      envelope = { result: { bindings: [], ...overrides.settings } };
    } else {
      assert.equal(path, `${pinned}/versions/${version}`);
      envelope = { result: seed };
    }
    return Response.json({ success: true, errors: [], ...envelope });
  };
  return { calls, request };
}
