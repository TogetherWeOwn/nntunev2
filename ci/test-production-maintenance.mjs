import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { finalSmokeFixture, mainSha, recoveryError, recoveryFixture } from './fixtures/production-worker-transport.mjs';
import { seedTransport } from './fixtures/production-maintenance-seed.mjs';
import { admitRecovery, intentFile } from './production-maintenance/recovery-evidence.mjs';
import { seedMaintenance } from './production-maintenance/seed.mjs';
import worker from './production-maintenance/worker.mjs';
import promotionProof from './release-promotion-proof.cjs';

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

test('a successful first release hands its original intent to recovery preparation', async (t) => {
  const transport = finalSmokeFixture(t, rollbackFixture);
  const { candidate, custody, env, rows, run, temp } = transport;
  await transport.prepare();
  assert.deepEqual(rows[0].versions, [{ version_id: candidate, percentage: 100 }]);
  assert.equal(rows[3].id, custody.maintenance.deployment);
  assert.deepEqual(rows[3].versions, [{ version_id: custody.maintenance.version, percentage: 100 }]);

  const [admission, deploy] = workflow.split('\n  deploy-production:\n');
  const verify = admission.split('      - name: Verify candidate deployment and accepted journey receipt\n')[1];
  const candidatePin = 'DEPLOY_SHA: ${{ needs.preflight.outputs.sha }}';
  assert.ok(verify.includes(candidatePin));
  const production = deploy.split('\n  bootstrap-maintenance:\n')[0];
  const staging = readFileSync(new URL('../.github/workflows/deploy.yml', import.meta.url), 'utf8')
    .split('\n  post-deploy-private-music:\n')[1];
  const runAttempt = '${{ github.run_id }}-${{ github.run_attempt }}';
  const uploads = [
    [staging, 'Upload candidate admission receipt',
      'staging-admission-${{ github.sha }}-' + runAttempt + '-private-music', 'staging-admission.json'],
    [production, 'Preserve sealed rollback intent before shifting traffic',
      'production-rollback-intent-' + runAttempt, '${{ runner.temp }}/' + intentFile],
    [production, 'Upload production promotion proof',
      'production-promotion-${{ needs.preflight.outputs.sha }}-' + runAttempt, '${{ runner.temp }}/promotion.json'],
  ];
  for (const [job, step, name, path] of uploads) {
    const upload = job.split(`\n      - name: ${step}\n`)[1]?.split('\n      - ')[0];
    assert.ok(upload, `missing upload step: ${step}`);
    assert.match(upload, /^ {8}uses: actions\/upload-artifact@/m);
    assert.ok(upload.split('\n').includes(`          name: ${name}`), `wrong artifact name: ${step}`);
    assert.ok(upload.split('\n').includes(`          path: ${path}`), `wrong artifact path: ${step}`);
  }
  const [pin, publication] = production.split('      - name: Record successful production promotion\n');
  assert.match(pin, /PROMOTED_VERSION: \$\{\{ steps\.gradual\.outputs\.promoted-version-id \}\}/);
  const [recordStep] = publication.split('      - name: Upload production promotion proof\n');
  assert.ok(recordStep.includes(candidatePin));
  assert.match(recordStep, /PROMOTED_VERSION_ID: \$\{\{ steps\.gradual\.outputs\.promoted-version-id \}\}/);
  const record = recordStep.match(/node <<'NODE'\n([\s\S]*?)^ {10}NODE$/m);
  assert.ok(record, 'production must supply the promotion receipt writer');
  const result = spawnSync(process.execPath, ['-'], {
    input: record[1], encoding: 'utf8', timeout: 5_000,
    env: { DEPLOY_SHA: sha, PROMOTED_VERSION_ID: candidate, RUNNER_TEMP: temp,
      GITHUB_REPOSITORY: env.GITHUB_REPOSITORY, GITHUB_RUN_ID: env.GITHUB_RUN_ID,
      GITHUB_RUN_ATTEMPT: env.GITHUB_RUN_ATTEMPT },
  });
  assert.equal(result.status, 0, result.stderr);
  const receipt = promotionProof.validateReceipt(JSON.parse(readFileSync(join(temp, 'promotion.json'), 'utf8')), {
    repo: env.GITHUB_REPOSITORY, sha, runId: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT,
  });
  assert.equal(receipt.promoted_version_id, candidate);

  const { fresh, evidence } = recoveryFixture(t, transport, 'success');
  const recovery = { ...fresh('13'), DEPLOY_SHA: receipt.sha, RECOVERY_RUN_ID: receipt.run_id };
  const before = structuredClone(rows);
  admitRecovery(recovery, evidence);
  assert.match(readFileSync(recovery.GITHUB_OUTPUT, 'utf8'), /^allow-submit=true$/m);
  assert.deepEqual(readFileSync(join(recovery.RUNNER_TEMP, intentFile)), readFileSync(join(temp, intentFile)));
  const callCount = transport.calls.length;
  await run('prepare-recovery', recovery);
  assert.ok(existsSync(join(recovery.RUNNER_TEMP, 'production-final-allocation.sealed')));
  assert.deepEqual(transport.calls.slice(callCount), ['GET', 'GET', 'GET', 'GET']);
  assert.deepEqual(rows, before);
});

