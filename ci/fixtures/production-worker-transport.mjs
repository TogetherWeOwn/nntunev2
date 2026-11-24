import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { admitRecovery, intentFile } from '../production-maintenance/recovery-evidence.mjs';
import { finalSmokeRollback } from '../production-maintenance/final-smoke-rollback.mjs';

export function finalSmokeFixture(t, { checkedAt, now, sha, version, workerId }, outcome = 'ack', kind = 'maintenance') {
  const temp = mkdtempSync(join(tmpdir(), 'final-smoke-'));
  t.after(() => rmSync(temp, { recursive: true, force: true }));
  const id = (n) => `00000000-0000-0000-0000-${String(n).padStart(12, '0')}`;
  const candidate = id(10);
  const account = 'd'.repeat(32);
  const references = { domains: [{ hostname: 'test.example.com', id: 'fixture' }], queues: [] };
  const module = readFileSync(new URL('../production-maintenance/worker.mjs', import.meta.url));
  const target = { version, deployment: id(1), sha: 'f'.repeat(40) };
  const custody = { account, worker: workerId, sha, domain: 'test.example.com', references,
    checked_at: new Date(now()).toISOString(), private_routes_frozen: true, exclusive_writers: true,
    evidence_sha256: 'e'.repeat(64), [kind]: kind === 'maintenance' ?
      { ...target, module_sha256: createHash('sha256').update(module).digest('hex') } :
      { ...target, tag: `${target.sha}-11-1` } };
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
  const seed = { id: version, bindings: [], annotations: { 'workers/tag': custody.previous?.tag ?? target.sha } };
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
      const label = kind === 'maintenance' ? 'First-release maintenance' : 'Previous Worker';
      assert.match(annotations['workers/message'], new RegExp(`^${label} recovery [a-f0-9]{32}$`));
      if (outcome !== 'unresolved') rows.unshift(deployment(5, versions, annotations));
      if (outcome !== 'ack') throw new Error('simulated lost response');
      return response(rows[0]);
    }
    const pinned = `/workers/${workerId}`;
    switch (path) {
      case pinned: return response(state);
      case `${pinned}/versions/${version}`: return response(seed);
      case `${pinned}/versions/${candidate}`: return response(versions[1]);
      case `${pinned}/versions?page=1&per_page=100`: return response(versions, versions.length);
      case '/scripts/nntune-web/deployments?page=1&per_page=100': return response({ deployments: rows }, rows.length);
      default: assert.fail('Unexpected offline request');
    }
  };
  const run = (command, overrides, clock = now, probe) => finalSmokeRollback(command,
    { ...env, ROLLBACK_CUSTODY: JSON.stringify(custody), ...overrides }, request, clock, probe);
  const shift = () => rows.unshift(deployment(4, [allocation(candidate)]),
    deployment(3, [allocation(candidate, 50), allocation(version, 50)]),
    deployment(2, [allocation(candidate, 5), allocation(version, 95)]));
  const prepare = async (clock = now) => {
    await run('check-seed');
    await run('record');
    shift();
    await run('pin-final', {}, clock);
  };
  return { calls, custody, rows, versions, seed, state, id, temp, env, run, prepare, shift };
}

export function recoveryFixture(t, transport, conclusion) {
  const run = { status: 'completed', conclusion, head_sha: transport.env.DEPLOY_SHA };
  const steps = [
    { name: 'Preserve sealed rollback intent before shifting traffic', status: 'completed', conclusion: 'success', number: 10 },
    { name: 'Gradual promotion 5-50-100 with automatic rollback', status: 'completed', conclusion, number: 20 },
  ];
  const claims = [];
  const zipped = spawnSync('python3', ['-c', `import io,sys,zipfile
buf = io.BytesIO()
with zipfile.ZipFile(buf, 'w') as z:
    z.writestr(sys.argv[1], sys.stdin.buffer.read())
sys.stdout.buffer.write(buf.getvalue())`, intentFile], { input: readFileSync(join(transport.temp, intentFile)) });
  assert.equal(zipped.status, 0, zipped.stderr.toString());
  const artifact = { digest: `sha256:${createHash('sha256').update(zipped.stdout).digest('hex')}` };
  const evidence = (repo) => {
    assert.equal(repo, 'example/fixture');
    return {
      source: (id, attempt) => {
        assert.deepEqual([id, attempt], ['12', '1']);
        return { run, job: { conclusion, steps } };
      },
      artifactFor: (producer, name) => {
        assert.equal(producer, run);
        assert.equal(name, 'production-rollback-intent-12-1');
        return { artifact, archive: zipped.stdout };
      },
      command: (program, args, maxBuffer, input) => {
        const read = spawnSync(program, args, { input, maxBuffer });
        assert.equal(read.status, 0, read.stderr.toString());
        return read.stdout;
      },
      list: (route, key) => {
        assert.equal(route, 'artifacts?name=production-worker-recovery-claim-12');
        assert.equal(key, 'artifacts');
        return claims;
      },
    };
  };
  function fresh(id) {
    const temp = mkdtempSync(join(tmpdir(), 'protected-recovery-'));
    t.after(() => rmSync(temp, { recursive: true, force: true }));
    return { ...transport.env, GITHUB_RUN_ID: id, RECOVERY_RUN_ID: '12', ACCEPTED_SHA: transport.env.DEPLOY_SHA,
      GITHUB_JOB: 'recover-worker', GITHUB_SHA: 'c'.repeat(40), GITHUB_WORKFLOW_SHA: 'c'.repeat(40),
      RUNNER_TEMP: temp, GITHUB_OUTPUT: join(temp, 'output') };
  }
  function enter(env) {
    admitRecovery(env, evidence);
    const output = readFileSync(env.GITHUB_OUTPUT, 'utf8');
    const canSubmit = /^allow-submit=true$/m.test(output);
    if (canSubmit) claims.push({ name: 'production-worker-recovery-claim-12' });
    env.CLAIM_VERIFIED = String(canSubmit);
    return transport.run('recover', env);
  }
  return { fresh, enter, steps, evidence, artifact };
}
