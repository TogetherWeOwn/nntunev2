import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { finalSmokeFixture, mainSha, recoveryError, recoveryFixture } from './fixtures/production-worker-transport.mjs';
import { seedTransport } from './fixtures/production-maintenance-seed.mjs';
import { admitRecovery } from './production-maintenance/recovery-evidence.mjs';
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
const account = 'd'.repeat(32);
const checkedAt = '2026-10-01T12:00:00Z';
const now = () => Date.parse(checkedAt) + 60_000;
const routeProof = JSON.stringify({ worker_id: workerId, sha, checked_at: checkedAt, route_count: 0 });
const bootstrap = workflow.split('\n  bootstrap-maintenance:\n')[1].split('\n  recover-worker:\n')[0];
const seedCommand = bootstrap.match(/\n        run: (node ci\/production-maintenance\/seed\.mjs)\n/)?.[1];

test('only exact-main bootstrap uses the protected production job after staging admission', (t) => {
  const preflight = workflow.match(/id: target\n        env:\n((?: {10}[^\n]*\n)+)        run: \|\n((?: {10}[^\n]*\n)+)/);
  assert.ok(preflight);
  assert.match(preflight[1], /^ {10}DEPLOY_MODE: \$\{\{ inputs\.mode \}\}$/m);
  assert.doesNotMatch(preflight[1], /secrets\./);
  const staging = workflow.split('\n  staging-evidence:\n')[1].split('\n  deploy-production:\n')[0];
  assert.match(staging, /needs: \[preflight\][\s\S]*?environment:\n      name: staging\n      deployment: false/);
  const guard = staging.match(/UNPROTECTED_CF_CONFIG: \$\{\{ secrets\.PRODUCTION_CLOUDFLARE_API_TOKEN != '' \|\| secrets\.PRODUCTION_CLOUDFLARE_ACCOUNT_ID != '' \}\}\n          UNPROTECTED_ROLLBACK_CONFIG: \$\{\{ inputs\.mode != 'bootstrap-maintenance' && \(secrets\.PRODUCTION_ROLLBACK_CUSTODY != '' \|\| secrets\.PRODUCTION_ROLLBACK_INTENT_KEY != ''\) \}\}\n        run: \|\n([\s\S]*?)          python3 -/);
  assert.ok(guard);
  for (const [credentials, rollback] of [['false', 'false'], ['true', 'false'], ['', 'false'], ['false', 'true'], ['false', '']]) {
    assert.equal(spawnSync('/bin/bash', ['-c', guard[1]], {
      env: { DEPLOY_MODE: 'recover-worker', UNPROTECTED_CF_CONFIG: credentials, UNPROTECTED_ROLLBACK_CONFIG: rollback }, timeout: 5_000,
    }).status, credentials === 'false' && rollback === 'false' ? 0 : 1);
  }
  for (const [mode, credentials, expected] of [
    ['bootstrap-maintenance', 'false', 42], ['promote', 'false', 0],
    ['promote', 'true', 1], ['unknown', 'false', 1],
  ]) {
    assert.equal(spawnSync('/bin/bash', ['-c', guard[1] + 'exit 42\n'], {
      env: { DEPLOY_MODE: mode, UNPROTECTED_CF_CONFIG: credentials, UNPROTECTED_ROLLBACK_CONFIG: 'false' },
      timeout: 5_000,
    }).status, expected);
  }
  const job = bootstrap;
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
  const attempts = [
    [{ PRODUCTION_DEPLOY_ENABLED: '' }, /Production lane disabled/],
    [{ DEPLOY_MODE: 'promote' }, /Application promotion is disabled/],
    [{ INPUT_SHA: 'c'.repeat(40) }, /must match the dispatched main commit/],
    [{}],
    [{ PRODUCTION_DEPLOY_ENABLED: 'false' }, /Production lane disabled/],
    [{ PRODUCTION_DEPLOY_ENABLED: 'false' }, /Production lane disabled/],
    [{ PRODUCTION_DEPLOY_ENABLED: 'false', DEPLOY_MODE: 'recover-worker', RECOVERY_RUN_ID: '12' }],
  ];
  for (const [index, [overrides, refusal]] of attempts.entries()) {
    const output = join(workspace, `github-output-${index}`);
    const result = spawnSync('/bin/bash', ['-c', preflight[2].replace(/^ {10}/gm, '')], {
      encoding: 'utf8', timeout: 5_000, env: {
        PATH: process.env.PATH, GITHUB_OUTPUT: output, GITHUB_REF: 'refs/heads/main', GITHUB_SHA: sha,
        INPUT_SHA: sha, DEPLOY_MODE: 'bootstrap-maintenance', PRODUCTION_DEPLOY_ENABLED: 'true',
        UNPROTECTED_ACCEPTED_SHA: '', GITHUB_RUN_ATTEMPT: '1',
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

function cloudflareFixture(overrides) {
  const { calls, request } = seedTransport({ account, checkedAt, sha, version, workerId }, overrides);
  return { calls, run: (input = {}) => seedMaintenance({ account, token: 'fixture-only', worker: workerId,
    sha, routeProof, now, request, ...input }) };
}

test('empty Worker is seeded once, read back at 100%, and refuses re-entry', async () => {
  const { calls, run } = cloudflareFixture();
  assert.equal(await run(), version);
  assert.deepEqual(calls.map(([method]) => method), ['GET', 'GET', 'GET', 'GET', 'POST', 'GET', 'GET', 'GET', 'GET']);
  await assert.rejects(run(), /not confirmed undeployed and unexposed/);
  assert.equal(calls.filter(([method]) => method === 'POST').length, 1);
});

test('incumbent, exposed or uncertain Worker never reaches a write', async () => {
  for (const [overrides, diagnostic] of [
    [{ unregistered: true }, /Worker identity.*HTTP 404/],
    [{ worker: { deployed_on: checkedAt } }, /not confirmed undeployed/],
    [{ worker: { id: 'c'.repeat(32) } }, /not confirmed undeployed/],
    [{ worker: { name: 'other-app' } }, /not confirmed undeployed/],
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

test('wrong deployment ID, allocation or page count fails readback without retry or deletion', async () => {
  for (const [id, version_id, percentage, total_pages] of [
    ['malformed', version, 100, 1],
    [version, version, 99, 1],
    [version, 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 100, 1],
    [version, version, 100, 2],
  ]) {
    const { calls, run } = cloudflareFixture({ readback: { result_info: { page: 1, count: 1, total_count: 1, total_pages },
      result: { deployments: [{ id, versions: [{ version_id, percentage }] }] } } });
    await assert.rejects(run(), total_pages === 1 ? /exactly the new seed version at 100%/ : /complete first page/);
    assert.deepEqual(calls.map(([method]) => method), ['GET', 'GET', 'GET', 'GET', 'POST', 'GET']);
  }
});

test('a foreign allocation or version cannot be accepted as the seed for this submission', async () => {
  const foreign = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
  for (const [overrides, diagnostic] of [
    [{ acknowledged: foreign }, /exactly the new seed version at 100%/],
    [{ submission: 'lost', seed: { annotations: { 'workers/tag': sha } } }, /this submission/],
    [{ submission: 'lost', seed: { bindings: [{ type: 'secret_text' }] } }, /binding-free seed/],
    [{ versionReadback: { result: [{ id: version }, { id: foreign }],
      result_info: { page: 1, count: 2, total_count: 2 } } }, /exactly 1 versions/],
    [{ versionReadback: { result: [{ id: foreign }] } }, /this submission/],
  ]) {
    const { calls, run } = cloudflareFixture(overrides);
    await assert.rejects(run(), diagnostic);
    assert.equal(calls.filter(([method]) => method === 'POST').length, 1);
  }
});

test('protected bootstrap command reconciles a committed seed after a lost response and refuses a partial one', () => {
  assert.ok(seedCommand);
  const transport = new URL('./fixtures/production-maintenance-seed.mjs', import.meta.url).href;
  const checkedAt = new Date().toISOString().slice(0, 19) + 'Z';
  for (const submission of ['lost', 'partial', 'no-commit']) {
    const preload = `
      import { seedTransport } from ${JSON.stringify(transport)};
      const { calls, request } = seedTransport(
        ${JSON.stringify({ account, checkedAt, sha, version, workerId })}, ${JSON.stringify({ submission })}
      );
      globalThis.fetch = request;
      process.on('exit', () => {
        const posts = calls.filter(([method]) => method === 'POST').length;
        if (posts !== 1) {
          console.error('Expected one seed POST, saw ' + posts);
          process.exitCode = 2;
        }
      });
    `;
    const result = spawnSync('/bin/bash', ['-c', seedCommand], {
      env: {
        PATH: process.env.PATH, NODE_OPTIONS: `--import=data:text/javascript;base64,${Buffer.from(preload).toString('base64')}`,
        CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: 'fixture-only', WORKER_ID: workerId,
        DEPLOY_SHA: sha, ROUTE_PROOF: JSON.stringify({ worker_id: workerId, sha, checked_at: checkedAt, route_count: 0 }),
      },
      encoding: 'utf8', timeout: 5_000,
    });
    assert.equal(result.status, submission === 'lost' ? 0 : 1, result.stderr);
    if (submission === 'lost') assert.match(result.stdout, new RegExp(`${version} verified at 100%`));
    else {
      assert.equal(result.stdout, '');
      assert.match(result.stderr, /Reconcile Worker, deployment and route state/);
    }
    assert.doesNotMatch(result.stdout + result.stderr, /fixture-only|synthetic private provider failure/);
  }
});

test('a registered version without a deployment refuses a fresh bootstrap without another write', async () => {
  const { calls, run } = cloudflareFixture({ submission: 'partial' });
  await assert.rejects(run(), /deployment/i);
  await assert.rejects(run(), /exactly 0 versions/);
  assert.equal(calls.filter(([method]) => method === 'POST').length, 1);
});

test('automatic rollback passes the selected maintenance UUID to the locked binary without npm fallback', () => {
  const seed = workflow.match(/- name: Require one-time production seed deployment\n[\s\S]+?        run: \|\n((?:          [^\n]*\n)+)/)?.[1];
  assert.ok(seed);
  for (const percentage of [100, 50]) {
    const fixture = JSON.stringify([{ versions: [{ percentage, version_id: version }] }]);
    const baseline = spawnSync('/bin/bash', ['-c', `npx() { printf '%s' '${fixture}'; }\n${seed.replace(/^ {10}/gm, '')}`], {
      encoding: 'utf8', timeout: 5_000, env: { PATH: process.env.PATH, WORKER_NAME: 'nntune-web' },
    });
    assert.equal(baseline.status, percentage === 100 ? 0 : 1, baseline.stderr);
    if (percentage === 100) assert.ok(baseline.stdout.endsWith('production seed ok\n'));
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

const rollbackFixture = { checkedAt, now, sha, version, workerId };

test('production rollout requires the verified seed and a unique attempt version before shifting traffic', async (t) => {
  const { calls, custody, versions, seed, temp, run } = finalSmokeFixture(t, rollbackFixture);
  await assert.rejects(run('check-seed', { ROLLBACK_CUSTODY: JSON.stringify({ ...custody, checked_at: checkedAt.replace('12:', '11:') }) }), /custody/);
  assert.deepEqual(calls, []);
  seed.bindings.push({ type: 'secret_text' });
  await assert.rejects(run('check-seed'), /Maintenance version drifted/);
  seed.bindings.pop();
  await run('check-seed');
  versions.push(versions[1]);
  await assert.rejects(run('record'), /did not resolve uniquely/);
  versions.pop();
  const times = [now(), now() + 31 * 60 * 1000];
  await assert.rejects(run('record', {}, () => times.shift()), /Stale production rollback custody/);
  assert.equal(existsSync(join(temp, 'production-rollback-intent.sealed')), false);
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
  state.name = 'other-app';
  await assert.rejects(run('restore'), /Worker identity/);
  state.name = 'nntune-web';
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

test('a regression during the release watch restores the verified previous Worker', async (t) => {
  const transport = finalSmokeFixture(t, rollbackFixture, 'ack', 'previous');
  await assert.rejects(transport.run('record', {}, now, async () => 1), /Previous build health/);
  await transport.run('record', {}, now, async (url, { expectedSha }) => {
    assert.deepEqual([url, expectedSha], ['https://test.example.com', transport.custody.previous.sha]);
    return 0;
  });
  transport.shift();
  await transport.run('pin-final', { PROMOTED_VERSION: transport.candidate, PREV_VERSION: version });
  const { fresh, enter } = recoveryFixture(t, transport, 'success');
  assert.match(await enter(fresh('13')), /previous Worker allocation verified/);
  assert.equal(transport.calls.filter((method) => method === 'POST').length, 1);
});

test('first-release switch-off recovers maintenance from a fresh runner and repeats without writes', async (t) => {
  const transport = finalSmokeFixture(t, { ...rollbackFixture, sha: mainSha() }, 'lost');
  assert.equal(transport.custody.previous, undefined);
  await transport.run('record');
  transport.shift();
  const { fresh, enter, claimedRecovery } = recoveryFixture(t, transport, 'cancelled');
  assert.equal(existsSync(join(transport.temp, 'production-final-allocation.sealed')), false);
  await assert.rejects(claimedRecovery(fresh('13'), now, false), /invalid or stale/);
  for (const age of [-1, 120_001, NaN]) {
    await assert.rejects(claimedRecovery(fresh('13'), () => now() + age), /proof expired/);
  }
  assert.ok(transport.calls.every((method) => method === 'GET'));
  transport.fail.nextRead = true;
  await assert.rejects(enter(fresh('13')), /Cloudflare request/);
  assert.match(await enter(fresh('14')), /reconciled by readback/);
  const afterRecovery = transport.calls.length;
  assert.match(await enter(fresh('15')), /already verified/);
  assert.match(await claimedRecovery(fresh('16'), now), /already verified/);
  assert.ok(transport.calls.slice(afterRecovery).every((method) => method === 'GET'));
  assert.deepEqual(transport.rows[0].versions, [{ version_id: version, percentage: 100 }]);
  assert.equal(transport.calls.filter((method) => method === 'POST').length, 1);
});

test('recovery refuses a readable rollback intent for another build', async (t) => {
  const pinned = finalSmokeFixture(t, rollbackFixture);
  const other = finalSmokeFixture(t, { ...rollbackFixture, sha: 'e'.repeat(40) });
  await pinned.prepare();
  await other.run('record');
  const { fresh, evidence } = recoveryFixture(t, pinned, 'success');
  const { evidence: otherEvidence } = recoveryFixture(t, other, 'success');
  const rollout = structuredClone(pinned.rows);
  const before = pinned.calls.length;
  const intent = 'production-rollback-intent.sealed';
  const plan = 'production-final-allocation.sealed';

  const pending = fresh('13');
  admitRecovery(pending, (repo) => {
    const selected = evidence(repo);
    const otherSource = otherEvidence(repo);
    const { run: otherRun } = otherSource.source('12', '1');
    return {
      ...selected,
      artifactFor(run, name) {
        assert.equal(run.head_sha, pinned.env.DEPLOY_SHA);
        return otherSource.artifactFor(otherRun, name);
      },
    };
  });
  assert.deepEqual(readFileSync(join(pending.RUNNER_TEMP, intent)), readFileSync(join(other.temp, intent)));
  await assert.rejects(pinned.run('prepare-recovery', pending), /Rollback intent or final allocation is missing, invalid or stale/);
  assert.equal(existsSync(join(pending.RUNNER_TEMP, plan)), false);
  assert.deepEqual(pinned.calls.slice(before), []);
  assert.deepEqual(pinned.rows, rollout);

  const exact = fresh('14');
  admitRecovery(exact, evidence);
  assert.deepEqual(readFileSync(join(exact.RUNNER_TEMP, intent)), readFileSync(join(pinned.temp, intent)));
  await pinned.run('prepare-recovery', exact);
  assert.ok(existsSync(join(exact.RUNNER_TEMP, plan)));
  assert.deepEqual(pinned.calls.slice(before), ['GET', 'GET', 'GET', 'GET']);
  assert.deepEqual(pinned.rows, rollout);
});

test('recovery refuses superseding deployments and any second unresolved submission', async (t) => {
  const transport = finalSmokeFixture(t, rollbackFixture, 'unresolved');
  await transport.run('record');
  transport.shift();
  const { fresh, enter } = recoveryFixture(t, transport, 'timed_out');
  await assert.rejects(enter(fresh('13')), /Restore result unresolved/);
  await assert.rejects(enter(fresh('14')), /readback only/);
  transport.rows.unshift({ ...transport.rows[0], id: transport.id(8), created_on: new Date(now() + 8000).toISOString() });
  await assert.rejects(enter(fresh('15')), /newer or foreign/);
  transport.rows[0].versions = [{ version_id: version, percentage: 100 }];
  await assert.rejects(enter(fresh('16')), /newer or foreign/);
  assert.equal(transport.calls.filter((method) => method === 'POST').length, 1);
});

test('recovery refuses unapproved or altered evidence and reconciles a prior final-smoke write only', async (t) => {
  const transport = finalSmokeFixture(t, rollbackFixture, 'unresolved');
  await transport.prepare();
  const { fresh, enter, artifact, evidence, steps } = recoveryFixture(t, transport, 'failure');
  assert.throws(() => admitRecovery({ ...fresh('13'), ACCEPTED_SHA: 'c'.repeat(40) }, evidence), /Invalid protected/);
  const digest = artifact.digest;
  artifact.digest = 'sha256:' + 'b'.repeat(64);
  assert.throws(() => admitRecovery(fresh('13'), evidence), /digest mismatch/);
  artifact.digest = digest;
  steps.push({ name: 'Restore recorded maintenance after failed final smoke', status: 'completed', conclusion: 'failure', number: 30 });
  await assert.rejects(enter(fresh('13')), /readback only/);
  assert.ok(transport.calls.every((method) => method === 'GET'));
});

test('protected recovery wiring keeps the production lock and contains source and provider errors', () => {
  const job = workflow.split('\n  recover-worker:\n')[1].split('\n  release:\n')[0];
  assert.match(job, /if: inputs.mode == 'recover-worker'\n    needs: \[preflight, staging-evidence\]/);
  assert.match(job, /group: production-worker-mutation\n      cancel-in-progress: false\n    environment: production/);
  const [proof, restore] = job.split('      - name: Verify the allocation before reserving a recovery submission');
  assert.doesNotMatch(proof, /secrets\.(?:PRODUCTION_CLOUDFLARE|PRODUCTION_ROLLBACK_CUSTODY)/);
  assert.match(restore, /CLAIM_VERIFIED: \$\{\{ steps\.claim\.outcome == 'success' \}\}[\s\S]*?prepare-recovery[\s\S]*?upload-artifact@[0-9a-f]{40}[\s\S]*?env: \*recovery-env[\s\S]*?rollback\.mjs recover/);
  for (const command of ['admit-recovery', 'recover']) {
    const result = recoveryError(command);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Protected rollback check failed/);
    assert.doesNotMatch(result.stdout + result.stderr, /fixture-private|Invalid protected/);
  }
});