test('recovery preparation binds the admitted rollback intent to its Worker', async (t) => {
  const production = finalSmokeFixture(t, rollbackFixture);
  await production.prepare();
  const { fresh, evidence } = recoveryFixture(t, production, 'success');
  const sealedIntent = readFileSync(join(production.temp, intentFile));
  const rollout = structuredClone(production.rows);
  const allocation = 'production-final-allocation.sealed';
  const admit = (env) => {
    admitRecovery(env, evidence);
    assert.match(readFileSync(env.GITHUB_OUTPUT, 'utf8'), /^allow-submit=true$/m);
    assert.deepEqual(readFileSync(join(env.RUNNER_TEMP, intentFile)), sealedIntent);
  };

  await t.test('another configured Worker refuses the same archive before readback', async (t) => {
    const staging = finalSmokeFixture(t, { ...rollbackFixture, workerId: 'c'.repeat(32) });
    staging.state.name = 'nntune-web-staging';
    staging.shift();
    assert.deepEqual(staging.custody, { ...production.custody, worker: staging.env.WORKER_ID });
    assert.deepEqual(staging.rows, rollout);
    const recovery = { ...fresh('13'), WORKER_ID: staging.env.WORKER_ID };
    admit(recovery);

    await assert.rejects(staging.run('prepare-recovery', recovery), /Rollback intent or final allocation is missing, invalid or stale/);
    assert.equal(existsSync(join(recovery.RUNNER_TEMP, allocation)), false);
    assert.deepEqual(staging.calls, []);
    assert.deepEqual(staging.rows, rollout);
  });

  await t.test('the original Worker prepares the allocation from the same archive', async () => {
    const recovery = fresh('14');
    admit(recovery);
    const callCount = production.calls.length;

    await production.run('prepare-recovery', recovery);
    assert.equal(existsSync(join(recovery.RUNNER_TEMP, allocation)), true);
    assert.deepEqual(production.calls.slice(callCount), ['GET', 'GET', 'GET', 'GET']);
    assert.deepEqual(production.rows, rollout);
  });
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

test('recovery preparation requires the archived prior deployment identity', async (t) => {
  const pinned = finalSmokeFixture(t, rollbackFixture);
  const alternate = finalSmokeFixture(t, rollbackFixture);
  alternate.custody.maintenance.deployment = alternate.id(9);
  alternate.rows[0].id = alternate.custody.maintenance.deployment;
  await pinned.prepare();
  await alternate.run('record');
  const pinnedHistory = structuredClone(pinned.rows);

  for (const [archive, runId] of [[alternate, '13'], [pinned, '14']]) {
    const { fresh, evidence } = recoveryFixture(t, archive, 'success');
    const recovery = fresh(runId);
    admitRecovery(recovery, evidence);
    assert.match(readFileSync(recovery.GITHUB_OUTPUT, 'utf8'), /^allow-submit=true$/m);
    assert.deepEqual(readFileSync(join(recovery.RUNNER_TEMP, intentFile)), readFileSync(join(archive.temp, intentFile)));

    const allocation = join(recovery.RUNNER_TEMP, 'production-final-allocation.sealed');
    const callCount = pinned.calls.length;
    if (archive === alternate) {
      await assert.rejects(pinned.run('prepare-recovery', recovery), /Rollback intent or final allocation is missing, invalid or stale/);
      assert.equal(existsSync(allocation), false);
      assert.deepEqual(pinned.calls.slice(callCount), []);
    } else {
      await pinned.run('prepare-recovery', recovery);
      assert.ok(existsSync(allocation));
      assert.deepEqual(pinned.calls.slice(callCount), ['GET', 'GET', 'GET', 'GET']);
    }
  }
  assert.deepEqual(pinned.rows, pinnedHistory);
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

test('maintenance-fallback preflight refuses missing, corrupted or mismatched bundle checksums before any write', async (t) => {
  const { calls, custody, rows, temp, run } = finalSmokeFixture(t, rollbackFixture);
  const baseline = structuredClone(rows);
  const validDigest = custody.maintenance.module_sha256;
  const badCustodies = [
    ['missing module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: undefined } }],
    ['empty module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: '' } }],
    ['truncated module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: validDigest.slice(0, 63) } }],
    ['uppercase module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: validDigest.toUpperCase() } }],
    ['non-hex module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: 'g'.repeat(64) } }],
    ['prefixed module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: `sha256:${validDigest}` } }],
    ['mismatched module_sha256', { ...custody, maintenance: { ...custody.maintenance, module_sha256: '0'.repeat(64) } }],
    ['missing evidence_sha256', { ...custody, evidence_sha256: undefined }],
    ['corrupted evidence_sha256', { ...custody, evidence_sha256: 'not-a-sha256' }],
  ];
  for (const command of ['check-seed', 'record']) {
    for (const [label, badCustody] of badCustodies) {
      await assert.rejects(
        run(command, { ROLLBACK_CUSTODY: JSON.stringify(badCustody) }),
        /Missing or mismatched production rollback custody/,
        `${command}: ${label}`,
      );
      assert.deepEqual(calls, [], `${command}: ${label} must not contact Cloudflare`);
      assert.equal(existsSync(join(temp, intentFile)), false, `${command}: ${label} must not seal intent`);
      assert.deepEqual(rows, baseline);
    }
  }
  await run('check-seed');
  await run('record');
  assert.equal(existsSync(join(temp, intentFile)), true);
  assert.ok(calls.length > 0 && calls.every((method) => method === 'GET'));
  assert.deepEqual(rows, baseline);
});

test('maintenance-fallback preflight refuses missing or leaked protected-environment approval before any write', async (t) => {
  const preflight = workflow.match(/id: target\n        env:\n((?: {10}[^\n]*\n)+)        run: \|\n((?: {10}[^\n]*\n)+)/);
  assert.ok(preflight);
  assert.match(preflight[1], /^ {10}UNPROTECTED_ACCEPTED_SHA: \$\{\{ vars\.PRODUCTION_ACCEPTED_SHA \}\}$/m);
  assert.match(preflight[1], /^ {10}UNPROTECTED_WORKER_ID: \$\{\{ vars\.PRODUCTION_WORKER_ID \}\}$/m);
  assert.match(preflight[1], /^ {10}UNPROTECTED_ROUTE_PROOF: \$\{\{ vars\.PRODUCTION_BOOTSTRAP_ROUTE_PROOF \}\}$/m);

  const workspace = mkdtempSync(join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), 'maintenance-approval-'));
  t.after(() => rmSync(workspace, { recursive: true }));
  const preflightRefusals = [
    [{ UNPROTECTED_ACCEPTED_SHA: sha }, /PRODUCTION_ACCEPTED_SHA must exist only in the production Environment/],
    [{ UNPROTECTED_WORKER_ID: workerId }, /Worker ID and route proof must exist only in the production Environment/],
    [{ UNPROTECTED_ROUTE_PROOF: routeProof }, /Worker ID and route proof must exist only in the production Environment/],
    [{ GITHUB_REF: 'refs/heads/feature' }, /Dispatch on main/],
    [{ INPUT_SHA: '' }, /40-char lowercase hex/],
    [{ PRODUCTION_DOMAIN: 'production-placeholder.example.invalid' }, /Production hostname not decided/],
  ];
  for (const [index, [overrides, refusal]] of preflightRefusals.entries()) {
    const output = join(workspace, `output-${index}`);
    const result = spawnSync('/bin/bash', ['-c', preflight[2].replace(/^ {10}/gm, '')], {
      encoding: 'utf8', timeout: 5_000, env: {
        PATH: process.env.PATH, GITHUB_OUTPUT: output, GITHUB_REF: 'refs/heads/main', GITHUB_SHA: sha,
        INPUT_SHA: sha, DEPLOY_MODE: 'bootstrap-maintenance', PRODUCTION_DEPLOY_ENABLED: 'true',
        UNPROTECTED_ACCEPTED_SHA: '', UNPROTECTED_WORKER_ID: '', UNPROTECTED_ROUTE_PROOF: '',
        GITHUB_RUN_ATTEMPT: '1', PRODUCTION_DOMAIN: 'example.com',
        PRODUCTION_DOMAIN_DEFAULT: 'production-placeholder.example.invalid',
        ...overrides,
      },
    });
    assert.equal(result.status, 1);
    assert.match(result.stdout, refusal);
    assert.equal(existsSync(output) ? readFileSync(output, 'utf8') : '', '');
  }

  const gate = bootstrap.match(/steps:\n      - name: Require the exact Release Manager-accepted SHA\n(.*?)        run: \|\n((?: {10}[^\n]*\n)+)      - uses: actions\/checkout@/s);
  assert.ok(gate);
  assert.doesNotMatch(gate[1], /if:|secrets\./);
  assert.match(gate[1], /ACCEPTED_SHA: \$\{\{ vars\.PRODUCTION_ACCEPTED_SHA \}\}/);
  assert.match(bootstrap, /uses: actions\/checkout@[0-9a-f]{40}[\s\S]*?ref: \$\{\{ needs\.preflight\.outputs\.sha \}\}[\s\S]*?DEPLOY_SHA: \$\{\{ needs\.preflight\.outputs\.sha \}\}\n        run: node ci\/production-maintenance\/seed\.mjs/);
  for (const [env, stream, expected] of [
    [{ DEPLOY_SHA: sha, ACCEPTED_SHA: '', GITHUB_RUN_ATTEMPT: '1' }, 'stdout', /PRODUCTION_ACCEPTED_SHA in the production Environment/],
    [{ DEPLOY_SHA: sha, GITHUB_RUN_ATTEMPT: '1' }, 'stderr', /ACCEPTED_SHA: unbound variable/],
  ]) {
    const result = spawnSync('/bin/bash', ['-c', gate[2].replace(/^ {10}/gm, '')], {
      encoding: 'utf8', timeout: 5_000, env,
    });
    assert.equal(result.status, 1);
    assert.match(result[stream], expected);
  }

  const { calls: seedCalls, run: runSeed } = cloudflareFixture();
  for (const [override, pattern] of [
    [{ account: '' }, /Missing or invalid production account/],
    [{ worker: '' }, /Missing or invalid production account/],
    [{ token: '' }, /Missing or invalid production account/],
    [{ sha: '' }, /Missing or invalid production account/],
    [{ routeProof: '{not-json' }, /Missing or malformed custodian route proof/],
    [{ routeProof: routeProof.replace(workerId, 'c'.repeat(32)) }, /bound to this Worker and SHA/],
  ]) {
    await assert.rejects(runSeed(override), pattern);
  }
  assert.deepEqual(seedCalls, []);

  const transport = finalSmokeFixture(t, rollbackFixture);
  await transport.prepare();
  const { fresh } = recoveryFixture(t, transport, 'success');
  let evidenceCalls = 0;
  const countingEvidence = () => {
    evidenceCalls += 1;
    assert.fail('admitRecovery must refuse missing approval before reading Actions evidence');
  };
  const before = transport.calls.length;
  for (const accepted of ['', undefined]) {
    const recovery = { ...fresh('13'), ACCEPTED_SHA: accepted };
    assert.throws(() => admitRecovery(recovery, countingEvidence), /Invalid protected recovery dispatch/);
    await assert.rejects(transport.run('prepare-recovery', recovery), /Invalid protected recovery dispatch/);
    assert.equal(existsSync(join(recovery.RUNNER_TEMP, 'production-final-allocation.sealed')), false);
  }
  assert.equal(evidenceCalls, 0);
  assert.deepEqual(transport.calls.slice(before), []);
});

test('maintenance-fallback preflight refuses mismatched staging and candidate SHA pins before any write', async (t) => {
  const { calls, custody, rows, seed, temp, versions, run } = finalSmokeFixture(t, rollbackFixture);
  const baseline = structuredClone(rows);
  const otherSha = 'e'.repeat(40);

  for (const command of ['check-seed', 'record']) {
    await assert.rejects(
      run(command, { ROLLBACK_CUSTODY: JSON.stringify({ ...custody, sha: otherSha }) }),
      /Missing or mismatched production rollback custody/,
    );
    await assert.rejects(run(command, { GITHUB_SHA: otherSha }), /Invalid protected production rollback context/);
    await assert.rejects(run(command, { GITHUB_WORKFLOW_SHA: otherSha }), /Invalid protected production rollback context/);
  }
  assert.deepEqual(calls, []);

  const originalSeedTag = seed.annotations['workers/tag'];
  seed.annotations['workers/tag'] = otherSha;
  await assert.rejects(run('check-seed'), /Maintenance version drifted/);
  seed.annotations['workers/tag'] = originalSeedTag;

  const originalCandidateTag = versions[1].annotations['workers/tag'];
  versions[1].annotations['workers/tag'] = `${otherSha}-12-1`;
  await assert.rejects(run('record'), /Candidate tag did not resolve uniquely/);
  versions[1].annotations['workers/tag'] = originalCandidateTag;

  assert.equal(existsSync(join(temp, intentFile)), false);
  assert.ok(calls.length > 0 && calls.every((method) => method === 'GET'));
  assert.deepEqual(rows, baseline);

  const staging = workflow.split('\n  staging-evidence:\n')[1].split('\n  deploy-production:\n')[0];
  const inlinePy = staging.match(/python3 - <<'PY'\n([\s\S]*?)^ {10}PY$/m)?.[1]?.replace(/^ {10}/gm, '');
  assert.ok(inlinePy, 'staging-evidence must retain the maintenance staging prerequisite check');
  const runPrereq = (responses) => spawnSync('python3', ['-c', `
import io, json, sys, urllib.request
responses = json.loads(sys.argv[1])
script = sys.argv[2]
class FakeResponse(io.BytesIO):
    def __enter__(self): return self
    def __exit__(self, *args): return False
def fake_urlopen(req, timeout=15):
    url = req.full_url
    assert url.startswith("https://api.github.com/repos/example/fixture"), url
    path = url.removeprefix("https://api.github.com/repos/example/fixture")
    return FakeResponse(json.dumps(responses[path]).encode())
urllib.request.urlopen = fake_urlopen
exec(compile(script, "<staging-evidence>", "exec"), {"__name__": "__main__"})
`, JSON.stringify(responses), inlinePy], {
    encoding: 'utf8', timeout: 5_000,
    env: { PATH: process.env.PATH, REPO: 'example/fixture', DEPLOY_SHA: sha, GITHUB_TOKEN: 'fixture-only' },
  });
  const runsPath = '/actions/workflows/deploy.yml/runs?branch=main&event=push&per_page=100';
  const jobsPath = '/actions/runs/501/jobs?per_page=100';
  const goodRun = { id: 501, head_sha: sha, event: 'push', status: 'completed', conclusion: 'success' };
  const goodJobs = [
    { name: 'staging-web', conclusion: 'success' },
    { name: 'staging-api', conclusion: 'success' },
    { name: 'post-deploy-e2e / full journey (signup → library → meter → limits)', conclusion: 'success' },
  ];
  for (const [label, responses, expected] of [
    ['mismatched push head_sha', { [runsPath]: { workflow_runs: [{ ...goodRun, head_sha: otherSha }] } }, /No push-triggered deploy run on main/],
    ['non-success staging-web', { [runsPath]: { workflow_runs: [goodRun] }, [jobsPath]: { jobs: [{ name: 'staging-web', conclusion: 'skipped' }, goodJobs[1], goodJobs[2]] } }, /no staging-web success/],
    ['non-success full journey', { [runsPath]: { workflow_runs: [goodRun] }, [jobsPath]: { jobs: [goodJobs[0], goodJobs[1], { name: goodJobs[2].name, conclusion: 'failure' }] } }, /post-deploy-e2e jobs are not all success/],
    ['smoke-only without full journey', { [runsPath]: { workflow_runs: [goodRun] }, [jobsPath]: { jobs: [goodJobs[0], goodJobs[1], { name: 'post-deploy-e2e / smoke', conclusion: 'success' }] } }, /no full journey success/],
  ]) {
    const result = runPrereq(responses);
    assert.equal(result.status, 1, label);
    assert.match(result.stdout, expected, label);
  }
  const ok = runPrereq({ [runsPath]: { workflow_runs: [goodRun] }, [jobsPath]: { jobs: goodJobs } });
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /staging evidence job prerequisites ok/);
});
